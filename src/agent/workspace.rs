use super::{arg, err, files, Reply};
use serde_json::{json, Value};
use std::{
    fs,
    io::{Read, Seek, SeekFrom, Write},
    path::{Component, Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::Mutex,
    thread,
    time::{Duration, Instant},
};

fn normalize_windows_path(path: PathBuf) -> PathBuf {
    #[cfg(windows)]
    {
        let value = path.to_string_lossy().into_owned();
        if value.len() < 240 {
            if let Some(rest) = value.strip_prefix(r"\\?\UNC\") {
                return PathBuf::from(format!(r"\\{rest}"));
            }
            if let Some(rest) = value.strip_prefix(r"\\?\") {
                return PathBuf::from(rest);
            }
        }
    }
    path
}

// Resolve symlinks before processing "..", including when only a path prefix exists.
fn physical(path: &Path) -> Result<PathBuf, String> {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            _ => {
                out.push(part.as_os_str());
                match fs::symlink_metadata(&out) {
                    Ok(m) if m.file_type().is_symlink() => out = out.canonicalize().map_err(err)?,
                    Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(err(e)),
                    _ => {}
                }
            }
        }
    }
    // Normalize the Windows verbatim prefix for existing and new paths alike.
    if out.exists() {
        return out.canonicalize().map(normalize_windows_path).map_err(err);
    }
    Ok(normalize_windows_path(out))
}
pub(super) fn resolve(a: &Value) -> Reply {
    let root = normalize_windows_path(Path::new(arg(a, "ws")?).canonicalize().map_err(err)?);
    let path = physical(&root.join(arg(a, "path")?))?;
    let metadata = fs::metadata(&path).ok();
    Ok(
        json!({"path":path,"inside":path.starts_with(&root),"exists":metadata.is_some(),"is_file":metadata.as_ref().is_some_and(|m| m.is_file()),"is_dir":metadata.as_ref().is_some_and(|m| m.is_dir())}),
    )
}

fn require_string<'a>(a: &'a Value, key: &str) -> Result<&'a str, String> {
    match a.get(key) {
        Some(value) if value.is_string() => value
            .as_str()
            .ok_or_else(|| format!("参数 {key} 不是有效字符串")),
        Some(_) => Err(format!("参数 {key} 必须是字符串")),
        None => Err(format!("缺少参数 {key}")),
    }
}

fn validate_tool_args(name: &str, a: &Value) -> Result<(), String> {
    if !a.is_object() {
        return Err("工具参数必须是 JSON 对象".into());
    }
    match name {
        "list_files" => {
            if a.get("path").is_some() {
                require_string(a, "path")?;
            }
            if let Some(depth) = a.get("depth") {
                if !depth.is_u64() {
                    return Err("参数 depth 必须是整数".into());
                }
            }
        }
        "read_file" => {
            require_string(a, "path")?;
            for key in ["start_line", "max_lines"] {
                if let Some(value) = a.get(key) {
                    if !value.is_u64() {
                        return Err(format!("参数 {key} 必须是整数"));
                    }
                }
            }
        }
        "create_file" | "write_file" => {
            require_string(a, "path")?;
            require_string(a, "content")?;
        }
        "edit_file" => {
            require_string(a, "path")?;
            require_string(a, "old_text")?;
            require_string(a, "new_text")?;
            if let Some(value) = a.get("replace_all") {
                if !value.is_boolean() {
                    return Err("参数 replace_all 必须是布尔值".into());
                }
            }
        }
        "make_dir" | "delete_path" | "open_html" => {
            require_string(a, "path")?;
        }
        "search_files" => {
            require_string(a, "pattern")?;
            if let Some(value) = a.get("path") {
                if !value.is_string() {
                    return Err("参数 path 必须是字符串".into());
                }
            }
            if let Some(value) = a.get("file_types") {
                if !value.is_string() {
                    return Err("参数 file_types 必须是字符串".into());
                }
            }
        }
        "run_command" => {
            require_string(a, "command")?;
            if let Some(value) = a.get("timeout") {
                if !value.is_u64() {
                    return Err("参数 timeout 必须是整数".into());
                }
            }
        }
        _ => return Err(format!("未知工具 {name}")),
    }
    Ok(())
}

fn walk(
    root: &Path,
    depth: u64,
    visit: &mut impl FnMut(&Path, &fs::FileType) -> Result<(), String>,
    budget: &mut usize,
) -> Result<(), String> {
    if depth == 0 || *budget == 0 {
        return Ok(());
    }
    let mut entries = fs::read_dir(root)
        .map_err(err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(err)?;
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        if *budget == 0 {
            break;
        }
        let path = entry.path();
        let ty = entry.file_type().map_err(err)?;
        if ty.is_dir()
            && matches!(
                entry.file_name().to_str(),
                Some(".git" | "node_modules" | "target" | ".venv" | "__pycache__")
            )
        {
            continue;
        }
        *budget -= 1;
        visit(&path, &ty)?;
        if ty.is_dir() {
            walk(&path, depth - 1, visit, budget)?;
        }
    }
    Ok(())
}
// None prevents queued IPC workers from spawning a process during shutdown.
static SHELLS: Mutex<Option<Vec<Child>>> = Mutex::new(Some(Vec::new()));
fn terminate(child: &mut Child) {
    #[cfg(unix)]
    let _ = Command::new("/bin/kill")
        .args(["-KILL", &format!("-{}", child.id())])
        .output();
    #[cfg(windows)]
    let _ = Command::new("taskkill")
        .args(["/F", "/T", "/PID", &child.id().to_string()])
        .output();
    let _ = child.kill();
    let _ = child.wait();
}
pub fn shutdown() {
    if let Some(mut children) = SHELLS.lock().unwrap().take() {
        for child in &mut children {
            terminate(child);
        }
    }
}
fn shell(root: &Path, a: &Value) -> Reply {
    let command = arg(a, "command")?;
    #[cfg(windows)]
    let mut cmd = {
        let mut c = Command::new("powershell.exe");
        c.args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            &format!("[Console]::OutputEncoding=[System.Text.UTF8Encoding]::new(); {command}"),
        ]);
        c
    };
    #[cfg(unix)]
    let mut cmd = {
        use std::os::unix::process::CommandExt;
        let mut c = Command::new("sh");
        c.args(["-c", command]).process_group(0);
        c
    };
    let mut log = tempfile::tempfile().map_err(err)?;
    let id = {
        let mut guard = SHELLS.lock().map_err(err)?;
        let children = guard.as_mut().ok_or("应用正在退出")?;
        let child = cmd
            .current_dir(root)
            .stdin(Stdio::null())
            .stdout(log.try_clone().map_err(err)?)
            .stderr(log.try_clone().map_err(err)?)
            .spawn()
            .map_err(err)?;
        let id = child.id();
        children.push(child);
        id
    };
    let until =
        Instant::now() + Duration::from_secs(a["timeout"].as_u64().unwrap_or(120).clamp(1, 600));
    let status = loop {
        {
            let mut guard = SHELLS.lock().map_err(err)?;
            let children = guard.as_mut().ok_or("应用正在退出")?;
            let pos = children
                .iter()
                .position(|c| c.id() == id)
                .ok_or("命令已停止")?;
            match children[pos].try_wait() {
                Ok(Some(status)) => {
                    children.swap_remove(pos);
                    break status.to_string();
                }
                Ok(None) if Instant::now() >= until => {
                    terminate(&mut children.swap_remove(pos));
                    break "命令超时".into();
                }
                Err(e) => {
                    terminate(&mut children.swap_remove(pos));
                    return Err(err(e));
                }
                _ => {}
            }
        }
        thread::sleep(Duration::from_millis(40));
    };
    log.seek(SeekFrom::Start(0)).map_err(err)?;
    let mut bytes = Vec::new();
    log.take(65536).read_to_end(&mut bytes).map_err(err)?;
    Ok(json!(format!(
        "{status}\n{}",
        String::from_utf8_lossy(&bytes)
    )))
}

pub(super) fn run(ws: &Path, name: &str, a: &Value) -> Reply {
    let root = normalize_windows_path(ws.canonicalize().map_err(err)?);
    validate_tool_args(name, a)?;
    if name == "run_command" {
        return shell(&root, a);
    }
    let raw = root.join(a["path"].as_str().unwrap_or("."));
    let path = physical(&raw)?;
    if !path.starts_with(&root) {
        return Err("Agent 文件工具只能访问工作文件夹内的路径".into());
    }
    match name {
        "list_files" | "search_files" => {
            let search = name == "search_files";
            let depth = if search {
                128
            } else {
                a["depth"].as_u64().unwrap_or(3).clamp(1, 6)
            };
            let pattern = if search {
                arg(a, "pattern")?.to_lowercase()
            } else {
                String::new()
            };
            let types = a["file_types"]
                .as_str()
                .unwrap_or("")
                .split(',')
                .map(|s| s.trim().trim_start_matches('.').to_lowercase())
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>();
            let mut lines = Vec::new();
            let mut visit = |p: &Path, ty: &fs::FileType| {
                if lines.len() >= 2000 {
                    return Ok(());
                }
                let rel = p.strip_prefix(&path).unwrap_or(p).display().to_string();
                if !search {
                    lines.push(format!("{rel}{}", if ty.is_dir() { "/" } else { "" }));
                    return Ok(());
                }
                if !types.is_empty()
                    && !types.contains(
                        &p.extension()
                            .and_then(|s| s.to_str())
                            .unwrap_or("")
                            .to_lowercase(),
                    )
                {
                    return Ok(());
                }
                if rel.to_lowercase().contains(&pattern) {
                    lines.push(rel.clone());
                }
                if ty.is_file() && p.metadata().map(|m| m.len() <= 2_000_000).unwrap_or(false) {
                    if let Ok(s) = fs::read_to_string(p) {
                        for (n, l) in s.lines().enumerate() {
                            if lines.len() >= 2000 {
                                break;
                            }
                            if l.to_lowercase().contains(&pattern) {
                                lines.push(format!(
                                    "{rel}:{}: {}",
                                    n + 1,
                                    l.chars().take(500).collect::<String>()
                                ));
                            }
                        }
                    }
                }
                Ok(())
            };
            if path.is_file() {
                visit(&path, &path.metadata().map_err(err)?.file_type())?;
            } else {
                walk(&path, depth, &mut visit, &mut 20000)?;
            }
            Ok(json!(lines.join("\n")))
        }
        "read_file" => {
            let start = a["start_line"].as_u64().unwrap_or(1).max(1) as usize;
            let count = a["max_lines"].as_u64().unwrap_or(2000).clamp(1, 20000) as usize;
            let data = files::document(&path)?;
            Ok(json!(data
                .lines()
                .enumerate()
                .skip(start - 1)
                .take(count)
                .map(|(i, l)| format!("{}\t{l}", i + 1))
                .collect::<Vec<_>>()
                .join("\n")))
        }
        "create_file" => {
            let s = arg(a, "content")?;
            if path.is_dir() {
                return Err(format!(
                    "目标路径已是文件夹，不是网页文件：{}。请用 create_file 指定文件路径并写入内容；不要用 make_dir 创建 .html/.htm。",
                    path.display()
                ));
            }
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).map_err(err)?;
            }
            let mut file = match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(file) => file,
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    return Err(format!("文件已存在，未覆盖：{}", path.display()));
                }
                Err(e) => return Err(err(e)),
            };
            file.write_all(s.as_bytes()).map_err(err)?;
            Ok(json!(format!("已新建 {}", path.display())))
        }
        "write_file" => {
            let s = arg(a, "content")?;
            if path.is_dir() {
                return Err(format!(
                    "目标路径是文件夹，不是文件：{}。请改用具体文件名；不要用 make_dir 创建 .html/.htm。",
                    path.display()
                ));
            }
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).map_err(err)?;
            }
            fs::write(&path, s).map_err(err)?;
            Ok(json!(format!("已写入 {}", path.display())))
        }
        "edit_file" => {
            let s = fs::read_to_string(&path).map_err(err)?;
            let old = arg(a, "old_text")?;
            let new = arg(a, "new_text")?;
            if old.is_empty() || !s.contains(old) {
                return Err("未找到原文".into());
            }
            if !a["replace_all"].as_bool().unwrap_or(false) && s.matches(old).count() != 1 {
                return Err("原文出现多次，请增加上下文".into());
            }
            fs::write(&path, s.replace(old, new)).map_err(err)?;
            Ok(json!(format!("已修改 {}", path.display())))
        }
        "make_dir" => {
            if path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("html") || e.eq_ignore_ascii_case("htm"))
            {
                return Err(format!(
                    "不能把网页文件路径创建为文件夹：{}。请用 create_file 写入 HTML 内容。",
                    path.display()
                ));
            }
            fs::create_dir_all(&path).map_err(err)?;
            Ok(json!(format!("已创建 {}", path.display())))
        }
        "open_html" => {
            validate_html_file(&path)?;
            open::that(&path).map_err(err)?;
            Ok(json!(format!("已在默认浏览器中打开 {}", path.display())))
        }
        "delete_path" => {
            if path == root || path.parent().is_none() {
                return Err("不能删除工作文件夹或文件系统根目录".into());
            }
            let ty = fs::symlink_metadata(&raw).map_err(err)?.file_type();
            if ty.is_dir() {
                fs::remove_dir_all(&raw)
            } else {
                fs::remove_file(&raw)
            }
            .map_err(err)?;
            Ok(json!(format!("已删除 {}", raw.display())))
        }
        _ => Err(format!("未知工具 {name}")),
    }
}

fn validate_html_file(path: &Path) -> Result<(), String> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(format!(
                "HTML 文件不存在：{}。请先用 create_file 写入页面内容，再用 open_html 预览。",
                path.display()
            ));
        }
        Err(e) => return Err(err(e)),
    };
    if !metadata.is_file() {
        return Err(format!(
            "目标路径是文件夹，不是 HTML 文件：{}。请先用 create_file 写入页面内容，再用 open_html 预览。",
            path.display()
        ));
    }
    if !path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("html") || e.eq_ignore_ascii_case("htm"))
    {
        return Err("本地网页只能是 .html 或 .htm 文件".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_require_unique_matches_and_preserve_other_content() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        run(
            root,
            "write_file",
            &json!({"path":"dir/a.txt","content":"x\nx\ny"}),
        )
        .unwrap();
        let mut args = json!({"path":"dir/a.txt","old_text":"x","new_text":"z"});
        assert!(run(root, "edit_file", &args).is_err());
        args["replace_all"] = json!(true);
        run(root, "edit_file", &args).unwrap();
        assert_eq!(
            run(
                root,
                "read_file",
                &json!({"path":"dir/a.txt","start_line":2})
            )
            .unwrap(),
            "2\tz\n3\ty"
        );
    }

    #[test]
    fn create_read_and_write_files_inside_workspace() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("workspace");
        fs::create_dir(&root).unwrap();
        run(
            &root,
            "create_file",
            &json!({"path":"nested/notes.txt","content":"first\nsecond"}),
        )
        .unwrap();
        assert_eq!(
            run(&root, "read_file", &json!({"path":"nested/notes.txt"})).unwrap(),
            "1\tfirst\n2\tsecond"
        );
        assert!(run(
            &root,
            "create_file",
            &json!({"path":"nested/notes.txt","content":"do not overwrite"})
        )
        .is_err());
        run(
            &root,
            "write_file",
            &json!({"path":"nested/notes.txt","content":"updated"}),
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(root.join("nested/notes.txt")).unwrap(),
            "updated"
        );
    }

    #[test]
    fn file_tools_reject_paths_outside_workspace() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("workspace");
        fs::create_dir(&root).unwrap();
        let outside = temp.path().join("outside.txt");
        fs::write(&outside, "keep me").unwrap();

        assert!(run(&root, "read_file", &json!({"path":"../outside.txt"})).is_err());
        assert!(run(
            &root,
            "write_file",
            &json!({"path":"../outside.txt","content":"changed"})
        )
        .is_err());
        assert_eq!(fs::read_to_string(outside).unwrap(), "keep me");
    }

    #[test]
    fn open_html_accepts_only_existing_html_files() {
        let temp = tempfile::tempdir().unwrap();
        let html = temp.path().join("index.html");
        fs::write(&html, "<h1>Hello</h1>").unwrap();
        assert!(validate_html_file(&html).is_ok());
        assert!(validate_html_file(&temp.path().join("missing.html")).is_err());

        let text = temp.path().join("notes.txt");
        fs::write(&text, "notes").unwrap();
        assert!(validate_html_file(&text).is_err());
        assert!(validate_html_file(temp.path()).is_err());
    }

    #[test]
    fn html_paths_cannot_be_created_as_directories_and_create_file_explains_conflicts() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let created = run(root, "make_dir", &json!({"path":"index.html"})).unwrap_err();
        assert!(created.contains("create_file"));
        assert!(!root.join("index.html").exists());

        fs::create_dir(root.join("existing.html")).unwrap();
        let error = run(
            root,
            "create_file",
            &json!({"path":"existing.html","content":"<h1>Timer</h1>"}),
        )
        .unwrap_err();
        assert!(error.contains("文件夹"));
        assert!(error.contains("create_file"));
    }

    #[test]
    fn open_html_reports_missing_files_and_directory_targets_actionably() {
        let temp = tempfile::tempdir().unwrap();
        let missing = run(
            temp.path(),
            "open_html",
            &json!({"path":"missing/index.html"}),
        )
        .unwrap_err();
        assert!(missing.contains("文件不存在"));
        assert!(missing.contains("create_file"));

        let directory = temp.path().join("index.html");
        fs::create_dir(&directory).unwrap();
        let error = run(temp.path(), "open_html", &json!({"path":"index.html"})).unwrap_err();
        assert!(error.contains("文件夹"));
        assert!(error.contains("create_file"));
    }

    #[test]
    fn tool_arguments_require_declared_fields_and_types() {
        let temp = tempfile::tempdir().unwrap();
        assert!(run(temp.path(), "create_file", &json!({"path":"x.txt"})).is_err());
        assert!(run(
            temp.path(),
            "create_file",
            &json!({"path":"x.txt","content":3})
        )
        .is_err());
        assert!(run(
            temp.path(),
            "read_file",
            &json!({"path":"x.txt","max_lines":"2"})
        )
        .is_err());
        assert!(run(temp.path(), "make_dir", &json!({"path":3})).is_err());
        assert!(run(temp.path(), "not_a_tool", &json!({})).is_err());
    }

    #[test]
    fn resolve_reports_file_and_directory_types() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("index.html"), "<h1>ok</h1>").unwrap();
        fs::create_dir(temp.path().join("assets")).unwrap();
        let file = resolve(&json!({"ws":temp.path(),"path":"index.html"})).unwrap();
        assert_eq!(file["exists"], true);
        assert_eq!(file["is_file"], true);
        assert_eq!(file["is_dir"], false);
        let dir = resolve(&json!({"ws":temp.path(),"path":"assets"})).unwrap();
        assert_eq!(dir["is_file"], false);
        assert_eq!(dir["is_dir"], true);
    }

    #[cfg(unix)]
    #[test]
    fn new_files_through_external_symlinks_are_outside() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("ws");
        fs::create_dir(&root).unwrap();
        std::os::unix::fs::symlink(temp.path(), root.join("link")).unwrap();
        for path in ["../new", "link/new", "link/../new"] {
            assert_eq!(
                resolve(&json!({"ws":root,"path":path})).unwrap()["inside"],
                false
            );
            assert!(run(
                &root,
                "write_file",
                &json!({"path":path,"content":"must stay inside"})
            )
            .is_err());
        }
        assert_eq!(
            resolve(&json!({"ws":root,"path":"new/child"})).unwrap()["inside"],
            true
        );
    }
}

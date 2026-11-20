use crate::{Ctx, Reply};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};
use std::{
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        LazyLock, Mutex,
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tauri::{AppHandle, Manager};

mod catalog;
mod engine;

pub use engine::{pid, stop};
fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}
fn s<'a>(v: &'a Value, key: &str) -> &'a str {
    v[key].as_str().unwrap_or("")
}
fn n(v: &Value, key: &str) -> u64 {
    v[key].as_u64().unwrap_or(0)
}
fn arr(v: &Value) -> &[Value] {
    v.as_array().map(Vec::as_slice).unwrap_or_default()
}
fn ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}
fn ensure(p: PathBuf) -> Result<PathBuf, String> {
    fs::create_dir_all(&p).map_err(err)?;
    Ok(p)
}
fn images(ctx: &Ctx, app: &AppHandle) -> Result<PathBuf, String> {
    let settings = ctx.settings()?;
    ensure(if s(&settings, "images_dir").is_empty() {
        app.path()
            .picture_dir()
            .unwrap_or_else(|_| ctx.root.clone())
            .join("LocMd")
    } else {
        PathBuf::from(s(&settings, "images_dir"))
    })
}
fn loras(ctx: &Ctx) -> Result<PathBuf, String> {
    ensure(ctx.root.join("loras"))
}
fn ext(p: &Path) -> String {
    p.extension()
        .and_then(|v| v.to_str())
        .unwrap_or("")
        .to_lowercase()
}
fn mime(p: &Path) -> Result<&'static str, String> {
    match ext(p).as_str() {
        "png" => Ok("image/png"),
        "jpeg" | "jpg" => Ok("image/jpeg"),
        "webp" => Ok("image/webp"),
        "gif" => Ok("image/gif"),
        "bmp" => Ok("image/bmp"),
        "avif" => Ok("image/avif"),
        "webm" => Ok("video/webm"),
        "mp4" => Ok("video/mp4"),
        _ => Err("不支持这种媒体文件".into()),
    }
}
fn decode(data: &str) -> Result<Vec<u8>, String> {
    STANDARD
        .decode(data.split_once(',').map(|(_, b)| b).unwrap_or(data))
        .map_err(err)
}
fn new_file(dir: &Path, prefix: &str, suffix: &str, bytes: &[u8]) -> Result<PathBuf, String> {
    let mut file = tempfile::Builder::new()
        .prefix(prefix)
        .suffix(suffix)
        .tempfile_in(dir)
        .map_err(err)?;
    file.write_all(bytes).map_err(err)?;
    Ok(file.keep().map_err(err)?.1)
}
fn copy_new(from: &Path, to: &Path) -> Result<(), String> {
    if to.exists() {
        if from.canonicalize().map_err(err)? == to.canonicalize().map_err(err)? {
            return Ok(());
        }
        return Err(format!("目标文件已存在：{}", to.display()));
    }
    let mut file =
        tempfile::NamedTempFile::new_in(to.parent().ok_or("缺少目标目录")?).map_err(err)?;
    io::copy(&mut fs::File::open(from).map_err(err)?, &mut file).map_err(err)?;
    file.persist_noclobber(to).map_err(err)?;
    Ok(())
}
static HISTORY: Mutex<()> = Mutex::new(());
fn update_history(ctx: &Ctx, new: &[Value]) -> Reply {
    let _guard = HISTORY.lock().map_err(err)?;
    let mut history = ctx.read("images", json!([]))?;
    history
        .as_array_mut()
        .ok_or("作品记录格式无效")?
        .splice(0..0, new.iter().cloned());
    ctx.write("images", &history)?;
    Ok(json!(new))
}
fn delete(ctx: &Ctx, path: &Path) -> Reply {
    let _guard = HISTORY.lock().map_err(err)?;
    let mut history = ctx.read("images", json!([]))?;
    let rows = history.as_array_mut().ok_or("作品记录格式无效")?;
    let count = rows.len();
    rows.retain(|r| Path::new(s(r, "path")) != path);
    if rows.len() == count {
        return Err("作品记录中没有这个文件".into());
    }
    if !path.exists() {
        ctx.write("images", &history)?;
        return Ok(Value::Null);
    }
    let dir = tempfile::tempdir_in(path.parent().ok_or("作品路径无效")?).map_err(err)?;
    let staged = dir.path().join("deleted");
    fs::rename(path, &staged).map_err(err)?;
    if let Err(e) = ctx.write("images", &history) {
        if let Err(restore) = fs::rename(&staged, path) {
            let _ = dir.keep();
            return Err(format!(
                "{e}。还原失败：{restore}。作品保存在 {}",
                staged.display()
            ));
        }
        return Err(e);
    }
    Ok(Value::Null)
}

static CLOUD_VIDEO_CANCEL: AtomicBool = AtomicBool::new(false);
static CLOUD_VIDEO_JOB: Mutex<()> = Mutex::new(());

fn supported_cloud_host(host: &str) -> bool {
    if matches!(
        host,
        "dashscope.aliyuncs.com"
            | "dashscope-intl.aliyuncs.com"
            | "dashscope-us.aliyuncs.com"
            | "cn-hongkong.dashscope.aliyuncs.com"
    ) {
        return true;
    }
    let labels = host.split('.').collect::<Vec<_>>();
    labels.len() == 5
        && !labels[0].is_empty()
        && labels[0]
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-')
        && matches!(
            labels[1],
            "cn-beijing"
                | "ap-southeast-1"
                | "ap-northeast-1"
                | "eu-central-1"
                | "us-east-1"
                | "cn-hongkong"
        )
        && labels[2..] == ["maas", "aliyuncs", "com"]
}

fn cloud_api(ctx: &Ctx, id: &str) -> Result<(String, String), String> {
    let profiles = ctx.read("apis", json!([]))?;
    let profile = arr(&profiles)
        .iter()
        .find(|p| s(p, "id") == id)
        .ok_or("找不到所选 API 配置")?;
    let key = s(profile, "key");
    if key.is_empty() {
        return Err("所选 API 配置没有填写 Key".into());
    }
    let base = reqwest::Url::parse(s(profile, "base_url")).map_err(|_| "API 地址无效")?;
    let host = base.host_str().ok_or("API 地址缺少主机名")?;
    if base.scheme() != "https" || !supported_cloud_host(host) || base.port().is_some() {
        return Err("云端媒体首版仅支持 HTTPS 阿里云百炼（DashScope）API 配置；请在 API 设置中使用百炼地域对应的兼容地址和 Key".into());
    }
    Ok((format!("https://{host}/api/v1"), key.to_owned()))
}

fn cloud_client() -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(30))
        .timeout(Duration::from_secs(600))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(err)
}

fn cloud_json(response: reqwest::blocking::Response, key: &str) -> Result<Value, String> {
    let status = response.status();
    let text = response.text().map_err(err)?;
    if !status.is_success() {
        let message = text.replace(key, "[Key 已隐藏]");
        return Err(format!(
            "云端 API 返回 HTTP {status}：{}",
            message.chars().take(1200).collect::<String>()
        ));
    }
    serde_json::from_str(&text).map_err(err)
}

fn cloud_asset(client: &reqwest::blocking::Client, url: &str) -> Result<Vec<u8>, String> {
    let parsed = reqwest::Url::parse(url).map_err(|_| "云端返回的作品链接无效")?;
    if parsed.scheme() != "https"
        || !parsed
            .host_str()
            .is_some_and(|host| host == "aliyuncs.com" || host.ends_with(".aliyuncs.com"))
    {
        return Err("云端返回了不受支持的作品下载地址".into());
    }
    let response = client.get(parsed).send().map_err(err)?;
    if !response.status().is_success() {
        return Err(format!("下载云端作品失败：HTTP {}", response.status()));
    }
    const MAX_ASSET_BYTES: u64 = 512 * 1024 * 1024;
    if response
        .content_length()
        .is_some_and(|n| n > MAX_ASSET_BYTES)
    {
        return Err("云端作品超过 512 MB，已停止下载".into());
    }
    let mut bytes = Vec::new();
    response
        .take(MAX_ASSET_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(err)?;
    if bytes.is_empty() || bytes.len() > 512 * 1024 * 1024 {
        return Err("云端作品为空或超过 512 MB".into());
    }
    Ok(bytes)
}

fn cloud_seed(req: &Value) -> Option<u64> {
    req["seed"].as_u64().filter(|seed| *seed <= 2_147_483_647)
}

fn image_output_ext(bytes: &[u8]) -> Result<&'static str, String> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Ok(".png")
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Ok(".jpg")
    } else if bytes.get(8..12) == Some(b"WEBP") {
        Ok(".webp")
    } else {
        Err("百炼返回了无法识别的图片格式".into())
    }
}

fn image_prompt(prompt: &str, negative: &str) -> Result<String, String> {
    let negative = negative.trim();
    let combined = if negative.is_empty() {
        prompt.to_owned()
    } else {
        format!("{prompt}\n\n请避免画面中出现：{negative}")
    };
    if combined.chars().count() > 5000 {
        return Err("云端图像提示词最多 5000 个字符（含避免内容）".into());
    }
    Ok(combined)
}

fn cloud_image_request(req: &Value, model: &str) -> Result<(Value, u64, u64, u64), String> {
    let prompt = s(req, "prompt").trim();
    if prompt.is_empty() {
        return Err("请先写图片描述".into());
    }
    let prompt = image_prompt(prompt, s(req, "negative_prompt"))?;
    let width = n(req, "width");
    let height = n(req, "height");
    let count = n(req, "count").clamp(1, 4);
    let pixels = width.saturating_mul(height);
    if width < 256
        || height < 256
        || width > 4096
        || height > 4096
        || !(768 * 768..=4096 * 4096).contains(&pixels)
        || width > height.saturating_mul(8)
        || height > width.saturating_mul(8)
    {
        return Err("云端图片尺寸超出百炼支持范围（总像素约 768²–4096²，宽高比不超过 8:1）".into());
    }
    if model == "wan2.7-image" && pixels > 2048 * 2048 {
        return Err("wan2.7-image 最多支持约 2K 图片；请选择 wan2.7-image-pro 或降低清晰度".into());
    }
    let has_ref = !s(req, "ref_image").is_empty();
    let mut content = vec![];
    if has_ref {
        content.push(json!({"image":s(req, "ref_image")}));
    }
    content.push(json!({"text":prompt}));
    let mut parameters = json!({
        "size": format!("{width}*{height}"),
        "n": if has_ref { 1 } else { count },
        "watermark": false
    });
    if !has_ref {
        parameters["thinking_mode"] = json!(true);
    }
    if let Some(seed) = cloud_seed(req) {
        parameters["seed"] = json!(seed);
    }
    Ok((
        json!({
            "model":model,
            "input":{"messages":[{"role":"user","content":content}]},
            "parameters":parameters
        }),
        width,
        height,
        count,
    ))
}

fn cloud_image(a: &Value, ctx: &Ctx, app: &AppHandle) -> Reply {
    let req = &a["req"];
    let prompt = s(req, "prompt").trim();
    let (base, key) = cloud_api(ctx, s(req, "api_id"))?;
    let model = match s(req, "model") {
        "wan2.7-image" => "wan2.7-image",
        "wan2.7-image-pro" | "" => "wan2.7-image-pro",
        _ => return Err("不支持的云端图像模型".into()),
    };
    let (body, width, height, count) = cloud_image_request(req, model)?;
    let client = cloud_client()?;
    let started = std::time::Instant::now();
    let response = client
        .post(format!(
            "{base}/services/aigc/multimodal-generation/generation"
        ))
        .bearer_auth(&key)
        .json(&body)
        .send()
        .map_err(err)?;
    let result = cloud_json(response, &key)?;
    let mut urls = vec![];
    for choice in arr(&result["output"]["choices"]) {
        for part in arr(&choice["message"]["content"]) {
            if let Some(url) = part["image"].as_str().filter(|url| !url.is_empty()) {
                urls.push(url.to_owned());
            }
        }
    }
    if urls.is_empty() {
        return Err(result["message"]
            .as_str()
            .unwrap_or("云端接口没有返回图片")
            .to_owned());
    }
    let output = images(ctx, app)?;
    let mut saved = vec![];
    for url in urls.into_iter().take(count as usize) {
        let bytes = if url.starts_with("data:") {
            decode(&url)?
        } else {
            cloud_asset(&client, &url)?
        };
        let path = new_file(&output, "LocMd_cloud_", image_output_ext(&bytes)?, &bytes)?;
        saved.push(json!({
            "path": path, "kind":"image", "model":format!("百炼 · {model}"), "model_id":model,
            "prompt":prompt, "negative_prompt":s(req,"negative_prompt"), "width":width, "height":height,
            "seed":req["seed"], "secs":started.elapsed().as_secs_f64(), "created_at":ms() as u64,
            "source":"cloud"
        }));
    }
    if saved.is_empty() {
        return Err("云端没有返回可保存的图片".into());
    }
    update_history(ctx, &saved)?;
    Ok(json!(saved))
}

fn cloud_video_request(
    req: &Value,
    model: &str,
) -> Result<(Value, &'static str, &'static str, u64), String> {
    let prompt = s(req, "prompt").trim();
    if prompt.is_empty() {
        return Err("请先写视频描述".into());
    }
    let width = n(req, "width");
    let height = n(req, "height");
    if width == 0 || height == 0 || width == height {
        return Err("云端视频尺寸无效".into());
    }
    let ratio = if width > height { "16:9" } else { "9:16" };
    let resolution = if s(req, "resolution") == "1080P" {
        "1080P"
    } else {
        "720P"
    };
    let duration = n(req, "duration").clamp(2, 15);
    let mut input = json!({"prompt":prompt});
    if !s(req, "negative_prompt").trim().is_empty() {
        input["negative_prompt"] = json!(s(req, "negative_prompt").trim());
    }
    let mut parameters = json!({
        "resolution": resolution, "ratio": ratio, "duration": duration,
        "prompt_extend": false, "watermark": false
    });
    if let Some(seed) = cloud_seed(req) {
        parameters["seed"] = json!(seed);
    }
    Ok((
        json!({"model":model,"input":input,"parameters":parameters}),
        ratio,
        resolution,
        duration,
    ))
}

fn cloud_video(a: &Value, ctx: &Ctx, app: &AppHandle) -> Reply {
    let req = &a["req"];
    let _guard = CLOUD_VIDEO_JOB
        .try_lock()
        .map_err(|_| "已有云端视频任务正在等待")?;
    CLOUD_VIDEO_CANCEL.store(false, Ordering::SeqCst);
    let (base, key) = cloud_api(ctx, s(req, "api_id"))?;
    let model = match s(req, "model") {
        "wan2.7-t2v" | "" => "wan2.7-t2v",
        "wan2.7-t2v-2026-06-12" => "wan2.7-t2v-2026-06-12",
        _ => return Err("不支持的云端视频模型".into()),
    };
    let (body, ratio, resolution, duration) = cloud_video_request(req, model)?;
    let client = cloud_client()?;
    let started = std::time::Instant::now();
    let response = client
        .post(format!(
            "{base}/services/aigc/video-generation/video-synthesis"
        ))
        .bearer_auth(&key)
        .header("X-DashScope-Async", "enable")
        .json(&body)
        .send()
        .map_err(err)?;
    let mut state = cloud_json(response, &key)?;
    let task_id = state["output"]["task_id"]
        .as_str()
        .or_else(|| state["task_id"].as_str())
        .filter(|id| !id.is_empty())
        .ok_or_else(|| {
            state["message"]
                .as_str()
                .unwrap_or("百炼没有返回视频任务编号")
                .to_owned()
        })?
        .to_owned();
    let deadline = std::time::Instant::now() + Duration::from_secs(60 * 60);
    let video_url = loop {
        if CLOUD_VIDEO_CANCEL.load(Ordering::SeqCst) {
            return Err(
                "已停止等待。云端任务可能仍在运行，费用也可能继续产生；请到百炼控制台查看任务状态"
                    .into(),
            );
        }
        let status = state["output"]["task_status"]
            .as_str()
            .or_else(|| state["status"].as_str())
            .unwrap_or("");
        match status {
            "SUCCEEDED" => {
                let url = state["output"]["video_url"]
                    .as_str()
                    .or_else(|| state["results"][0]["video_url"].as_str())
                    .filter(|url| !url.is_empty())
                    .ok_or("云端任务成功但没有返回视频链接")?;
                break url.to_owned();
            }
            "FAILED" | "CANCELED" | "CANCELLED" => {
                return Err(state["message"]
                    .as_str()
                    .or_else(|| state["output"]["message"].as_str())
                    .unwrap_or("云端视频任务失败或已取消")
                    .to_owned());
            }
            "PENDING" | "RUNNING" | "" => {}
            other => return Err(format!("百炼返回未知视频任务状态：{other}")),
        }
        if std::time::Instant::now() >= deadline {
            return Err("云端视频仍未完成，已停止轮询；任务可能继续运行并产生费用".into());
        }
        for _ in 0..150 {
            if CLOUD_VIDEO_CANCEL.load(Ordering::SeqCst) {
                return Err("已停止等待。云端任务可能仍在运行，费用也可能继续产生；请到百炼控制台查看任务状态".into());
            }
            thread::sleep(Duration::from_millis(100));
        }
        let response = client
            .get(format!("{base}/tasks/{task_id}"))
            .bearer_auth(&key)
            .send()
            .map_err(err)?;
        state = cloud_json(response, &key)?;
    };
    let bytes = cloud_asset(&client, &video_url)?;
    let output = images(ctx, app)?;
    let path = new_file(&output, "LocMd_cloud_", ".mp4", &bytes)?;
    let (out_w, out_h) = match (resolution, ratio) {
        ("1080P", "16:9") => (1920, 1080),
        ("1080P", _) => (1080, 1920),
        (_, "16:9") => (1280, 720),
        _ => (720, 1280),
    };
    let saved = vec![json!({
        "path":path, "kind":"video", "model":format!("百炼 · {model}"), "model_id":model,
        "prompt":s(req,"prompt"), "negative_prompt":s(req,"negative_prompt"), "width":out_w, "height":out_h,
        "fps":1, "frames":duration, "secs":started.elapsed().as_secs_f64(), "created_at":ms() as u64,
        "source":"cloud"
    })];
    update_history(ctx, &saved)?;
    Ok(json!(saved))
}

fn list_loras(ctx: &Ctx) -> Reply {
    let mut rows = vec![];
    for file in fs::read_dir(loras(ctx)?).map_err(err)? {
        let p = file.map_err(err)?.path();
        if p.is_file() && matches!(ext(&p).as_str(), "safetensors" | "gguf") {
            rows.push(json!({"name":p.file_stem().unwrap_or_default().to_string_lossy(),"file":p.file_name().unwrap_or_default().to_string_lossy(),"path":p,"size":p.metadata().map_err(err)?.len()}))
        }
    }
    rows.sort_by(|a, b| s(a, "name").cmp(s(b, "name")));
    Ok(json!(rows))
}
fn import_loras(ctx: &Ctx) -> Reply {
    let dir = loras(ctx)?;
    let files = rfd::FileDialog::new()
        .add_filter("LoRA", &["safetensors", "gguf"])
        .pick_files()
        .unwrap_or_default();
    let mut names = vec![];
    for p in files {
        copy_new(&p, &dir.join(p.file_name().ok_or("文件名无效")?))?;
        names.push(json!(p.file_stem().unwrap_or_default().to_string_lossy()));
    }
    Ok(json!(names))
}
fn upload(ctx: &Ctx, data: &str) -> Reply {
    let bytes = decode(data)?;
    let ext = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        ".png"
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        ".jpg"
    } else if bytes.starts_with(b"GIF8") {
        ".gif"
    } else if bytes.get(8..12) == Some(b"WEBP") {
        ".webp"
    } else if bytes.get(4..12) == Some(b"ftypavif") || bytes.get(4..12) == Some(b"ftypavis") {
        ".avif"
    } else if bytes.starts_with(b"BM") {
        ".bmp"
    } else {
        return Err("上传的数据不是支持的图片格式".into());
    };
    Ok(json!(new_file(
        &ensure(ctx.root.join("uploads"))?,
        "upload_",
        ext,
        &bytes
    )?))
}
pub fn handle(cmd: &str, a: &Value, ctx: &Ctx, app: &AppHandle) -> Option<Reply> {
    Some(match cmd {
        "read_image" => (|| {
            let p = Path::new(s(a, "path"));
            Ok(json!(format!(
                "data:{};base64,{}",
                mime(p)?,
                STANDARD.encode(fs::read(p).map_err(err)?)
            )))
        })(),
        "images_dir_path" => images(ctx, app).map(|dir| json!(dir)),
        "open_images_dir" => images(ctx, app)
            .and_then(|dir| open::that(dir).map_err(err))
            .map(|_| Value::Null),
        "save_upload_image" => upload(ctx, s(a, "data")),
        "pick_image_files" => Ok(json!(rfd::FileDialog::new()
            .add_filter(
                "图片",
                &["png", "jpg", "jpeg", "webp", "gif", "bmp", "avif"]
            )
            .pick_files()
            .unwrap_or_default())),
        "image_history" => ctx.read("images", json!([])),
        "delete_image" => delete(ctx, Path::new(s(a, "path"))),
        "generate_cloud_image" => cloud_image(a, ctx, app),
        "generate_cloud_video" => cloud_video(a, ctx, app),
        "cancel_cloud_video_wait" => {
            CLOUD_VIDEO_CANCEL.store(true, Ordering::SeqCst);
            Ok(Value::Null)
        }
        "lora_dir_path" => loras(ctx).map(|dir| json!(dir)),
        "import_lora" => import_loras(ctx),
        "list_loras" => list_loras(ctx),
        _ => return catalog::handle(cmd, a, ctx, app).or_else(|| engine::handle(cmd, a, ctx, app)),
    })
}
pub use catalog::cancel_download;

#[cfg(test)]
mod cloud_tests {
    use super::*;

    #[test]
    fn cloud_host_allowlist_accepts_dashscope_and_workspace_endpoints() {
        assert!(supported_cloud_host("dashscope.aliyuncs.com"));
        assert!(supported_cloud_host(
            "ws-example.cn-beijing.maas.aliyuncs.com"
        ));
        assert!(supported_cloud_host(
            "trial.ap-southeast-1.maas.aliyuncs.com"
        ));
        assert!(!supported_cloud_host("evil.example.com"));
        assert!(!supported_cloud_host("evil.maas.aliyuncs.com"));
        assert!(!supported_cloud_host(
            "ws-example.unknown-region.maas.aliyuncs.com"
        ));
    }

    #[test]
    fn image_request_uses_supported_fields_and_preserves_avoid_text() {
        let req = json!({
            "prompt":"樱花树下的猫",
            "negative_prompt":"水印，模糊",
            "width":1024,
            "height":1024,
            "count":2,
            "seed":7
        });
        let (body, width, height, count) = cloud_image_request(&req, "wan2.7-image-pro").unwrap();
        assert_eq!((width, height, count), (1024, 1024, 2));
        assert_eq!(body["model"], "wan2.7-image-pro");
        assert_eq!(body["parameters"]["n"], 2);
        assert_eq!(body["parameters"]["seed"], 7);
        assert!(body["parameters"]["negative_prompt"].is_null());
        assert_eq!(
            body["input"]["messages"][0]["content"][0]["text"],
            "樱花树下的猫\n\n请避免画面中出现：水印，模糊"
        );
    }

    #[test]
    fn image_request_rejects_unsupported_resolution_for_base_model() {
        let req = json!({"prompt":"测试","width":2049,"height":2048});
        assert!(cloud_image_request(&req, "wan2.7-image").is_err());
    }

    #[test]
    fn video_request_matches_wan_async_schema() {
        let req = json!({
            "prompt":"一只猫跑过草地",
            "negative_prompt":"字幕",
            "width":1280,
            "height":720,
            "resolution":"720P",
            "duration":20,
            "seed":12
        });
        let (body, ratio, resolution, duration) = cloud_video_request(&req, "wan2.7-t2v").unwrap();
        assert_eq!((ratio, resolution, duration), ("16:9", "720P", 15));
        assert_eq!(body["input"]["prompt"], "一只猫跑过草地");
        assert_eq!(body["input"]["negative_prompt"], "字幕");
        assert_eq!(body["parameters"]["resolution"], "720P");
        assert_eq!(body["parameters"]["duration"], 15);
        assert_eq!(body["parameters"]["seed"], 12);
        assert!(body["parameters"]["negative_prompt"].is_null());
    }

    #[test]
    fn image_output_extension_uses_actual_bytes() {
        assert_eq!(image_output_ext(b"\x89PNG\r\n\x1a\nrest"), Ok(".png"));
        assert_eq!(image_output_ext(&[0xff, 0xd8, 0xff, 0]), Ok(".jpg"));
        assert_eq!(image_output_ext(b"RIFFxxxxWEBPrest"), Ok(".webp"));
        assert!(image_output_ext(b"not an image").is_err());
    }
}

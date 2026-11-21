use super::{err, Reply};
use reqwest::{blocking::Client, Url};
use scraper::{Html, Selector};
use serde_json::json;
use std::{io::Read, time::Duration};

fn get(url: Url) -> Result<(String, String), String> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err("仅支持 HTTP 或 HTTPS".into());
    }
    let response = Client::builder()
        .timeout(Duration::from_secs(25))
        .user_agent("Mozilla/5.0 (compatible; LocMd/3.1)")
        .build()
        .map_err(err)?
        .get(url)
        .send()
        .map_err(err)?
        .error_for_status()
        .map_err(err)?;
    let mime = response
        .headers()
        .get("content-type")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("")
        .to_lowercase();
    let mut data = Vec::new();
    response
        .take(4_000_000)
        .read_to_end(&mut data)
        .map_err(err)?;
    Ok((mime, String::from_utf8_lossy(&data).into_owned()))
}

fn webpage_url(raw: &str) -> Result<Url, String> {
    let url = Url::parse(raw).map_err(|_| {
        "网页地址必须是完整的 HTTP(S) 地址；打开工作文件夹里的本地 HTML 请使用 open_html 工具"
            .to_owned()
    })?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("只支持 HTTP 或 HTTPS 网页；本地 HTML 请使用 open_html 工具".into());
    }
    Ok(url)
}

pub(super) fn fetch(url: &str) -> Reply {
    let (mime, data) = get(webpage_url(url)?)?;
    let content = if mime.contains("html") {
        let doc = Html::parse_document(&data);
        let root = doc
            .select(&Selector::parse("main,article").unwrap())
            .next()
            .or_else(|| doc.select(&Selector::parse("body").unwrap()).next())
            .unwrap_or(doc.root_element());
        root.descendants()
            .filter_map(|n| {
                let t = n.value().as_text()?;
                if n.ancestors()
                    .filter_map(|p| p.value().as_element())
                    .any(|e| matches!(e.name(), "script" | "style" | "noscript"))
                {
                    return None;
                }
                let s = t.trim();
                (!s.is_empty()).then_some(s)
            })
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        data
    };
    Ok(json!(content.chars().take(60000).collect::<String>()))
}
pub(super) fn search(query: &str) -> Reply {
    let url = Url::parse_with_params(
        "https://www.bing.com/search",
        &[("format", "rss"), ("count", "10"), ("q", query)],
    )
    .map_err(err)?;
    if let Ok((_, rss)) = get(url) {
        let mut reader = quick_xml::Reader::from_str(&rss);
        use quick_xml::events::Event;
        let mut in_item = false;
        let mut field = false;
        let mut parts = Vec::new();
        loop {
            match reader.read_event() {
                Ok(Event::Start(e)) => match e.name().as_ref() {
                    b"item" => in_item = true,
                    b"title" | b"link" | b"description" => field = in_item,
                    _ => {}
                },
                Ok(Event::Text(t)) if field => parts.push(t.decode().map_err(err)?.into_owned()),
                Ok(Event::CData(t)) if field => parts.push(t.decode().map_err(err)?.into_owned()),
                Ok(Event::End(e)) => {
                    field = false;
                    if e.name().as_ref() == b"item" {
                        in_item = false;
                        parts.push(String::new());
                    }
                }
                Ok(Event::Eof) | Err(_) => break,
                _ => {}
            }
        }
        if !parts.is_empty() {
            return Ok(json!(parts.join("\n")));
        }
    }
    let url = Url::parse_with_params("https://html.duckduckgo.com/html/", &[("q", query)])
        .map_err(err)?;
    let (_, html) = get(url)?;
    let doc = Html::parse_document(&html);
    let title = Selector::parse(".result__a").unwrap();
    let snippet = Selector::parse(".result__snippet").unwrap();
    let results = doc
        .select(&Selector::parse(".result").unwrap())
        .filter_map(|entry| {
            let link = entry.select(&title).next()?;
            let href = Url::parse("https://duckduckgo.com")
                .ok()?
                .join(link.attr("href")?)
                .ok()?;
            let url = href
                .query_pairs()
                .find(|(k, _)| k == "uddg")
                .map(|(_, v)| v.into_owned())
                .unwrap_or_else(|| href.to_string());
            Some(format!(
                "{}\n{url}\n{}",
                link.text().collect::<String>(),
                entry
                    .select(&snippet)
                    .next()
                    .map(|n| n.text().collect::<String>())
                    .unwrap_or_default()
            ))
        })
        .take(10)
        .collect::<Vec<_>>();
    if results.is_empty() {
        return Err("搜索服务未返回结果".into());
    }
    Ok(json!(results.join("\n\n")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_local_paths_explain_the_html_open_tool() {
        let error = webpage_url("index.html").unwrap_err();
        assert!(error.contains("完整的 HTTP(S) 地址"));
        assert!(error.contains("open_html"));
    }

    #[test]
    fn web_fetch_rejects_non_http_schemes_with_actionable_message() {
        let error = webpage_url("file:///tmp/index.html").unwrap_err();
        assert!(error.contains("只支持 HTTP 或 HTTPS"));
        assert!(error.contains("open_html"));
    }
}

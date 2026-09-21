//! 仓库元数据：About 描述（来自仓库页面）与创建时间（来自 REST API）。
//!
//! 两条数据来自两个不同的地址，所以一次抓取会并行发两个请求，并把每一路
//! 的失败逐条记录下来。上层据此决定哪些字段还是「待获取」。

use futures::join;
use serde_json::Value;

use crate::github::describe_http_error;
use crate::shared::net::{GetRequest, get};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepoMeta {
    /// About 的原文。
    pub description: Option<String>,
    /// 仓库创建时间（Unix 秒）。
    pub created_at: Option<i64>,
    pub stars: Option<u64>,
}

#[derive(Debug, Clone, Default)]
pub struct RepoMetaOutcome {
    pub meta: RepoMeta,
    /// 逐条列出没拿到的部分，例如「创建时间：HTTP 403」。
    pub errors: Vec<String>,
    pub rate_limit_remaining: Option<u64>,
    /// 配额已经用完。批量补全时必须据此提前收手，否则后面每一步都会白跑一趟。
    pub rate_limited: bool,
}

/// 抓一个仓库的元数据。两个来源互不阻塞，任一失败不影响另一个。
pub async fn fetch_repo_meta(full_name: &str, token: Option<String>) -> RepoMetaOutcome {
    let page_url = format!("https://github.com/{full_name}");
    let api_url = format!("https://api.github.com/repos/{full_name}");

    let page = get(GetRequest::new(page_url));
    let api = get(
        GetRequest::new(api_url)
            .bearer(token)
            .accept("application/vnd.github+json"),
    );

    let (page, api) = join!(page, api);

    let mut outcome = RepoMetaOutcome::default();

    match page {
        Ok(response) => match parse_about(&response.body, full_name) {
            Some(description) => outcome.meta.description = Some(description),
            None => outcome
                .errors
                .push("描述：页面的 About 区域里没有内容".to_string()),
        },
        Err(error) => outcome
            .errors
            .push(format!("描述：{}", describe_http_error(&error))),
    }

    match api {
        Ok(response) => {
            outcome.rate_limit_remaining = response.rate_limit_remaining;
            outcome.meta.created_at = parse_created_at(&response.body);
            outcome.meta.stars = parse_stars(&response.body);
            if outcome.meta.created_at.is_none() {
                outcome
                    .errors
                    .push("创建时间：接口响应里没有 created_at".to_string());
            }
        }
        Err(error) => {
            outcome.rate_limited = matches!(
                error,
                crate::shared::net::HttpError::Status {
                    code: 403 | 429,
                    ..
                }
            );
            outcome
                .errors
                .push(format!("创建时间：{}", describe_http_error(&error)));
        }
    }

    if outcome.rate_limit_remaining == Some(0) {
        outcome.rate_limited = true;
    }

    outcome
}

/// 从仓库页面里取 About 的原文。
///
/// GitHub 已把 About 内容内联在页面的 JSON 负载里（`sidebarAbout.description`），
/// 比匹配 CSS 类名稳得多。取不到时退回 `og:description`，
/// 它是同一份内容、只是被追加了 ` - owner/repo`。
pub fn parse_about(html: &str, full_name: &str) -> Option<String> {
    if let Some(description) = about_from_embedded_json(html) {
        return Some(description);
    }
    about_from_meta(html, full_name)
}

fn about_from_embedded_json(html: &str) -> Option<String> {
    const MARKER: &str = "data-target=\"react-app.embeddedData\"";

    let marker_at = html.find(MARKER)?;
    let open_at = html[marker_at..].find('>')? + marker_at + 1;
    let close_at = html[open_at..].find("</script>")? + open_at;
    let raw = html[open_at..close_at].trim();

    let payload: Value = serde_json::from_str(raw)
        .or_else(|_| serde_json::from_str(&unescape_html_entities(raw)))
        .ok()?;

    let description = payload
        .get("payload")?
        .get("sidebarAbout")?
        .get("description")?
        .as_str()?
        .trim();

    (!description.is_empty()).then(|| description.to_string())
}

fn about_from_meta(html: &str, full_name: &str) -> Option<String> {
    for marker in ["property=\"og:description\"", "name=\"description\""] {
        let Some(at) = html.find(marker) else {
            continue;
        };
        let tag_start = html[..at].rfind('<').unwrap_or(at);
        let tag_end = html[at..].find('>').map(|offset| offset + at)?;
        let tag = &html[tag_start..tag_end];
        let Some(content_at) = tag.find("content=\"") else {
            continue;
        };
        let value_start = content_at + "content=\"".len();
        let Some(value_len) = tag[value_start..].find('"') else {
            continue;
        };
        let text = unescape_html_entities(&tag[value_start..value_start + value_len]);
        let text = text.trim();

        // GitHub 会把「描述 - owner/repo」拼进 og:description，去掉这段尾巴。
        let suffix = format!(" - {full_name}");
        let cleaned = text.strip_suffix(&suffix).unwrap_or(text).trim();

        if !cleaned.is_empty() && cleaned != full_name {
            return Some(cleaned.to_string());
        }
    }
    None
}

/// `GET /repos/{owner}/{repo}` 的 `created_at` 是 RFC 3339。
pub fn parse_created_at(json: &str) -> Option<i64> {
    let payload: Value = serde_json::from_str(json).ok()?;
    let raw = payload.get("created_at")?.as_str()?;
    chrono::DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|time| time.timestamp())
}

pub fn parse_stars(json: &str) -> Option<u64> {
    let payload: Value = serde_json::from_str(json).ok()?;
    payload.get("stargazers_count")?.as_u64()
}

/// 内联负载偶尔是 HTML 转义过的，退化为纯文本再解一次 JSON。
fn unescape_html_entities(text: &str) -> String {
    text.replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;

    const REPO_PAGE: &str = r#"
<html><head>
<meta property="og:description" content="Secure environments for developers and their agents - coder/coder" />
</head><body>
<script type="application/json" data-target="react-app.embeddedData">{"payload":{"codeViewRepoRoute":{"path":"/"},"sidebarAbout":{"description":"Secure environments for developers and their agents","website":"https://coder.com","stargazerCount":15769}}}</script>
</body></html>
"#;

    #[test]
    fn about_comes_from_the_embedded_payload() {
        assert_eq!(
            parse_about(REPO_PAGE, "coder/coder").as_deref(),
            Some("Secure environments for developers and their agents")
        );
    }

    #[test]
    fn about_falls_back_to_og_description_without_the_suffix() {
        let page = r#"<html><head>
<meta property="og:description" content="An open source project - foo/bar" />
</head><body>没有内联负载</body></html>"#;
        assert_eq!(parse_about(page, "foo/bar").as_deref(), Some("An open source project"));
    }

    #[test]
    fn about_is_none_when_the_page_has_no_description() {
        assert_eq!(parse_about("<html><body>404</body></html>", "foo/bar"), None);
    }

    #[test]
    fn reads_created_at_and_stars_from_the_api_payload() {
        let json = r#"{"full_name":"coder/coder","created_at":"2021-11-15T20:06:55Z","stargazers_count":15769}"#;
        assert_eq!(
            parse_created_at(json),
            chrono::DateTime::parse_from_rfc3339("2021-11-15T20:06:55Z")
                .ok()
                .map(|time| time.timestamp())
        );
        assert_eq!(parse_stars(json), Some(15_769));
    }

    #[test]
    fn malformed_api_payload_yields_none_instead_of_panicking() {
        assert_eq!(parse_created_at("<html>not json</html>"), None);
        assert_eq!(parse_stars("{}"), None);
    }

    /// 真机探针：会联网、会消耗 GitHub 的匿名配额，默认不跑。
    ///
    /// 用来回答「补全为什么这么慢」这类只有真实网络才能回答的问题——
    /// 打印每个仓库的耗时与逐项失败原因。
    ///
    /// ```text
    /// cargo test probe_real_metadata -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "需要联网，且会消耗 GitHub 匿名接口配额"]
    fn probe_real_metadata_reports_latency_and_failures() {
        const TARGETS: [&str; 8] = [
            "coder/coder",
            "anthropics/claude-code",
            "Tencent/WeKnora",
            "alibaba/open-code-review",
            "astral-sh/uv",
            "tokio-rs/tokio",
            "rust-lang/rust",
            "serde-rs/serde",
        ];

        let started = std::time::Instant::now();
        for name in TARGETS {
            let t0 = std::time::Instant::now();
            let outcome = futures::executor::block_on(fetch_repo_meta(name, None));
            println!(
                "{name:<32} {:>8.2}s  created_at={:?}  remaining={:?}  errors={:?}",
                t0.elapsed().as_secs_f64(),
                outcome.meta.created_at.is_some(),
                outcome.rate_limit_remaining,
                outcome.errors,
            );
        }
        println!("合计 {:?}", started.elapsed());
    }
}

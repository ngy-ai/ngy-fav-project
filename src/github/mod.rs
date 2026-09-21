//! GitHub 抓取层。
//!
//! 这里只负责「把页面 / 接口变成结构化的数据」，不碰存储与界面。
//! 解析函数都是纯函数，可以直接用真实抓下来的 HTML 片段做单测。

pub mod repo_meta;
pub mod trending;

pub use repo_meta::{RepoMeta, RepoMetaOutcome, fetch_repo_meta};
pub use trending::{TrendingEntry, parse_trending};

use crate::shared::net::{GetRequest, HttpError, get};

/// GitHub 页面是 HTML。
pub async fn fetch_html(url: &str) -> Result<String, HttpError> {
    get(GetRequest::new(url))
        .await
        .map(|response| response.body)
}

/// 抓取一个 discovered 地址（GitHub Trending 及其同构页面）。
pub async fn fetch_trending(url: &str) -> Result<Vec<TrendingEntry>, String> {
    let html = fetch_html(url).await.map_err(|error| error.to_string())?;
    parse_trending(&html)
}

/// 把 HTTP 失败翻译成给用户看的一句话。
///
/// 403 / 429 单独说明，因为未认证的 GitHub API 只有 60 次/小时——
/// 这是最可能遇到、也最容易误判成「程序坏了」的一种失败。
pub fn describe_http_error(error: &HttpError) -> String {
    match error {
        HttpError::Status {
            code: code @ (403 | 429),
            url,
        } => format!(
            "请求被拒绝（HTTP {code}）。GitHub 未认证接口每小时只有 60 次，\
             稍后重试或在「设置」里填一个访问令牌（{url}）"
        ),
        HttpError::Status { code, url } => format!("HTTP {code}：{url}"),
        other => other.to_string(),
    }
}

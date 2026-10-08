//! GitHub 抓取层。
//!
//! 这里只负责「把页面 / 接口变成结构化的数据」，不碰存储与界面。
//! 解析函数都是纯函数，可以直接用真实抓下来的 HTML 片段做单测。
//!
//! 发现源有两类页面（见 `source.rs`）：trending 榜单与 topics 话题页。
//! 两者的条目形状相同（`entry::RepoEntry`），选择器各写一个模块。

pub mod entry;
pub mod rate_limit;
pub mod repo_meta;
pub mod source;
pub mod topic;
pub mod trending;

pub use entry::RepoEntry;
pub use rate_limit::{RateLimit, RateLimitGate, describe_exhausted};
pub use repo_meta::{RepoMeta, RepoMetaOutcome, fetch_repo_meta};
pub use source::{SourcePage, UNSUPPORTED_URL_HINT, tags_for_source};
pub use topic::parse_topics;
pub use trending::parse_trending;

use crate::shared::net::{GetRequest, HttpError, get};

/// GitHub 页面是 HTML。
pub async fn fetch_html(url: &str) -> Result<String, HttpError> {
    get(GetRequest::new(url))
        .await
        .map(|response| response.body)
}

/// 抓一个发现源，按地址形状挑解析器。
///
/// 地址认不出来时**在发请求之前**就停住：拿 trending 的选择器去套一个别的页面，
/// 得到的是一句「结构可能已经变化」，那会把「我们不支持这个地址」说成
/// 「GitHub 改版了」，让人往错的方向查。添加发现源时 `SourcePage::of` 已经拦过一次，
/// 这里再拦一次是因为地址也可能是手改配置文件写进去的。
pub async fn fetch_source(url: &str) -> Result<Vec<RepoEntry>, String> {
    let Some(page) = SourcePage::of(url) else {
        return Err(format!("不认识的发现地址：{url}（{UNSUPPORTED_URL_HINT}）"));
    };

    let html = fetch_html(url).await.map_err(|error| error.to_string())?;
    match page {
        SourcePage::Trending => parse_trending(&html),
        SourcePage::Topic { .. } => parse_topics(&html),
    }
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
            ..
        } => format!(
            "请求被拒绝（HTTP {code}）。GitHub 未认证接口每小时只有 60 次，\
             稍后重试或在「设置」里填一个访问令牌（{url}）"
        ),
        HttpError::Status { code, url, .. } => format!("HTTP {code}：{url}"),
        other => other.to_string(),
    }
}

//! 发现源与后台任务的配置模型。

use serde::{Deserialize, Serialize};

use crate::github::{SourcePage, UNSUPPORTED_URL_HINT};

/// 一个发现地址。身份即 URL，和 `Repo` 用 `full_name` 做身份是同一套做法，
/// 不需要再维护一个可能对不上的 id。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoverySource {
    /// 界面上显示的名称。
    pub name: String,
    /// 地址，例如 `https://github.com/trending?since=weekly&spoken_language_code=`。
    pub url: String,
    pub enabled: bool,
    /// 最近一次运行结果。从未运行过则为 `None`。
    #[serde(default)]
    pub last_run: Option<SourceRun>,
}

impl DiscoverySource {
    pub fn new(name: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            url: url.into(),
            enabled: true,
            last_run: None,
        }
    }

    /// 上次运行新增了几个仓库；从未运行时为 `None`。
    pub fn last_added(&self) -> Option<usize> {
        self.last_run.as_ref().map(|run| run.added)
    }
}

/// 一次运行的账目。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceRun {
    pub started_at: i64,
    pub finished_at: i64,
    /// 页面上解析出的条目数。
    pub found: usize,
    /// 新入库的数量。
    pub added: usize,
    /// 因为已在库中而跳过的数量。
    pub skipped: usize,
    /// 整源失败时的原因；`None` 表示这一轮跑通了。
    #[serde(default)]
    pub error: Option<String>,
}

/// 后台发现任务的配置。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoverySettings {
    /// 是否按间隔自动运行。关闭后仍可手动运行。
    pub auto_discover: bool,
    /// 自动运行的间隔（分钟）。
    pub interval_minutes: u32,
    /// 每轮最多补全多少个仓库的元数据。
    ///
    /// GitHub 未认证接口每小时只有 60 次请求，一轮全量补全校验会让配额瞬间耗尽，
    /// 所以这里必须有个上限，让积压分批消化。
    pub metadata_batch: u32,
    /// 可选的 GitHub 访问令牌，用于把接口配额从 60 次/小时提到 5000。
    #[serde(default)]
    pub github_token: Option<String>,
}

impl Default for DiscoverySettings {
    fn default() -> Self {
        Self {
            auto_discover: true,
            interval_minutes: 180,
            metadata_batch: 8,
            github_token: None,
        }
    }
}

impl DiscoverySettings {
    /// 令牌允许为空字符串（界面上清空输入框就会这样），统一收成 `None`。
    pub fn token(&self) -> Option<String> {
        self.github_token
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    }

    /// 间隔的下限。0 会让后台任务变成忙循环。
    pub fn effective_interval_minutes(&self) -> u32 {
        self.interval_minutes.max(1)
    }
}

/// 首次启动时给出的默认发现源。
pub fn default_sources() -> Vec<DiscoverySource> {
    vec![DiscoverySource::new(
        "周榜 · 全部语言",
        "https://github.com/trending?since=weekly&spoken_language_code=",
    )]
}

/// 校验并规范化一个发现地址。
///
/// GitHub Trending 的查询参数决定了榜单口径（`since`、`spoken_language_code`、
/// `language`），topics 的语言筛选在 `l` 上，这些参数必须原样保留，
/// 所以这里只做协议、主机名与「这一页我们认不认得」的检查，
/// 不重写路径或参数。
///
/// **认不出的地址在添加时就拒掉**：放它进去，只会在每一轮运行时报一句
/// 「结构可能已经变化」，把「我们不支持这个地址」说成「GitHub 改版了」。
pub fn validate_source_url(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("请填写发现地址".to_string());
    }
    if !trimmed.starts_with("https://") && !trimmed.starts_with("http://") {
        return Err("地址需要以 https:// 或 http:// 开头".to_string());
    }

    let rest = trimmed
        .strip_prefix("https://")
        .or_else(|| trimmed.strip_prefix("http://"))
        .unwrap_or_default();
    let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if host.is_empty() {
        return Err("地址里缺少主机名".to_string());
    }

    if SourcePage::of(trimmed).is_none() {
        return Err(format!("这个地址不是能列出仓库的页面；{UNSUPPORTED_URL_HINT}"));
    }

    Ok(trimmed.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blank_token_is_treated_as_absent() {
        let mut settings = DiscoverySettings::default();
        assert_eq!(settings.token(), None);

        settings.github_token = Some("   ".to_string());
        assert_eq!(settings.token(), None);

        settings.github_token = Some(" ghp_x ".to_string());
        assert_eq!(settings.token().as_deref(), Some("ghp_x"));
    }

    #[test]
    fn zero_interval_is_clamped() {
        let settings = DiscoverySettings {
            interval_minutes: 0,
            ..DiscoverySettings::default()
        };
        assert_eq!(settings.effective_interval_minutes(), 1);
    }

    #[test]
    fn source_url_keeps_its_query_parameters() {
        let url = validate_source_url("  https://github.com/trending?since=weekly&spoken_language_code=  ")
            .expect("合法地址");
        assert_eq!(
            url,
            "https://github.com/trending?since=weekly&spoken_language_code="
        );
    }

    #[test]
    fn source_url_rejects_input_without_a_host() {
        assert!(validate_source_url("").is_err());
        assert!(validate_source_url("github.com/trending").is_err());
        assert!(validate_source_url("https://").is_err());
    }

    #[test]
    fn topics_urls_are_accepted_with_their_filters() {
        let url = validate_source_url("  https://github.com/topics/ai-agent?l=python  ")
            .expect("topics 地址合法");
        assert_eq!(url, "https://github.com/topics/ai-agent?l=python");
    }

    /// 认不出的页面在添加时就拒掉，别留到运行时每轮报一句「结构可能已经变化」。
    /// 报错要把支持什么说清楚，否则用户只能自己猜。
    #[test]
    fn pages_that_cannot_list_repositories_are_rejected() {
        let error =
            validate_source_url("https://github.com/explore").expect_err("应当拒绝这个地址");
        assert!(error.contains("trending"), "错误信息该说清支持什么；实际：{error}");
        assert!(error.contains("topics"), "错误信息该说清支持什么；实际：{error}");

        assert!(validate_source_url("https://github.com/topics").is_err());
        assert!(validate_source_url("https://gitlab.com/topics/ai-agent").is_err());
    }
}

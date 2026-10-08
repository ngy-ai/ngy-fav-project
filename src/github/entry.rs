//! 发现页条目的形状，以及两种页面解析器共用的取文本工具。
//!
//! trending 与 topics 的页面结构完全不同，选择器各写各的；但产出的字段是同一套
//! （`owner/repo`、描述、语言、星数）。形状与这些取文本的小工具放在这里共用，
//! 免得两个解析器各写一份、各自漂移——比如「`38,000` 怎么变成数字」这种事
//! 有两份实现，迟早会有一份忘记处理缩写。

use scraper::{ElementRef, Selector};

/// 从发现页面上解析出来的一个仓库条目。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoEntry {
    /// `owner/repo`，大小写保持页面原样。
    pub full_name: String,
    pub url: String,
    /// 条目自带的描述。GitHub 上这个文本与仓库页 About 是同一份内容。
    pub description: Option<String>,
    pub language: Option<String>,
    pub stars: Option<u64>,
}

/// 从 `/owner/repo` 形式的链接里取出仓库标识与完整地址。
///
/// 页面上同时存在指向用户/组织的链接（`/owner`，只有一段），
/// 用「路径正好两段」把它们挡掉。
pub(crate) fn repo_from_href(href: &str) -> Option<(String, String)> {
    let path = href.trim_end_matches('/');
    let parts: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    if parts.len() != 2 || parts[0].is_empty() || parts[1].is_empty() {
        return None;
    }
    // 页面里也可能出现 /login?return_to= 这类链接，用字符集挡掉。
    if !parts
        .iter()
        .all(|part| part.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')))
    {
        return None;
    }
    Some((
        format!("{}/{}", parts[0], parts[1]),
        format!("https://github.com/{}/{}", parts[0], parts[1]),
    ))
}

/// 从字符串里挑出所有数字拼成一个数：`38,000` → `38000`。
pub(crate) fn parse_count(text: &str) -> Option<u64> {
    let digits: String = text.chars().filter(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

/// 星数计数器上的完整数字。
///
/// 计数器**显示的文字可能是缩写**（topics 页面上 248198 星写成 `248k`），
/// 直接取文本会得到 248——一个看着像真数据的错数。完整数字在属性上：
/// `title="248,198"`、`aria-label="248198 users starred this repository"`。
/// 两个都试，最后才退回文本（trending 页面的星标链接写的就是完整数字）。
pub(crate) fn stars_from_counter(element: &ElementRef) -> Option<u64> {
    ["title", "aria-label"]
        .into_iter()
        .filter_map(|attr| element.value().attr(attr))
        .find_map(parse_count)
        .or_else(|| parse_count(&element.text().collect::<String>()))
}

/// HTML 文本节点里塞满了缩进与换行，压成单行便于存储与展示。
pub(crate) fn collapse_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 取元素里的文本并压成单行；全是空白时给 `None`。
///
/// 统一在这里收口，是因为「空文本」在入库时必须是 `None` 而不是 `Some("")`：
/// 空描述会被当成「已经补过元数据」，那些仓库就再也等不到补全了。
pub(crate) fn element_text(element: &ElementRef) -> Option<String> {
    let text = collapse_whitespace(&element.text().collect::<String>());
    (!text.is_empty()).then_some(text)
}

pub(crate) fn selector(css: &str) -> Selector {
    Selector::parse(css).expect("内置选择器必须合法")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_two_segment_paths_are_repositories() {
        assert_eq!(
            repo_from_href("/coder/coder"),
            Some((
                "coder/coder".to_string(),
                "https://github.com/coder/coder".to_string()
            ))
        );
        // 指向用户/组织的链接（topics 卡片里的 `h3` 同时有这两个）。
        assert_eq!(repo_from_href("/NousResearch"), None);
        // 带查询串的登录链接。
        assert_eq!(repo_from_href("/login?return_to=%2Fa%2Fb"), None);
        assert_eq!(repo_from_href("/trending/rust?since=weekly"), None);
        assert_eq!(repo_from_href("/"), None);
    }

    #[test]
    fn counts_are_read_through_separators() {
        assert_eq!(parse_count("38,000"), Some(38_000));
        assert_eq!(parse_count("248198 users starred this repository"), Some(248_198));
        assert_eq!(parse_count("1 user starred this repository"), Some(1));
        assert_eq!(parse_count("no digits here"), None);
    }

    /// 回归：`248k` 这种缩写不能当成 248。
    #[test]
    fn abbreviated_counter_text_is_not_trusted() {
        let html = r#"<span class="Counter js-social-count" title="248,198" aria-label="248198 users starred this repository">248k</span>"#;
        let document = scraper::Html::parse_document(html);
        let counter = document
            .select(&selector("span.js-social-count"))
            .next()
            .expect("应当选中计数器");
        assert_eq!(stars_from_counter(&counter), Some(248_198));
    }

    #[test]
    fn blank_text_becomes_none() {
        let html = "<p>   \n  </p>";
        let document = scraper::Html::parse_document(html);
        let paragraph = document.select(&selector("p")).next().expect("应当选中段落");
        assert_eq!(element_text(&paragraph), None);
    }
}

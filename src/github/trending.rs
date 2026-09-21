//! GitHub Trending 页面解析。
//!
//! 选择器对着 2026-09 的真实页面结构写，并在 `tests` 里用真实片段钉住。
//! 页面结构变了就应当报错，而不是返回一个空列表——空列表会被上层
//! 当成「本轮没有新项目」，从而静默丢掉发现能力。

use scraper::{Html, Selector};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrendingEntry {
    /// `owner/repo`，大小写保持页面原样。
    pub full_name: String,
    pub url: String,
    /// 条目自带的描述。GitHub 上这个文本与仓库页 About 是同一份内容。
    pub description: Option<String>,
    pub language: Option<String>,
    pub stars: Option<u64>,
}

/// 解析 trending 页面。
///
/// `Err` 只在「页面里一个条目都没有」时出现：正常的 trending 页总有若干条，
/// 一条都没有说明拿到的是错误页或结构已变。
pub fn parse_trending(html: &str) -> Result<Vec<TrendingEntry>, String> {
    let document = Html::parse_document(html);

    let article = selector("article.Box-row");
    let heading_link = selector("h2 a[href]");
    let description = selector("p.col-9");
    let language = selector("span[itemprop=programmingLanguage]");
    let stargazers = selector("a[href$='/stargazers']");

    let mut entries = Vec::new();
    for node in document.select(&article) {
        let Some((full_name, url)) = node
            .select(&heading_link)
            .filter_map(|link| link.value().attr("href"))
            .find_map(repo_from_href)
        else {
            // 单个条目缺链接时跳过它，但页面整体仍算解析成功。
            continue;
        };

        let description = node
            .select(&description)
            .next()
            .map(|element| collapse_whitespace(&element.text().collect::<String>()))
            .filter(|text| !text.is_empty());

        let language = node
            .select(&language)
            .next()
            .map(|element| collapse_whitespace(&element.text().collect::<String>()))
            .filter(|text| !text.is_empty());

        let stars = node
            .select(&stargazers)
            .next()
            .and_then(|element| parse_count(&element.text().collect::<String>()));

        entries.push(TrendingEntry {
            full_name,
            url,
            description,
            language,
            stars,
        });
    }

    if entries.is_empty() {
        return Err(
            "页面里没有解析到任何仓库条目，trending 的结构可能已经变化".to_string(),
        );
    }

    Ok(entries)
}

/// 从 `/owner/repo` 形式的链接里取出仓库标识与完整地址。
fn repo_from_href(href: &str) -> Option<(String, String)> {
    let path = href.trim_end_matches('/');
    let parts: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    if parts.len() != 2 || parts[0].is_empty() || parts[1].is_empty() {
        return None;
    }
    // trending 条目里也可能出现 /login?return_to= 这类链接，用字符集挡掉。
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

/// `38,000` → `38000`。
fn parse_count(text: &str) -> Option<u64> {
    let digits: String = text.chars().filter(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

/// HTML 文本节点里塞满了缩进与换行，压成单行便于存储与展示。
fn collapse_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn selector(css: &str) -> Selector {
    Selector::parse(css).expect("内置选择器必须合法")
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: &str = r#"
<html><body>
<article class="Box-row">
  <div class="float-right d-flex"><a href="/login?return_to=%2Falibaba%2Fopen-code-review">Star</a></div>
  <h2 class="h3 lh-condensed">
    <a href="/alibaba/open-code-review" class="Link">
      <span class="text-normal">alibaba /</span>
      open-code-review</a>  </h2>
    <p class="col-9 color-fg-muted my-1 tmp-pr-4">
      Hybrid architecture code review tool: deterministic pipelines +
      LLM Agent, OpenAI &amp; Anthropic compatible.
    </p>
  <div class="f6 color-fg-muted mt-2">
    <span class="tmp-mr-3"><span itemprop="programmingLanguage">Go</span></span>
    <a href="/alibaba/open-code-review/stargazers" class="Link">38,000</a>
    <a href="/alibaba/open-code-review/forks" class="Link">2,703</a>
    <span class="d-inline-block float-sm-right">15,028 stars this week</span>
  </div>
</article>
<article class="Box-row">
  <h2 class="h3 lh-condensed"><a href="/coder/coder">coder</a></h2>
  <a href="/coder/coder/stargazers" class="Link">15,769</a>
</article>
</body></html>
"#;

    #[test]
    fn parses_entries_from_a_real_fragment() {
        let entries = parse_trending(PAGE).expect("应当解析成功");
        assert_eq!(entries.len(), 2);

        let first = &entries[0];
        assert_eq!(first.full_name, "alibaba/open-code-review");
        assert_eq!(first.url, "https://github.com/alibaba/open-code-review");
        assert_eq!(
            first.description.as_deref(),
            Some("Hybrid architecture code review tool: deterministic pipelines + LLM Agent, OpenAI & Anthropic compatible.")
        );
        assert_eq!(first.language.as_deref(), Some("Go"));
        assert_eq!(first.stars, Some(38_000));

        // 缺少描述与语言的条目也要能解析出来。
        let second = &entries[1];
        assert_eq!(second.full_name, "coder/coder");
        assert_eq!(second.description, None);
        assert_eq!(second.stars, Some(15_769));
    }

    #[test]
    fn ignores_non_repository_links() {
        // `Star` 按钮指向 /login?return_to=...，不能被当成仓库链接。
        assert_eq!(repo_from_href("/login?return_to=%2Falibaba%2Fopen-code-review"), None);
        assert_eq!(repo_from_href("/trending/rust?since=weekly"), None);
        assert_eq!(repo_from_href("/coder/coder"), Some((
            "coder/coder".to_string(),
            "https://github.com/coder/coder".to_string()
        )));
    }

    #[test]
    fn empty_page_is_an_error_not_an_empty_list() {
        let error = parse_trending("<html><body>rate limited</body></html>")
            .expect_err("没有条目时应当报错");
        assert!(error.contains("结构可能已经变化"), "实际：{error}");
    }
}

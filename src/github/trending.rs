//! GitHub Trending 页面解析。
//!
//! 选择器对着 2026-09 的真实页面结构写，并在 `tests` 里用真实片段钉住。
//! 页面结构变了就应当报错，而不是返回一个空列表——空列表会被上层
//! 当成「本轮没有新项目」，从而静默丢掉发现能力。
//!
//! 条目形状与取文本的工具在 `entry.rs`，与 topics 的解析器共用。

use scraper::Html;

use crate::github::entry::{RepoEntry, element_text, parse_count, repo_from_href, selector};

/// 解析 trending 页面。
///
/// `Err` 只在「页面里一个条目都没有」时出现：正常的 trending 页总有若干条，
/// 一条都没有说明拿到的是错误页或结构已变。
pub fn parse_trending(html: &str) -> Result<Vec<RepoEntry>, String> {
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

        let language = node.select(&language).next().and_then(|e| element_text(&e));

        // 星标链接上的文本是完整数字（`38,000`），不是缩写——会缩写成 `248k`
        // 的是 topics 卡片上那个计数器，所以这边可以直接读文本。
        let stars = node
            .select(&stargazers)
            .next()
            .and_then(|element| parse_count(&element.text().collect::<String>()));

        entries.push(RepoEntry {
            full_name,
            url,
            description: node.select(&description).next().and_then(|e| element_text(&e)),
            language,
            stars,
        });
    }

    if entries.is_empty() {
        return Err("页面里没有解析到任何仓库条目，trending 的结构可能已经变化".to_string());
    }

    Ok(entries)
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
    fn empty_page_is_an_error_not_an_empty_list() {
        let error = parse_trending("<html><body>rate limited</body></html>")
            .expect_err("没有条目时应当报错");
        assert!(error.contains("结构可能已经变化"), "实际：{error}");
    }
}

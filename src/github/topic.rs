//! GitHub Topics 页面解析（`https://github.com/topics/<话题>`）。
//!
//! 和 trending 是两套完全不同的页面结构——那边是 `article.Box-row` + `h2`，
//! 这边是 `article` 卡片 + `h3` 里的「owner / repo」两个链接——所以选择器
//! 各写各的，只共用条目形状（`RepoEntry`）。
//!
//! 选择器对着 2026-09 的真实页面写，并在 `tests` 里用真实片段钉住。
//! 页面结构变了就应当报错，而不是返回一个空列表——空列表会被上层
//! 当成「本轮没有新项目」，从而静默丢掉发现能力。
//!
//! 另外：**别用 `tmp-` 前缀的工具类做选择器**。GitHub 正在把 `mr-3`、`px-3`
//! 这类类名逐个改成 `tmp-mr-3`、`tmp-px-3`，现在同一张卡片上两种混着出现，
//! 拿它们当锚点会在迁移过程中突然失效。这里只依赖结构（`article`/`h3`/`p`）
//! 与语义标记（`itemprop="programmingLanguage"`、`js-social-count`）。

use scraper::Html;

use crate::github::entry::{
    RepoEntry, element_text, repo_from_href, selector, stars_from_counter,
};

/// 解析 topics 页面。
///
/// `Err` 只在「页面里一个条目都没有」时出现：正常的话题页总有若干条，
/// 一条都没有说明拿到的是错误页（比如被限流）或结构已变。
pub fn parse_topics(html: &str) -> Result<Vec<RepoEntry>, String> {
    let document = Html::parse_document(html);

    // 用 `article` 而不是某个类名：卡片类名（`border rounded color-shadow-small
    // color-bg-subtle tmp-my-4`）里混着正在迁移的工具类，靠它做锚点不稳。
    // 代价是别的 `article` 也会被选中——所以下面按「有没有仓库链接」自筛，
    // 没有的当片段跳过，整页仍算解析成功。
    let card = selector("article");
    // 必须限定在 `h3` 里：卡片的下方有一排 tab 链接（Code / Issues / Pull requests），
    // 其中「Code」也指向 `/owner/repo`，不限定就会把同一张卡片解析两遍。
    let repo_link = selector("h3 a[href]");
    let description = selector("p.color-fg-muted");
    let language = selector("span[itemprop=programmingLanguage]");
    let stars = selector("span.js-social-count");

    let mut entries = Vec::new();
    for node in document.select(&card) {
        let Some((full_name, url)) = node
            .select(&repo_link)
            .filter_map(|link| link.value().attr("href"))
            .find_map(repo_from_href)
        else {
            continue;
        };

        entries.push(RepoEntry {
            full_name,
            url,
            description: node.select(&description).next().and_then(|e| element_text(&e)),
            language: node.select(&language).next().and_then(|e| element_text(&e)),
            stars: node.select(&stars).next().and_then(|e| stars_from_counter(&e)),
        });
    }

    if entries.is_empty() {
        return Err("页面里没有解析到任何仓库条目，topics 的结构可能已经变化".to_string());
    }

    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-09 的真实卡片（省略了 svg 与上万字符的埋点属性，结构原样保留）。
    const PAGE: &str = r#"
<html><body>
<article class="border rounded color-shadow-small color-bg-subtle tmp-my-4">
  <div class="tmp-px-3">
    <div class="d-flex flex-justify-between flex-items-start flex-wrap gap-2 tmp-my-3">
      <div class="d-flex flex-1">
        <span style="margin-top:2px">[icon]</span>
        <h3 class="f3 color-fg-muted text-normal lh-condensed" >
          <a href="/NousResearch" class="Link">NousResearch</a>
          /
          <a href="/NousResearch/hermes-agent" class="Link text-bold wb-break-word">hermes-agent</a>
        </h3>
      </div>
      <div class="d-flex flex-items-center">
        <div class="BtnGroup d-flex">
          <a href="/login?return_to=%2FNousResearch%2Fhermes-agent" rel="nofollow" class="btn-sm btn color-bg-default">
            <span class="d-inline"> Star </span>
            <span id="repo-stars-counter-star" aria-label="248198 users starred this repository" title="248,198" class="Counter js-social-count">248k</span>
          </a>
        </div>
      </div>
    </div>
  </div>
  <nav aria-label="Repository menu: NousResearch/hermes-agent" class="tabnav tmp-px-3 mb-0 tmp-mb-0">
    <ul class="tabnav-tabs">
      <li class="d-inline-flex"><a href="/NousResearch/hermes-agent" class="tabnav-tab">Code</a></li>
      <li class="d-inline-flex"><a href="/NousResearch/hermes-agent/issues" class="tabnav-tab">Issues</a></li>
    </ul>
  </nav>
  <div class="color-bg-default rounded-bottom-2">
    <div class="tmp-px-3 tmp-pt-3">
      <p class="color-fg-muted mb-0" >The agent that grows with you</p>
    </div>
    <div class="d-flex flex-wrap border-bottom color-border-muted tmp-px-3 pt-2 pb-2">
      <a title="Topic: ai" href="/topics/ai" class="topic-tag topic-tag-link Link f6 mb-2 tmp-mb-2">ai</a>
      <a title="Topic: openai" href="/topics/openai" class="topic-tag topic-tag-link Link f6 mb-2 tmp-mb-2">openai</a>
    </div>
    <div class="tmp-p-3">
      <ul class="d-flex f6 list-style-none color-fg-muted">
        <li class="tmp-mr-4">Updated <relative-time datetime="2026-09-23T07:44:36Z" class="no-wrap">Sep 23, 2026</relative-time></li>
        <li class="tmp-mr-4">
          <span class="f6 my-1 ml-0 tmp-ml-0">
            <span class="repo-language-color" style="background-color: #3572A5"></span>
            <span itemprop="programmingLanguage">Python</span>
          </span>
        </li>
      </ul>
    </div>
  </div>
</article>
<article class="border rounded color-shadow-small color-bg-subtle tmp-my-4">
  <h3 class="f3 color-fg-muted text-normal lh-condensed" >
    <a href="/coder" class="Link">coder</a> / <a href="/coder/coder" class="Link text-bold">coder</a>
  </h3>
</article>
<div class="promo"><article>没有仓库链接的一块广告</article></div>
</body></html>
"#;

    #[test]
    fn parses_entries_from_a_real_fragment() {
        let entries = parse_topics(PAGE).expect("应当解析成功");
        assert_eq!(entries.len(), 2);

        let first = &entries[0];
        assert_eq!(first.full_name, "NousResearch/hermes-agent");
        assert_eq!(first.url, "https://github.com/NousResearch/hermes-agent");
        assert_eq!(first.description.as_deref(), Some("The agent that grows with you"));
        assert_eq!(first.language.as_deref(), Some("Python"));
        // 页面上写的是 `248k`，完整的数只在属性里。
        assert_eq!(first.stars, Some(248_198));

        // 缺描述、缺语言、缺星数的条目也要能解析出来。
        let second = &entries[1];
        assert_eq!(second.full_name, "coder/coder");
        assert_eq!(second.description, None);
        assert_eq!(second.language, None);
        assert_eq!(second.stars, None);
    }

    /// 卡片的 tab 栏里有指向同一个仓库的 `Code` 链接。要是没限定在 `h3` 里，
    /// 每张卡片会被解析成两条。
    #[test]
    fn each_card_yields_exactly_one_entry() {
        let entries = parse_topics(PAGE).expect("应当解析成功");
        let names: Vec<&str> = entries.iter().map(|e| e.full_name.as_str()).collect();
        assert_eq!(names, vec!["NousResearch/hermes-agent", "coder/coder"]);
    }

    /// 没有仓库链接的 `article`（广告之类）跳过就好，不该把整页判成失败。
    #[test]
    fn articles_without_a_repository_are_skipped() {
        let entries = parse_topics(PAGE).expect("应当解析成功");
        assert!(entries.iter().all(|entry| entry.full_name.contains('/')));
    }

    #[test]
    fn an_empty_page_is_an_error_not_an_empty_list() {
        let error = parse_topics("<html><body>rate limited</body></html>")
            .expect_err("没有条目时应当报错");
        assert!(error.contains("结构可能已经变化"), "实际：{error}");
    }
}

//! 一个发现地址指向的是哪一类页面。
//!
//! 发现源的**身份就是 URL**（和 `Repo` 用 `full_name` 做身份是同一套做法），
//! 所以这里不引入新的持久化字段——类型的判断完全从地址形状推出来，
//! 免得地址与「类型」两个字段各说各话（改了地址忘了改类型，就会出现
//! 拿 trending 的选择器去套 topics 页面这种事）。
//!
//! 判断是纯函数，认不出就返回 `None`，由调用方决定怎么说：
//! 添加时是「这个地址不支持」，运行时是「这个源没法解析」。

/// 添加发现源时，认不出地址该怎么说。
///
/// 与运行时那句「结构可能已经变化」分开写：这里的原因是**我们不认这个地址**，
/// 不是「GitHub 改版了」。混成一句会让人跑去 GitHub 上找并不存在的结构变化。
pub const UNSUPPORTED_URL_HINT: &str = "只支持 GitHub 的两种发现页：trending（例如 \
     https://github.com/trending?since=weekly&spoken_language_code=rust）与 topics（例如 \
     https://github.com/topics/ai-agent）";

/// 一个发现地址指向的页面。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SourcePage {
    /// `https://github.com/trending` 以及带查询参数的变体。
    Trending,
    /// `https://github.com/topics/<话题>`。
    Topic {
        /// 路径里的那一级，例如 `ai-agent`。用来给发现源起个默认名字。
        slug: String,
    },
}

impl SourcePage {
    /// 从地址推出页面类型。认不出返回 `None`。
    ///
    /// 查询串与锚点一律忽略：trending 的口径在 `?since=` / `?spoken_language_code=` 上，
    /// topics 的语言筛选在 `?l=` 上，那些参数会**原样**留给请求去用，
    /// 但类型只看路径。
    pub fn of(url: &str) -> Option<Self> {
        let mut segments = github_path(url)?.split('/').filter(|part| !part.is_empty());
        match segments.next()? {
            "trending" => {
                // `/trending/<语言>` 是合法的榜单地址，但 `/trending/developers`
                // 是一份**人**的榜单，页面结构完全不同。放它过去只会拿到一句
                // 「结构可能已经变化」，把人引到错误的方向。
                if segments.next() == Some("developers") {
                    return None;
                }
                Some(Self::Trending)
            }
            "topics" => {
                let slug = segments.next()?;
                // `/topics/<话题>` 之后不该再有层级；多出来的说明这不是我们要的那一页。
                if segments.next().is_some() {
                    return None;
                }
                Some(Self::Topic {
                    slug: slug.to_string(),
                })
            }
            _ => None,
        }
    }

    /// 这个页面上的仓库该自带的标签。
    ///
    /// 只有话题页有：**话题名本身就是标签**（`/topics/ai-agent` → `ai-agent`），
    /// 所以不需要额外存一份「源 → 标签」的映射——地址里已经有了。
    ///
    /// trending 是一份**榜单**不是主题，给它挂 `weekly`、`rust` 之类的标签
    /// 全都不是用户想表达的意思，所以它是空的。
    pub fn tags(&self) -> Vec<String> {
        match self {
            Self::Trending => Vec::new(),
            Self::Topic { slug } => vec![slug.clone()],
        }
    }
}

/// 一个发现地址会带来的标签。地址认不出来时自然没有。
pub fn tags_for_source(url: &str) -> Vec<String> {
    SourcePage::of(url).map(|page| page.tags()).unwrap_or_default()
}

/// 取 github.com 上的路径部分；不是 GitHub 的地址给 `None`。
///
/// 只认 github.com 是因为两个解析器都是对着它的页面结构写的。别人镜像一份
/// 长得很像的页面、或者 GitHub 换了域名，都会让选择器悄悄失配——
/// 与其等到运行时报「结构可能已经变化」，不如在添加时就说不支持。
fn github_path(url: &str) -> Option<&str> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    // 查询串与锚点里都可能含 `/`，先切掉再找主机与路径的分界。
    let rest = rest.split(['?', '#']).next()?;
    let (host, path) = match rest.split_once('/') {
        Some((host, path)) => (host, path),
        None => (rest, ""),
    };

    // 大小写不敏感：先小写化再剥 `www.`。反过来的话 `WWW.GitHub.com`
    // 会因为前缀没剥掉而落到「不是 GitHub」那一支。
    let host = host.to_ascii_lowercase();
    let host = host.strip_prefix("www.").unwrap_or(&host);
    if host != "github.com" {
        return None;
    }
    Some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trending_pages_are_recognised() {
        assert_eq!(
            SourcePage::of("https://github.com/trending?since=weekly&spoken_language_code="),
            Some(SourcePage::Trending)
        );
        assert_eq!(SourcePage::of("https://github.com/trending"), Some(SourcePage::Trending));
        // 按语言的榜单也是 trending。
        assert_eq!(
            SourcePage::of("https://github.com/trending/rust?since=daily"),
            Some(SourcePage::Trending)
        );
    }

    #[test]
    fn topic_pages_carry_their_slug() {
        assert_eq!(
            SourcePage::of("https://github.com/topics/ai-agent"),
            Some(SourcePage::Topic {
                slug: "ai-agent".to_string()
            })
        );
        // 语言筛选与排序写在查询串上，不影响类型。
        assert_eq!(
            SourcePage::of("https://github.com/topics/ai-agent?l=python&o=desc&s=stars"),
            Some(SourcePage::Topic {
                slug: "ai-agent".to_string()
            })
        );
        // 末尾多一个斜杠不算层级。
        assert_eq!(
            SourcePage::of("https://github.com/topics/ai-agent/"),
            Some(SourcePage::Topic {
                slug: "ai-agent".to_string()
            })
        );
    }

    /// topics 的索引页（没有话题名）是一堆话题标签，不是仓库列表；
    /// 多一层级的地址也不认——宁可拒绝，也别去猜那是哪一页。
    #[test]
    fn pages_without_a_repository_list_are_rejected() {
        assert_eq!(SourcePage::of("https://github.com/topics"), None);
        assert_eq!(SourcePage::of("https://github.com/topics/ai-agent/extra"), None);
        // 开发者榜单的页面结构与仓库榜单完全不同。
        assert_eq!(SourcePage::of("https://github.com/trending/developers"), None);
        assert_eq!(SourcePage::of("https://github.com/explore"), None);
        assert_eq!(SourcePage::of("https://github.com/"), None);
    }

    #[test]
    fn only_github_is_accepted() {
        assert_eq!(SourcePage::of("https://gitlab.com/topics/ai-agent"), None);
        assert_eq!(SourcePage::of("https://example.com/trending"), None);
        // 带 www 与大小写的主机名仍然算数。
        assert_eq!(
            SourcePage::of("https://WWW.GitHub.com/topics/ai-agent"),
            Some(SourcePage::Topic {
                slug: "ai-agent".to_string()
            })
        );
    }

    /// 锚点里可能带 `/`，不能让它把路径切坏。
    #[test]
    fn fragments_do_not_break_the_path() {
        assert_eq!(
            SourcePage::of("https://github.com/topics/ai-agent#readme"),
            Some(SourcePage::Topic {
                slug: "ai-agent".to_string()
            })
        );
    }

    /// 从 topics 发现的仓库自动挂话题名做标签；语言筛选写在查询串上，
    /// 不改变话题，也不该多出一个标签。
    #[test]
    fn only_topic_pages_carry_a_tag() {
        assert_eq!(
            tags_for_source("https://github.com/topics/ai-agent"),
            ["ai-agent".to_string()]
        );
        assert_eq!(
            tags_for_source("https://github.com/topics/ai-agent?l=python&o=desc&s=stars"),
            ["ai-agent".to_string()]
        );

        // 榜单不是主题。
        assert!(tags_for_source("https://github.com/trending?since=weekly").is_empty());
        assert!(tags_for_source("https://github.com/trending/rust?since=daily").is_empty());
        // 认不出的地址没有标签。
        assert!(tags_for_source("https://github.com/explore").is_empty());
    }
}

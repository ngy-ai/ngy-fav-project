//! 收藏仓库的数据模型，以及列表查询的条件与结果形状。

/// 仓库的稳定身份：`owner/repo` 的小写形式。
///
/// 排序、去重、编辑都作用在它上面；展示时才回到 `Repo::full_name` 的原始大小写。
/// 库里它就是主键。
pub type RepoId = String;

/// 列表默认一页装多少个。
pub const DEFAULT_PAGE_SIZE: usize = 50;

/// 一个被管理的仓库。
///
/// 这对应 `repos` 表的一行。分类用 `category_id` 引用分类树上的一个节点而不是
/// 存名字：改分类名或挪走整棵子树时，仓库这一行完全不用动。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Repo {
    /// `owner/repo`，保留 GitHub 上的原始大小写。
    pub full_name: String,
    /// 仓库地址，例如 `https://github.com/coder/coder`。
    pub url: String,
    /// About 的原文，来自仓库页面。
    pub description: Option<String>,
    /// 人工填写的汉语描述。留空表示尚未填写。
    pub description_zh: Option<String>,
    /// 发现时间（即入库时间），Unix 秒。
    pub discovered_at: i64,
    /// 仓库创建时间，Unix 秒。需要 GitHub 接口，可能暂时为空。
    pub created_at: Option<i64>,
    /// 发现来源的名称；手工添加时为 `None`。
    pub source: Option<String>,
    pub language: Option<String>,
    pub stars: Option<u64>,
    /// 所属分类在树上的节点 id。`None` 表示未分类。
    pub category_id: Option<i64>,
    /// 标签名字，顺序由词表决定。
    pub tags: Vec<String>,
}

impl Repo {
    /// 手工添加时使用：描述与创建时间留空，等元数据补全步骤去填。
    pub fn new(full_name: String, source: Option<String>, discovered_at: i64) -> Self {
        Self {
            url: format!("https://github.com/{full_name}"),
            full_name,
            description: None,
            description_zh: None,
            discovered_at,
            created_at: None,
            source,
            language: None,
            stars: None,
            category_id: None,
            tags: Vec::new(),
        }
    }

    pub fn id(&self) -> RepoId {
        self.full_name.to_lowercase()
    }

    /// 列表里显示的一行描述：汉语优先，其次原文。
    pub fn display_description(&self) -> Option<&str> {
        clean_text(self.description_zh.as_deref())
            .or_else(|| clean_text(self.description.as_deref()))
    }

    /// 还有字段没取到，需要再次访问 GitHub。
    pub fn has_pending_metadata(&self) -> bool {
        clean_text(self.description.as_deref()).is_none() || self.created_at.is_none()
    }
}

/// 列表里渲染一行所需的全部东西。是 `Repo` 的展示快照，不持有领域对象。
///
/// 分类在这里已经是可读的路径而不是 id——列表离数据库最远，不该知道 id 的存在。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepoRow {
    pub id: RepoId,
    pub full_name: String,
    pub subtitle: String,
    pub language: Option<String>,
    pub pending_metadata: bool,
    /// 完整路径，例如 `开发/前端`。
    pub category_path: Option<String>,
    /// 列表行很窄，色块只画末级名字。
    pub category_leaf: Option<String>,
    pub tags: Vec<String>,
}

/// 列表上方的分类筛选。
///
/// 「未分类」是一个独立的选项而不是「名字为空」：没归类的仓库是真实存在的一类，
/// 得能单独看它们。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum CategoryFilter {
    #[default]
    All,
    Uncategorized,
    /// 按完整路径筛选，选中上级等于选中它整棵子树。
    Named(String),
}

/// 分类筛选落到 SQL 上的形状：路径已经在内存里解析成了节点 id。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CategoryScope {
    All,
    Uncategorized,
    /// 选中的那棵子树里的全部节点 id（含选中节点自己）。
    Subtrees(Vec<i64>),
}

/// 一次列表查询的条件。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepoQuery {
    pub search: String,
    pub category: CategoryFilter,
    /// 从 0 开始的页码。
    pub page: usize,
    pub page_size: usize,
}

impl Default for RepoQuery {
    fn default() -> Self {
        Self {
            search: String::new(),
            category: CategoryFilter::All,
            page: 0,
            page_size: DEFAULT_PAGE_SIZE,
        }
    }
}

/// 已经解析好、可以直接拼进 SQL 的条件。
pub struct RepoQuerySpec {
    /// 小写并转义过的 `LIKE` 模式；`None` 表示不搜。
    pub search: Option<String>,
    pub category: CategoryScope,
    /// 搜索词命中的分类节点（含它们的后代）。搜索时用得上。
    pub search_categories: Vec<i64>,
    pub offset: usize,
    pub limit: usize,
}

/// 一页查询结果。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RepoPage {
    pub rows: Vec<RepoRow>,
    /// 符合条件的总数，不是这一页的条数。
    pub total: usize,
    pub page: usize,
    pub page_size: usize,
}

impl RepoPage {
    /// 总页数。一条都没有时也算一页——界面照样要显示「第 1/1 页」而不是「第 1/0 页」。
    pub fn page_count(&self) -> usize {
        if self.page_size == 0 {
            return 1;
        }
        self.total.div_ceil(self.page_size).max(1)
    }

    /// 页码是否已经越界（比如筛完之后停在了后面几页）。
    pub fn is_out_of_range(&self) -> bool {
        self.page >= self.page_count()
    }

    pub fn has_prev(&self) -> bool {
        self.page > 0
    }

    pub fn has_next(&self) -> bool {
        self.page + 1 < self.page_count()
    }

    /// 这一页第一条在全体里的序号，从 1 开始；空页为 0。
    pub fn first_index(&self) -> usize {
        if self.rows.is_empty() { 0 } else { self.page * self.page_size + 1 }
    }

    pub fn last_index(&self) -> usize {
        if self.rows.is_empty() {
            0
        } else {
            self.page * self.page_size + self.rows.len()
        }
    }
}

/// 把 `owner/repo`、`https://github.com/owner/repo`、`github.com/owner/repo.git`
/// 之类的输入统一成 `owner/repo`。
pub fn parse_full_name(input: &str) -> Option<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }

    let without_scheme = trimmed
        .strip_prefix("https://")
        .or_else(|| trimmed.strip_prefix("http://"))
        .unwrap_or(trimmed);
    let path = without_scheme
        .strip_prefix("github.com/")
        .unwrap_or(without_scheme);

    let mut parts = path.trim_matches('/').split('/');
    let owner = parts.next()?;
    let name = parts.next()?;
    if parts.next().is_some() {
        // 多出来的段（/tree/main 之类）不算仓库标识。
        return None;
    }

    let name = name.strip_suffix(".git").unwrap_or(name);
    if !is_repo_segment(owner) || !is_repo_segment(name) {
        return None;
    }
    Some(format!("{owner}/{name}"))
}

fn is_repo_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// 人工输入里常见全角空格与多余换行，统一收干净。
fn clean_text(text: Option<&str>) -> Option<&str> {
    text.map(str::trim).filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_the_forms_people_actually_paste() {
        for input in [
            "coder/coder",
            "  coder/coder  ",
            "https://github.com/coder/coder",
            "http://github.com/coder/coder/",
            "github.com/coder/coder",
            "https://github.com/coder/coder.git",
        ] {
            assert_eq!(
                parse_full_name(input).as_deref(),
                Some("coder/coder"),
                "输入：{input}"
            );
        }
    }

    #[test]
    fn rejects_input_that_is_not_a_repository() {
        for input in [
            "",
            "coder",
            "coder/coder/tree/main",
            "https://gitlab.com/coder/coder",
            "https://github.com/",
        ] {
            assert_eq!(parse_full_name(input), None, "输入：{input}");
        }
    }

    #[test]
    fn id_is_case_insensitive_so_the_same_repo_never_lands_twice() {
        let upper = Repo::new("Coder/Coder".to_string(), None, 0);
        let lower = Repo::new("coder/coder".to_string(), None, 0);
        assert_eq!(upper.id(), lower.id());
    }

    #[test]
    fn a_fresh_repo_has_no_category_and_no_tags() {
        let repo = Repo::new("coder/coder".to_string(), None, 1);
        assert_eq!(repo.category_id, None);
        assert!(repo.tags.is_empty());
        assert_eq!(repo.url, "https://github.com/coder/coder");
    }

    #[test]
    fn display_description_prefers_the_manual_chinese_text() {
        let mut repo = Repo::new("coder/coder".to_string(), None, 0);
        assert_eq!(repo.display_description(), None);

        repo.description = Some("原文".into());
        assert_eq!(repo.display_description(), Some("原文"));

        repo.description_zh = Some("  汉语  ".into());
        assert_eq!(repo.display_description(), Some("汉语"));

        // 只有空白的手工描述等同于没填。
        repo.description_zh = Some("   ".into());
        assert_eq!(repo.display_description(), Some("原文"));
    }

    #[test]
    fn pending_metadata_tracks_both_missing_fields() {
        let mut repo = Repo::new("coder/coder".to_string(), None, 0);
        assert!(repo.has_pending_metadata());

        repo.description = Some("描述".into());
        assert!(repo.has_pending_metadata(), "创建时间仍然缺失");

        repo.created_at = Some(1);
        assert!(!repo.has_pending_metadata());
    }

    /// 造一页结果：只关心条数，行内容无所谓。
    fn page(total: usize, page: usize, rows: usize) -> RepoPage {
        RepoPage {
            rows: (0..rows)
                .map(|index| RepoRow {
                    id: format!("a/{index}"),
                    full_name: format!("a/{index}"),
                    subtitle: String::new(),
                    language: None,
                    pending_metadata: false,
                    category_path: None,
                    category_leaf: None,
                    tags: Vec::new(),
                })
                .collect(),
            total,
            page,
            page_size: 50,
        }
    }

    #[test]
    fn page_count_never_drops_to_zero() {
        assert_eq!(page(0, 0, 0).page_count(), 1, "空库也要显示第 1/1 页");
        assert_eq!(page(50, 0, 50).page_count(), 1);
        assert_eq!(page(51, 0, 50).page_count(), 2);
        assert_eq!(page(120, 1, 50).page_count(), 3);
    }

    #[test]
    fn the_last_page_may_be_partial() {
        let last = page(120, 2, 20);
        assert_eq!(last.first_index(), 101);
        assert_eq!(last.last_index(), 120);
        assert!(last.has_prev());
        assert!(!last.has_next());

        let first = page(120, 0, 50);
        assert_eq!(first.first_index(), 1);
        assert_eq!(first.last_index(), 50);
        assert!(!first.has_prev());
        assert!(first.has_next());

        let empty = page(120, 3, 0);
        assert_eq!(empty.first_index(), 0);
        assert!(empty.is_out_of_range(), "筛完之后页码可能落在范围外");
    }
}

//! 仓库列表的 delegate。
//!
//! `List` 要求所有条目等高，所以每一行都固定渲染两行文本——第二行没有内容时
//! 用占位文字顶上，避免出现「有的行高一点」这种把虚拟列表算错的情况。分类色块
//! 挂在第一行：它是单值，不会再撑出一行来。

use gpui_kit::assets::IconName;
use gpui_kit::component::{
    ActiveTheme as _, Icon, IndexPath, h_flex, v_flex,
    list::{ListDelegate, ListItem, ListState},
};
use gpui_kit::prelude::*;
use gpui_kit::*;

use crate::repos::RepoId;
use crate::taxonomy::{chip, model as tax};

/// 列表上方的分类筛选。
///
/// 「未分类」是一个独立的选项而不是「名字为空」：没归类的仓库是真实存在的一类，
/// 得能单独看它们。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum CategoryFilter {
    #[default]
    All,
    Uncategorized,
    Named(String),
}

impl CategoryFilter {
    fn accepts(&self, category: Option<&str>) -> bool {
        match (self, category) {
            (Self::All, _) => true,
            (Self::Uncategorized, category) => category.is_none(),
            (Self::Named(name), Some(category)) => tax::same_name(name, category),
            (Self::Named(_), None) => false,
        }
    }
}

/// 列表里渲染一行所需的数据。是 `Repo` 的展示快照，不持有领域对象。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepoRow {
    pub id: RepoId,
    pub full_name: String,
    pub subtitle: String,
    pub language: Option<String>,
    pub pending_metadata: bool,
    pub category: Option<String>,
    pub tags: Vec<String>,
}

impl RepoRow {
    /// 搜索匹配：仓库名、描述、语言、分类、标签任一命中即可。
    fn matches(&self, query: &str) -> bool {
        self.full_name.to_lowercase().contains(query)
            || self.subtitle.to_lowercase().contains(query)
            || self
                .language
                .as_deref()
                .is_some_and(|language| language.to_lowercase().contains(query))
            || self
                .category
                .as_deref()
                .is_some_and(|category| category.to_lowercase().contains(query))
            || self
                .tags
                .iter()
                .any(|tag| tag.to_lowercase().contains(query))
    }
}

pub struct RepoListDelegate {
    /// 全量数据。
    all: Vec<RepoRow>,
    /// 按当前筛选条件过滤后的结果，`items_count` 与 `render_item` 都看它。
    rows: Vec<RepoRow>,
    query: String,
    category: CategoryFilter,
    selected: Option<RepoId>,
}

impl Default for RepoListDelegate {
    fn default() -> Self {
        Self::new()
    }
}

impl RepoListDelegate {
    pub fn new() -> Self {
        Self {
            all: Vec::new(),
            rows: Vec::new(),
            query: String::new(),
            category: CategoryFilter::All,
            selected: None,
        }
    }

    /// 用最新的仓库数据替换全量集合，并沿用当前的查询与筛选重新过滤。
    pub fn set_rows(&mut self, rows: Vec<RepoRow>) {
        self.all = rows;
        self.refilter();
    }

    pub fn set_selected(&mut self, id: Option<RepoId>) {
        self.selected = id;
    }

    /// 当前选中项。列表是选中的唯一持有者，视图靠观察它来同步详情面板。
    pub fn selection(&self) -> Option<&RepoId> {
        self.selected.as_ref()
    }

    /// 整个列表是不是空的（跟当前筛选无关），用于区分「一个仓库都没有」和
    /// 「筛完什么都不剩」。
    pub fn is_really_empty(&self) -> bool {
        self.all.is_empty()
    }

    /// 当前条件下的可见行数。
    pub fn visible_count(&self) -> usize {
        self.rows.len()
    }

    pub fn category_filter(&self) -> &CategoryFilter {
        &self.category
    }

    pub fn set_category_filter(&mut self, filter: CategoryFilter) {
        if self.category != filter {
            self.category = filter;
            self.refilter();
        }
    }

    pub fn row_at(&self, ix: IndexPath) -> Option<&RepoRow> {
        self.rows.get(ix.row)
    }

    fn refilter(&mut self) {
        let query = self.query.trim().to_lowercase();
        self.rows = self
            .all
            .iter()
            .filter(|row| self.category.accepts(row.category.as_deref()))
            .filter(|row| query.is_empty() || row.matches(&query))
            .cloned()
            .collect();
    }
}

impl ListDelegate for RepoListDelegate {
    type Item = ListItem;

    fn items_count(&self, _section: usize, _cx: &App) -> usize {
        self.rows.len()
    }

    fn render_item(
        &mut self,
        ix: IndexPath,
        _window: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> Option<Self::Item> {
        let row = self.rows.get(ix.row)?.clone();
        let is_selected = self.selected.as_deref() == Some(row.id.as_str());
        let (muted, dark) = {
            let theme = cx.theme();
            (theme.muted_foreground, theme.is_dark())
        };

        // 身份来自仓库本身，而不是行号：新增仓库会让行号整体后移，
        // 用行号做 ElementId 会让旧行的状态跟到新行上。
        let element_id: ElementId = format!("repo-row:{}", row.id).into();

        Some(
            ListItem::new(element_id)
                .selected(is_selected)
                .child(
                    v_flex()
                        .w_full()
                        .min_w_0()
                        .gap_1()
                        .child(
                            h_flex()
                                .w_full()
                                .min_w_0()
                                .gap_2()
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .text_sm()
                                        .truncate()
                                        .child(row.full_name.clone()),
                                )
                                .when_some(row.category.clone(), |this, category| {
                                    this.child(
                                        div()
                                            .flex_shrink_0()
                                            .child(chip::category_tag(&category, dark)),
                                    )
                                })
                                .when_some(row.language.clone(), |this, language| {
                                    this.child(
                                        div().flex_shrink_0().text_xs().text_color(muted).child(language),
                                    )
                                })
                                .when(row.pending_metadata, |this| {
                                    this.child(
                                        div()
                                            .flex_shrink_0()
                                            .text_xs()
                                            .text_color(muted)
                                            .child("待补全"),
                                    )
                                }),
                        )
                        .child(
                            div()
                                .w_full()
                                .min_w_0()
                                .text_xs()
                                .text_color(muted)
                                .truncate()
                                .child(row.subtitle.clone()),
                        ),
                ),
        )
    }

    fn perform_search(
        &mut self,
        query: &str,
        _window: &mut Window,
        _cx: &mut Context<ListState<Self>>,
    ) -> Task<()> {
        self.query = query.to_string();
        self.refilter();
        Task::ready(())
    }

    fn set_selected_index(
        &mut self,
        ix: Option<IndexPath>,
        _window: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) {
        self.selected = ix.and_then(|ix| self.rows.get(ix.row)).map(|row| row.id.clone());
        cx.notify();
    }

    fn render_empty(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let empty_library = self.all.is_empty();

        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_2()
            .p_4()
            .child(Icon::new(IconName::Inbox).size_8().text_color(muted))
            .child(
                div()
                    .text_sm()
                    .child(if empty_library { "这里还没有仓库" } else { "没有符合条件的仓库" }),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .text_center()
                    .child(if empty_library {
                        "点右上角「添加仓库」，或到「发现」页运行一次抓取"
                    } else {
                        "换个关键词，或把上方的分类筛选切回「全部」"
                    }),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // `use gpui_kit::*` 会把 GPUI 的 `#[gpui::test]` 也带进来，把 std 的
    // `#[test]` 盖掉；显式再导一次标准 prelude 里的那个，恢复原语义。
    use core::prelude::v1::test;

    fn row(id: &str, subtitle: &str, language: Option<&str>) -> RepoRow {
        RepoRow {
            id: id.to_string(),
            full_name: id.to_string(),
            subtitle: subtitle.to_string(),
            language: language.map(str::to_string),
            pending_metadata: false,
            category: None,
            tags: Vec::new(),
        }
    }

    fn tagged(id: &str, category: Option<&str>, tags: &[&str]) -> RepoRow {
        RepoRow {
            category: category.map(str::to_string),
            tags: tags.iter().map(|tag| tag.to_string()).collect(),
            ..row(id, "描述", None)
        }
    }

    #[test]
    fn filter_matches_name_description_and_language() {
        let mut delegate = RepoListDelegate::new();
        delegate.set_rows(vec![
            row("coder/coder", "云开发环境", Some("Go")),
            row("rust-lang/rust", "系统编程语言", Some("Rust")),
        ]);

        delegate.query = "coder".into();
        delegate.refilter();
        assert_eq!(delegate.rows.len(), 1);
        assert_eq!(delegate.rows[0].id, "coder/coder");

        delegate.query = "系统编程".into();
        delegate.refilter();
        assert_eq!(delegate.rows[0].id, "rust-lang/rust");

        delegate.query = "RUST".into();
        delegate.refilter();
        assert_eq!(delegate.rows[0].id, "rust-lang/rust");

        delegate.query = "不存在的词".into();
        delegate.refilter();
        assert!(delegate.rows.is_empty());
    }

    #[test]
    fn search_reaches_categories_and_tags() {
        let mut delegate = RepoListDelegate::new();
        delegate.set_rows(vec![
            tagged("coder/coder", Some("开发工具"), &["cli"]),
            tagged("rust-lang/rust", Some("语言"), &["待读"]),
        ]);

        delegate.query = "开发".into();
        delegate.refilter();
        assert_eq!(delegate.rows[0].id, "coder/coder");

        delegate.query = "待读".into();
        delegate.refilter();
        assert_eq!(delegate.rows[0].id, "rust-lang/rust");
    }

    #[test]
    fn category_filter_separates_named_and_uncategorized() {
        let mut delegate = RepoListDelegate::new();
        delegate.set_rows(vec![
            tagged("a/one", Some("工具"), &[]),
            tagged("b/two", Some("语言"), &[]),
            tagged("c/three", None, &[]),
        ]);

        assert_eq!(delegate.visible_count(), 3, "默认看全部");

        delegate.set_category_filter(CategoryFilter::Named("工具".into()));
        assert_eq!(delegate.visible_count(), 1);
        assert_eq!(delegate.rows[0].id, "a/one");

        delegate.set_category_filter(CategoryFilter::Uncategorized);
        assert_eq!(delegate.visible_count(), 1);
        assert_eq!(delegate.rows[0].id, "c/three");

        delegate.set_category_filter(CategoryFilter::Named("tool".into()));
        assert_eq!(delegate.visible_count(), 0, "名字不匹配就是空列表");
    }

    #[test]
    fn category_filter_compares_names_ignoring_case() {
        let mut delegate = RepoListDelegate::new();
        delegate.set_rows(vec![tagged("a/one", Some("AI"), &[])]);

        delegate.set_category_filter(CategoryFilter::Named("ai".into()));
        assert_eq!(delegate.visible_count(), 1);
    }

    #[test]
    fn category_filter_and_search_stack() {
        let mut delegate = RepoListDelegate::new();
        delegate.set_rows(vec![
            tagged("a/one", Some("工具"), &[]),
            tagged("b/two", Some("工具"), &[]),
            tagged("c/three", Some("语言"), &[]),
        ]);

        delegate.set_category_filter(CategoryFilter::Named("工具".into()));
        delegate.query = "b/two".into();
        delegate.refilter();
        assert_eq!(delegate.visible_count(), 1);
        assert_eq!(delegate.rows[0].id, "b/two");
    }

    #[test]
    fn replacing_rows_keeps_the_active_query_and_filter() {
        let mut delegate = RepoListDelegate::new();
        delegate.query = "coder".into();
        delegate.set_category_filter(CategoryFilter::Named("工具".into()));
        delegate.set_rows(vec![tagged("coder/coder", Some("工具"), &[])]);

        // 后台发现了新仓库时，查询和筛选都不能被清掉。
        delegate.set_rows(vec![
            tagged("coder/coder", Some("工具"), &[]),
            tagged("coder/other", Some("工具"), &[]),
            tagged("foo/bar", Some("语言"), &[]),
        ]);
        assert_eq!(delegate.visible_count(), 2);

        delegate.query = "other".into();
        delegate.refilter();
        assert_eq!(delegate.visible_count(), 1);
        assert_eq!(delegate.rows[0].id, "coder/other");
    }

    #[test]
    fn empty_library_is_distinguishable_from_an_empty_filter() {
        let mut delegate = RepoListDelegate::new();
        assert!(delegate.is_really_empty());

        delegate.set_rows(vec![tagged("a/one", Some("工具"), &[])]);
        delegate.set_category_filter(CategoryFilter::Named("没有这个".into()));
        assert!(!delegate.is_really_empty(), "有仓库，只是一个都没筛出来");
        assert_eq!(delegate.visible_count(), 0);
    }
}

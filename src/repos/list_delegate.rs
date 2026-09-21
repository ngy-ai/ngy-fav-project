//! 仓库列表的 delegate。
//!
//! **列表是按页从数据库取的**：delegate 手上只有当前这一页，搜索和分类筛选也都是
//! 往 SQL 里下的条件。它自己不碰数据库——只记「想要什么条件」和「上一次拿回来
//! 什么」，查库由视图做（视图才拿得到 `Library`）。所以这个文件里全是纯状态，
//! 一条测试都不需要启动 GPUI。
//!
//! 一次完整的来回长这样：
//!
//! 1. 有人改了条件（输入搜索词、点分类、点翻页）→ delegate 里的 `query` 变了；
//! 2. 视图拿 `query` 去问库，把结果 `accept_page` 塞回来；
//! 3. `accept_page` 告诉你这一页跟原来是不是一样——一样就什么都不用做，
//!    否则视图重绘。同一页反复对比是幂等的，所以「改条件 → 查库 → 塞回来」
//!    这个环不会转不停。
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

use crate::repos::model::{CategoryFilter, DEFAULT_PAGE_SIZE, RepoId, RepoPage, RepoQuery, RepoRow};
use crate::taxonomy::chip;

pub struct RepoListDelegate {
    /// 当前想要的查询条件。翻页就是改它里面的 `page`。
    query: RepoQuery,
    /// 上一次查回来的那一页。
    page: RepoPage,
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
            query: RepoQuery::default(),
            page: RepoPage {
                page_size: DEFAULT_PAGE_SIZE,
                ..Default::default()
            },
            selected: None,
        }
    }

    pub fn query(&self) -> &RepoQuery {
        &self.query
    }

    /// 上一次查回来的那一页。翻页条上的「第 N/M 页」和「共几条」都在这里。
    pub fn page(&self) -> &RepoPage {
        &self.page
    }

    /// 符合条件的总数，不是这一页的条数。
    pub fn total(&self) -> usize {
        self.page.total
    }

    /// 这一页有几行。
    pub fn visible_count(&self) -> usize {
        self.page.rows.len()
    }

    /// 把刚查回来的一页放进来。返回 `true` 表示跟原来不一样。
    ///
    /// 视图拿这个返回值决定要不要重绘：结果没变就不通知，否则「通知 → 观察者
    /// 再查一次 → 再放进来」会转成一个停不下来的环。
    pub fn accept_page(&mut self, page: RepoPage) -> bool {
        if page.page != self.query.page {
            // 库那边发现页码越界时会把结果回退到最后一页，条件得跟着改，
            // 否则下一次翻页又从原来那个越界的页码上算。
            self.query.page = page.page;
        }
        if !self.page_changed(&page) {
            return false;
        }
        self.page = page;
        true
    }

    fn page_changed(&self, page: &RepoPage) -> bool {
        self.page.page != page.page
            || self.page.total != page.total
            || self.page.page_size != page.page_size
            || self.page.rows.len() != page.rows.len()
            || self
                .page
                .rows
                .iter()
                .zip(page.rows.iter())
                .any(|(old, new)| old != new)
    }

    /// 换了搜索词就从第一页重新看——停在第 3 页搜出一个只有半页的结果没有意义。
    pub fn set_search(&mut self, text: &str) {
        self.query.search = text.to_string();
        self.query.page = 0;
    }

    pub fn category_filter(&self) -> &CategoryFilter {
        &self.query.category
    }

    /// 切换分类筛选，同样回到第一页。
    pub fn set_category_filter(&mut self, filter: CategoryFilter) {
        if self.query.category == filter {
            return;
        }
        self.query.category = filter;
        self.query.page = 0;
    }

    /// 请求某一页。越界由 `Library` 那边回退，这里不做二次判断。
    pub fn request_page(&mut self, page: usize) {
        self.query.page = page;
    }

    /// 请求下一页／上一页。已经在头尾时返回 `false`，`query` 不动。
    pub fn go_to_next_page(&mut self) -> bool {
        if !self.page.has_next() {
            return false;
        }
        self.request_page(self.page.page + 1);
        true
    }

    pub fn go_to_prev_page(&mut self) -> bool {
        if !self.page.has_prev() {
            return false;
        }
        self.request_page(self.page.page - 1);
        true
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
        self.page.total == 0
            && self.query.search.trim().is_empty()
            && self.query.category == CategoryFilter::All
    }

    pub fn row_at(&self, ix: IndexPath) -> Option<&RepoRow> {
        self.page.rows.get(ix.row)
    }
}

impl ListDelegate for RepoListDelegate {
    type Item = ListItem;

    fn items_count(&self, _section: usize, _cx: &App) -> usize {
        self.page.rows.len()
    }

    fn render_item(
        &mut self,
        ix: IndexPath,
        _window: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> Option<Self::Item> {
        let row = self.page.rows.get(ix.row)?.clone();
        let is_selected = self.selected.as_deref() == Some(row.id.as_str());
        let (muted, dark) = {
            let theme = cx.theme();
            (theme.muted_foreground, theme.is_dark())
        };

        // 身份来自仓库本身，而不是行号：翻页之后同一个行号上是另一个仓库，
        // 用行号做 ElementId 会让旧行的状态跟到新行上。
        let element_id: ElementId = format!("repo-row:{}", row.id).into();
        // 路径决定颜色、末级名字决定显示的字，两者都是查库时算好的。
        let category = row.category_path.clone().zip(row.category_leaf.clone());

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
                                .when_some(category, |this, (path, leaf)| {
                                    this.child(
                                        div()
                                            .flex_shrink_0()
                                            .child(chip::category_leaf_tag(&path, &leaf, dark)),
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
        cx: &mut Context<ListState<Self>>,
    ) -> Task<()> {
        self.set_search(query);
        // 通知出去，视图收到之后才会拿新条件去查库。
        cx.notify();
        Task::ready(())
    }

    fn set_selected_index(
        &mut self,
        ix: Option<IndexPath>,
        _window: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) {
        self.selected = ix
            .and_then(|ix| self.page.rows.get(ix.row))
            .map(|row| row.id.clone());
        cx.notify();
    }

    fn render_empty(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        // 一条都没有、也没加过条件，才是真的什么都没有。
        let empty_library = self.is_really_empty();

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

    fn row(id: &str) -> RepoRow {
        RepoRow {
            id: id.to_string(),
            full_name: id.to_string(),
            subtitle: "描述".to_string(),
            language: None,
            pending_metadata: false,
            category_path: None,
            category_leaf: None,
            tags: Vec::new(),
        }
    }

    /// 造一页结果。`rows` 只给 id，够判断「变没变」了。
    fn page(total: usize, page: usize, rows: &[&str]) -> RepoPage {
        RepoPage {
            rows: rows.iter().map(|id| row(id)).collect(),
            total,
            page,
            page_size: DEFAULT_PAGE_SIZE,
        }
    }

    #[test]
    fn searching_goes_back_to_the_first_page() {
        let mut delegate = RepoListDelegate::new();
        delegate.accept_page(page(200, 2, &["a/one"]));

        delegate.set_search("rust");
        assert_eq!(delegate.query().page, 0);
        assert_eq!(delegate.query().search, "rust");
    }

    #[test]
    fn switching_the_category_goes_back_to_the_first_page() {
        let mut delegate = RepoListDelegate::new();
        delegate.accept_page(page(200, 3, &["a/one"]));

        delegate.set_category_filter(CategoryFilter::Named("工具".into()));
        assert_eq!(delegate.query().page, 0);
        assert_eq!(delegate.category_filter(), &CategoryFilter::Named("工具".into()));

        // 再点一次同一个分类不该把条件重置掉——那会把用户翻到的页数抹掉。
        delegate.accept_page(page(200, 1, &["a/one"]));
        delegate.set_category_filter(CategoryFilter::Named("工具".into()));
        assert_eq!(delegate.query().page, 1);
    }

    #[test]
    fn accept_page_says_whether_anything_changed() {
        let mut delegate = RepoListDelegate::new();

        assert!(
            delegate.accept_page(page(2, 0, &["a/one", "b/two"])),
            "第一次查回来一定有变化"
        );
        assert!(
            !delegate.accept_page(page(2, 0, &["a/one", "b/two"])),
            "一模一样的一页不该让视图再重绘一次，否则通知会自己转下去"
        );

        assert!(
            delegate.accept_page(page(2, 0, &["a/one", "c/three"])),
            "行内容变了也算变了"
        );
        assert!(delegate.accept_page(page(3, 0, &["a/one", "c/three"])), "总数变了");
        assert!(delegate.accept_page(page(3, 1, &[])), "页码变了");
    }

    #[test]
    fn the_effective_page_wins_over_the_requested_one() {
        let mut delegate = RepoListDelegate::new();
        delegate.request_page(9);
        assert_eq!(delegate.query().page, 9);

        // 库里只有两页，查回来的是第 1 页（从 0 数）。
        delegate.accept_page(page(60, 1, &["a/one"]));
        assert_eq!(
            delegate.query().page,
            1,
            "条件要跟着实际生效的页码走，否则下一次翻页又从 9 往下算"
        );
    }

    #[test]
    fn page_requests_stop_at_the_ends() {
        let mut delegate = RepoListDelegate::new();
        delegate.accept_page(page(60, 0, &["a/one"]));

        assert!(!delegate.go_to_prev_page(), "已经在第一页");
        assert_eq!(delegate.query().page, 0);

        assert!(delegate.go_to_next_page());
        assert_eq!(delegate.query().page, 1);

        delegate.accept_page(page(60, 1, &["z/last"]));
        assert!(!delegate.go_to_next_page(), "已经在最后一页");
        assert_eq!(delegate.query().page, 1);
    }

    #[test]
    fn an_empty_library_is_distinguishable_from_an_empty_filter() {
        let mut delegate = RepoListDelegate::new();
        delegate.accept_page(page(0, 0, &[]));
        assert!(delegate.is_really_empty(), "库里一条都没有");

        delegate.accept_page(page(0, 0, &[]));
        delegate.set_category_filter(CategoryFilter::Named("没有这个".into()));
        delegate.accept_page(page(0, 0, &[]));
        assert!(!delegate.is_really_empty(), "有筛选条件，只是没筛出东西");

        let mut delegate = RepoListDelegate::new();
        delegate.set_search("rust");
        delegate.accept_page(page(0, 0, &[]));
        assert!(!delegate.is_really_empty(), "搜不到不等于库是空的");
    }

    #[test]
    fn the_selection_survives_a_page_change() {
        let mut delegate = RepoListDelegate::new();
        delegate.accept_page(page(200, 0, &["a/one"]));
        delegate.set_selected(Some("a/one".to_string()));

        delegate.go_to_next_page();
        delegate.accept_page(page(200, 1, &["b/two"]));
        assert_eq!(
            delegate.selection().map(String::as_str),
            Some("a/one"),
            "选中项只是不在这页上了，不该被当成不存在"
        );
    }

    #[test]
    fn the_total_is_not_the_number_of_rows_on_this_page() {
        let mut delegate = RepoListDelegate::new();
        delegate.accept_page(page(120, 0, &["a/one", "b/two"]));

        assert_eq!(delegate.total(), 120);
        assert_eq!(delegate.visible_count(), 2);
        assert_eq!(delegate.page().page_count(), 3);
    }
}

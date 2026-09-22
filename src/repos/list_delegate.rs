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
//!
//! 每一行左边还有一个勾选框，为的是**批量操作**（眼下只有整批改分类）。勾选集合跟
//! `selected` 是两件事：那是「详情面板正在看哪一个」，同时只能有一个。勾选可以跨页、
//! 跨筛选条件地留着——用户勾了就是不希望它因为翻页而消失。
//!
//! 「全选」选的是**筛出来的整批**，不是这一页：分页只是取的时候分，用户说的「这些」
//! 是全部。整批的 id 是视图查一次塞进来的（[`RepoListDelegate::set_matching`]），
//! 这里只拿它算那个方框的样子。

use std::collections::BTreeSet;

use gpui_kit::assets::IconName;
use gpui_kit::component::{
    ActiveTheme as _, Icon, IndexPath, Sizable as _, h_flex, v_flex,
    checkbox::Checkbox,
    list::{ListDelegate, ListItem, ListState},
};
use gpui_kit::prelude::*;
use gpui_kit::*;

use crate::repos::model::{CategoryFilter, DEFAULT_PAGE_SIZE, RepoId, RepoPage, RepoQuery, RepoRow};
use crate::shared::text::{copyable, copyable_as};
use crate::taxonomy::chip;
use crate::taxonomy::model as tax;

/// 「全选」那个方框的样子。**三态而不是两态**：筛出 200 条、勾了 3 条时，方框既不是
/// 空的也不该是满的，说「全选了」是假话。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectAll {
    /// 匹配项一个都没勾。
    None,
    /// 勾了一部分。
    Partial,
    /// 匹配项全勾上了。
    All,
}

pub struct RepoListDelegate {
    /// 当前想要的查询条件。翻页就是改它里面的 `page`。
    query: RepoQuery,
    /// 上一次查回来的那一页。
    page: RepoPage,
    /// 详情面板正在看的那一个。跟 `checked` 是两件事。
    selected: Option<RepoId>,
    /// 当前条件下**全部**匹配的 id（跨页），顺序跟列表一致。
    ///
    /// 只用来算「全选」那个方框的样子，由视图查完塞进来。看不出「整批有多全」，
    /// 光靠这一页的行算不出来。
    matching: Vec<RepoId>,
    /// 勾选集合，批量操作作用在它身上。
    ///
    /// 在集合里但不在当前筛选里是正常的——那说明用户在别的条件下勾过它。所以它
    /// 不能跟着筛选条件走，只有仓库真的从库里消失了才该摘掉
    /// （[`RepoListDelegate::retain_checked`]）。
    checked: BTreeSet<RepoId>,
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
            matching: Vec::new(),
            checked: BTreeSet::new(),
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

    /// 当前选的语言。`None` 是「不限语言」。
    pub fn language_filter(&self) -> Option<&str> {
        self.query.language.as_deref()
    }

    /// 换一种语言；`None` 退回「不限语言」。同样回到第一页。
    pub fn set_language(&mut self, language: Option<String>) {
        if self.query.language == language {
            return;
        }
        self.query.language = language;
        self.query.page = 0;
    }

    /// 当前选中的标签。
    pub fn tags_filter(&self) -> &[String] {
        &self.query.tags
    }

    /// 整体替换标签选择。落进来的东西会先收干净：去空白、忽略大小写去重，
    /// 再按 [`name_key`](crate::taxonomy::model::name_key) 排序。
    ///
    /// **顺序不跟着点击走**是有意的：「先点 A 再点 B」和「先点 B 再点 A」是同一
    /// 组选择，必须写出同一份条件——否则「条件没变就不重查」这类比较会失效，
    /// 顺带把 SQL 里的参数顺序也弄成看运气的。
    pub fn set_tags(&mut self, tags: Vec<String>) {
        let mut cleaned: Vec<String> = Vec::with_capacity(tags.len());
        for tag in tags {
            let tag = tag.trim();
            if tag.is_empty() || cleaned.iter().any(|it| tax::same_name(it, tag)) {
                continue;
            }
            cleaned.push(tag.to_string());
        }
        cleaned.sort_by_key(|name| tax::name_key(name));

        if self.query.tags == cleaned {
            return;
        }
        self.query.tags = cleaned;
        self.query.page = 0;
    }

    /// 点一下某个标签：挂着就摘掉，没挂就挂上。
    pub fn toggle_tag(&mut self, name: &str) {
        let mut tags = self.query.tags.clone();
        match tags.iter().position(|it| tax::same_name(it, name)) {
            Some(position) => {
                tags.remove(position);
            }
            None => tags.push(name.to_string()),
        }
        self.set_tags(tags);
    }

    /// 清掉三个下拉上的筛选。**不动搜索词**：搜索框里显示什么由它自己的状态管，
    /// 从这里抹掉只会让框里的字和实际条件对不上。
    pub fn clear_filters(&mut self) {
        let had = self.has_facet_filters();
        self.query.category = CategoryFilter::All;
        self.query.language = None;
        self.query.tags.clear();
        if had {
            self.query.page = 0;
        }
    }

    /// 三个下拉上有条件没有——「清除筛选」按钮显不显示就看它。
    /// 按钮清的东西和这个判断必须是同一件事，所以放在一起。
    pub fn has_facet_filters(&self) -> bool {
        self.query.category != CategoryFilter::All
            || self.query.language.is_some()
            || !self.query.tags.is_empty()
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

    // ---- 勾选（批量操作用） ----

    /// 记下「当前条件下全部匹配的 id」。视图每次查完库顺手塞一次。
    pub fn set_matching(&mut self, ids: Vec<RepoId>) {
        self.matching = ids;
    }

    pub fn is_checked(&self, id: &str) -> bool {
        self.checked.contains(id)
    }

    /// 勾上 / 摘掉一个。
    ///
    /// 拆成两个方法而不是一个「切换」：复选框交给回调的是**用户想要的样子**，
    /// 由它翻译成这两件事。取反会让「点一下却什么都没发生」看起来像失灵。
    pub fn check(&mut self, id: &str) {
        self.checked.insert(id.to_string());
    }

    pub fn uncheck(&mut self, id: &str) {
        self.checked.remove(id);
    }

    pub fn clear_checked(&mut self) {
        self.checked.clear();
    }

    /// 整体替换勾选集合。用它的只有一处：把已经不在库里的那些摘掉之后写回来。
    pub fn set_checked(&mut self, ids: Vec<RepoId>) {
        self.checked = ids.into_iter().collect();
    }

    /// 勾了多少个。界面上的「已选 N 个」就是它。
    pub fn checked_count(&self) -> usize {
        self.checked.len()
    }

    /// 勾上的那些 id，交给批量操作。
    pub fn checked_ids(&self) -> Vec<RepoId> {
        self.checked.iter().cloned().collect()
    }

    /// 摘掉已经不在库里的勾选（列表上勾完，别处把它删了）。返回有没有真摘掉。
    pub fn retain_checked(&mut self, keep: impl Fn(&str) -> bool) -> bool {
        let before = self.checked.len();
        self.checked.retain(|id| keep(id));
        self.checked.len() != before
    }

    /// 「全选」那个方框现在该长什么样。
    pub fn select_all_state(&self) -> SelectAll {
        let mut any = false;
        let mut missing = false;
        for id in &self.matching {
            if self.checked.contains(id) {
                any = true;
            } else {
                missing = true;
            }
        }
        match (any, missing) {
            // 匹配项一条都没有时也是「没勾」——那时没东西可选，方框就该是空的。
            (false, _) => SelectAll::None,
            (true, false) => SelectAll::All,
            (true, true) => SelectAll::Partial,
        }
    }

    /// 点「全选」：把当前匹配的整批勾上；已经全勾上时反过来把它们摘掉。
    ///
    /// 摘的是**匹配的那些**，不是整个勾选集合——里面可能还有别的条件下勾的，
    /// 那不属于这一次点击的范围，不该被顺手清掉。
    pub fn toggle_select_all(&mut self) {
        match self.select_all_state() {
            SelectAll::All => {
                let matching: BTreeSet<RepoId> = self.matching.iter().cloned().collect();
                self.checked.retain(|id| !matching.contains(id));
            }
            _ => self.checked.extend(self.matching.iter().cloned()),
        }
    }

    /// 整个列表是不是空的（跟当前筛选无关），用于区分「一个仓库都没有」和
    /// 「筛完什么都不剩」。
    pub fn is_really_empty(&self) -> bool {
        self.page.total == 0 && !self.query.has_filters()
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
        let checked = self.is_checked(&row.id);

        Some(
            ListItem::new(element_id)
                .selected(is_selected)
                .child(
                    h_flex()
                        .w_full()
                        .min_w_0()
                        .gap_2()
                        .items_start()
                        .child(
                            // 勾选框是批量操作的入口。外面这层只为了给它一个可观测的身份
                            // （`Checkbox` 是 `RenderOnce`，挂不上 `test_support`）。
                            div()
                                .id(ElementId::from(format!("repo-check-slot:{}", row.id)))
                                .flex_shrink_0()
                                .mt(px(2.))
                                // 勾选不该顺手把右边的详情面板也换掉——那是「点这一行」的事。
                                .on_mouse_up(
                                    MouseButton::Left,
                                    cx.listener(|_, _, _, cx| cx.stop_propagation()),
                                )
                                .child(
                                    Checkbox::new(format!("repo-check:{}", row.id))
                                        .checked(checked)
                                        .small()
                                        .tooltip("勾上，之后可以整批改分类")
                                        .on_change(cx.listener({
                                            let id = row.id.clone();
                                            move |state, wanted: &bool, _, cx| {
                                                let delegate = state.delegate_mut();
                                                if *wanted {
                                                    delegate.check(&id);
                                                } else {
                                                    delegate.uncheck(&id);
                                                }
                                                cx.notify();
                                            }
                                        })),
                                )
                                .test_support(),
                        )
                        .child(
                            v_flex()
                                .flex_1()
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
                                                .child(copyable(row.full_name.clone())),
                                        )
                                        .when_some(category, |this, (path, leaf)| {
                                            this.child(
                                                div().flex_shrink_0().child(
                                                    chip::category_leaf_tag(
                                                        format!("repo-category:{}", row.id),
                                                        &path,
                                                        &leaf,
                                                        dark,
                                                    ),
                                                ),
                                            )
                                        })
                                        .when_some(row.language.clone(), |this, language| {
                                            this.child(
                                                div()
                                                    .flex_shrink_0()
                                                    .text_xs()
                                                    .text_color(muted)
                                                    .child(copyable(language)),
                                            )
                                        })
                                        .when(row.pending_metadata, |this| {
                                            this.child(
                                                div()
                                                    .flex_shrink_0()
                                                    .text_xs()
                                                    .text_color(muted)
                                                    .child(copyable("待补全")),
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
                                        .child(copyable(row.subtitle.clone())),
                                ),
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
                div().text_sm().child(if empty_library {
                    copyable_as("repos-empty-title-none", "这里还没有仓库")
                } else {
                    copyable_as("repos-empty-title-filtered", "没有符合条件的仓库")
                }),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .text_center()
                    .child(if empty_library {
                        copyable_as(
                            "repos-empty-hint-none",
                            "点右上角「添加仓库」，或到「发现」页运行一次抓取",
                        )
                    } else {
                        copyable_as(
                            "repos-empty-hint-filtered",
                            "换个关键词，或把上方的分类筛选切回「全部」",
                        )
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
    fn picking_a_language_goes_back_to_the_first_page() {
        let mut delegate = RepoListDelegate::new();
        delegate.accept_page(page(200, 3, &["a/one"]));

        delegate.set_language(Some("Rust".to_string()));
        assert_eq!(delegate.query().page, 0);
        assert_eq!(delegate.language_filter(), Some("Rust"));

        // 再点同一个不该把页码重置掉——那会把用户翻到的位置抹了。
        delegate.accept_page(page(200, 2, &["a/one"]));
        delegate.set_language(Some("Rust".to_string()));
        assert_eq!(delegate.query().page, 2);

        delegate.set_language(None);
        assert_eq!(delegate.language_filter(), None, "「不限语言」要把条件清掉");
    }

    #[test]
    fn a_tag_is_toggled_on_and_off() {
        let mut delegate = RepoListDelegate::new();
        delegate.accept_page(page(200, 3, &["a/one"]));

        delegate.toggle_tag("cli");
        assert_eq!(delegate.tags_filter(), ["cli"]);
        assert_eq!(delegate.query().page, 0, "加了筛选就回第一页");

        delegate.toggle_tag("待读");
        assert_eq!(delegate.tags_filter(), ["cli", "待读"]);

        // 大小写不同也是同一个标签，再点一次要摘掉。
        delegate.toggle_tag("CLI");
        assert_eq!(delegate.tags_filter(), ["待读"]);

        delegate.toggle_tag("待读");
        assert!(delegate.tags_filter().is_empty());
    }

    #[test]
    fn the_tag_order_follows_the_name_not_the_click_order() {
        // 同一组选择必须写出同一份条件，否则「条件没变就不重查」的比较会失效，
        // 库里那条 SQL 的参数顺序也变成看运气。
        let mut one = RepoListDelegate::new();
        one.toggle_tag("cli");
        one.toggle_tag("待读");

        let mut other = RepoListDelegate::new();
        other.toggle_tag("待读");
        other.toggle_tag("cli");

        assert_eq!(one.tags_filter(), other.tags_filter());
        assert_eq!(one.query(), other.query());
    }

    #[test]
    fn clearing_puts_every_dropdown_back_but_leaves_the_search_word() {
        let mut delegate = RepoListDelegate::new();
        delegate.set_search("rust");
        delegate.set_category_filter(CategoryFilter::Named("工具".into()));
        delegate.set_language(Some("Rust".to_string()));
        delegate.toggle_tag("cli");
        assert!(delegate.has_facet_filters());

        delegate.clear_filters();
        // 搜索框里的字由它自己的状态管，从这里抹掉只会让框里显示的和实际条件对不上。
        assert_eq!(delegate.query().search, "rust", "搜索词不归「清除」管");
        assert_eq!(delegate.category_filter(), &CategoryFilter::All);
        assert_eq!(delegate.language_filter(), None);
        assert!(delegate.tags_filter().is_empty());
        assert!(!delegate.has_facet_filters());
    }

    #[test]
    fn a_language_or_tag_filter_is_not_an_empty_library() {
        let mut delegate = RepoListDelegate::new();
        delegate.set_language(Some("Rust".to_string()));
        delegate.accept_page(page(0, 0, &[]));
        assert!(!delegate.is_really_empty(), "有语言条件，只是没筛出东西");

        let mut delegate = RepoListDelegate::new();
        delegate.toggle_tag("cli");
        delegate.accept_page(page(0, 0, &[]));
        assert!(!delegate.is_really_empty(), "有标签条件，只是没筛出东西");
    }

    #[test]
    fn accept_page_says_whether_anything_changed() {        let mut delegate = RepoListDelegate::new();

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

    #[test]
    fn the_select_all_box_has_three_states_not_two() {
        let mut delegate = RepoListDelegate::new();
        delegate.set_matching(vec!["a/one".to_string(), "b/two".to_string()]);
        assert_eq!(delegate.select_all_state(), SelectAll::None, "一条都没勾");

        delegate.check("a/one");
        assert_eq!(delegate.select_all_state(), SelectAll::Partial, "勾了一半");

        delegate.check("b/two");
        assert_eq!(delegate.select_all_state(), SelectAll::All);

        // 勾选里还有匹配之外的（别的条件下勾的），不影响这个方框——它说的是
        // 「筛出来的这些勾全了吗」。
        delegate.check("c/three");
        assert_eq!(delegate.select_all_state(), SelectAll::All);

        // 一条都没筛出来时没东西可选，方框该是空的。
        delegate.set_matching(Vec::new());
        assert_eq!(delegate.select_all_state(), SelectAll::None);
    }

    #[test]
    fn select_all_takes_the_whole_filtered_batch_not_this_page() {
        let mut delegate = RepoListDelegate::new();
        // 这一页只装得下两条，而筛出来的是四条。
        delegate.accept_page(page(4, 0, &["a/one", "b/two"]));
        delegate.set_matching(vec![
            "a/one".to_string(),
            "b/two".to_string(),
            "c/three".to_string(),
            "d/four".to_string(),
        ]);

        delegate.toggle_select_all();
        assert_eq!(
            delegate.checked_count(),
            4,
            "翻页看不见的那两条也要勾上，这才是「全选筛选结果」"
        );
        assert_eq!(delegate.select_all_state(), SelectAll::All);

        // 再点一次：把匹配的这批摘掉，别的条件下勾的留着。
        delegate.check("z/kept");
        delegate.toggle_select_all();
        assert_eq!(
            delegate.checked_ids(),
            ["z/kept"],
            "只摘匹配的那些，不顺手清掉整个勾选集合"
        );
    }

    #[test]
    fn checking_a_row_outlives_paging_and_filtering() {
        let mut delegate = RepoListDelegate::new();
        delegate.accept_page(page(200, 0, &["a/one"]));
        delegate.check("a/one");

        delegate.go_to_next_page();
        delegate.accept_page(page(200, 1, &["b/two"]));
        assert!(delegate.is_checked("a/one"), "翻页不该把勾选丢掉");

        delegate.set_language(Some("Rust".to_string()));
        delegate.accept_page(page(1, 0, &["b/two"]));
        assert!(delegate.is_checked("a/one"), "换个筛选条件也一样");

        // 勾选跟「详情面板正在看哪一个」是两件事。
        delegate.set_selected(Some("b/two".to_string()));
        assert!(delegate.is_checked("a/one"));
        assert!(!delegate.is_checked("b/two"));
    }

    #[test]
    fn a_checked_repo_that_left_the_library_is_dropped() {
        let mut delegate = RepoListDelegate::new();
        delegate.check("a/one");
        delegate.check("b/two");

        assert!(
            delegate.retain_checked(|id| id != "a/one"),
            "真摘掉了才回报 true"
        );
        assert_eq!(delegate.checked_ids(), ["b/two"]);
        assert!(!delegate.retain_checked(|_| true), "没什么可摘就别谎报");
    }

    #[test]
    fn checking_and_unchecking_a_single_row_leaves_the_rest_alone() {
        let mut delegate = RepoListDelegate::new();
        delegate.check("a/one");
        delegate.check("b/two");

        delegate.uncheck("a/one");
        assert_eq!(delegate.checked_ids(), ["b/two"]);
        // 复选框交回来的是「想要的样子」，重复写同一个值不该翻车。
        delegate.check("b/two");
        assert_eq!(delegate.checked_count(), 1);

        delegate.clear_checked();
        assert_eq!(delegate.checked_count(), 0);
        assert_eq!(delegate.select_all_state(), SelectAll::None);
    }
}

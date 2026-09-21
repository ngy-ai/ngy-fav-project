//! 仓库页：左侧列表 + 右侧详情。
//!
//! 选中状态只有一份，放在列表的 delegate 里（它无论如何都要知道谁被选中才能
//! 画出高亮），本视图通过观察列表把它镜像出来给详情面板用。

use gpui_kit::assets::IconName;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Sizable as _, WindowExt as _, h_flex, v_flex,
    button::{Button, ButtonVariants as _},
    description_list::DescriptionList,
    dialog::{DialogAction, DialogClose, DialogFooter},
    input::{InputEvent, InputState},
    list::{List, ListState},
    notification::Notification,
    tag::Tag,
};
use gpui_kit::prelude::*;
use gpui_kit::*;

use crate::discovery::DiscoveryRunner;
use crate::library::{CategoryRow, Library};
use crate::repos::add_repo_dialog;
use crate::repos::edit_description_dialog;
use crate::repos::list_delegate::RepoListDelegate;
use crate::repos::model::{CategoryFilter, RepoPage};
use crate::repos::taxonomy_dialog;
use crate::repos::RepoId;
use crate::shared::clock::{format_local_date, format_local_minute, format_relative, now_unix};
use crate::shared::text::{copyable, copyable_as};
use crate::taxonomy::chip;
use crate::taxonomy::model as tax;

/// 列表栏宽度。固定宽度让详情面板有稳定的阅读宽度，也避免拖拽时重新布局。
const LIST_WIDTH: f32 = 320.;

/// 详情面板需要的一次性快照。
///
/// 渲染时先把要显示的东西抄成自有数据，避免在借用 `Library` 的同时还要
/// 生成 `cx.listener(...)` 回调。
struct RepoDetail {
    id: RepoId,
    full_name: String,
    url: String,
    description: Option<String>,
    description_zh: Option<String>,
    language: Option<String>,
    stars: Option<u64>,
    discovered_at: i64,
    created_at: Option<i64>,
    source: Option<String>,
    category: Option<String>,
    tags: Vec<String>,
    pending: bool,
}

pub struct ReposView {
    library: Entity<Library>,
    runner: Entity<DiscoveryRunner>,
    list: Entity<ListState<RepoListDelegate>>,
    /// 「添加仓库」对话框的输入框。由本视图长期持有，原因见 `add_repo_dialog`。
    input: Entity<InputState>,
    selected: Option<RepoId>,
    _subscriptions: Vec<Subscription>,
}

impl ReposView {
    pub fn new(
        library: Entity<Library>,
        runner: Entity<DiscoveryRunner>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let list = cx.new(|cx| ListState::new(RepoListDelegate::new(), window, cx).searchable(true));
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("owner/repo 或 https://github.com/owner/repo")
                .submit_on_enter(true)
        });

        let mut subscriptions = Vec::new();

        // 数据变了（后台发现了新仓库、详情里改过分类）就重新查一次当前页。
        subscriptions.push(cx.observe(&library, |this, _, cx| {
            this.sync_page(cx);
        }));

        // 列表通知过来的两种情况：选中项变了，或者 delegate 里的查询条件变了
        // （输入搜索词、点翻页）。前者镜像给详情面板，后者得去查一次库。
        subscriptions.push(cx.observe(&list, |this, list, cx| {
            let selection = list.read(cx).delegate().selection().cloned();
            if this.selected != selection {
                this.selected = selection;
            }
            this.sync_page(cx);
        }));

        // 输入框只在对话框里被渲染，能收到回车就说明对话框正开着。
        subscriptions.push(cx.subscribe_in(
            &input,
            window,
            |this, _, event, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    this.submit_input(window, cx);
                }
            },
        ));

        let mut view = Self {
            library,
            runner,
            list,
            input,
            selected: None,
            _subscriptions: subscriptions,
        };
        view.sync_page(cx);
        view
    }

    /// 拿列表当前的条件去 `Library` 查一页，再塞回列表。
    ///
    /// 它是「条件 / 数据 → 界面」这条路上的唯一一步：搜索词、分类筛选、页码都只
    /// 存在 delegate 里，查询只有这里做一次。查回来的结果跟原来一样时
    /// `accept_page` 返回 `false`，所以不会出现「通知 → 再查 → 再通知」转不停。
    fn sync_page(&mut self, cx: &mut Context<Self>) {
        // 先把要用的东西抄出来——下面还要 `list.update`，不能一直借着 `cx`。
        let (query, categories) = {
            let library = self.library.read(cx);
            let delegate = self.list.read(cx).delegate();
            (delegate.query().clone(), library.category_rows())
        };

        // 分类被删掉之后，原来按它筛的列表会永远空着——退回「全部」。
        let category_gone = match &query.category {
            CategoryFilter::Named(path) => {
                !categories.iter().any(|row| tax::same_name(&row.path, path))
            }
            _ => false,
        };
        let filter = if category_gone {
            CategoryFilter::All
        } else {
            query.category.clone()
        };

        let mut wanted = query;
        if category_gone {
            wanted.category = CategoryFilter::All;
            wanted.page = 0;
        }

        let (page, selected_gone) = {
            let library = self.library.read(cx);
            (
                library.query_repos(&wanted),
                self.selected
                    .as_ref()
                    .is_some_and(|id| library.repo(id).is_none()),
            )
        };
        // 被移除的仓库不能继续占着选中状态，否则详情面板会指向一个不存在的东西。
        // 判据是「还在不在库里」而不是「在不在这一页」——翻了页的选中不该丢。
        if selected_gone {
            self.selected = None;
        }
        let selected = self.selected.clone();

        self.list.update(cx, |state, cx| {
            let delegate = state.delegate_mut();
            delegate.set_category_filter(filter);
            let changed = delegate.accept_page(page);
            delegate.set_selected(selected);
            if changed {
                cx.notify();
            }
        });
        // 页一样也可能有别的东西要跟着变（筛选项上的仓库数），让本视图重绘一次。
        cx.notify();
    }

    /// 切换分类筛选。条件存在 delegate 里，改完通知一声，[`Self::sync_page`]
    /// 就会去查库。
    fn set_category_filter(&mut self, filter: CategoryFilter, cx: &mut Context<Self>) {
        self.list.update(cx, |state, cx| {
            state.delegate_mut().set_category_filter(filter);
            cx.notify();
        });
    }

    /// 翻一页。已经在头尾时 delegate 不动，也就不会白查一次。
    fn turn_page(&mut self, forward: bool, cx: &mut Context<Self>) {
        self.list.update(cx, |state, cx| {
            let delegate = state.delegate_mut();
            let moved = if forward {
                delegate.go_to_next_page()
            } else {
                delegate.go_to_prev_page()
            };
            if moved {
                cx.notify();
            }
        });
    }

    fn submit_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let raw = self.input.read(cx).value().to_string();
        if add_repo_dialog::submit(&self.library, &self.runner, &self.input, &raw, window, cx) {
            // 提交成功后对话框会关闭，这里只需要让列表立刻反映新增项。
            self.sync_page(cx);
        }
    }

    fn repo_detail(&self, id: &str, cx: &Context<Self>) -> Option<RepoDetail> {
        let library = self.library.read(cx);
        let repo = library.repo(id)?;
        Some(RepoDetail {
            id: repo.id(),
            full_name: repo.full_name.clone(),
            url: repo.url.clone(),
            description: repo.description.clone(),
            description_zh: repo.description_zh.clone(),
            language: repo.language.clone(),
            stars: repo.stars,
            discovered_at: repo.discovered_at,
            created_at: repo.created_at,
            source: repo.source.clone(),
            // 分类存的是树上的节点 id，显示要的是路径——从树里现取。
            category: library.repo_category_path(id),
            tags: repo.tags.clone(),
            pending: repo.has_pending_metadata(),
        })
    }

    fn open_add_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        add_repo_dialog::open(&self.input, &self.library, &self.runner, window, cx);
    }

    /// 把还缺字段的仓库交给后台补齐。配额有限，所以按设置里的批次上限走。
    fn refresh_pending(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let batch = self.library.read(cx).settings().metadata_batch as usize;
        let pending = self.library.read(cx).repos_pending_metadata(batch);
        if pending.is_empty() {
            window.push_notification(Notification::info("没有待补全的仓库"), cx);
            return;
        }

        let count = pending.len();
        let started = self
            .runner
            .update(cx, |runner, cx| runner.refresh_metadata_now(pending, cx));
        if started {
            window.push_notification(Notification::info(format!("正在补全 {count} 个仓库")), cx);
        } else {
            window.push_notification(Notification::warning("后台任务正在运行，稍后再试"), cx);
        }
    }

    /// 分类筛选条。词表为空时整条不画——只有一个「全部」的筛选条纯属占地方。
    ///
    /// 只列有仓库的分类，外加当前正选中的那个：空分类摆一排是噪音，但选中的
    /// 那个必须留着，否则筛完之后连点回去的地方都没有。计数是**整棵子树**的
    /// 数量，跟点下去之后看到的结果条数一致。
    ///
    /// 按钮上写完整路径（`开发/前端 3`）而不是靠缩进排成树：筛选条是横着折行的，
    /// 缩进在这里没有稳定的参照，路径本身反而最短。
    fn render_filter_bar(&self, total: usize, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (categories, uncategorized, filter) = {
            let library = self.library.read(cx);
            let categories: Vec<(String, usize)> = library
                .category_rows()
                .into_iter()
                .map(|row: CategoryRow| (row.path, row.repos))
                .collect();
            let filter = self.list.read(cx).delegate().category_filter().clone();
            (categories, library.uncategorized_count(), filter)
        };

        if categories.is_empty() {
            return None;
        }

        let (muted, border) = {
            let theme = cx.theme();
            (theme.muted_foreground, theme.border)
        };

        let mut chips: Vec<AnyElement> = Vec::new();
        for (value, label, count) in std::iter::once((CategoryFilter::All, "全部".to_string(), total))
            .chain(std::iter::once((
                CategoryFilter::Uncategorized,
                "未分类".to_string(),
                uncategorized,
            )))
            .chain(
                categories
                    .into_iter()
                    .map(|(name, count)| (CategoryFilter::Named(name.clone()), name, count)),
            )
        {
            let active = filter == value;
            // 没仓库的分类不摆出来，除非它正被选中。
            if count == 0 && !active && value != CategoryFilter::All {
                continue;
            }

            let mut button = Button::new(filter_element_id(&value))
                .label(format!("{label} {count}"))
                .small();
            button = if active { button.primary() } else { button.outline() };
            chips.push(
                button
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.set_category_filter(value.clone(), cx);
                    }))
                    .into_any_element(),
            );
        }

        Some(
            v_flex()
                .gap_1()
                .pt_1()
                .border_t_1()
                .border_color(border)
                .child(div().text_xs().text_color(muted).child("按分类看"))
                .child(h_flex().flex_wrap().gap_1().children(chips))
                .into_any_element(),
        )
    }

    fn render_list_panel(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let border = theme.border;
        let background = theme.background;

        // 库里的总数，不是当前这一页的条数。
        let total = self.library.read(cx).repo_count();
        let filter_bar = self.render_filter_bar(total, cx);
        let (pending, page) = {
            let library = self.library.read(cx);
            (
                library.pending_metadata_count(),
                self.list.read(cx).delegate().page().clone(),
            )
        };

        let count_text = if page.total == total {
            format!("{total} 个")
        } else {
            format!("{total} 个 · 筛出 {}", page.total)
        };
        let count_text = match pending {
            0 => count_text,
            pending => format!("{count_text} · {pending} 个待补全"),
        };

        v_flex()
            .w(px(LIST_WIDTH))
            .h_full()
            .min_h_0()
            .flex_shrink_0()
            .border_r_1()
            .border_color(border)
            .bg(background)
            .child(
                v_flex()
                    .gap_3()
                    .p_3()
                    .border_b_1()
                    .border_color(border)
                    .child(
                        h_flex()
                            .gap_2()
                            .justify_between()
                            .child(
                                v_flex()
                                    .min_w_0()
                                    .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child("仓库"))
                                    .child(div().text_xs().text_color(muted).child(count_text)),
                            )
                            .child(
                                h_flex()
                                    .gap_1()
                                    .child(
                                        Button::new("refresh-pending-metadata")
                                            .icon(IconName::RefreshCw)
                                            .ghost()
                                            .small()
                                            .tooltip("补全缺失的描述与创建时间")
                                            .disabled(pending == 0)
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.refresh_pending(window, cx);
                                            })),
                                    )
                                    .child(
                                        Button::new("open-add-repo")
                                            .icon(IconName::Plus)
                                            .label("添加")
                                            .primary()
                                            .small()
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.open_add_dialog(window, cx);
                                            })),
                                    ),
                            ),
                    )
                    .when_some(filter_bar, |this, bar| this.child(bar)),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .child(List::new(&self.list).search_placeholder("搜索名称、描述、语言、分类或标签")),
            )
            // 只有一页时不画：一条没用不上的控件只会让人以为还能翻。
            .when(page.page_count() > 1, |this| this.child(self.render_pager(&page, muted, border, cx)))
    }

    /// 底部的翻页条：左边说清楚现在看的是哪几条，右边两个按钮。
    fn render_pager(
        &self,
        page: &RepoPage,
        muted: Hsla,
        border: Hsla,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        h_flex()
            .justify_between()
            .items_center()
            .gap_2()
            .px_3()
            .py_1()
            .flex_shrink_0()
            .border_t_1()
            .border_color(border)
            .child(
                div().text_xs().text_color(muted).child(format!(
                    "第 {}/{} 页 · 第 {}-{} 条",
                    page.page + 1,
                    page.page_count(),
                    page.first_index(),
                    page.last_index()
                )),
            )
            .child(
                h_flex()
                    .gap_1()
                    .child(
                        Button::new("repo-page-prev")
                            .icon(IconName::ChevronLeft)
                            .ghost()
                            .small()
                            .tooltip("上一页")
                            .disabled(!page.has_prev())
                            .on_click(cx.listener(|this, _, _, cx| this.turn_page(false, cx))),
                    )
                    .child(
                        Button::new("repo-page-next")
                            .icon(IconName::ChevronRight)
                            .ghost()
                            .small()
                            .tooltip("下一页")
                            .disabled(!page.has_next())
                            .on_click(cx.listener(|this, _, _, cx| this.turn_page(true, cx))),
                    ),
            )
    }

    fn render_empty_detail(&self, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;

        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_3()
            .child(Icon::new(IconName::Bookmark).size_10().text_color(muted))
            .child(div().text_color(muted).child("从左侧选一个仓库"))
            .child(
                Button::new("open-add-repo-from-empty")
                    .icon(IconName::Plus)
                    .label("添加仓库")
                    .outline()
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.open_add_dialog(window, cx);
                    })),
            )
            .into_any_element()
    }

    fn render_detail(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(id) = self.selected.clone() else {
            return self.render_empty_detail(cx);
        };
        let Some(detail) = self.repo_detail(&id, cx) else {
            return self.render_empty_detail(cx);
        };

        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let border = theme.border;
        let dark = theme.is_dark();
        let now = now_unix();

        let source_text = detail.source.clone().unwrap_or_else(|| "手工添加".to_string());
        let created_text = match detail.created_at {
            Some(created_at) => {
                format!("{}（{}）", format_local_date(created_at), format_relative(created_at, now))
            }
            None => "尚未取到".to_string(),
        };
        let stars_text = detail
            .stars
            .map(|stars| stars.to_string())
            .unwrap_or_else(|| "未知".to_string());
        let language_text = detail.language.clone().unwrap_or_else(|| "未识别".to_string());

        // 描述块：汉语描述在上（它才是平时要读的那份），原文在下。
        let zh_missing = detail
            .description_zh
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .is_none();
        let zh_block = v_flex()
            .gap_2()
            .child(
                h_flex()
                    .gap_2()
                    .justify_between()
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(muted)
                                    .child("汉语描述"),
                            )
                            .when(zh_missing, |this| {
                                this.child(
                                    Tag::secondary().small().child("未填写").into_any_element(),
                                )
                            }),
                    )
                    .child({
                        let library = self.library.clone();
                        let id = detail.id.clone();
                        let full_name = detail.full_name.clone();
                        let text = detail.description_zh.clone();
                        Button::new("edit-zh-description")
                            .icon(IconName::Pencil)
                            .label(if zh_missing { "填写" } else { "编辑" })
                            .ghost()
                            .small()
                            .on_click(move |_, window, cx| {
                                edit_description_dialog::open(
                                    library.clone(),
                                    id.clone(),
                                    full_name.clone(),
                                    text.clone(),
                                    window,
                                    cx,
                                );
                            })
                    }),
            )
            .child(match detail.description_zh.as_deref() {
                Some(text) if !text.trim().is_empty() => div().child(copyable(text.to_string())),
                _ => div()
                    .text_color(muted)
                    .child("还没写。写一句自己的话，之后列表里就显示它。"),
            })
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(muted)
                            .child("原始描述（GitHub About）"),
                    )
                    .child(match detail.description.as_deref() {
                        Some(text) if !text.trim().is_empty() => {
                            div().child(copyable(text.to_string()))
                        }
                        _ => div()
                            .text_color(muted)
                            .child("尚未取到，点上面的「补全」按钮抓一次"),
                    }),
            );

        // 分类与标签：一个仓库最多属于一个分类、可以挂多个标签，所以分类画一个
        // 色块，标签横着铺一排。分类没有色块可画时就明说「未分类」。
        let category_content = match detail.category.as_deref() {
            Some(name) => chip::category_tag(format!("detail-category:{}", detail.id), name, dark)
                .into_any_element(),
            None => div().text_color(muted).child("未分类").into_any_element(),
        };
        let tag_chips: Vec<AnyElement> = detail
            .tags
            .iter()
            .map(|tag| {
                chip::tag_tag(format!("detail-tag:{}:{tag}", detail.id), tag).into_any_element()
            })
            .collect();
        let tag_content = if tag_chips.is_empty() {
            div().text_color(muted).child("没有标签").into_any_element()
        } else {
            h_flex().flex_wrap().gap_1().children(tag_chips).into_any_element()
        };
        let taxonomy_row = |label: &str, content: AnyElement| {
            h_flex()
                .items_start()
                .gap_3()
                .child(
                    div()
                        .w(px(52.))
                        .flex_shrink_0()
                        .text_xs()
                        .text_color(muted)
                        .child(label.to_string()),
                )
                .child(div().flex_1().min_w_0().child(content))
        };
        let taxonomy_block = v_flex()
            .gap_2()
            .child(
                h_flex()
                    .gap_2()
                    .justify_between()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(muted)
                            .child("分类与标签"),
                    )
                    .child({
                        let library = self.library.clone();
                        let id = detail.id.clone();
                        let full_name = detail.full_name.clone();
                        Button::new("edit-taxonomy")
                            .icon(IconName::Tag)
                            .label("编辑")
                            .ghost()
                            .small()
                            .on_click(move |_, window, cx| {
                                taxonomy_dialog::open(
                                    library.clone(),
                                    id.clone(),
                                    full_name.clone(),
                                    window,
                                    cx,
                                );
                            })
                    }),
            )
            .child(taxonomy_row("分类", category_content))
            .child(taxonomy_row("标签", tag_content));

        v_flex()
            .size_full()
            .min_w_0()
            .gap_4()
            .p_6()
            // 滚动方法在 `Stateful<Div>` 上，先给容器一个稳定 id 才能滚动。
            .id("repo-detail-scroll")
            .overflow_y_scroll()
            .child(
                v_flex()
                    .gap_2()
                    .child(
                        h_flex()
                            .gap_3()
                            .child(
                                div()
                                    .text_lg()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(copyable_as(
                                        format!("detail-name:{}", detail.id),
                                        detail.full_name.clone(),
                                    )),
                            )
                            .when(detail.pending, |this| {
                                this.child(Tag::warning().small().child("待补全").into_any_element())
                            }),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("open-repo-url")
                                    .icon(IconName::ExternalLink)
                                    .label("打开仓库")
                                    .outline()
                                    .small()
                                    .on_click({
                                        let url = detail.url.clone();
                                        move |_, _, cx| cx.open_url(&url)
                                    }),
                            )
                            .child({
                                let runner = self.runner.clone();
                                let id = detail.id.clone();
                                let full_name = detail.full_name.clone();
                                Button::new("refresh-repo-metadata")
                                    .icon(IconName::RefreshCw)
                                    .label("刷新信息")
                                    .ghost()
                                    .small()
                                    .on_click(move |_, window, cx| {
                                        let started = runner.update(cx, |runner, cx| {
                                            runner.refresh_metadata_now(vec![id.clone()], cx)
                                        });
                                        let note = if started {
                                            Notification::info(format!("正在获取 {full_name}"))
                                        } else {
                                            Notification::warning("后台任务正在运行，稍后再试")
                                        };
                                        window.push_notification(note, cx);
                                    })
                            }),
                    ),
            )
            .child(
                DescriptionList::new()
                    .columns(2)
                    .label_width(px(76.))
                    .item("地址", detail.url.clone(), 2)
                    .item("语言", language_text, 1)
                    .item("Star", stars_text, 1)
                    .item(
                        "发现时间",
                        format!(
                            "{}（{}）",
                            format_local_minute(detail.discovered_at),
                            format_relative(detail.discovered_at, now)
                        ),
                        2,
                    )
                    .item("创建时间", created_text, 2)
                    .item("来源", source_text, 2),
            )
            .child(div().h(px(1.)).w_full().bg(border))
            .child(taxonomy_block)
            .child(div().h(px(1.)).w_full().bg(border))
            .child(zh_block)
            .child(
                h_flex().justify_end().child({
                    let library = self.library.clone();
                    let list = self.list.clone();
                    let id = detail.id.clone();
                    let full_name = detail.full_name.clone();
                    Button::new("remove-repo")
                        .icon(IconName::Trash)
                        .label("移除")
                        .danger()
                        .small()
                        .on_click(move |_, window, cx| {
                            // 汉语描述是人工成果，删掉就找不回来了，所以这里值得一次确认。
                            let library = library.clone();
                            let list = list.clone();
                            let id = id.clone();
                            let full_name = full_name.clone();
                            window.open_dialog(cx, move |dialog, _, _| {
                                let library = library.clone();
                                let list = list.clone();
                                let id = id.clone();
                                let full_name = full_name.clone();
                                dialog
                                    .title("移除仓库")
                                    .child(format!(
                                        "把 {full_name} 从列表里移除？如果写过汉语描述，会一起丢掉。"
                                    ))
                                    .w(px(420.))
                                    .footer(
                                        DialogFooter::new()
                                            .child(DialogClose::new().child(
                                                Button::new("cancel-remove-repo")
                                                    .label("取消")
                                                    .on_click(|_, window, cx| window.close_dialog(cx)),
                                            ))
                                            .child(DialogAction::new().child(
                                                Button::new("confirm-remove-repo")
                                                    .label("移除")
                                                    .danger(),
                                            )),
                                    )
                                    .on_ok(move |_, window, cx| {
                                        // 落库由 `Library` 自己做完，这里不必再存一次盘。
                                        let removed = library.update(cx, |library, cx| {
                                            let removed = library.remove_repo(&id);
                                            if removed {
                                                cx.notify();
                                            }
                                            removed
                                        });
                                        // 选中状态由列表持有，移除后必须一起清掉。
                                        list.update(cx, |state, cx| {
                                            state.delegate_mut().set_selected(None);
                                            cx.notify();
                                        });
                                        if removed {
                                            window.push_notification(
                                                Notification::success(format!("已移除 {full_name}")),
                                                cx,
                                            );
                                        }
                                        true
                                    })
                            });
                        })
                }),
            )
            .into_any_element()
    }
}

impl Render for ReposView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let background = theme.background;
        let foreground = theme.foreground;

        let detail = self.render_detail(cx);
        let list_panel = self.render_list_panel(cx);

        h_flex()
            .size_full()
            .min_w_0()
            .bg(background)
            .text_color(foreground)
            .child(list_panel)
            .child(div().flex_1().min_w_0().h_full().child(detail))
    }
}

/// 筛选按钮的身份。名字进到 id 里，别用下标——分类增删之后下标会错位。
fn filter_element_id(filter: &CategoryFilter) -> ElementId {
    match filter {
        CategoryFilter::All => "repo-filter:all".into(),
        CategoryFilter::Uncategorized => "repo-filter:uncategorized".into(),
        CategoryFilter::Named(name) => format!("repo-filter:{name}").into(),
    }
}

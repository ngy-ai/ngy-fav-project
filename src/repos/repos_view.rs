//! 仓库页：左侧列表 + 右侧详情。
//!
//! 选中状态只有一份，放在列表的 delegate 里（它无论如何都要知道谁被选中才能
//! 画出高亮），本视图通过观察列表把它镜像出来给详情面板用。

use gpui_kit::assets::IconName;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IndexPath, Sizable as _, WindowExt as _,
    h_flex, v_flex,
    button::{Button, ButtonVariants as _},
    description_list::DescriptionList,
    dialog::{DialogAction, DialogClose, DialogFooter},
    input::{InputEvent, InputState},
    list::{List, ListState},
    notification::Notification,
    resizable::{ResizableState, h_resizable, resizable_panel},
    tag::Tag,
};
use gpui_kit::prelude::*;
use gpui_kit::*;

use crate::discovery::DiscoveryRunner;
use crate::library::{CategoryRow, FacetRow, Library};
use crate::repos::add_repo_dialog;
use crate::repos::bulk_category_dialog;
use crate::repos::edit_description_dialog;
use crate::repos::list_delegate::{RepoListDelegate, SelectAll};
use crate::repos::model::{CategoryFilter, RepoPage};
use crate::repos::taxonomy_dialog;
use crate::repos::RepoId;
use crate::shared::clock::{format_local_date, format_local_minute, format_relative, now_unix};
use crate::shared::text::{copyable, copyable_as};
use crate::taxonomy::chip;
use crate::taxonomy::model as tax;
use crate::taxonomy::picker::{self, PickerRow};

/// 列表栏一开始多宽。之后由分栏状态记着，拖出来的就是它。
const LIST_WIDTH: f32 = 320.;

/// 列表栏能拖到的范围。下限保证名字还看得见，上限保证详情面板还有地方待。
const LIST_MIN_WIDTH: f32 = 240.;
const LIST_MAX_WIDTH: f32 = 560.;

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
    /// 列表 ↔ 详情那一刀的分栏状态。宽度记在它里面——它自己每帧都在量、
    /// 在改，另存一份镜像迟早对不上。
    split: Entity<ResizableState>,
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
        let split = cx.new(|_| ResizableState::default());

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
            split,
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
        let (query, categories, languages, tags) = {
            let library = self.library.read(cx);
            let delegate = self.list.read(cx).delegate();
            (
                delegate.query().clone(),
                library.category_rows(),
                library.language_rows(),
                library.tag_rows(),
            )
        };

        // 词表里已经没有的东西不能继续挂在条件上：分类被删了、语言因为重新抓了
        // 元数据而不存在了、标签被摘掉了。留着只会让列表一直空着，而用户看不出
        // 是为什么——筛选条上那一项明明还在（它显示的是「当前条件」）。
        let mut wanted = query;
        let stale_category = match &wanted.category {
            CategoryFilter::Named(path) => {
                !categories.iter().any(|row| tax::same_name(&row.path, path))
            }
            _ => false,
        };
        if stale_category {
            wanted.category = CategoryFilter::All;
        }

        let stale_language = wanted
            .language
            .as_deref()
            .is_some_and(|language| !languages.iter().any(|row| row.name == language));
        if stale_language {
            wanted.language = None;
        }

        let kept_tags: Vec<String> = wanted
            .tags
            .iter()
            .filter(|name| tags.iter().any(|row| tax::same_name(&row.name, name)))
            .cloned()
            .collect();
        let stale_tags = kept_tags.len() != wanted.tags.len();
        if stale_tags {
            wanted.tags = kept_tags;
        }

        if stale_category || stale_language || stale_tags {
            // 条件变了就回第一页：停在第 3 页看一个只剩半页的结果没有意义。
            wanted.page = 0;
        }

        let (page, matching, selected_gone, checked_kept) = {
            let library = self.library.read(cx);
            (
                library.query_repos(&wanted),
                // 「全选」要的是筛出来的整批，而列表一次只取一页，所以这条单独查一次。
                // 结果只用来算那个方框的样子，行本身不靠它。
                library.repo_ids_matching(&wanted),
                self.selected
                    .as_ref()
                    .is_some_and(|id| library.repo(id).is_none()),
                // 勾选里已经不在库里的（列表上勾完，别处把它删了）要摘掉，否则
                // 「已选 N 个」会一直多算那几个。勾着的通常没几个，扫一遍不贵。
                self.list
                    .read(cx)
                    .delegate()
                    .checked_ids()
                    .into_iter()
                    .filter(|id| library.repo(id).is_some())
                    .collect::<Vec<RepoId>>(),
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
            // 条件要跟着**实际生效**的那份走：`wanted` 可能刚把失效的筛选摘掉了。
            delegate.set_category_filter(wanted.category.clone());
            delegate.set_language(wanted.language.clone());
            delegate.set_tags(wanted.tags.clone());
            delegate.set_matching(matching);
            if delegate.checked_count() != checked_kept.len() {
                delegate.set_checked(checked_kept);
            }
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

    /// 换一种语言；`None` 是「不限语言」。
    fn set_language_filter(&mut self, language: Option<String>, cx: &mut Context<Self>) {
        self.list.update(cx, |state, cx| {
            state.delegate_mut().set_language(language);
            cx.notify();
        });
    }

    /// 点一下某个标签：挂着就摘掉，没挂就挂上。多选就这一条规则。
    fn toggle_tag_filter(&mut self, name: &str, cx: &mut Context<Self>) {
        self.list.update(cx, |state, cx| {
            state.delegate_mut().toggle_tag(name);
            cx.notify();
        });
    }

    /// 标签那一维的「不限」：选中的标签全摘掉。
    fn clear_tag_filter(&mut self, cx: &mut Context<Self>) {
        self.list.update(cx, |state, cx| {
            state.delegate_mut().set_tags(Vec::new());
            cx.notify();
        });
    }

    /// 三个下拉一起清掉。搜索词不在其中（它有自己的输入框）。
    fn clear_filters(&mut self, cx: &mut Context<Self>) {
        self.list.update(cx, |state, cx| {
            state.delegate_mut().clear_filters();
            cx.notify();
        });
    }

    /// 点「全选」：把当前筛出来的整批勾上；已经全勾上时反过来把它们摘掉。
    fn toggle_select_all(&mut self, cx: &mut Context<Self>) {
        self.list.update(cx, |state, cx| {
            state.delegate_mut().toggle_select_all();
            cx.notify();
        });
    }

    /// 清空勾选。
    fn clear_checked(&mut self, cx: &mut Context<Self>) {
        self.list.update(cx, |state, cx| {
            state.delegate_mut().clear_checked();
            cx.notify();
        });
    }

    /// 打开「整批改分类」。这批是**现在**勾着的那些，打开之后就定下来了。
    fn open_bulk_category(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ids = self.list.read(cx).delegate().checked_ids();
        let list = self.list.clone();
        bulk_category_dialog::open(
            self.library.clone(),
            ids,
            move |cx: &mut App| {
                // 处理完了就把勾选清掉：留着只会让人以为还没做。
                list.update(cx, |state, cx| {
                    state.delegate_mut().clear_checked();
                    cx.notify();
                });
            },
            window,
            cx,
        );
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
        let on_added = select_added_callback(&cx.entity().downgrade());
        let close = add_repo_dialog::submit(
            &self.library,
            &self.runner,
            &self.input,
            &raw,
            on_added,
            window,
            cx,
        );
        if close {
            // 提交成功后对话框会关闭，这里只需要让列表立刻反映新增项。
            self.sync_page(cx);
        }
    }

    /// 手工加进来一个仓库之后：把它选中，并回到第一页。
    ///
    /// 新加的仓库 `discovered_at` 就是现在，而列表按 `discovered_at DESC,
    /// full_name ASC` 排——**它一定落在第一页**，所以回第一页就够了；停在第 3 页
    /// 上会看不见刚加的东西。
    ///
    /// 只动页码，**不碰筛选条件**：用户可能正看着某个分类，把他设的条件悄悄清掉，
    /// 比「详情面板里是它、列表里没高亮」更让人意外。条件确实把它挡住时（新加的
    /// 仓库既没分类也没标签），选中照样落到它身上，只是列表里没有一行可高亮。
    fn select_added(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.selected = Some(id.to_string());
        self.list
            .update(cx, |state, _| state.delegate_mut().request_page(0));
        self.sync_page(cx);

        // 页取回来之后才谈得上「它排第几行」——行号是对着**当前这一页**解释的。
        let position = self.list.read(cx).delegate().position_of(id);
        if let Some(row) = position {
            self.list.update(cx, |state, cx| {
                state.set_selected_index(Some(IndexPath::new(row)), window, cx);
                // 连滚动一起做：列表可能停在下面别处，选中了却看不见等于没选中。
                state.scroll_to_selected_item(window, cx);
                cx.notify();
            });
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
        let on_added = select_added_callback(&cx.entity().downgrade());
        add_repo_dialog::open(
            &self.input,
            &self.library,
            &self.runner,
            on_added,
            window,
            cx,
        );
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

    /// 列表上方的筛选条：三个下拉——分类、语言、标签。
    ///
    /// 分类是**缩进排出来的树**：早先这里是一排色块、横着折行，分类一多就占掉好
    /// 几行，而且路径写法（`AI Agent/Tools`）在折行里读不出层级。改成下拉之后列表
    /// 上方只占一行，层级交给缩进表达——所以树里的子分类只写自己那一段名字
    /// （全路径只留在收起的按钮上，那是唯一需要它说清楚「选的是哪个」的地方）。
    /// 语言是扁平枚举，标签要用同一个交互。
    ///
    /// 三个维度之间是**且**：分类筛完、语言再筛、标签还得都挂着。
    ///
    /// 每个下拉上的数字都是**全库**的计数，跟分类那一维一个口径——它回答的是
    /// 「选它会看到几条」，不是「在现在这堆条件里还剩下几条」。后者要按维度交叉
    /// 重算，摆出来的数字也会一直变，反而看不出该点哪个。
    ///
    /// 一维都没得筛（没建分类、全库只有一种语言、词表里没有标签）就整条不画：
    /// 光摆一排「不限」纯属占地方。
    fn render_filter_bar(&self, total: usize, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (categories, languages, tag_facets) = {
            let library = self.library.read(cx);
            (
                library.category_rows(),
                library.language_rows(),
                library.tag_rows(),
            )
        };

        let (current_category, current_language, current_tags) = {
            let delegate = self.list.read(cx).delegate();
            (
                delegate.category_filter().clone(),
                delegate.language_filter().map(str::to_string),
                delegate.tags_filter().to_vec(),
            )
        };

        // 每一维各自判断值不值得摆：只有一种语言时，筛它和不筛是一回事。
        let show_category = !categories.is_empty();
        let show_language = languages.len() > 1;
        let show_tags = !tag_facets.is_empty();
        if !show_category && !show_language && !show_tags {
            return None;
        }

        let border = cx.theme().border;
        let view = cx.entity();

        let mut bar = h_flex().items_center().gap_2().flex_wrap();

        if show_category {
            let rows = filter_rows(categories, self.library.read(cx).uncategorized_count(), total);
            let key = filter_key(&current_category);
            // 收起时按钮上写什么：名字 + 数量，免得点进去才发现是个空分类。
            let label = row_label(rows.iter().find(|row| row.key == key))
                .unwrap_or_else(|| filter_label(&current_category));

            let view = view.clone();
            bar = bar.child(
                picker::Picker::new("repo-category-filter", IconName::FolderTree, label)
                    .rows(rows)
                    .selected(vec![key])
                    .accent(current_category != CategoryFilter::All)
                    .on_pick(move |key, _window, cx| {
                        view.update(cx, |this, cx| {
                            this.set_category_filter(filter_from_key(&key), cx)
                        });
                    })
                    .into_element(),
            );
        }

        if show_language {
            let rows = language_filter_rows(languages, total);
            let key = language_filter_key(current_language.as_deref());
            let label = row_label(rows.iter().find(|row| row.key == key))
                .unwrap_or_else(|| key.clone());

            let view = view.clone();
            bar = bar.child(
                picker::Picker::new("repo-language-filter", IconName::Languages, label)
                    .rows(rows)
                    .selected(vec![key])
                    .accent(current_language.is_some())
                    .on_pick(move |key, _window, cx| {
                        // 固定项「不限」不是一个语言名，别的都是。
                        let language = (key != FILTER_ANY).then_some(key);
                        view.update(cx, |this, cx| this.set_language_filter(language, cx));
                    })
                    .into_element(),
            );
        }

        if show_tags {
            let rows = tag_filter_rows(tag_facets, total);

            let view = view.clone();
            bar = bar.child(
                picker::Picker::new(
                    "repo-tag-filter",
                    IconName::Tags,
                    tags_label(&current_tags, &rows),
                )
                .rows(rows)
                // 选中的是一批：下拉里那几行的勾都要打上。
                .selected(current_tags.clone())
                .accent(!current_tags.is_empty())
                .on_pick(move |key, _window, cx| {
                    if key == FILTER_ANY {
                        view.update(cx, |this, cx| this.clear_tag_filter(cx));
                    } else {
                        view.update(cx, |this, cx| this.toggle_tag_filter(&key, cx));
                    }
                })
                .into_element(),
            );
        }

        // 有筛选时才摆「清除」：它管的是三个下拉，搜索框里的字归搜索框自己。
        if self.list.read(cx).delegate().has_facet_filters() {
            bar = bar.child(
                // 外面这层只为了给它一个可观测的身份（`Button` 挂不上 `test_support`）。
                div()
                    .id("clear-repo-filters-slot")
                    .test_support()
                    .child(
                        Button::new("clear-repo-filters")
                            .icon(IconName::X)
                            .label("清除")
                            .ghost()
                            .small()
                            .on_click(cx.listener(|this, _, _, cx| this.clear_filters(cx))),
                    ),
            );
        }

        Some(
            v_flex()
                .pt_1()
                .border_t_1()
                .border_color(border)
                .child(bar)
                .into_any_element(),
        )
    }

    /// 列表上方的批量操作条：全选、勾了多少、整批改分类。
    ///
    /// 「全选」选的是**当前筛出来的整批**（跨页），不是这一页——分页只是取的时候
    /// 分的，用户说的「这些」是全部，所以按钮上那个数字也是筛出来的总数。
    ///
    /// 那个方框是**三态**的：全勾上、勾了一部分、一个没勾。筛出 200 条勾了 3 条时
    /// 摆一个空方框或满方框都是假话，而组件库的 `Checkbox` 只有这两种样子——
    /// 所以这里用方框图标自己摆一个。
    ///
    /// 一条都没筛出来时整条不画：没东西可选，摆一排灰按钮纯属占地方。
    fn render_batch_bar(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (state, checked, total) = {
            let delegate = self.list.read(cx).delegate();
            (
                delegate.select_all_state(),
                delegate.checked_count(),
                delegate.total(),
            )
        };
        if total == 0 {
            return None;
        }

        let (muted, border) = {
            let theme = cx.theme();
            (theme.muted_foreground, theme.border)
        };

        let all_icon = match state {
            SelectAll::All => IconName::SquareCheck,
            SelectAll::Partial => IconName::SquareMinus,
            SelectAll::None => IconName::Square,
        };
        // 一个没勾时说清「全选」会选走多少，不然那个数只能靠猜。
        let count_text = if checked == 0 {
            format!("全选这 {total} 个")
        } else {
            format!("已选 {checked} 个")
        };

        let mut bar = h_flex()
            .items_center()
            .gap_2()
            .child(
                // 外面这层只为了给它一个可观测的身份（`Button` 挂不上 `test_support`）。
                div().id("repo-select-all-slot").test_support().child(
                    Button::new("repo-select-all")
                        .icon(all_icon)
                        .label("全选")
                        .ghost()
                        .small()
                        .tooltip("选中上面筛出来的全部，不只是这一页")
                        .on_click(cx.listener(|this, _, _, cx| this.toggle_select_all(cx))),
                ),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_xs()
                    .text_color(muted)
                    .child(count_text),
            );

        // 没勾东西时操作按钮不摆：摆一排点不动的按钮只会让人去试。
        if checked > 0 {
            bar = bar
                .child(
                    div().id("repo-bulk-category-slot").test_support().child(
                        Button::new("repo-bulk-category")
                            .icon(IconName::FolderTree)
                            .label("改分类")
                            .outline()
                            .small()
                            .on_click(
                                cx.listener(|this, _, window, cx| {
                                    this.open_bulk_category(window, cx)
                                }),
                            ),
                    ),
                )
                .child(
                    div().id("repo-clear-checked-slot").test_support().child(
                        Button::new("repo-clear-checked")
                            .icon(IconName::X)
                            .ghost()
                            .small()
                            .tooltip("清空勾选")
                            .on_click(cx.listener(|this, _, _, cx| this.clear_checked(cx))),
                    ),
                );
        }

        Some(
            v_flex()
                .px_3()
                .py_1()
                .border_b_1()
                .border_color(border)
                .child(bar)
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
        let batch_bar = self.render_batch_bar(cx);
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
            // 宽度交给分栏的那块面板；这里再写死一个像素值，拖动时就会「面板变了、
            // 列表没变」。右边那道竖线保留：它是列表与详情之间的那道分隔。
            .w_full()
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
                    .when_some(filter_bar, |this, bar| this.child(bar))
                    .when_some(batch_bar, |this, bar| this.child(bar)),
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
            .child(
                // 两块面板，中间那一刀可以拖。宽度记在分栏状态里，翻页、换筛选、
                // 切页面都不会动到它。
                h_resizable("repos-split")
                    .with_state(&self.split)
                    .child(
                        resizable_panel()
                            .size(px(LIST_WIDTH))
                            .size_range(px(LIST_MIN_WIDTH)..px(LIST_MAX_WIDTH))
                            // 列表栏是固定宽度的那一块，别跟着详情一起长。
                            .flex_none()
                            // 这一层只为量尺寸：无头窗口里要能量到列表栏实际多宽，
                            // 好确认拖动真的落到了布局上，而不只是改了状态里的数。
                            .child(div().id("repo-list-column").test_support().size_full().child(list_panel)),
                    )
                    .child(resizable_panel().child(div().flex_1().min_w_0().h_full().child(detail))),
            )
    }
}

/// 「全部」与「未分类」两个固定项的 key。它们不是树上的节点，所以不能和路径撞名。
const FILTER_ALL: &str = "all";
const FILTER_UNCATEGORIZED: &str = "uncategorized";

/// 语言与标签那一维的「不限」。
const FILTER_ANY: &str = "any";

/// 把分类树摊成筛选下拉里的行：先「全部」「未分类」两条，再按树序排分类。
///
/// 固定项摆在最前，是因为它们本来就不是分类：「全部」是取消筛选，「未分类」是
/// 树之外的那一堆。混进树里排序只会让人找不着。
///
/// 做成纯函数是为了能直接测（见本文件末尾）：顺序、缩进、计数、身份都有规则。
fn filter_rows(categories: Vec<CategoryRow>, uncategorized: usize, total: usize) -> Vec<PickerRow> {
    let mut rows = Vec::with_capacity(categories.len() + 2);
    rows.push(PickerRow::fixed(FILTER_ALL, "全部", Some(total)));
    rows.push(PickerRow::fixed(
        FILTER_UNCATEGORIZED,
        "未分类",
        Some(uncategorized),
    ));
    rows.extend(picker::category_rows(categories, true));
    rows
}

/// 一个筛选项在下拉里的 key。分类用**路径**——名字进到元素 id 里，别用下标，
/// 分类增删之后下标会错位。
fn filter_key(filter: &CategoryFilter) -> String {
    match filter {
        CategoryFilter::All => FILTER_ALL.to_string(),
        CategoryFilter::Uncategorized => FILTER_UNCATEGORIZED.to_string(),
        CategoryFilter::Named(path) => path.clone(),
    }
}

/// 下拉交出来的 key 换回筛选条件。key 就是路径本身，所以只有两个固定项要认。
fn filter_from_key(key: &str) -> CategoryFilter {
    match key {
        FILTER_ALL => CategoryFilter::All,
        FILTER_UNCATEGORIZED => CategoryFilter::Uncategorized,
        path => CategoryFilter::Named(path.to_string()),
    }
}

/// 收起的按钮上写的那句话。
fn filter_label(filter: &CategoryFilter) -> String {
    match filter {
        CategoryFilter::All => "全部".to_string(),
        CategoryFilter::Uncategorized => "未分类".to_string(),
        CategoryFilter::Named(path) => path.clone(),
    }
}

/// 「加好了，把新加的那个选中」这件事的回调。
///
/// 对话框的「添加」按钮和输入框回车是两条路，都会经过 `add_repo_dialog::submit`，
/// 但那里只有 `&mut App`，拿不到本视图的 `Context`——所以从视图句柄里再进去一次。
fn select_added_callback(
    this: &WeakEntity<ReposView>,
) -> impl Fn(&str, &mut Window, &mut App) + Clone + 'static {
    let this = this.clone();
    move |id, window, cx| {
        this.update(cx, |view, cx| view.select_added(id, window, cx))
            .ok();
    }
}

/// 一行在按钮上的写法：名字 + 数量。数量对筛选是有意义的——它说明点下去会看到几条。
fn row_label(row: Option<&PickerRow>) -> Option<String> {
    row.map(|row| match row.count {
        Some(count) => format!("{} · {}", row.label, count),
        None => row.label.clone(),
    })
}

/// 语言筛选下拉里的行：最前面一条「不限」，然后按语言清单的顺序（数量多的在前）。
///
/// 语言没有「未标注」这一项：没语言是元数据还没补上，属于「待补全」那件事，
/// 不该伪装成一种可选的语言。
fn language_filter_rows(languages: Vec<FacetRow>, total: usize) -> Vec<PickerRow> {
    let mut rows = Vec::with_capacity(languages.len() + 1);
    rows.push(PickerRow::fixed(FILTER_ANY, "不限", Some(total)));
    rows.extend(languages.into_iter().map(|language| {
        // 语言名既是显示的字，也是交出去的 key——它本来就是唯一的。
        PickerRow::fixed(language.name.clone(), language.name, Some(language.repos))
    }));
    rows
}

/// 当前语言在下拉里的 key。
fn language_filter_key(language: Option<&str>) -> String {
    language.unwrap_or(FILTER_ANY).to_string()
}

/// 标签筛选下拉里的行：最前面一条「不限」，其余是词表里的标签。
///
/// **标签的每一行都留着不收**（`keeping_open`）：标签是多选，挂上 cli 之后还要接着
/// 挂「待读」，点一下收一次就没法连点了。只有「不限」是收尾的那一下，点完该收。
fn tag_filter_rows(tags: Vec<FacetRow>, total: usize) -> Vec<PickerRow> {
    let mut rows = Vec::with_capacity(tags.len() + 1);
    rows.push(PickerRow::fixed(FILTER_ANY, "不限", Some(total)));
    rows.extend(tags.into_iter().map(|tag| {
        PickerRow::fixed(tag.name.clone(), tag.name, Some(tag.repos)).keeping_open()
    }));
    rows
}

/// 标签那一维收起来之后按钮上写什么。
///
/// 挂多了写不下（按钮只有一颗），所以只写第一个 + 还剩几个；全列出来会把
/// 筛选条挤成两行，把那点信息量全赔进去。
fn tags_label(selected: &[String], rows: &[PickerRow]) -> String {
    match selected.split_first() {
        None => "不限".to_string(),
        Some((first, rest)) => {
            let name = row_label(rows.iter().find(|row| &row.key == first))
                .unwrap_or_else(|| first.clone());
            if rest.is_empty() {
                name
            } else {
                format!("{name} +{}", rest.len())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;
    use gpui_kit::TestAppContext;
    use gpui_kit::test::TestWindowExt as _;
    use std::cell::RefCell;
    use std::rc::Rc;

    fn category(id: i64, path: &str, leaf: &str, depth: usize, repos: usize) -> CategoryRow {
        CategoryRow {
            id,
            // 拖拽排序才用得上；这里只摆下拉里的树，按路径取上级就够了。
            parent: path.rsplit_once('/').map(|(parent, _)| parent.to_string()),
            path: path.to_string(),
            leaf: leaf.to_string(),
            depth,
            children: 0,
            repos,
        }
    }

    #[test]
    fn the_dropdown_starts_with_all_and_uncategorized() {
        let rows = filter_rows(Vec::new(), 4, 7);
        assert_eq!(
            rows,
            [
                PickerRow::fixed(FILTER_ALL, "全部", Some(7)),
                PickerRow::fixed(FILTER_UNCATEGORIZED, "未分类", Some(4)),
            ],
            "两个固定项摆在分类前面，各自带自己的数量"
        );
    }

    #[test]
    fn a_child_row_shows_only_its_own_name_because_indentation_says_the_rest() {
        let rows = filter_rows(
            vec![
                category(1, "开发", "开发", 0, 5),
                category(2, "开发/前端", "前端", 1, 3),
                category(3, "开发/前端/构建", "构建", 2, 0),
            ],
            1,
            6,
        );

        let labels: Vec<(&str, usize, Option<usize>)> = rows
            .iter()
            .map(|row| (row.label.as_str(), row.depth, row.count))
            .collect();
        assert_eq!(
            labels,
            [
                ("全部", 0, Some(6)),
                ("未分类", 0, Some(1)),
                ("开发", 0, Some(5)),
                ("前端", 1, Some(3)),
                ("构建", 2, Some(0)),
            ],
            "按树序排、缩进带上层数，空分类也留着"
        );
    }

    #[test]
    fn every_row_has_its_own_identity_by_path_not_by_position() {
        // 同一个分类在树里换个位置（父分类被删、被改名）之后，身份仍然是路径，
        // 不会因为下标变了就串到别人身上。key 同时是元素 id 与「选中项」的比对依据。
        let named = CategoryFilter::Named("开发/前端".to_string());
        assert_eq!(filter_key(&named), "开发/前端");
        assert_ne!(filter_key(&named), filter_key(&CategoryFilter::Named("工具/前端".into())));
        assert_ne!(filter_key(&CategoryFilter::All), filter_key(&CategoryFilter::Uncategorized));
    }

    #[test]
    fn a_key_from_the_dropdown_comes_back_as_the_same_filter() {
        // 下拉交出来的是字符串，收回去必须是同一个筛选条件——分类走路径，
        // 两个固定项走各自的常量，都不能被当成同名分类。
        for filter in [
            CategoryFilter::All,
            CategoryFilter::Uncategorized,
            CategoryFilter::Named("开发/前端".to_string()),
        ] {
            assert_eq!(filter_from_key(&filter_key(&filter)), filter);
        }
    }

    #[test]
    fn the_trigger_label_carries_the_count() {
        let rows = filter_rows(vec![category(1, "开发", "开发", 0, 5)], 1, 6);
        let current = CategoryFilter::Named("开发".to_string());
        let row = rows.iter().find(|row| row.key == filter_key(&current));
        assert_eq!(row_label(row), Some("开发 · 5".to_string()));
        // 分类被删掉之后筛选会退回「全部」，所以兜底那一路也得有话说。
        assert_eq!(
            row_label(rows.iter().find(|row| row.key == "不存在的分类")),
            None
        );
        assert_eq!(filter_label(&CategoryFilter::Uncategorized), "未分类");
    }

    #[test]
    fn an_empty_vocabulary_leaves_nothing_to_filter_so_the_bar_is_hidden() {
        // 界面上「只有两行」就等于没有分类可筛，整条不画（见 render_filter_bar）。
        // 这两行是固定项，不是分类。
        let rows = filter_rows(Vec::new(), 0, 0);
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|row| row.depth == 0));
    }

    #[test]
    fn the_language_dropdown_leads_with_no_limit_and_names_its_rows_by_language() {
        let rows = language_filter_rows(
            vec![
                FacetRow {
                    name: "Rust".to_string(),
                    repos: 9,
                },
                FacetRow {
                    name: "Go".to_string(),
                    repos: 2,
                },
            ],
            11,
        );
        let shown: Vec<(&str, Option<usize>)> = rows
            .iter()
            .map(|row| (row.key.as_str(), row.count))
            .collect();
        assert_eq!(
            shown,
            [("any", Some(11)), ("Rust", Some(9)), ("Go", Some(2))],
            "「不限」在最前，其余保持清单给的顺序（数量多的在前，那是 library 排的）"
        );
        assert!(rows.iter().all(|row| row.close), "语言是单选，点完就该收");

        assert_eq!(language_filter_key(Some("Rust")), "Rust");
        assert_eq!(
            language_filter_key(None),
            FILTER_ANY,
            "没选时亮的是「不限」那一行的勾"
        );
    }

    #[test]
    fn every_tag_row_stays_open_but_the_no_limit_row_closes() {
        let rows = tag_filter_rows(
            vec![
                FacetRow {
                    name: "cli".to_string(),
                    repos: 3,
                },
                FacetRow {
                    name: "待读".to_string(),
                    repos: 1,
                },
            ],
            4,
        );
        assert_eq!(rows.len(), 3);
        assert!(rows[0].close, "「不限」是收尾的那一下，点完该收");
        assert!(
            rows[1..].iter().all(|row| !row.close),
            "标签要多选：点完 cli 还得能接着点「待读」"
        );
    }

    #[test]
    fn the_tag_trigger_summarizes_what_is_selected() {
        let rows = tag_filter_rows(
            vec![FacetRow {
                name: "cli".to_string(),
                repos: 3,
            }],
            4,
        );
        assert_eq!(tags_label(&[], &rows), "不限");
        assert_eq!(tags_label(&["cli".to_string()], &rows), "cli · 3");
        // 挂多了按钮上写不下：写第一个 + 还剩几个。
        assert_eq!(
            tags_label(&["cli".to_string(), "待读".to_string()], &rows),
            "cli · 3 +1"
        );
    }

    /// 一个在临时目录里、塞好分类与仓库的库。
    fn seeded_library(name: &str) -> Library {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::time::{SystemTime, UNIX_EPOCH};

        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default();
        let dir = std::env::temp_dir().join(format!(
            "ngy-fav-repos-view-{name}-{}-{stamp}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));

        let mut library = Library::load_from(dir.join("library.db"));
        // 词表先建好：`add_repo` 落库时是按名字把标签换成 id 的。
        library.add_name(crate::taxonomy::model::TaxonomyKind::Tag, "cli");
        library.add_name(crate::taxonomy::model::TaxonomyKind::Tag, "待读");

        for (repo, category, language, tags) in [
            ("a/rust", Some("开发/前端"), Some("Rust"), &["cli"][..]),
            ("b/other", Some("工具"), Some("Go"), &["待读"][..]),
            ("c/plain", None, None, &[][..]),
        ] {
            let mut model = crate::repos::model::Repo::new(repo.to_string(), None, 1);
            model.language = language.map(str::to_string);
            model.tags = tags.iter().map(|tag| tag.to_string()).collect();
            library.add_repo(model);
            if let Some(category) = category {
                library.add_name(crate::taxonomy::model::TaxonomyKind::Category, category);
                assert!(library.set_repo_category(repo, Some(category)));
            }
        }
        library
    }

    /// 测试窗口的根。
    ///
    /// 窗口的根必须是一个自己实现了 `Render` 的类型（`Entity<T>` 不算），而被测的
    /// 视图又只能在拿到窗口之后才能建（它要 `&mut Window`）。所以套一层壳：壳在
    /// 窗口里建视图，句柄另存一份给外面用。
    struct Harness {
        view: Entity<ReposView>,
    }

    impl Render for Harness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            self.view.clone()
        }
    }

    /// 这条测试盯的是**只有跑起来才说得清**的那一段：下拉框点得开吗、点一行之
    /// 后列表真跟着筛了吗、菜单自己收起来了吗。这些事在渲染代码里，纯函数测不
    /// 到——以前只能靠人手点，所以干脆用无头窗口点一遍。
    #[gpui_kit::test]
    fn picking_a_category_from_the_dropdown_narrows_the_list_and_closes_the_menu(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_kit::init);

        let library = cx.new(|_| seeded_library("dropdown"));
        let runner = cx.new(|cx| DiscoveryRunner::new(library.clone(), cx));
        let slot: Rc<RefCell<Option<Entity<ReposView>>>> = Rc::new(RefCell::new(None));
        let handle = {
            let slot = slot.clone();
            cx.add_window(move |window, cx| {
                let view = cx.new(|cx| ReposView::new(library, runner, window, cx));
                *slot.borrow_mut() = Some(view.clone());
                Harness { view }
            })
        };

        let filter = |cx: &mut TestAppContext| {
            let view = slot.borrow().clone().expect("窗口已经建好");
            cx.update(|app| view.read(app).list.read(app).delegate().category_filter().clone())
        };
        let total = |cx: &mut TestAppContext| {
            let view = slot.borrow().clone().expect("窗口已经建好");
            cx.update(|app| view.read(app).list.read(app).delegate().total())
        };

        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            // 收起时只有按钮：上面那排色块没了，列表区不会被分类挤掉好几行。
            assert!(
                window.try_find("repo-category-filter-menu").is_none(),
                "没点之前不该画出来"
            );

            window.click("repo-category-filter-slot", cx);
            window.render_frame(cx);
            assert!(
                window.try_find("repo-category-filter-menu").is_some(),
                "点一下按钮要展开分类树"
            );

            // 位置也得对：挂在按钮**下面**、宽度像一块菜单。锚点配错时它可能跑到
            // 窗口外面去，而那种错光看「元素存在」是发现不了的。
            let slot = window.find("repo-category-filter-slot").bounds();
            let menu = window.find("repo-category-filter-menu").bounds();
            assert!(
                menu.origin.y >= slot.origin.y,
                "下拉该挂在按钮下方，而不是盖住按钮或跑到上面：{menu:?} vs {slot:?}"
            );
            assert!(menu.size.width >= px(200.), "太窄了就不像菜单：{menu:?}");
        })
        .unwrap();
        assert_eq!(total(cx), 3, "还没筛之前三条都在");

        // 点「开发/前端」——注意它是**子分类**，缩进里的那一级。
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("repo-category-filter-row:开发/前端", cx);
            window.render_frame(cx);
            assert!(
                window.try_find("repo-category-filter-menu").is_none(),
                "选完要自己收起来，不然会挡住列表"
            );
        })
        .unwrap();
        assert_eq!(
            filter(cx),
            CategoryFilter::Named("开发/前端".to_string()),
            "点一行要落到筛选条件上"
        );
        assert_eq!(total(cx), 1, "列表要跟着筛：只剩开发/前端里的那一条");

        // 再点开一次，改成「未分类」——收起来之后还得能再打开。
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("repo-category-filter-slot", cx);
            window.render_frame(cx);
            window.click("repo-category-filter-row:uncategorized", cx);
            window.render_frame(cx);
        })
        .unwrap();
        assert_eq!(filter(cx), CategoryFilter::Uncategorized);
        assert_eq!(total(cx), 1, "另外一条没分类的");
    }

    /// 语言是单选、标签是多选——两种交互的差别就在「点完收不收」上，而这件事只有
    /// 真点一遍才说得清。顺手把「三个维度之间是且」也钉住。
    #[gpui_kit::test]
    fn the_language_and_tag_dropdowns_narrow_the_list(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);

        let library = cx.new(|_| seeded_library("facets"));
        let runner = cx.new(|cx| DiscoveryRunner::new(library.clone(), cx));
        let slot: Rc<RefCell<Option<Entity<ReposView>>>> = Rc::new(RefCell::new(None));
        let handle = {
            let slot = slot.clone();
            cx.add_window(move |window, cx| {
                let view = cx.new(|cx| ReposView::new(library, runner, window, cx));
                *slot.borrow_mut() = Some(view.clone());
                Harness { view }
            })
        };

        let language = |cx: &mut TestAppContext| {
            let view = slot.borrow().clone().expect("窗口已经建好");
            cx.update(|app| {
                view.read(app)
                    .list
                    .read(app)
                    .delegate()
                    .language_filter()
                    .map(str::to_string)
            })
        };
        let tags = |cx: &mut TestAppContext| {
            let view = slot.borrow().clone().expect("窗口已经建好");
            cx.update(|app| {
                view.read(app)
                    .list
                    .read(app)
                    .delegate()
                    .tags_filter()
                    .to_vec()
            })
        };
        let total = |cx: &mut TestAppContext| {
            let view = slot.borrow().clone().expect("窗口已经建好");
            cx.update(|app| view.read(app).list.read(app).delegate().total())
        };

        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);

            // 语言：单选，点一行就收。
            window.click("repo-language-filter-slot", cx);
            window.render_frame(cx);
            assert!(
                window.try_find("repo-language-filter-menu").is_some(),
                "点一下要展开语言列表"
            );
            window.click("repo-language-filter-row:Rust", cx);
            window.render_frame(cx);
            assert!(
                window.try_find("repo-language-filter-menu").is_none(),
                "单选点完要收起来"
            );
        })
        .unwrap();
        assert_eq!(language(cx).as_deref(), Some("Rust"));
        assert_eq!(total(cx), 1, "只剩用 Rust 的那一条");

        cx.update_window(handle.into(), |_, window, cx| {
            // 标签：多选，点完不收，好接着点下一个。
            window.click("repo-tag-filter-slot", cx);
            window.render_frame(cx);
            window.click("repo-tag-filter-row:待读", cx);
            window.render_frame(cx);
            assert!(
                window.try_find("repo-tag-filter-menu").is_some(),
                "多选点完要留着，不然选不了第二个"
            );
        })
        .unwrap();
        assert_eq!(tags(cx), ["待读"]);
        assert_eq!(
            total(cx),
            0,
            "维度之间是且：Rust 那条挂的是 cli，不是「待读」"
        );

        // 「不限」是收尾的那一下：点完收起、标签清空、列表回到语言筛完的样子。
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("repo-tag-filter-row:any", cx);
            window.render_frame(cx);
            assert!(
                window.try_find("repo-tag-filter-menu").is_none(),
                "「不限」点完该收"
            );
        })
        .unwrap();
        assert!(tags(cx).is_empty());
        assert_eq!(total(cx), 1);

        // 「清除」把三个下拉一起归位（搜索框不在其中）。
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("clear-repo-filters-slot", cx);
            window.render_frame(cx);
        })
        .unwrap();
        assert_eq!(language(cx), None);
        assert_eq!(total(cx), 3, "三条都回来了");
    }

    /// 勾选是批量操作的入口，而「点勾选框」跟「点这一行」是两件事：前者只该勾上，
    /// 不该顺手把右边的详情面板也换过去（那样连勾五个就会看见详情来回乱跳）。
    ///
    /// 这条只有在无头窗口里真点一下才说得清——勾选框嵌在可点击的列表行里，
    /// 事件怎么走是靠 gpui 的命中测试定的，光看代码看不出来。
    #[gpui_kit::test]
    fn the_checkbox_checks_without_stealing_the_row_click(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);

        let library = cx.new(|_| seeded_library("check"));
        let runner = cx.new(|cx| DiscoveryRunner::new(library.clone(), cx));
        let slot: Rc<RefCell<Option<Entity<ReposView>>>> = Rc::new(RefCell::new(None));
        let handle = {
            let slot = slot.clone();
            cx.add_window(move |window, cx| {
                let view = cx.new(|cx| ReposView::new(library, runner, window, cx));
                *slot.borrow_mut() = Some(view.clone());
                Harness { view }
            })
        };

        // 每次都要把 delegate 里的东西读出来；几个小闭包省得反复写借用。
        let checked_count = |cx: &mut TestAppContext| {
            let view = slot.borrow().clone().expect("窗口已经建好");
            cx.update(|app| view.read(app).list.read(app).delegate().checked_count())
        };
        let checked_ids = |cx: &mut TestAppContext| {
            let view = slot.borrow().clone().expect("窗口已经建好");
            cx.update(|app| view.read(app).list.read(app).delegate().checked_ids())
        };
        let selection = |cx: &mut TestAppContext| {
            let view = slot.borrow().clone().expect("窗口已经建好");
            cx.update(|app| {
                view.read(app)
                    .list
                    .read(app)
                    .delegate()
                    .selection()
                    .cloned()
            })
        };
        let click = |cx: &mut TestAppContext, id: &str| {
            let id = id.to_string();
            cx.update_window(handle.into(), move |_, window, cx| {
                window.click(id, cx);
                window.render_frame(cx);
            })
            .unwrap();
        };

        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(
                window.try_find("repo-select-all-slot").is_some(),
                "列表上方要有一条批量操作条"
            );
            assert!(
                window.try_find("repo-bulk-category-slot").is_none(),
                "什么都没勾时不摆操作按钮，摆一排点不动的只会让人去试"
            );
        })
        .unwrap();

        click(cx, "repo-check-slot:a/rust");
        assert_eq!(checked_count(cx), 1, "点一下勾上一个");
        assert_eq!(
            selection(cx),
            None,
            "点勾选框不该顺手把详情面板换过去——那是「点这一行」的事"
        );

        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(
                window.try_find("repo-bulk-category-slot").is_some(),
                "勾上了才摆操作按钮"
            );
        })
        .unwrap();

        click(cx, "repo-check-slot:a/rust");
        assert_eq!(checked_count(cx), 0, "再点一下摘掉");

        // 「全选」勾的是筛出来的整批。
        click(cx, "repo-select-all-slot");
        assert_eq!(checked_count(cx), 3, "三条都在筛选结果里");
        assert_eq!(selection(cx), None, "全选也不该动详情面板");

        click(cx, "repo-select-all-slot");
        assert_eq!(checked_count(cx), 0, "已经全勾上了，再点就是取消");

        // 勾一个再「清空」，那个按钮管的正是这件事。
        click(cx, "repo-check-slot:b/other");
        click(cx, "repo-clear-checked-slot");
        assert_eq!(checked_count(cx), 0);

        // 换个筛选条件，全选跟着变——它从来不是「这一页」。
        click(cx, "repo-language-filter-slot");
        click(cx, "repo-language-filter-row:Rust");
        click(cx, "repo-select-all-slot");
        assert_eq!(
            checked_ids(cx),
            ["a/rust"],
            "筛到只剩一条，全选就只勾这一条"
        );
    }

    /// 列表与详情之间那一刀能拖。宽度这件事只有真拖一遍才说得清：分栏状态里
    /// 存着数与布局真的按它铺开，是两回事。
    ///
    /// 抓手在分隔条**左边那 4 个像素**里（组件库把把手画成一条 1px 的线、往左让出
    /// 4px 的抓取带），所以 `from` 取「列表栏宽度 - 2」。
    #[gpui_kit::test]
    fn the_divider_between_the_list_and_the_detail_can_be_dragged(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);

        let library = cx.new(|_| seeded_library("split"));
        let runner = cx.new(|cx| DiscoveryRunner::new(library.clone(), cx));
        let slot: Rc<RefCell<Option<Entity<ReposView>>>> = Rc::new(RefCell::new(None));
        let handle = {
            let slot = slot.clone();
            cx.add_window(move |window, cx| {
                let view = cx.new(|cx| ReposView::new(library, runner, window, cx));
                *slot.borrow_mut() = Some(view.clone());
                Harness { view }
            })
        };

        cx.update_window(handle.into(), |_, window, cx| {
            let view = slot.borrow().clone().expect("窗口已经建好");
            // 列表栏**渲染出来**多宽。分栏里那个数只是它的来源，两者不一致就是
            // 「状态变了、布局没动」那类错，光看状态是发现不了的。
            let list_column = |window: &mut Window| -> f32 {
                window
                    .find("repo-list-column")
                    .bounds()
                    .size
                    .width
                    .as_f32()
            };
            let state_width = |cx: &App| -> f32 {
                view.read(cx)
                    .split
                    .read(cx)
                    .sizes()
                    .first()
                    .map(|size| size.as_f32())
                    .unwrap_or_default()
            };

            window.render_frame(cx);
            let list = list_column(window);
            assert!(
                (list - LIST_WIDTH).abs() < 1.,
                "一开始是 {LIST_WIDTH}：{list}"
            );
            assert!(
                (state_width(cx) - list).abs() < 1.,
                "分栏记的宽度与铺出来的宽度得是同一个数"
            );

            // 抓住分隔条往右拖 118。
            let y = window.viewport_size().height / 2.;
            window.drag(point(px(list - 2.), y), point(px(list + 118.), y), cx);
            window.render_frame(cx);
            let wider = list_column(window);
            assert!(
                (wider - (list + 118.)).abs() < 1.5,
                "列表栏要跟着光标走：{wider}（本来 {list}）"
            );
            assert!(
                (state_width(cx) - wider).abs() < 1.5,
                "宽度落到了布局上，分栏里也该是同一个数：{}",
                state_width(cx)
            );

            // 往左拖过头会被下限拦住：详情不该被挤成一条缝。
            window.drag(point(px(wider - 2.), y), point(px(10.), y), cx);
            window.render_frame(cx);
            assert!(
                (list_column(window) - LIST_MIN_WIDTH).abs() < 1.5,
                "拖到下限就停住：{}",
                list_column(window)
            );
        })
        .unwrap();
    }

    /// 手工加进来的仓库要被**选中**：详情面板看它，列表里那一行高亮。
    ///
    /// 走的是对话框提交完之后那一步（`select_added_callback`）。`submit` 本身在这
    /// 里调不动：它会给新仓库拉一次元数据，那是真联网。
    #[gpui_kit::test]
    fn a_manually_added_repo_arrives_selected_on_the_first_page(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);

        let library = cx.new(|_| {
            let mut library = seeded_library("added-selected");
            // 撑出第二页（一页 50 条）：不然「回到第一页」没有可观察的后果。
            for index in 0..51 {
                library.add_repo(crate::repos::model::Repo::new(
                    format!("z/r{index:02}"),
                    None,
                    1,
                ));
            }
            library
        });
        let runner = cx.new(|cx| DiscoveryRunner::new(library.clone(), cx));
        let slot: Rc<RefCell<Option<Entity<ReposView>>>> = Rc::new(RefCell::new(None));
        let handle = {
            let slot = slot.clone();
            cx.add_window(move |window, cx| {
                let view = cx.new(|cx| ReposView::new(library, runner, window, cx));
                *slot.borrow_mut() = Some(view.clone());
                Harness { view }
            })
        };
        let window_slot = || slot.borrow().clone().expect("窗口已经建好");

        // 详情面板看的那个（`selected`）、列表高亮的那个（delegate 的选中）、
        // 当前页码、以及「d/new 在第几行」——一次读齐。
        let read = |cx: &mut TestAppContext| {
            let view = window_slot();
            cx.update(move |app| {
                let view = view.read(app);
                let delegate = view.list.read(app).delegate();
                (
                    view.selected.clone(),
                    delegate.selection().cloned(),
                    delegate.page().page,
                    delegate.position_of("d/new"),
                )
            })
        };

        // 先站到第二页上，这样「回第一页」才看得出效果。
        cx.update_window(handle.into(), |_, window, cx| {
            let view = window_slot();
            view.update(cx, |view, cx| view.turn_page(true, cx));
            window.render_frame(cx);
        })
        .unwrap();
        assert_eq!(read(cx).2, 1, "先翻到第二页");

        // 加一个（这一步由对话框做），再走「加完了」这一步。
        cx.update_window(handle.into(), |_, window, cx| {
            let view = window_slot();
            view.update(cx, |view, cx| {
                view.library.update(cx, |library, cx| {
                    library.add_repo(crate::repos::model::Repo::new(
                        "d/new".to_string(),
                        None,
                        now_unix(),
                    ));
                    cx.notify();
                });
                cx.notify();
            });
            select_added_callback(&view.downgrade())("d/new", window, cx);
            window.render_frame(cx);
        })
        .unwrap();

        let (selected, selection, page, position) = read(cx);
        assert_eq!(selection.as_deref(), Some("d/new"), "加进来就该被选中");
        assert_eq!(
            selected.as_deref(),
            Some("d/new"),
            "详情面板看的也得是它——这才是「选中」"
        );
        assert_eq!(
            position,
            Some(0),
            "它是最新的，按发现时间倒序排在第一页第一行"
        );
        assert_eq!(page, 0, "得把它拽回第一页来，不然加完了看不见");
    }
}

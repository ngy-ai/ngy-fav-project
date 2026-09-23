//! 应用外壳：左侧导航 + 五个功能页 + 窗口级覆盖层。
//!
//! 一个窗口只有一个 `Root`（在 `main` 里包住本视图），而对话框、通知这些
//! 覆盖层的内容由应用自己渲染——`Root` 只负责调度，不会替你画出来。
//!
//! 三栏（侧栏 / 列表 / 详情）的宽度都能拖：这里管侧栏那一刀，列表与详情之间
//! 那一刀在 `repos_view` 里。

use gpui_kit::assets::IconName;
use gpui_kit::component::{
    ActiveTheme as _, Icon, Root, h_flex, v_flex,
    alert::Alert,
    resizable::{ResizablePanel, ResizableState, h_resizable, resizable_panel},
    sidebar::{
        Sidebar, SidebarCollapsible, SidebarFooter, SidebarHeader, SidebarMenu, SidebarMenuItem,
        SidebarToggleButton,
    },
};
use gpui_kit::prelude::*;
use gpui_kit::*;

use std::cell::Cell;
use std::ops::Range;
use std::rc::Rc;

use crate::discovery::runner::{DiscoveryRunner, RunPhase};
use crate::discovery::SourcesView;
use crate::library::{Library, SaveState};
use crate::repos::ReposView;
use crate::settings::SettingsView;
use crate::shared::text::copyable;
use crate::taxonomy::{TaxonomyKind, TaxonomyView};

/// 侧栏展开时一开始有多宽。拖过之后以拖出来的为准。
const SIDEBAR_WIDTH: f32 = 208.;
/// 侧栏收起来只剩图标时的宽度。与组件库 `Sidebar` 的 `COLLAPSED_WIDTH` 一致，
/// 两边得是同一个数，否则面板与里面那一栏会差出一截白边。
const SIDEBAR_COLLAPSED_WIDTH: f32 = 48.;
/// 侧栏能拖到多宽。下限比收起宽度大一截：拖到 48 那种宽度会让文字全挤没，
/// 想变窄应该用收起（顶栏那个按钮），不是把它拖没。
const SIDEBAR_MIN_WIDTH: f32 = 168.;
const SIDEBAR_MAX_WIDTH: f32 = 360.;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Repos,
    Categories,
    Tags,
    Discovery,
    Settings,
}

impl Page {
    fn title(&self) -> &'static str {
        match self {
            Self::Repos => "仓库",
            Self::Categories => "分类",
            Self::Tags => "标签",
            Self::Discovery => "发现",
            Self::Settings => "设置",
        }
    }
}

pub struct AppShell {
    library: Entity<Library>,
    runner: Entity<DiscoveryRunner>,
    repos: Entity<ReposView>,
    /// 分类页与标签页是同一个视图的两份实例，各自管一个词表。
    categories: Entity<TaxonomyView>,
    tags: Entity<TaxonomyView>,
    sources: Entity<SourcesView>,
    settings: Entity<SettingsView>,
    page: Page,
    sidebar_collapsed: bool,
    /// 侧栏展开时该有多宽。只由**拖动**改（分栏在鼠标松开时把宽度交回来）；
    /// 收起与展开都不动它——收起只是把那一栏夹到图标宽，展开是把这个数还回去。
    sidebar_width: f32,
    /// 上一次拖完时那一栏的宽度，由分栏在鼠标松开时写进来。`on_resize` 的回调拿不到
    /// 本视图的 context，只好用个共享格子转一手，下一帧渲染时读走。
    dropped_column_width: Rc<Cell<f32>>,
    /// 侧栏 ↔ 内容那一刀的分栏状态。宽度记在它里面——它自己每帧都在量、在改，
    /// 另存一份镜像迟早对不上。
    shell_split: Entity<ResizableState>,
    _subscriptions: Vec<Subscription>,
}

impl AppShell {
    pub fn new(
        library: Entity<Library>,
        runner: Entity<DiscoveryRunner>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let repos = cx.new(|cx| ReposView::new(library.clone(), runner.clone(), window, cx));
        let categories =
            cx.new(|cx| TaxonomyView::new(library.clone(), TaxonomyKind::Category, window, cx));
        let tags = cx.new(|cx| TaxonomyView::new(library.clone(), TaxonomyKind::Tag, window, cx));
        let sources = cx.new(|cx| SourcesView::new(library.clone(), runner.clone(), window, cx));
        let settings = cx.new(|cx| SettingsView::new(library.clone(), window, cx));
        let shell_split = cx.new(|_| ResizableState::default());
        let subscriptions = vec![
            cx.observe(&library, |_, _, cx| cx.notify()),
            cx.observe(&runner, |_, _, cx| cx.notify()),
        ];

        Self {
            library,
            runner,
            repos,
            categories,
            tags,
            sources,
            settings,
            page: Page::Repos,
            sidebar_collapsed: false,
            sidebar_width: SIDEBAR_WIDTH,
            dropped_column_width: Rc::new(Cell::new(0.)),
            shell_split,
            _subscriptions: subscriptions,
        }
    }

    fn go(&mut self, page: Page, cx: &mut Context<Self>) {
        if self.page != page {
            self.page = page;
            cx.notify();
        }
    }

    /// 侧栏能拖到的范围。
    ///
    /// 收起时夹成一点：那一栏已经只剩图标，再拖就没意义了（拖动本身也会把它
    /// 拉成一个怪样子）。展开时的下限放到图标宽是为了「收起」那一下能直接把宽度
    /// 设成 48——分栏按**上一次绘制**的范围夹，收起那一刻手里还是展开时的范围，
    /// 范围里不含 48 就会被夹回下限。拖得比 `SIDEBAR_MIN_WIDTH` 窄的情况另有人管：
    /// 松手后自动收起（见 `render`）。
    fn sidebar_range(&self) -> Range<Pixels> {
        if self.sidebar_collapsed {
            px(SIDEBAR_COLLAPSED_WIDTH)..px(SIDEBAR_COLLAPSED_WIDTH)
        } else {
            px(SIDEBAR_COLLAPSED_WIDTH)..px(SIDEBAR_MAX_WIDTH)
        }
    }

    /// 收起 / 展开侧栏。
    ///
    /// 宽度是分栏记着的，所以两件事都得自己做。
    ///
    /// 收起就是把宽度设成图标宽。展开反过来**不能**直接设宽度：分栏按上一次绘制
    /// 记下的尺寸区间夹宽度，而收起那一帧记的是 48..48，设多少都会被夹回 48。
    /// 所以展开只把这块面板重置掉（把「上一次设过的尺寸」清空），宽度交给渲染时
    /// 那个 `.size()`——面板没有别的尺寸来源时才会用它，而它正是我们记着的那一个。
    fn toggle_sidebar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.sidebar_collapsed {
            self.sidebar_collapsed = false;
            self.shell_split
                .update(cx, |state, cx| state.reset_panel(0, cx));
        } else {
            self.sidebar_collapsed = true;
            let width = px(SIDEBAR_COLLAPSED_WIDTH);
            self.shell_split
                .update(cx, |state, cx| state.resize_panel(0, width, window, cx));
        }
        cx.notify();
    }

    /// 侧栏里那一行状态字：在跑什么、或是自动发现开着还是关着。
    fn status_text(&self, cx: &Context<Self>) -> String {
        let runner = self.runner.read(cx);
        match runner.phase() {
            RunPhase::Running { step } => format!("运行中 · {step}"),
            RunPhase::Idle => {
                if self.library.read(cx).settings().auto_discover {
                    "自动发现已开启".to_string()
                } else {
                    "自动发现已关闭".to_string()
                }
            }
        }
    }

    /// 返回具体类型而不是 `impl IntoElement`：Rust 2024 下 `impl Trait` 会捕获
    /// 入参里的 `&mut Context` 生命周期，两个这样的辅助方法就没法连着调了。
    fn render_sidebar(&self, status: &str, cx: &mut Context<Self>) -> Sidebar<SidebarMenu> {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let primary = theme.primary;
        let primary_foreground = theme.primary_foreground;
        let radius = theme.radius;
        let collapsed = self.sidebar_collapsed;

        let item = |page: Page, icon: IconName, this_page: Page| {
            SidebarMenuItem::new(page.title())
                .icon(icon)
                .active(this_page == page)
        };

        Sidebar::new("app-sidebar")
            .collapsible(SidebarCollapsible::Icon)
            .collapsed(collapsed)
            // 宽度交给分栏的那块面板：这里写死一个像素值，拖动时就会「面板变了、
            // 栏没变」。写 `w_full()` 还有个副作用是正好关掉组件库自带的宽度过渡动画
            // （它只在像素宽度下才启用），而那动画会拖在光标后面，拖起来是橡皮筋手感。
            .w_full()
            .header(
                SidebarHeader::new()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_center()
                            .size_6()
                            .flex_shrink_0()
                            .rounded(radius)
                            .bg(primary)
                            .text_color(primary_foreground)
                            .child(Icon::new(IconName::Star).size_3p5()),
                    )
                    .when(!collapsed, |this| {
                        this.child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .overflow_hidden()
                                .child(
                                    div()
                                        .text_sm()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .truncate()
                                        .child("GitHub 收藏"),
                                )
                                .child(
                                    div().text_xs().text_color(muted).truncate().child("发现 · 管理"),
                                ),
                        )
                    }),
            )
            .child(
                SidebarMenu::new().children([
                    item(Page::Repos, IconName::Library, self.page)
                        .on_click(cx.listener(|this, _, _, cx| this.go(Page::Repos, cx))),
                    item(Page::Categories, IconName::FolderTree, self.page)
                        .on_click(cx.listener(|this, _, _, cx| this.go(Page::Categories, cx))),
                    item(Page::Tags, IconName::Tag, self.page)
                        .on_click(cx.listener(|this, _, _, cx| this.go(Page::Tags, cx))),
                    item(Page::Discovery, IconName::Radar, self.page)
                        .on_click(cx.listener(|this, _, _, cx| this.go(Page::Discovery, cx))),
                    item(Page::Settings, IconName::Settings, self.page)
                        .on_click(cx.listener(|this, _, _, cx| this.go(Page::Settings, cx))),
                ]),
            )
            .footer(
                SidebarFooter::new().child(
                    h_flex()
                        .gap_2()
                        .min_w_0()
                        .child(
                            div()
                                .size_2()
                                .rounded_full()
                                .flex_shrink_0()
                                .bg(muted),
                        )
                        .when(!collapsed, |this| {
                            this.child(
                                div()
                                    .text_xs()
                                    .text_color(muted)
                                    .truncate()
                                    .child(copyable(status.to_string())),
                            )
                        }),
                ),
            )
    }

    /// 侧栏那一块分栏面板。
    fn render_sidebar_panel(&self, sidebar: Sidebar<SidebarMenu>) -> ResizablePanel {
        resizable_panel()
            .size(px(self.sidebar_width))
            .size_range(self.sidebar_range())
            // 有固定宽度的一栏不该跟着兄弟一起长，组件库的注释里点名要写这一句。
            .flex_none()
            // 这一层只为量尺寸：无头窗口里要能量到「侧栏现在到底多宽」，
            // 而 `Sidebar` 是组件、挂不上观测点。开了特性才存在，平时是透明的一层。
            .child(
                div()
                    .id("app-sidebar-column")
                    .test_support()
                    .size_full()
                    .child(sidebar),
            )
    }

    fn render_top_bar(&self, status: &str, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let border = theme.border;
        let collapsed = self.sidebar_collapsed;

        h_flex()
            .h_10()
            .px_3()
            .gap_2()
            .flex_shrink_0()
            .border_b_1()
            .border_color(border)
            .child(
                SidebarToggleButton::new()
                    .collapsed(collapsed)
                    .on_click(cx.listener(|this, _, window, cx| this.toggle_sidebar(window, cx))),
            )
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(self.page.title()),
            )
            .child(div().flex_1())
            .child(div().text_xs().text_color(muted).child(copyable(status.to_string())))
    }
}

impl Render for AppShell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let background = theme.background;
        let foreground = theme.foreground;
        let border = theme.border;

        let (load_warning, load_note, save_error) = {
            let library = self.library.read(cx);
            let save_error = match library.save_state() {
                SaveState::Failed(error) => Some(error.clone()),
                _ => None,
            };
            (
                library.load_warning().map(str::to_string),
                library.load_note().map(str::to_string),
                save_error,
            )
        };

        // 上一次拖动的结果。分栏在鼠标松开时把宽度交回来，这里读走：拖得太窄的那次
        // 不是「窄侧栏」而是「想收起来」，停在中间那个宽度既挤得没法看、换了页面也
        // 留不住，所以直接收起。判据只能来自这个数——拖动的范围本身允许拖到 48
        // （收起那一下要用它），拦不住这件事。
        let dropped = self.dropped_column_width.replace(0.);
        if !self.sidebar_collapsed && dropped > 0. {
            if dropped < SIDEBAR_MIN_WIDTH {
                self.sidebar_collapsed = true;
                let width = px(SIDEBAR_COLLAPSED_WIDTH);
                self.shell_split
                    .update(cx, |state, cx| state.resize_panel(0, width, window, cx));
            } else {
                self.sidebar_width = dropped;
            }
        }

        let status = self.status_text(cx);
        let sidebar = self.render_sidebar(&status, cx);
        let top_bar = self.render_top_bar(&status, cx);

        // 读取或写库出问题必须一直摆在脸上，不能只在通知里闪一下。迁移这类
        // 「一次性好消息」也走横幅：它只在真的搬迁过的那一次启动里出现。
        let banners: Vec<AnyElement> = [
            load_warning.map(|warning| {
                Alert::warning("library-load-warning", warning)
                    .title("数据库读取异常")
                    .banner()
                    .into_any_element()
            }),
            load_note.map(|note| {
                Alert::info("library-load-note", note)
                    .title("数据已迁移")
                    .banner()
                    .into_any_element()
            }),
            save_error.map(|error| {
                Alert::error("library-save-error", error)
                    .title("数据保存失败")
                    .banner()
                    .into_any_element()
            }),
        ]
        .into_iter()
        .flatten()
        .collect();

        let content: AnyElement = match self.page {
            Page::Repos => self.repos.clone().into_any_element(),
            Page::Categories => self.categories.clone().into_any_element(),
            Page::Tags => self.tags.clone().into_any_element(),
            Page::Discovery => self.sources.clone().into_any_element(),
            Page::Settings => self.settings.clone().into_any_element(),
        };

        h_flex()
            .size_full()
            .min_w_0()
            .bg(background)
            .text_color(foreground)
            .child(
                // 侧栏与内容各占一块面板，中间那一刀由组件库画、由它处理拖动。
                // 覆盖层（对话框、通知）不能挂在这里：分栏只收「面板」这一种孩子，
                // 多塞一个就多出一刀来。
                h_resizable("app-shell")
                    .with_state(&self.shell_split)
                    // 拖完的那一下把宽度记回去，好让上面那段判断「拖得太窄」。
                    .on_resize({
                        let slot = self.dropped_column_width.clone();
                        move |state, _, cx| {
                            if let Some(width) = state.read(cx).sizes().first() {
                                slot.set(width.as_f32());
                            }
                        }
                    })
                    .child(self.render_sidebar_panel(sidebar))
                    .child(resizable_panel().child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .border_l_1()
                            .border_color(border)
                            .child(top_bar)
                            .children(banners)
                            .child(div().flex_1().min_h_0().child(content)),
                    )),
            )
            .children(Root::render_dialog_layer(window, cx))
            .children(Root::render_sheet_layer(window, cx))
            .children(Root::render_notification_layer(window, cx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // `gpui_kit::*` 会把 GPUI 的 `#[gpui::test]` 带进来盖掉 std 的 `#[test]`。
    use core::prelude::v1::test;
    use gpui_kit::TestAppContext;
    use gpui_kit::test::TestWindowExt as _;
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    /// 一个在临时目录里的空库：这条测试只关心那一栏多宽，不关心里面有什么。
    fn empty_library(name: &str) -> Library {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default();
        let dir = std::env::temp_dir().join(format!(
            "ngy-fav-shell-{name}-{}-{stamp}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        Library::load_from(dir.join("library.db"))
    }

    /// 窗口的根。
    ///
    /// `Root` 要摆成窗口的第一层（对话框、通知都往它上面挂），但 `add_window` 要的
    /// 是一个自己实现了 `Render` 的类型，而 `Entity<Root>` 不算——所以套一层壳。
    struct Shell {
        root: Entity<Root>,
    }

    impl Render for Shell {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            self.root.clone()
        }
    }

    /// 侧栏那一栏：拖得动，收起时夹到图标宽，展开回来还是拖出来的那个宽度。
    ///
    /// 「收起以后再展开」这一段特别值得钉住：收起只是把那一栏**夹窄**，分栏状态里
    /// 记着的宽度不该被改掉——真改掉了，展开回来就是 48 宽的一栏配着文字，看着像坏了。
    ///
    /// 抓手在分隔条左边那 4 个像素里，所以 `from` 取「那一栏宽度 - 2」。
    #[gpui_kit::test]
    fn the_sidebar_column_can_be_dragged_and_keeps_its_width_across_collapsing(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_kit::init);

        let slot: Rc<RefCell<Option<Entity<AppShell>>>> = Rc::new(RefCell::new(None));
        let handle = {
            let slot = slot.clone();
            cx.add_window(move |window, cx| {
                let library = cx.new(|_| empty_library("split"));
                let runner = cx.new(|cx| DiscoveryRunner::new(library.clone(), cx));
                let shell = cx.new(|cx| AppShell::new(library, runner, window, cx));
                *slot.borrow_mut() = Some(shell.clone());
                Shell {
                    root: cx.new(|cx| Root::new(shell, window, cx)),
                }
            })
        };

        cx.update_window(handle.into(), |_, window, cx| {
            let shell = slot.borrow().clone().expect("窗口已经建好");
            // 那一栏**渲染出来**多宽。用户看得见的就是它，分栏里那个数只是它的来源；
            // 两者不一致，就是「面板变了、栏没变」那类错。
            let rendered = |window: &mut Window| -> f32 {
                window
                    .find("app-sidebar-column")
                    .bounds()
                    .size
                    .width
                    .as_f32()
            };

            window.render_frame(cx);
            let start = rendered(window);
            assert!(
                (start - SIDEBAR_WIDTH).abs() < 1.,
                "一开始是 {SIDEBAR_WIDTH}：{start}"
            );

            // 抓住分隔条往右拖 78。
            let y = window.viewport_size().height / 2.;
            window.drag(point(px(start - 2.), y), point(px(start + 78.), y), cx);
            window.render_frame(cx);
            let wider = rendered(window);
            assert!(
                (wider - (start + 78.)).abs() < 1.5,
                "侧栏跟着光标走：{wider}（本来 {start}）"
            );
            assert!(
                (shell.read(cx).sidebar_width - wider).abs() < 1.,
                "拖出来的宽度要记下来，将来展开时用它：{}",
                shell.read(cx).sidebar_width
            );

            // 收起：只剩图标宽。
            shell.update(cx, |shell, cx| shell.toggle_sidebar(window, cx));
            window.render_frame(cx);
            assert!(
                (rendered(window) - SIDEBAR_COLLAPSED_WIDTH).abs() < 1.,
                "收起只剩图标宽：{}",
                rendered(window)
            );

            // 展开：回到拖出来的那个宽度，不是回到默认值。
            shell.update(cx, |shell, cx| shell.toggle_sidebar(window, cx));
            window.render_frame(cx);
            assert!(
                (rendered(window) - wider).abs() < 1.,
                "展开回来还是拖出来的宽度：{} vs {wider}",
                rendered(window)
            );

            // 一路拖到比下限还窄：这不是「很窄的侧栏」，是「收起来」。
            let now = rendered(window);
            window.drag(point(px(now - 2.), y), point(px(10.), y), cx);
            // 松手那一帧分栏刚把宽度交回来，下一帧才据此决定要不要收起。多走一帧。
            window.render_frame(cx);
            window.render_frame(cx);
            assert!(
                shell.read(cx).sidebar_collapsed,
                "拖到比下限窄就该自动收起，而不是停在半宽上"
            );
            assert!(
                (rendered(window) - SIDEBAR_COLLAPSED_WIDTH).abs() < 1.,
                "收起来之后是图标宽：{}",
                rendered(window)
            );
            assert!(
                (shell.read(cx).sidebar_width - wider).abs() < 1.,
                "自动收起不该啃掉记着的宽度：{} vs {wider}",
                shell.read(cx).sidebar_width
            );

            // 再展开一次，还是那个宽度。
            shell.update(cx, |shell, cx| shell.toggle_sidebar(window, cx));
            window.render_frame(cx);
            assert!(
                (rendered(window) - wider).abs() < 1.,
                "再展开还是那个宽度：{} vs {wider}",
                rendered(window)
            );
        })
        .unwrap();
    }
}

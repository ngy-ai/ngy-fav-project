//! 应用外壳：左侧导航 + 三个功能页 + 窗口级覆盖层。
//!
//! 一个窗口只有一个 `Root`（在 `main` 里包住本视图），而对话框、通知这些
//! 覆盖层的内容由应用自己渲染——`Root` 只负责调度，不会替你画出来。

use gpui_kit::assets::IconName;
use gpui_kit::component::{
    ActiveTheme as _, Icon, Root, h_flex, v_flex,
    alert::Alert,
    sidebar::{
        Sidebar, SidebarCollapsible, SidebarFooter, SidebarHeader, SidebarMenu, SidebarMenuItem,
        SidebarToggleButton,
    },
};
use gpui_kit::prelude::*;
use gpui_kit::*;

use crate::discovery::runner::{DiscoveryRunner, RunPhase};
use crate::discovery::SourcesView;
use crate::library::{Library, SaveState};
use crate::repos::ReposView;
use crate::settings::SettingsView;
use crate::taxonomy::TaxonomyView;

const SIDEBAR_WIDTH: f32 = 208.;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Repos,
    Taxonomy,
    Discovery,
    Settings,
}

impl Page {
    fn title(&self) -> &'static str {
        match self {
            Self::Repos => "仓库",
            Self::Taxonomy => "分类与标签",
            Self::Discovery => "发现",
            Self::Settings => "设置",
        }
    }
}

pub struct AppShell {
    library: Entity<Library>,
    runner: Entity<DiscoveryRunner>,
    repos: Entity<ReposView>,
    taxonomy: Entity<TaxonomyView>,
    sources: Entity<SourcesView>,
    settings: Entity<SettingsView>,
    page: Page,
    sidebar_collapsed: bool,
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
        let taxonomy = cx.new(|cx| TaxonomyView::new(library.clone(), window, cx));
        let sources = cx.new(|cx| SourcesView::new(library.clone(), runner.clone(), window, cx));
        let settings = cx.new(|cx| SettingsView::new(library.clone(), window, cx));
        let subscriptions = vec![
            cx.observe(&library, |_, _, cx| cx.notify()),
            cx.observe(&runner, |_, _, cx| cx.notify()),
        ];

        Self {
            library,
            runner,
            repos,
            taxonomy,
            sources,
            settings,
            page: Page::Repos,
            sidebar_collapsed: false,
            _subscriptions: subscriptions,
        }
    }

    fn go(&mut self, page: Page, cx: &mut Context<Self>) {
        if self.page != page {
            self.page = page;
            cx.notify();
        }
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
            .w(px(SIDEBAR_WIDTH))
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
                    item(Page::Taxonomy, IconName::Tags, self.page)
                        .on_click(cx.listener(|this, _, _, cx| this.go(Page::Taxonomy, cx))),
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
                            this.child(div().text_xs().text_color(muted).truncate().child(status.to_string()))
                        }),
                ),
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
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.sidebar_collapsed = !this.sidebar_collapsed;
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(self.page.title()),
            )
            .child(div().flex_1())
            .child(div().text_xs().text_color(muted).child(status.to_string()))
    }
}

impl Render for AppShell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let background = theme.background;
        let foreground = theme.foreground;
        let border = theme.border;

        let (load_warning, save_error) = {
            let library = self.library.read(cx);
            let save_error = match library.save_state() {
                SaveState::Failed(error) => Some(error.clone()),
                _ => None,
            };
            (library.load_warning().map(str::to_string), save_error)
        };

        let status = self.status_text(cx);
        let sidebar = self.render_sidebar(&status, cx);
        let top_bar = self.render_top_bar(&status, cx);

        // 读取或写盘出问题必须一直摆在脸上，不能只在通知里闪一下。
        let banners: Vec<AnyElement> = [
            load_warning.map(|warning| {
                Alert::warning("library-load-warning", warning)
                    .title("数据文件读取异常")
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
            Page::Taxonomy => self.taxonomy.clone().into_any_element(),
            Page::Discovery => self.sources.clone().into_any_element(),
            Page::Settings => self.settings.clone().into_any_element(),
        };

        h_flex()
            .size_full()
            .min_w_0()
            .bg(background)
            .text_color(foreground)
            .child(sidebar)
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .border_l_1()
                    .border_color(border)
                    .child(top_bar)
                    .children(banners)
                    .child(div().flex_1().min_h_0().child(content)),
            )
            .children(Root::render_dialog_layer(window, cx))
            .children(Root::render_sheet_layer(window, cx))
            .children(Root::render_notification_layer(window, cx))
    }
}

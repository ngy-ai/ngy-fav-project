//! 发现页：管理发现地址，手动触发一轮，看最近一次跑了什么。

use gpui_kit::assets::IconName;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _, WindowExt as _, h_flex, v_flex,
    button::{Button, ButtonVariants as _},
    input::{Input, InputEvent, InputState},
    notification::Notification,
    switch::Switch,
    tag::Tag,
};
use gpui_kit::prelude::*;
use gpui_kit::*;

use crate::discovery::model::{DiscoverySource, SourceRun, validate_source_url};
use crate::discovery::runner::{DiscoveryRunner, RunPhase};
use crate::library::{AddOutcome, Library};
use crate::shared::clock::{format_local_minute, format_relative, now_unix};

/// 页面要用到的主题色。`Theme` 借自 `cx`，用之前先抄成值，
/// 免得后面还要 `&mut cx` 时和它撞上。
struct Palette {
    background: Hsla,
    foreground: Hsla,
    muted: Hsla,
    border: Hsla,
    radius: Pixels,
    primary: Hsla,
    success: Hsla,
    warning: Hsla,
}

impl Palette {
    fn of(cx: &App) -> Self {
        let theme = cx.theme();
        Self {
            background: theme.background,
            foreground: theme.foreground,
            muted: theme.muted_foreground,
            border: theme.border,
            radius: theme.radius,
            primary: theme.primary,
            success: theme.success,
            warning: theme.warning,
        }
    }
}

/// 发现源行的一次性快照。
#[derive(Clone)]
struct SourceRow {
    name: String,
    url: String,
    enabled: bool,
    last_run: Option<SourceRun>,
}

impl SourceRow {
    /// 最近一次运行的一句话总结。`None` 表示从未运行过。
    fn last_run_text(&self) -> (String, bool) {
        match &self.last_run {
            None => ("还没运行过".to_string(), false),
            Some(run) => {
                let when = format_relative(run.finished_at, now_unix());
                let summary = format!(
                    "{} · 抓取 {} 条，新增 {} 条，跳过 {} 条",
                    when, run.found, run.added, run.skipped
                );
                match &run.error {
                    Some(error) => (format!("{summary} · 失败：{error}"), true),
                    None => (summary, false),
                }
            }
        }
    }
}

pub struct SourcesView {
    library: Entity<Library>,
    runner: Entity<DiscoveryRunner>,
    /// 「添加发现地址」的输入框。和添加仓库同样是长期持有的实体。
    input: Entity<InputState>,
    _subscriptions: Vec<Subscription>,
}

impl SourcesView {
    pub fn new(
        library: Entity<Library>,
        runner: Entity<DiscoveryRunner>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("https://github.com/trending?since=weekly&spoken_language_code=rust")
                .submit_on_enter(true)
        });

        let mut subscriptions = Vec::new();
        subscriptions.push(cx.observe(&library, |_, _, cx| cx.notify()));
        subscriptions.push(cx.observe(&runner, |_, _, cx| cx.notify()));
        subscriptions.push(cx.subscribe_in(
            &input,
            window,
            |this, _, event, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    this.add_source(window, cx);
                }
            },
        ));

        Self {
            library,
            runner,
            input,
            _subscriptions: subscriptions,
        }
    }

    /// 添加一个发现地址。名称先从地址里推一个能认出来的默认值。
    fn add_source(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let raw = self.input.read(cx).value().to_string();
        let url = match validate_source_url(&raw) {
            Ok(url) => url,
            Err(reason) => {
                window.push_notification(Notification::error(reason), cx);
                return;
            }
        };

        let name = suggest_name(&url);
        let outcome = self.library.update(cx, |library, cx| {
            let outcome = library.add_source(DiscoverySource::new(name.clone(), url.clone()));
            if outcome == AddOutcome::Added {
                library.persist();
                cx.notify();
            }
            outcome
        });

        match outcome {
            AddOutcome::AlreadyPresent => {
                window.push_notification(Notification::warning("这个地址已经在列表里了"), cx);
            }
            AddOutcome::Added => {
                self.input
                    .update(cx, |state, cx| state.set_value("", window, cx));
                window.push_notification(Notification::success(format!("已添加「{name}」")), cx);
            }
        }
    }

    fn run_now(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.runner.read(cx).phase().is_running() {
            window.push_notification(Notification::info("已经在运行了"), cx);
            return;
        }
        self.runner.update(cx, |runner, cx| runner.run_now(cx));
    }

    fn render_status(&self, palette: &Palette, cx: &Context<Self>) -> AnyElement {
        let runner = self.runner.read(cx);

        let (dot_color, headline) = match runner.phase() {
            RunPhase::Running { step } => (palette.primary, step.clone()),
            RunPhase::Idle => match runner.last_finished_at() {
                Some(finished_at) => (
                    palette.success,
                    format!(
                        "空闲 · 上次运行 {}",
                        format_relative(finished_at, now_unix())
                    ),
                ),
                None => (palette.muted, "空闲 · 还没运行过".to_string()),
            },
        };
        let last_added = runner.last_added();
        let remaining = runner.rate_limit_remaining();
        let last_error = runner.last_error().map(str::to_string);
        let ran_before = runner.last_finished_at().is_some();
        let next_run = runner.next_run_at(cx);

        v_flex()
            .gap_1()
            .child(
                h_flex()
                    .gap_2()
                    .child(div().size_2().rounded_full().flex_shrink_0().bg(dot_color))
                    .child(div().text_sm().text_color(palette.foreground).child(headline)),
            )
            .when_some(next_run, |this, at| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(palette.muted)
                        .child(format!("下次自动运行：{}", format_local_minute(at))),
                )
            })
            .when(ran_before && last_added > 0, |this| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(palette.muted)
                        .child(format!("上次新增 {last_added} 个仓库")),
                )
            })
            .when_some(remaining, |this, remaining| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(palette.muted)
                        .child(format!("GitHub 接口剩余配额：{remaining} 次/小时")),
                )
            })
            .when_some(last_error, |this, error| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(palette.warning)
                        .child(format!("上次运行有失败项：{error}")),
                )
            })
            .into_any_element()
    }

    /// 渲染一行。回调自己持有 `Library` 句柄，所以这里不需要 `cx`。
    fn render_source_row(&self, row: &SourceRow, palette: &Palette) -> AnyElement {
        let (last_run_text, failed) = row.last_run_text();
        let url = row.url.clone();
        let name = row.name.clone();
        let enabled = row.enabled;

        let switch_id: ElementId = format!("source-enabled:{}", row.url).into();
        let remove_id: ElementId = format!("source-remove:{}", row.url).into();

        v_flex()
            .gap_2()
            .p_3()
            .rounded(palette.radius)
            .border_1()
            .border_color(palette.border)
            .child(
                h_flex()
                    .gap_3()
                    .justify_between()
                    .child(
                        v_flex()
                            .min_w_0()
                            .gap_1()
                            .child(
                                h_flex()
                                    .gap_2()
                                    .child(
                                        div()
                                            .text_sm()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .truncate()
                                            .child(name.clone()),
                                    )
                                    .when(failed, |this| {
                                        this.child(
                                            Tag::danger()
                                                .small()
                                                .child("有失败项")
                                                .into_any_element(),
                                        )
                                    }),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(palette.muted)
                                    .truncate()
                                    .child(url.clone()),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .flex_shrink_0()
                            .child(
                                Switch::new(switch_id)
                                    .checked(enabled)
                                    .tooltip(if enabled { "已启用" } else { "已停用" })
                                    .on_change({
                                        let library = self.library.clone();
                                        let url = url.clone();
                                        move |checked, _, cx| {
                                            library.update(cx, |library, cx| {
                                                library.set_source_enabled(&url, *checked);
                                                library.persist();
                                                cx.notify();
                                            });
                                        }
                                    }),
                            )
                            .child({
                                let library = self.library.clone();
                                let url = url.clone();
                                let name = name.clone();
                                Button::new(remove_id)
                                    .icon(IconName::Trash)
                                    .ghost()
                                    .small()
                                    .tooltip("移除这个发现地址")
                                    .on_click(move |_, window, cx| {
                                        let removed = library.update(cx, |library, cx| {
                                            let removed = library.remove_source(&url);
                                            if removed {
                                                library.persist();
                                                cx.notify();
                                            }
                                            removed
                                        });
                                        if removed {
                                            window.push_notification(
                                                Notification::success(format!(
                                                    "已移除「{name}」，需要的话重新添加同一个地址即可"
                                                )),
                                                cx,
                                            );
                                        }
                                    })
                            }),
                    ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(if failed { palette.warning } else { palette.muted })
                    .child(last_run_text),
            )
            .into_any_element()
    }
}

impl Render for SourcesView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = Palette::of(cx);

        let rows: Vec<SourceRow> = self
            .library
            .read(cx)
            .sources()
            .iter()
            .map(|source| SourceRow {
                name: source.name.clone(),
                url: source.url.clone(),
                enabled: source.enabled,
                last_run: source.last_run.clone(),
            })
            .collect();

        let enabled_count = rows.iter().filter(|row| row.enabled).count();
        let status = self.render_status(&palette, cx);
        let source_rows: Vec<AnyElement> = rows
            .iter()
            .map(|row| self.render_source_row(row, &palette))
            .collect();

        v_flex()
            .size_full()
            .min_w_0()
            .bg(palette.background)
            .text_color(palette.foreground)
            .child(
                v_flex()
                    .gap_3()
                    .p_6()
                    .border_b_1()
                    .border_color(palette.border)
                    .child(
                        h_flex()
                            .gap_3()
                            .justify_between()
                            .child(
                                div()
                                    .text_lg()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("仓库发现"),
                            )
                            .child(
                                Button::new("run-discovery-now")
                                    .icon(IconName::Play)
                                    .label("立即运行")
                                    .primary()
                                    .disabled(enabled_count == 0)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.run_now(window, cx);
                                    })),
                            ),
                    )
                    .child(status),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .gap_4()
                    .p_6()
                    // 滚动方法在 `Stateful<Div>` 上，先给容器一个稳定 id。
                    .id("sources-scroll")
                    .overflow_y_scroll()
                    .child(
                        v_flex()
                            .gap_2()
                            .child(
                                h_flex()
                                    .gap_2()
                                    .child(
                                        div()
                                            .text_sm()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .child("发现地址"),
                                    )
                                    .child(div().text_xs().text_color(palette.muted).child(format!(
                                        "{} 个，{} 个启用中",
                                        rows.len(),
                                        enabled_count
                                    ))),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(palette.muted)
                                    .child(
                                        "GitHub Trending 的查询参数决定榜单口径（since、language、\
                                         spoken_language_code），地址怎么填就怎么抓。",
                                    ),
                            )
                            .child(
                                h_flex()
                                    .gap_2()
                                    .child(div().flex_1().min_w_0().child(Input::new(&self.input)))
                                    .child(
                                        Button::new("add-source")
                                            .icon(IconName::Plus)
                                            .label("添加")
                                            .outline()
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.add_source(window, cx);
                                            })),
                                    ),
                            ),
                    )
                    .children(source_rows)
                    .when(rows.is_empty(), |this| {
                        this.child(
                            v_flex()
                                .gap_1()
                                .p_4()
                                .rounded(palette.radius)
                                .border_1()
                                .border_color(palette.border)
                                .child(div().text_sm().child("还没有发现地址"))
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(palette.muted)
                                        .child("在上面粘一个 GitHub Trending 地址就能开始"),
                                ),
                        )
                    }),
            )
    }
}

/// 把一个发现地址变成人看得懂的默认名称。
///
/// 只有 `since` 与 `language` / `spoken_language_code` 会改变榜单口径，
/// 所以名称只从这两处取；其它参数不影响「这是什么榜单」。
fn suggest_name(url: &str) -> String {
    let query = url.split('?').nth(1).unwrap_or_default();
    let mut since = None;
    let mut language = None;
    for pair in query.split('&') {
        let Some((key, value)) = pair.split_once('=') else {
            continue;
        };
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        match key {
            "since" => since = Some(value.to_string()),
            "language" | "spoken_language_code" => language = Some(value.to_string()),
            _ => {}
        }
    }

    let window = match since.as_deref() {
        Some("daily") => "日榜",
        Some("monthly") => "月榜",
        _ => "周榜",
    };
    match language {
        Some(language) => format!("{window} · {language}"),
        None => format!("{window} · 全部语言"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // `use gpui_kit::*` 会把 GPUI 的 `#[gpui::test]` 也带进来，把 std 的
    // `#[test]` 盖掉；显式再导一次标准 prelude 里的那个，恢复原语义。
    use core::prelude::v1::test;

    #[test]
    fn name_reads_the_window_and_language() {
        assert_eq!(
            suggest_name("https://github.com/trending?since=weekly&spoken_language_code="),
            "周榜 · 全部语言"
        );
        assert_eq!(
            suggest_name("https://github.com/trending?since=daily&spoken_language_code=rust"),
            "日榜 · rust"
        );
        assert_eq!(
            suggest_name("https://github.com/trending?since=monthly&language=go"),
            "月榜 · go"
        );
    }

    #[test]
    fn name_ignores_parameters_that_do_not_change_the_ranking() {
        assert_eq!(
            suggest_name("https://github.com/trending?since=weekly&spoken_language_code=rust&foo=1"),
            "周榜 · rust"
        );
    }

    #[test]
    fn last_run_text_reports_failures() {
        let row = SourceRow {
            name: "x".into(),
            url: "u".into(),
            enabled: true,
            last_run: Some(SourceRun {
                started_at: 0,
                finished_at: now_unix(),
                found: 25,
                added: 3,
                skipped: 22,
                error: Some("HTTP 403".to_string()),
            }),
        };
        let (text, failed) = row.last_run_text();
        assert!(failed);
        assert!(text.contains("新增 3 条"));
        assert!(text.contains("HTTP 403"));
    }

    #[test]
    fn last_run_text_handles_the_never_run_case() {
        let row = SourceRow {
            name: "x".into(),
            url: "u".into(),
            enabled: true,
            last_run: None,
        };
        assert_eq!(row.last_run_text(), ("还没运行过".to_string(), false));
    }
}

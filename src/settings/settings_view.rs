//! 设置页：自动发现的开关与参数。
//!
//! 开关类配置改完立即生效；需要打字的三项（间隔、批次、令牌）配一个显式的
//! 「应用」，并在旁边显示**当前生效值**——否则用户改完数字、忘了应用，
//! 界面上看不出差别。

use gpui_kit::assets::IconName;
use gpui_kit::component::{
    ActiveTheme as _, Icon, WindowExt as _, h_flex, v_flex,
    button::Button,
    input::{Input, InputEvent, InputState, NumberInput},
    notification::Notification,
    switch::Switch,
};
use gpui_kit::prelude::*;
use gpui_kit::*;

use crate::discovery::model::DiscoverySettings;
use crate::library::{Library, SaveState};

/// 打字类配置的提交入口。三个字段共用一个方法，校验规则也集中在一处。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Field {
    Interval,
    Batch,
    Token,
}

/// 令牌输入的统一清洗：全空白等同于「清除」。
fn clean_token(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// 一块带边框的设置面板。
fn panel(radius: Pixels, border: Hsla, child: impl IntoElement) -> impl IntoElement {
    v_flex()
        .gap_3()
        .p_4()
        .rounded(radius)
        .border_1()
        .border_color(border)
        .child(child)
}

/// 面板标题 + 说明。
fn heading(title: &str, hint: &str, muted: Hsla) -> impl IntoElement {
    v_flex()
        .gap_1()
        .child(
            div()
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .child(title.to_string()),
        )
        .child(div().text_xs().text_color(muted).child(hint.to_string()))
}

pub struct SettingsView {
    library: Entity<Library>,
    interval: Entity<InputState>,
    batch: Entity<InputState>,
    token: Entity<InputState>,
    _subscriptions: Vec<Subscription>,
}

impl SettingsView {
    pub fn new(library: Entity<Library>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let settings = library.read(cx).settings().clone();

        let interval = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(settings.effective_interval_minutes().to_string())
                .submit_on_enter(true)
        });
        let batch = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(settings.metadata_batch.to_string())
                .submit_on_enter(true)
        });
        let token = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder("ghp_…（可选）")
                .default_value(settings.token().unwrap_or_default())
                .submit_on_enter(true)
        });

        let mut subscriptions = Vec::new();
        subscriptions.push(cx.observe(&library, |_, _, cx| cx.notify()));
        for (field, state) in [
            (Field::Interval, interval.clone()),
            (Field::Batch, batch.clone()),
            (Field::Token, token.clone()),
        ] {
            subscriptions.push(cx.subscribe_in(
                &state,
                window,
                move |this, _, event, window, cx| {
                    if matches!(event, InputEvent::PressEnter { .. }) {
                        this.apply(field, window, cx);
                    }
                },
            ));
        }

        Self {
            library,
            interval,
            batch,
            token,
            _subscriptions: subscriptions,
        }
    }

    fn mutate(&mut self, f: impl FnOnce(&mut DiscoverySettings), cx: &mut Context<Self>) {
        self.library.update(cx, |library, cx| {
            let mut settings = library.settings().clone();
            f(&mut settings);
            library.update_settings(settings);
            cx.notify();
        });
    }

    /// 把输入框里的文字提交成配置。解析不了就明说，不动现有配置。
    fn apply(&mut self, field: Field, window: &mut Window, cx: &mut Context<Self>) {
        match field {
            Field::Interval => {
                let raw = self.interval.read(cx).value().to_string();
                match raw.trim().parse::<u32>() {
                    Ok(minutes) if minutes >= 1 => {
                        self.mutate(|settings| settings.interval_minutes = minutes, cx);
                        // 合法写法很多（"007"、" 10 "），回显统一成生效值。
                        let effective = self.library.read(cx).settings().effective_interval_minutes();
                        self.interval.update(cx, |state, cx| {
                            state.set_value(effective.to_string(), window, cx)
                        });
                        window.push_notification(
                            Notification::success(format!("发现间隔已设为 {effective} 分钟")),
                            cx,
                        );
                    }
                    _ => window.push_notification(
                        Notification::error("间隔需要是不小于 1 的整数（分钟）"),
                        cx,
                    ),
                }
            }
            Field::Batch => {
                let raw = self.batch.read(cx).value().to_string();
                match raw.trim().parse::<u32>() {
                    Ok(batch) => {
                        self.mutate(|settings| settings.metadata_batch = batch, cx);
                        let effective = self.library.read(cx).settings().metadata_batch;
                        self.batch.update(cx, |state, cx| {
                            state.set_value(effective.to_string(), window, cx)
                        });
                        let note = if effective == 0 {
                            "批次设为 0：不再自动补全元数据".to_string()
                        } else {
                            format!("每轮最多补全 {effective} 个仓库")
                        };
                        window.push_notification(Notification::success(note), cx);
                    }
                    Err(_) => window.push_notification(
                        Notification::error("批次需要是整数，0 表示不自动补全"),
                        cx,
                    ),
                }
            }
            Field::Token => {
                let raw = self.token.read(cx).value().to_string();
                let cleaned = clean_token(&raw);
                let present = cleaned.is_some();
                self.mutate(|settings| settings.github_token = cleaned, cx);
                // 遮罩字段回显原文字没有意义，清空才是「已保存」的准确反馈。
                self.token
                    .update(cx, |state, cx| state.set_value("", window, cx));
                let note = if present {
                    "令牌已保存，接口配额提升到 5000 次/小时"
                } else {
                    "令牌已清除，回到未认证的 60 次/小时"
                };
                window.push_notification(Notification::success(note), cx);
            }
        }
    }
}

impl Render for SettingsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (background, foreground, muted, border, radius, warning) = {
            let theme = cx.theme();
            (
                theme.background,
                theme.foreground,
                theme.muted_foreground,
                theme.border,
                theme.radius,
                theme.warning,
            )
        };

        let (
            auto_discover,
            interval_minutes,
            batch,
            has_token,
            repo_count,
            source_count,
            path,
            save_state,
        ) = {
            let library = self.library.read(cx);
            let settings = library.settings();
            (
                settings.auto_discover,
                settings.effective_interval_minutes(),
                settings.metadata_batch,
                settings.token().is_some(),
                library.repo_count(),
                library.sources().len(),
                library.path().display().to_string(),
                library.save_state().clone(),
            )
        };

        let save_text = match &save_state {
            SaveState::Untouched => "配置尚未写入磁盘".to_string(),
            SaveState::Saved => "配置已写入磁盘".to_string(),
            SaveState::Failed(error) => format!("配置写入失败：{error}"),
        };
        let save_failed = matches!(save_state, SaveState::Failed(_));

        let auto_panel = panel(
            radius,
            border,
            v_flex()
                .gap_2()
                .child(
                    h_flex()
                        .gap_3()
                        .justify_between()
                        .child(heading(
                            "自动发现",
                            "开着的时候，后台按下面的间隔抓取所有启用的发现地址；关掉只保留手动运行。",
                            muted,
                        ))
                        .child({
                            let library = self.library.clone();
                            Switch::new("auto-discover")
                                .checked(auto_discover)
                                .on_change(move |checked, _, cx| {
                                    library.update(cx, |library, cx| {
                                        let mut settings = library.settings().clone();
                                        settings.auto_discover = *checked;
                                        library.update_settings(settings);
                                        cx.notify();
                                    });
                                })
                        }),
                ),
        );

        let interval_panel = panel(
            radius,
            border,
            v_flex()
                .gap_2()
                .child(heading("发现间隔", "单位分钟，最小 1 分钟。", muted))
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            div().w(px(180.)).child(
                                NumberInput::new(&self.interval)
                                    .suffix(div().text_xs().text_color(muted).child("分钟")),
                            ),
                        )
                        .child(
                            Button::new("apply-interval")
                                .label("应用")
                                .outline()
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.apply(Field::Interval, window, cx);
                                })),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(muted)
                                .child(format!("当前生效：{interval_minutes} 分钟")),
                        ),
                ),
        );

        let batch_panel = panel(
            radius,
            border,
            v_flex()
                .gap_2()
                .child(heading(
                    "每轮补全的仓库数",
                    "GitHub 未认证接口每小时只有 60 次请求，积压的元数据要分批消化。0 表示不自动补全。",
                    muted,
                ))
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            div().w(px(180.)).child(
                                NumberInput::new(&self.batch)
                                    .suffix(div().text_xs().text_color(muted).child("个")),
                            ),
                        )
                        .child(
                            Button::new("apply-batch")
                                .label("应用")
                                .outline()
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.apply(Field::Batch, window, cx);
                                })),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(muted)
                                .child(format!("当前生效：{batch} 个")),
                        ),
                ),
        );

        let token_panel = panel(
            radius,
            border,
            v_flex()
                .gap_2()
                .child(heading(
                    "GitHub 访问令牌",
                    "可选。填了之后接口配额从 60 次/小时提到 5000；令牌只存在本地数据库里。",
                    muted,
                ))
                .child(
                    h_flex()
                        .gap_2()
                        .child(div().flex_1().min_w_0().child(Input::new(&self.token)))
                        .child(
                            Button::new("apply-token")
                                .label("保存")
                                .outline()
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.apply(Field::Token, window, cx);
                                })),
                        ),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child(if has_token {
                            "当前：已设置。清空输入框再保存即可移除。"
                        } else {
                            "当前：未设置。"
                        }),
                ),
        );

        let data_panel = panel(
            radius,
            border,
            v_flex()
                .gap_2()
                .child(heading(
                    "数据",
                    "仓库、发现源与这里的配置都在这一个 JSON 文件里。",
                    muted,
                ))
                .child(
                    v_flex()
                        .gap_1()
                        .child(
                            div()
                                .text_xs()
                                .text_color(muted)
                                .child(format!("{repo_count} 个仓库 · {source_count} 个发现地址")),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(muted)
                                .truncate()
                                .child(format!("数据库：{path}")),
                        ),
                ),
        );

        v_flex()
            .size_full()
            .min_w_0()
            .bg(background)
            .text_color(foreground)
            .child(
                v_flex()
                    .gap_1()
                    .p_6()
                    .border_b_1()
                    .border_color(border)
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                div()
                                    .text_lg()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("设置"),
                            )
                            .child(Icon::new(IconName::Settings).size_4().text_color(muted)),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(if save_failed { warning } else { muted })
                            .child(save_text),
                    ),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .gap_4()
                    .p_6()
                    // 滚动方法在 `Stateful<Div>` 上，先给容器一个稳定 id。
                    .id("settings-scroll")
                    .overflow_y_scroll()
                    .child(auto_panel)
                    .child(interval_panel)
                    .child(batch_panel)
                    .child(token_panel)
                    .child(data_panel),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // `use gpui_kit::*` 会把 GPUI 的 `#[gpui::test]` 也带进来，把 std 的
    // `#[test]` 盖掉；显式再导一次标准 prelude 里的那个，恢复原语义。
    use core::prelude::v1::test;

    #[test]
    fn blank_token_clears_instead_of_storing_whitespace() {
        assert_eq!(clean_token(""), None);
        assert_eq!(clean_token("   "), None);
        assert_eq!(clean_token("  ghp_x  ").as_deref(), Some("ghp_x"));
    }
}

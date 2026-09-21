//! 「添加仓库」对话框。
//!
//! 输入框由 `ReposView` 持有：`open_dialog` 的 builder 每次重绘都会被调用，
//! 在里面 `cx.new` 会每帧造一个新实体、把已输入的内容丢掉；而输入框对回车
//! 事件的订阅也必须活到对话框关闭之后，交给一个长期存在的视图保管最省心。

use gpui_kit::component::{
    ActiveTheme as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    dialog::{DialogAction, DialogClose, DialogFooter},
    input::{Input, InputState},
    notification::Notification,
};
use gpui_kit::prelude::*;
use gpui_kit::*;

use crate::discovery::runner::DiscoveryRunner;
use crate::library::{AddOutcome, Library};
use crate::repos::model::{Repo, parse_full_name};
use crate::shared::clock::now_unix;

pub fn open(
    input: &Entity<InputState>,
    library: &Entity<Library>,
    runner: &Entity<DiscoveryRunner>,
    window: &mut Window,
    cx: &mut App,
) {
    let input = input.clone();
    let library = library.clone();
    let runner = runner.clone();

    window.open_dialog(cx, move |dialog, _window, cx| {
        // `open_dialog` 的 builder 是 `Fn`（每帧都会被调用），所以每次都要重新
        // clone 一份句柄交给 `on_ok`，不能把捕获的变量移进去。
        let library = library.clone();
        let runner = runner.clone();
        let input = input.clone();

        dialog
            .title("添加仓库")
            .child(Input::new(&input))
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("支持 owner/repo、https://github.com/owner/repo 两种写法"),
            )
            .footer(
                DialogFooter::new()
                    .child(
                        DialogClose::new().child(
                            Button::new("cancel-add-repo")
                                .label("取消")
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        ),
                    )
                    .child(
                        DialogAction::new()
                            .child(Button::new("confirm-add-repo").label("添加").primary()),
                    ),
            )
            .on_ok(move |_, window, cx| {
                let raw = input.read(cx).value().to_string();
                submit(&library, &runner, &input, &raw, window, cx)
            })
    });
}

/// 校验并入库。返回 `true` 表示可以关闭对话框。
///
/// 输入不合法时留在对话框里并把原因说出来——用户刚打的字不该被清掉。
pub fn submit(
    library: &Entity<Library>,
    runner: &Entity<DiscoveryRunner>,
    input: &Entity<InputState>,
    raw: &str,
    window: &mut Window,
    cx: &mut App,
) -> bool {
    let Some(full_name) = parse_full_name(raw) else {
        window.push_notification(
            Notification::error("请填成 owner/repo，或直接粘贴仓库地址"),
            cx,
        );
        return false;
    };

    let mut added = false;
    let outcome = library.update(cx, |library, cx| {
        let outcome = library.add_repo(Repo::new(full_name.clone(), None, now_unix()));
        added = outcome == AddOutcome::Added;
        if added {
            cx.notify();
        }
        outcome
    });

    input.update(cx, |state, cx| state.set_value("", window, cx));

    match outcome {
        AddOutcome::AlreadyPresent => {
            window.push_notification(
                Notification::warning(format!("{full_name} 已经在列表里了")),
                cx,
            );
        }
        AddOutcome::Added => {
            // 手工添加的仓库没有描述和创建时间，顺手拉一次；
            // 后台任务正忙时交给后续周期补，并把这件事说出来。
            let started = runner
                .clone()
                .update(cx, |runner, cx| runner.refresh_metadata_now(vec![full_name.clone()], cx));

            let message = if started {
                format!("已添加 {full_name}，正在获取仓库信息")
            } else {
                format!("已添加 {full_name}，仓库信息稍后补齐")
            };
            window.push_notification(Notification::success(message), cx);
        }
    }
    true
}

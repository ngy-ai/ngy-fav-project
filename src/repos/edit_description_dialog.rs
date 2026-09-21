//! 「编辑汉语描述」对话框。

use gpui_kit::component::{
    WindowExt as _,
    button::{Button, ButtonVariants as _},
    dialog::{DialogAction, DialogClose, DialogFooter},
    input::{Textarea, TextareaState},
    notification::Notification,
};
use gpui_kit::prelude::*;
use gpui_kit::*;

use crate::library::Library;
use crate::repos::RepoId;

/// 打开编辑框。`text` 是当前已有的汉语描述，用于预填。
pub fn open(
    library: Entity<Library>,
    id: RepoId,
    full_name: String,
    text: Option<String>,
    window: &mut Window,
    cx: &mut App,
) {
    let editor = cx.new(|cx| {
        TextareaState::new(window, cx)
            .placeholder("用自己的话记一句，留空则回到显示原始描述")
            .default_value(text.unwrap_or_default())
    });

    window.open_dialog(cx, move |dialog, _, _| {
        let library = library.clone();
        let editor = editor.clone();
        let id = id.clone();
        let full_name = full_name.clone();

        dialog
            .title(format!("{full_name} · 汉语描述"))
            .w(px(520.))
            .child(Textarea::new(&editor).h(px(140.)))
            .footer(
                DialogFooter::new()
                    .child(
                        DialogClose::new().child(
                            Button::new("cancel-edit-description")
                                .label("取消")
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        ),
                    )
                    .child(
                        DialogAction::new()
                            .child(Button::new("confirm-edit-description").label("保存").primary()),
                    ),
            )
            .on_ok(move |_, window, cx| {
                let text = editor.read(cx).value().to_string();
                let changed = library.update(cx, |library, cx| {
                    let changed = library.set_description_zh(&id, Some(text));
                    if changed {
                        cx.notify();
                    }
                    changed
                });
                if changed {
                    window.push_notification(Notification::success("汉语描述已保存"), cx);
                }
                true
            })
    });
}

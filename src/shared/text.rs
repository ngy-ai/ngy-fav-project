//! 可以选中、可以复制的文本。
//!
//! 界面上的字默认只是画出来的一块颜色：鼠标划不过去，`Ctrl+C` 也没有可操作的
//! 对象。而这个程序里到处是要抄走的字符串——仓库全名、分类路径、发现地址、
//! 数据库放在哪——所以内容性的文本都走这里。
//!
//! 底下用的是 `gpui-base` 的窗口级选中：它把每一段文本登记成一个「可选中片段」，
//! 窗口外壳（`Root`）负责画选中底色、处理 `Ctrl+C`。这里只负责给每个片段一个
//! 安稳的身份。

use gpui_kit::base::SelectableText;
use gpui_kit::*;

/// 一段能划选、能复制的文本。
///
/// 身份取**代码位置**：同一处代码画出来的字换了（仓库改了名、路径变了），元素
/// 还是同一个，选中不会莫名其妙跳到别处；不同的调用位置又天然拿到不同的身份，
/// 不必给每一处现编一个名字。
///
/// 同一个代码位置、同一个父元素下要渲染出**多份**时（循环、列表行）不能用它，
/// 见 [`copyable_as`]。
#[track_caller]
pub fn copyable(text: impl Into<SharedString>) -> SelectableText {
    SelectableText::new(
        ElementId::CodeLocation(*core::panic::Location::caller()),
        text,
    )
}

/// 显式指定身份的版本。
///
/// 用在同一处代码会渲染出多份的地方：身份里带上那一份自己的东西（仓库名、
/// 地址、分类路径），两份才不会认成同一个元素。
pub fn copyable_as(id: impl Into<ElementId>, text: impl Into<SharedString>) -> SelectableText {
    SelectableText::new(id, text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;
    use std::cell::Cell;
    use std::rc::Rc;

    use gpui_kit::TestAppContext;
    use gpui_kit::test::TestWindowExt as _;

    /// 一个最小界面：一整行能点，行里那段字能选。真实界面里这两件事经常挤在
    /// 同一处（下拉框的每一行、仓库列表的每一行），所以这里就按那个形状摆。
    struct Row {
        clicks: Rc<Cell<usize>>,
    }

    impl Render for Row {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let clicks = self.clicks.clone();
            div()
                .id("row")
                .test_support()
                .on_click(move |_, _, _| clicks.set(clicks.get() + 1))
                .child(
                    div()
                        .id("row-text")
                        .test_support()
                        .child(copyable("仓库名 owner/repo")),
                )
        }
    }

    /// 这条测试是给一处**曾经的想当然**兜底的：当时以为「可选中的文本会插一层
    /// hitbox，把外层的点击吃掉」，所以按钮、列表行里一律不敢用可复制的文本。
    /// 实际不是——`SelectableText` 的 hitbox 是普通行为（`HitboxBehavior::Normal`），
    /// gpui 的命中测试会把包含指针的每一层 hitbox 都算进来，父级的
    /// `hitbox.is_hovered()` 照样为真，点击照常冒泡上去。
    ///
    /// 于是「点名字 = 触发整行」和「划名字 = 选中文字」可以同时成立。
    #[gpui_kit::test]
    fn clicking_text_still_reaches_the_row_around_it(cx: &mut TestAppContext) {
        let clicks = Rc::new(Cell::new(0));
        let handle = cx.add_window(|_, _| Row {
            clicks: clicks.clone(),
        });

        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            // 点的是**文字那一块**的中心，不是行里的空白处。
            window.click("row-text", cx);
        })
        .unwrap();

        assert_eq!(
            clicks.get(),
            1,
            "点在可选中的文字上，外层的点击照样要收到"
        );
    }
}

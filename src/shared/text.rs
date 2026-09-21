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

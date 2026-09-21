//! 分类与标签的小色块。
//!
//! 分类按名字取一个稳定的色相——同一个分类在详情、列表、管理页里永远是同一个
//! 颜色，认颜色比读字快。标签统一走中性色：它出现的数量比分类多得多，各自上色
//! 只会把界面吵成一片。
//!
//! 色块里的字是可以选中复制的（`copyable_as`），所以要带一个身份进来：这几个
//! 函数是共用的，同一个位置会被渲染很多份，光靠代码位置分不开。

use gpui_kit::component::{Sizable as _, tag::Tag};
use gpui_kit::prelude::*;
use gpui_kit::*;

use crate::shared::text::copyable_as;
use crate::taxonomy::model::hue_seed;

/// 分类色块的三种颜色：底色、字色、描边。
///
/// 浅色主题要淡底深字，深色主题反过来；两边都从同一个色相出发，
/// 所以换了皮肤也还是「那个颜色」。
pub fn category_colors(name: &str, dark: bool) -> (Hsla, Hsla, Hsla) {
    let hue = f32::from(hue_seed(name)) / 360.;
    if dark {
        (
            hsla(hue, 0.32, 0.24, 1.),
            hsla(hue, 0.66, 0.82, 1.),
            hsla(hue, 0.30, 0.36, 1.),
        )
    } else {
        (
            hsla(hue, 0.62, 0.93, 1.),
            hsla(hue, 0.72, 0.30, 1.),
            hsla(hue, 0.48, 0.84, 1.),
        )
    }
}

/// 分类色块，显示完整路径。
///
/// 颜色取自**整条路径**而不是末级名字：`开发/前端` 和 `工具/前端` 是两个分类，
/// 不该长得一模一样。
pub fn category_tag(id: impl Into<ElementId>, name: &str, dark: bool) -> Tag {
    let (background, foreground, border) = category_colors(name, dark);
    Tag::custom(background, foreground, border)
        .small()
        .rounded_full()
        .child(copyable_as(id, name.to_string()))
}

/// 分类色块，只显示末级名字。
///
/// `path` 决定颜色——`开发/前端` 与 `工具/前端` 是两个分类，不该长得一模一样；
/// `leaf` 决定写什么字。列表行很窄，画不下整条路径。
pub fn category_leaf_tag(id: impl Into<ElementId>, path: &str, leaf: &str, dark: bool) -> Tag {
    let (background, foreground, border) = category_colors(path, dark);
    Tag::custom(background, foreground, border)
        .small()
        .rounded_full()
        .child(copyable_as(id, leaf.to_string()))
}

/// 标签色块。前面带 `#`，一眼能和分类区分开。
pub fn tag_tag(id: impl Into<ElementId>, name: &str) -> Tag {
    Tag::secondary()
        .small()
        .rounded_full()
        .child(copyable_as(id, format!("#{name}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    // `use gpui_kit::*` 会把 GPUI 的 `#[gpui::test]` 也带进来，把 std 的
    // `#[test]` 盖掉；显式再导一次标准 prelude 里的那个，恢复原语义。
    use core::prelude::v1::test;

    /// 同一个名字换个主题换次重绘，色相不能变——否则颜色会随重绘乱跳。
    #[test]
    fn category_color_follows_the_name() {
        let (light_bg, _, _) = category_colors("AI", false);
        let (dark_bg, _, _) = category_colors("AI", true);

        assert_ne!(light_bg.l, dark_bg.l, "深浅主题应当给出不同的明度");
        assert!((0.0..=1.0).contains(&light_bg.h), "色相必须归一化到 0..1");
        assert!((0.0..=1.0).contains(&light_bg.s));
        assert!((0.0..=1.0).contains(&light_bg.l));

        let (again, _, _) = category_colors("AI", false);
        assert_eq!(light_bg, again);
    }

    #[test]
    fn different_categories_get_different_colors() {
        let (a, _, _) = category_colors("AI", false);
        let (b, _, _) = category_colors("命令行工具", false);
        assert_ne!(a.h, b.h);
    }
}

//! 词表管理页：管词表本身。
//!
//! 一个词表一个页面，`kind` 决定这一页管分类还是标签——两边的规则完全一样，
//! 分开的只是入口。仓库上的归属在仓库页编辑，这里只管「有哪些名字」。改名和
//! 删除都会跟着改到仓库上，所以删除前先把会影响到几个仓库说清楚。

use gpui_kit::assets::IconName;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _, WindowExt as _, h_flex, v_flex,
    button::{Button, ButtonVariants as _},
    dialog::{DialogAction, DialogClose, DialogFooter},
    input::{Input, InputEvent, InputState},
    notification::Notification,
};
use gpui_kit::prelude::*;
use gpui_kit::*;

use crate::library::Library;
use crate::taxonomy::chip;
use crate::taxonomy::model::{self as tax, AddName, MoveName, RenameName, TaxonomyKind};

/// 页面要用到的主题色。`Theme` 借自 `cx`，用之前先抄成值，
/// 免得后面还要 `&mut cx` 时和它撞上。
#[derive(Clone, Copy)]
struct Palette {
    background: Hsla,
    foreground: Hsla,
    muted: Hsla,
    border: Hsla,
    radius: Pixels,
    dark: bool,
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
            dark: theme.is_dark(),
        }
    }
}

/// 词表里的一项：名字 + 有多少仓库在用 + 在树里的位置。
struct NameRow {
    /// 完整路径。分类是 `开发/前端`，标签就是一个名字。
    name: String,
    /// 显示成色块的那个字：分类是末级名字（缩进已经说明了层级），标签就是名字。
    leaf: String,
    /// 用了这个分类／标签的仓库数。分类按**整棵子树**算。
    used_by: usize,
    /// 层级，顶层是 1。缩进按它算；标签恒为 1。
    depth: usize,
    /// 直接子分类数量；标签恒为 0。
    children: usize,
    /// 同级里它前面还有没有兄弟。没有就把「上移」灰掉——按下不会有任何反应，
    /// 不如直接说明没有余地。
    has_prev: bool,
    /// 同级里它后面还有没有兄弟。
    has_next: bool,
}

pub struct TaxonomyView {
    library: Entity<Library>,
    /// 这一页管哪个词表。分类页和标签页是同一种视图的两份实例。
    kind: TaxonomyKind,
    /// 新建名字的输入框。和「添加仓库」同样是长期持有的实体。
    input: Entity<InputState>,
    _subscriptions: Vec<Subscription>,
}

impl TaxonomyView {
    pub fn new(
        library: Entity<Library>,
        kind: TaxonomyKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| {
            let placeholder = if kind.is_hierarchical() {
                // 一个输入框就够：路径里带上上级，缺的上级会自动建。
                "新分类，用 / 分层，如 开发/前端".to_string()
            } else {
                "新标签，回车添加".to_string()
            };
            InputState::new(window, cx)
                .placeholder(placeholder)
                .submit_on_enter(true)
        });

        let mut subscriptions = Vec::new();
        subscriptions.push(cx.observe(&library, |_, _, cx| cx.notify()));
        subscriptions.push(cx.subscribe_in(&input, window, |this, _, event, window, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.add(window, cx);
            }
        }));

        Self {
            library,
            kind,
            input,
            _subscriptions: subscriptions,
        }
    }

    fn add(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let kind = self.kind;
        let input = &self.input;
        let raw = input.read(cx).value().to_string();

        let outcome = self.library.update(cx, |library, cx| {
            let outcome = library.add_name(kind, &raw);
            if outcome == AddName::Added {
                cx.notify();
            }
            outcome
        });

        match outcome {
            AddName::Added => {
                input.update(cx, |state, cx| state.set_value("", window, cx));
                let name = cleaned_for(kind, &raw).unwrap_or_default();
                window.push_notification(
                    Notification::success(format!("已添加{}「{name}」", kind.label())),
                    cx,
                );
            }
            AddName::AlreadyPresent => window.push_notification(
                Notification::warning(format!("已经有这个{}了", kind.label())),
                cx,
            ),
            AddName::Invalid => window.push_notification(Notification::error(invalid_note(kind)), cx),
            AddName::Failed => window.push_notification(Notification::error(write_failed_note()), cx),
        }
    }

    /// 按显示顺序铺平词表。
    ///
    /// 分类的顺序、层级、子分类数都是树的投影，一次 `category_rows` 全拿到；
    /// 标签是扁平的，本身就已经是显示顺序。
    fn rows(&self, cx: &Context<Self>) -> Vec<NameRow> {
        let library = self.library.read(cx);
        let mut rows: Vec<NameRow> = if self.kind.is_hierarchical() {
            library
                .category_rows()
                .into_iter()
                .map(|row| NameRow {
                    name: row.path,
                    leaf: row.leaf,
                    used_by: row.repos,
                    depth: row.depth,
                    children: row.children,
                    has_prev: false,
                    has_next: false,
                })
                .collect()
        } else {
            library
                .tags()
                .iter()
                .map(|tag| NameRow {
                    name: tag.name.clone(),
                    leaf: tag.name.clone(),
                    used_by: library.repo_count_using(TaxonomyKind::Tag, &tag.name),
                    depth: 1,
                    children: 0,
                    has_prev: false,
                    has_next: false,
                })
                .collect()
        };
        mark_siblings(self.kind, &mut rows);
        rows
    }

    fn render_body(&self, palette: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let kind = self.kind;
        let rows = self.rows(cx);
        let add_id: ElementId = match kind {
            TaxonomyKind::Category => "add-category".into(),
            TaxonomyKind::Tag => "add-tag".into(),
        };

        let last = rows.len().saturating_sub(1);
        let count = rows.len();
        let entries: Vec<AnyElement> = rows
            .into_iter()
            .enumerate()
            .map(|(index, row)| self.render_row(row, index == last, palette))
            .collect();

        let table = (!entries.is_empty()).then(|| {
            v_flex()
                .rounded(palette.radius)
                .border_1()
                .border_color(palette.border)
                .children(entries)
                .into_any_element()
        });

        v_flex()
            .gap_3()
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(format!("全部{}", kind.label())),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(palette.muted)
                            .child(format!("{count} 个")),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(div().flex_1().min_w_0().child(Input::new(&self.input)))
                    .child(
                        Button::new(add_id)
                            .icon(IconName::Plus)
                            .label("添加")
                            .outline()
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.add(window, cx);
                            })),
                    ),
            )
            .when_some(table, |this, table| this.child(table))
            .when(count == 0, |this| {
                this.child(
                    v_flex()
                        .gap_1()
                        .p_4()
                        .rounded(palette.radius)
                        .border_1()
                        .border_color(palette.border)
                        .child(div().text_sm().child(format!("还没有{}", kind.label())))
                        .child(
                            div()
                                .text_xs()
                                .text_color(palette.muted)
                                .child(if kind.is_hierarchical() {
                                    "在上面输入一个分类名就能建；写「开发/前端」会把上级一起建上"
                                } else {
                                    "在上面输入一个名字就能建，也可以在仓库详情里直接新建"
                                }),
                        ),
                )
            })
            .into_any_element()
    }

    fn render_row(&self, row: NameRow, is_last: bool, palette: &Palette) -> AnyElement {
        let kind = self.kind;
        let name = row.name.clone();
        let rename_id: ElementId = match kind {
            TaxonomyKind::Category => format!("rename-category:{}", row.name).into(),
            TaxonomyKind::Tag => format!("rename-tag:{}", row.name).into(),
        };
        let delete_id: ElementId = match kind {
            TaxonomyKind::Category => format!("delete-category:{}", row.name).into(),
            TaxonomyKind::Tag => format!("delete-tag:{}", row.name).into(),
        };

        // 层级靠缩进表达，比画线省事，也不会把文字挤走。
        let indent = 12. + (row.depth.saturating_sub(1) as f32) * 18.;
        // 沿用别人的分类来新建下级：上级已经在手边，不用再打一遍路径。
        let add_child_id: ElementId = format!("add-child:{}", row.name).into();

        h_flex()
            .w_full()
            .gap_3()
            .justify_between()
            .pr_3()
            .pl(px(indent))
            .py_2()
            .when(!is_last, |this| this.border_b_1().border_color(palette.border))
            .child(match kind {
                // 缩进已经说明了它是谁的子级，这里只画末级名字；颜色仍取自整条路径。
                // 色块的身份带上整条路径：不同的枝上可以有同名的一级（`开发/前端`
                // 与 `工具/前端`）。
                TaxonomyKind::Category => chip::category_leaf_tag(
                    format!("taxonomy-category:{}", row.name),
                    &row.name,
                    &row.leaf,
                    palette.dark,
                )
                .into_any_element(),
                TaxonomyKind::Tag => {
                    chip::tag_tag(format!("taxonomy-tag:{}", row.leaf), &row.leaf).into_any_element()
                }
            })
            .child(
                h_flex()
                    .gap_1()
                    .flex_shrink_0()
                    // 前后挪一格。位置只影响这一页怎么摆，跟仓库归属无关，
                    // 所以是个随手就能按的小按钮，不像改名那样还要填对话框。
                    .child(anchored(
                        move_id(kind, true, &row.name),
                        Button::new(format!("{}-button", move_id(kind, true, &row.name)))
                            .icon(IconName::ArrowUp)
                            .ghost()
                            .small()
                            .tooltip("在同级里往前挪一格")
                            .disabled(!row.has_prev)
                            .on_click({
                                let library = self.library.clone();
                                let name = name.clone();
                                move |_, window, cx| {
                                    move_name(&library, kind, name.clone(), -1, window, cx);
                                }
                            }),
                    ))
                    .child(anchored(
                        move_id(kind, false, &row.name),
                        Button::new(format!("{}-button", move_id(kind, false, &row.name)))
                            .icon(IconName::ArrowDown)
                            .ghost()
                            .small()
                            .tooltip("在同级里往后挪一格")
                            .disabled(!row.has_next)
                            .on_click({
                                let library = self.library.clone();
                                let name = name.clone();
                                move |_, window, cx| {
                                    move_name(&library, kind, name.clone(), 1, window, cx);
                                }
                            }),
                    ))
                    .when(row.children > 0, |this| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(palette.muted)
                                .child(format!("{} 个子分类", row.children)),
                        )
                    })
                    .child(
                        div()
                            .text_xs()
                            .text_color(palette.muted)
                            .child(if row.used_by == 0 {
                                "暂无仓库".to_string()
                            } else {
                                format!("{} 个仓库", row.used_by)
                            }),
                    )
                    .when(kind.is_hierarchical(), |this| {
                        this.child(
                            Button::new(add_child_id)
                                .icon(IconName::FolderPlus)
                                .ghost()
                                .small()
                                .tooltip("在它下面新建子分类")
                                .on_click({
                                    let library = self.library.clone();
                                    let name = name.clone();
                                    move |_, window, cx| {
                                        open_new_child_dialog(
                                            library.clone(),
                                            name.clone(),
                                            window,
                                            cx,
                                        );
                                    }
                                }),
                        )
                    })
                    .child(
                        Button::new(rename_id)
                            .icon(IconName::Pencil)
                            .ghost()
                            .small()
                            .tooltip(if kind.is_hierarchical() {
                                "改名或改路径（子分类与仓库引用一起走）"
                            } else {
                                "改名（仓库上的引用会一起改）"
                            })
                            .on_click({
                                let library = self.library.clone();
                                let name = name.clone();
                                move |_, window, cx| {
                                    open_rename_dialog(library.clone(), kind, name.clone(), window, cx);
                                }
                            }),
                    )
                    .child(
                        Button::new(delete_id)
                            .icon(IconName::Trash)
                            .ghost()
                            .small()
                            .tooltip("删除")
                            .on_click({
                                let library = self.library.clone();
                                let name = name.clone();
                                let used_by = row.used_by;
                                move |_, window, cx| {
                                    open_delete_dialog(
                                        library.clone(),
                                        kind,
                                        name.clone(),
                                        used_by,
                                        window,
                                        cx,
                                    );
                                }
                            }),
                    ),
            )
            .into_any_element()
    }
}

impl Render for TaxonomyView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = Palette::of(cx);
        let kind = self.kind;
        let count = self.library.read(cx).names(kind).len();
        let body = self.render_body(&palette, cx);
        // 滚动方法在 `Stateful<Div>` 上，先给容器一个稳定 id。
        let scroll_id: ElementId = match kind {
            TaxonomyKind::Category => "category-scroll".into(),
            TaxonomyKind::Tag => "tag-scroll".into(),
        };

        v_flex()
            .size_full()
            .min_w_0()
            .bg(palette.background)
            .text_color(palette.foreground)
            .child(
                v_flex()
                    .gap_2()
                    .p_6()
                    .border_b_1()
                    .border_color(palette.border)
                    .child(
                        div()
                            .text_lg()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(kind.label()),
                    )
                    .child(div().text_xs().text_color(palette.muted).child(page_note(kind)))
                    .child(
                        div()
                            .text_xs()
                            .text_color(palette.muted)
                            .child(format!("{count} 个")),
                    ),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .p_6()
                    .id(scroll_id)
                    .overflow_y_scroll()
                    .child(body),
            )
    }
}

/// 上移/下移按钮的元素 id。带名字不带下标：下标会随着增删错位，
/// 而「按下去要动的那一项」永远是按名字找出来的那一个。
fn move_id(kind: TaxonomyKind, up: bool, name: &str) -> ElementId {
    let what = match (kind, up) {
        (TaxonomyKind::Category, true) => "move-up-category",
        (TaxonomyKind::Category, false) => "move-down-category",
        (TaxonomyKind::Tag, true) => "move-up-tag",
        (TaxonomyKind::Tag, false) => "move-down-tag",
    };
    format!("{what}:{name}").into()
}

/// 给按钮套一层带身份的外壳。
///
/// 只为测试能按 id 点到它：`Button` 这种一次性渲染的组件挂不上 `test_support`，
/// 而外壳是个 `div`，能挂。真实点击照样落在里面的按钮上。
fn anchored(id: ElementId, button: impl IntoElement) -> AnyElement {
    div().id(id).test_support().child(button).into_any_element()
}

/// 标记每一行在同级里还有没有前后邻居，那两个按钮据此决定亮不亮。
///
/// 分类不能只看相邻两行：`开发 / 开发/前端 / 开发/后端` 里「后端」上面那一行是
/// 「前端」的子分类，不是它的兄弟。树序铺平之后，**往上遇到的第一个不比它深的行**
/// 才是它上一个兄弟——更深的是别人家的后代，更浅的是它的上级。
fn mark_siblings(kind: TaxonomyKind, rows: &mut [NameRow]) {
    if !kind.is_hierarchical() {
        // 标签是平的，同级就是全部。
        let last = rows.len().saturating_sub(1);
        for (index, row) in rows.iter_mut().enumerate() {
            row.has_prev = index > 0;
            row.has_next = index < last;
        }
        return;
    }

    // 先整批算出来再写回：算的时候要读整个 `rows`，边算边写会和借用打起来。
    let flags: Vec<(bool, bool)> = (0..rows.len())
        .map(|index| {
            let depth = rows[index].depth;
            let above = rows[..index]
                .iter()
                .rev()
                .find(|row| row.depth <= depth)
                .is_some_and(|row| row.depth == depth);
            let below = rows[index + 1..]
                .iter()
                .find(|row| row.depth <= depth)
                .is_some_and(|row| row.depth == depth);
            (above, below)
        })
        .collect();
    for (row, (has_prev, has_next)) in rows.iter_mut().zip(flags) {
        row.has_prev = has_prev;
        row.has_next = has_next;
    }
}

/// 挪一格，再把结果说给用户听。
fn move_name(
    library: &Entity<Library>,
    kind: TaxonomyKind,
    name: String,
    delta: i64,
    window: &mut Window,
    cx: &mut App,
) {
    let outcome = library.update(cx, |library, cx| {
        let outcome = library.move_name(kind, &name, delta);
        if outcome == MoveName::Moved {
            cx.notify();
        }
        outcome
    });

    let note = match outcome {
        // 位置变了，这一页上直接看得见，不用再弹一句。
        MoveName::Moved => return,
        MoveName::AtEdge => Notification::warning(format!(
            "「{name}」已经在{}了",
            if delta < 0 { "最前面" } else { "最后面" }
        )),
        MoveName::NotFound => Notification::warning(format!("「{name}」已经不在词表里了")),
        MoveName::Failed => Notification::error(write_failed_note()),
    };
    window.push_notification(note, cx);
}

/// 页面头部那句说明。两种词表的区别只在归属规则上，说清楚这一点就够了。
fn page_note(kind: TaxonomyKind) -> &'static str {
    match kind {
        TaxonomyKind::Category => {
            "分类可以分级，用 / 表示层级，如「开发/前端」。一个仓库最多只属于一个分类，\
             选中上级就等于同时属于它下面所有分类；筛选时也按整棵子树算。归属在「仓库」页\
             点「编辑」设置，这里管的是有哪些分类。"
        }
        TaxonomyKind::Tag => {
            "一个仓库可以挂任意多个标签。归属在「仓库」页选中项目后点「编辑」设置；\
             这里管的是有哪些标签。改名与删除会同时改到已经挂上它的仓库上。"
        }
    }
}

/// 改名对话框。名字的预填值是当前的完整路径，回车即提交。
///
/// 分类这里改路径就等于移动整棵子树（`开发/前端` → `工具/前端`），所以「挪到别的
/// 分类底下」不需要另做一个功能。
fn open_rename_dialog(
    library: Entity<Library>,
    kind: TaxonomyKind,
    old: String,
    window: &mut Window,
    cx: &mut App,
) {
    let input = cx.new(|cx| {
        InputState::new(window, cx)
            .default_value(old.clone())
            .submit_on_enter(true)
    });
    let note = if kind.is_hierarchical() {
        format!("当前路径：{old}。改路径等于连同子分类一起挪动。")
    } else {
        "改名之后，已经挂着它的仓库会一起改。".to_string()
    };
    // `Theme` 借自 `cx`，要在造 builder 之前抄成值。
    let muted = cx.theme().muted_foreground;

    window.open_dialog(cx, move |dialog, _, _| {
        let library = library.clone();
        let input = input.clone();
        let old = old.clone();
        let note = note.clone();

        dialog
            .title(format!("改{}名", kind.label()))
            .w(px(460.))
            .child(Input::new(&input))
            .child(div().text_xs().text_color(muted).child(note))
            .footer(
                DialogFooter::new()
                    .child(
                        DialogClose::new().child(
                            Button::new("cancel-rename")
                                .label("取消")
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        ),
                    )
                    .child(
                        DialogAction::new()
                            .child(Button::new("confirm-rename").label("保存").primary()),
                    ),
            )
            .on_ok(move |_, window, cx| {
                let raw = input.read(cx).value().to_string();
                let outcome = library.update(cx, |library, cx| {
                    let outcome = library.rename_name(kind, &old, &raw);
                    if outcome == RenameName::Renamed {
                        cx.notify();
                    }
                    outcome
                });

                match outcome {
                    RenameName::Renamed => {
                        window.push_notification(
                            Notification::success(format!("已改名为「{raw}」")),
                            cx,
                        );
                        true
                    }
                    // 只有空白或大小写变了，算同一个名字，不改也不吵。
                    RenameName::Unchanged => true,
                    RenameName::NameTaken => {
                        window.push_notification(
                            Notification::error(format!("已经有同名的{}了", kind.label())),
                            cx,
                        );
                        false
                    }
                    RenameName::Invalid => {
                        window.push_notification(Notification::error(invalid_note(kind)), cx);
                        false
                    }
                    RenameName::Circular => {
                        window.push_notification(
                            Notification::error("不能把一个分类挪到它自己的下级里"),
                            cx,
                        );
                        false
                    }
                    RenameName::NotFound => {
                        window.push_notification(
                            Notification::error(format!("这个词已经不在{}里了", kind.label())),
                            cx,
                        );
                        true
                    }
                    RenameName::Failed => {
                        window.push_notification(Notification::error(write_failed_note()), cx);
                        // 没写成就别关窗：名字还在框里，用户可以再试一次。
                        false
                    }
                }
            })
    });
}

/// 删除对话框。被仓库用着的时候必须说清后果——那些仓库会跟着变。
fn open_delete_dialog(
    library: Entity<Library>,
    kind: TaxonomyKind,
    name: String,
    used_by: usize,
    window: &mut Window,
    cx: &mut App,
) {
    let consequence = match (kind, used_by) {
        (_, 0) => format!("「{name}」还没有任何仓库在用，直接删掉？"),
        (TaxonomyKind::Category, count) => format!(
            "有 {count} 个仓库属于「{name}」（含它的子分类）。删掉之后它们会变成未分类，\
             仓库本身不会被移除。"
        ),
        (TaxonomyKind::Tag, count) => format!(
            "有 {count} 个仓库挂着「{name}」。删掉之后这个标签会从它们身上摘掉，仓库本身不会被移除。"
        ),
    };

    window.open_dialog(cx, move |dialog, _, _| {
        let library = library.clone();
        let name = name.clone();

        dialog
            .title(format!("删除{}", kind.label()))
            .child(consequence.clone())
            .w(px(440.))
            .footer(
                DialogFooter::new()
                    .child(
                        DialogClose::new().child(
                            Button::new("cancel-delete-name")
                                .label("取消")
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        ),
                    )
                    .child(
                        DialogAction::new()
                            .child(Button::new("confirm-delete-name").label("删除").danger()),
                    ),
            )
            .on_ok(move |_, window, cx| {
                let affected = library.update(cx, |library, cx| {
                    let affected = library.remove_name(kind, &name);
                    cx.notify();
                    affected
                });

                let message = if affected == 0 {
                    format!("已删除「{name}」")
                } else {
                    format!("已删除「{name}」，{affected} 个仓库已更新")
                };
                window.push_notification(Notification::success(message), cx);
                true
            })
    });
}

/// 在某个分类下面新建一个子分类。上级是固定的，所以这里只要末级名字。
///
/// 单独做一个对话框而不是复用顶部的输入框：顶部的框要人自己把整条路径打全，
/// 从树上点下来的路径已经知道上级是谁了，不该再让他补一遍。
fn open_new_child_dialog(
    library: Entity<Library>,
    parent: String,
    window: &mut Window,
    cx: &mut App,
) {
    let input = cx.new(|cx| {
        InputState::new(window, cx)
            .placeholder("子分类名，回车即建")
            .submit_on_enter(true)
    });
    let note = format!("会建在「{parent}」下面，路径是 {parent}/名字。");
    let muted = cx.theme().muted_foreground;

    window.open_dialog(cx, move |dialog, _, _| {
        let library = library.clone();
        let input = input.clone();
        let parent = parent.clone();
        let note = note.clone();

        dialog
            .title("新建子分类")
            .w(px(460.))
            .child(Input::new(&input))
            .child(div().text_xs().text_color(muted).child(note))
            .footer(
                DialogFooter::new()
                    .child(
                        DialogClose::new().child(
                            Button::new("cancel-new-child")
                                .label("取消")
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        ),
                    )
                    .child(
                        DialogAction::new()
                            .child(Button::new("confirm-new-child").label("新建").primary()),
                    ),
            )
            .on_ok(move |_, window, cx| {
                let leaf = input.read(cx).value().to_string();
                let path = tax::join_path(&[parent.clone(), leaf.clone()]);

                let outcome = library.update(cx, |library, cx| {
                    let outcome = library.add_name(TaxonomyKind::Category, &path);
                    if outcome == AddName::Added {
                        cx.notify();
                    }
                    outcome
                });

                match outcome {
                    AddName::Added => {
                        window.push_notification(
                            Notification::success(format!("已添加分类「{path}」")),
                            cx,
                        );
                        true
                    }
                    AddName::AlreadyPresent => {
                        window.push_notification(
                            Notification::warning(format!("「{parent}」下面已经有它了")),
                            cx,
                        );
                        false
                    }
                    AddName::Invalid => {
                        window.push_notification(Notification::error(invalid_note(
                            TaxonomyKind::Category,
                        )), cx);
                        false
                    }
                    AddName::Failed => {
                        window.push_notification(Notification::error(write_failed_note()), cx);
                        false
                    }
                }
            })
    });
}

fn invalid_note(kind: TaxonomyKind) -> String {
    if kind.is_hierarchical() {
        format!(
            "每一级都不能为空，单级不超过 {} 个字，最多 {} 级",
            tax::MAX_NAME_CHARS,
            tax::MAX_DEPTH
        )
    } else {
        format!(
            "{}名不能为空，也不要超过 {} 个字",
            kind.label(),
            tax::MAX_NAME_CHARS
        )
    }
}

/// 写库失败的统一说法。具体原因（磁盘、权限之类）在页面顶部的横幅上，
/// 这里只负责把人指过去——通知一闪就没，说不清那种细节。
fn write_failed_note() -> &'static str {
    "写数据库失败，这次改动没有保存；原因见顶部的提示条"
}

/// 按词表种类收干净输入。和 `Library` 里的规则同源，这里只为把提示文字说对。
fn cleaned_for(kind: TaxonomyKind, raw: &str) -> Option<String> {
    if kind.is_hierarchical() {
        tax::clean_path(raw)
    } else {
        tax::clean_name(raw)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // `gpui_kit::*` 会把 GPUI 的 `#[gpui::test]` 带进来盖掉 std 的 `#[test]`。
    use core::prelude::v1::test;
    use gpui_kit::test::TestWindowExt as _;
    use std::cell::RefCell;
    use std::rc::Rc;

    /// 每次都给一个新的临时目录：固定目录名会让第二次跑读到上一次留下的顺序，
    /// 断言就开始飘。
    fn seeded_library(name: &str) -> Library {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::time::{SystemTime, UNIX_EPOCH};

        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default();
        let dir = std::env::temp_dir().join(format!(
            "ngy-fav-taxonomy-view-{name}-{}-{stamp}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));

        let mut library = Library::load_from(dir.join("library.db"));
        for category in ["工具", "工具/前端", "学习", "学习/书"] {
            library.add_name(TaxonomyKind::Category, category);
        }
        for tag in ["cli", "待读"] {
            library.add_name(TaxonomyKind::Tag, tag);
        }
        library
    }

    /// 窗口的根必须是一个自己实现了 `Render` 的类型（`Entity<T>` 不算），而被测的
    /// 视图要拿到窗口才建得出来。所以套一层壳：壳在窗口里建视图，句柄另存一份。
    struct Shell {
        view: Entity<TaxonomyView>,
    }

    impl Render for Shell {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            self.view.clone()
        }
    }

    /// 这一层只有跑起来才说得清：按钮有没有连上、灰掉的那两个是不是真的按不动、
    /// 按完之后顺序有没有真的落到库里。
    #[gpui_kit::test]
    fn the_move_buttons_reorder_the_tree(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);

        let library = cx.new(|_| seeded_library("move-buttons"));
        let slot: Rc<RefCell<Option<Entity<TaxonomyView>>>> = Rc::new(RefCell::new(None));
        let handle = {
            let slot = slot.clone();
            let library = library.clone();
            cx.add_window(move |window, cx| {
                let view = cx
                    .new(|cx| TaxonomyView::new(library, TaxonomyKind::Category, window, cx));
                *slot.borrow_mut() = Some(view.clone());
                Shell { view }
            })
        };

        let names = |cx: &mut TestAppContext| {
            let view = slot.borrow().clone().expect("窗口已经建好");
            cx.update(|app| {
                view.read(app)
                    .library
                    .read(app)
                    .names(TaxonomyKind::Category)
            })
        };

        assert_eq!(
            names(cx),
            ["工具", "工具/前端", "学习", "学习/书"],
            "一开始是建起来的顺序"
        );

        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            // 已经在最前面的那一项，按钮是灰的：按下去什么都不会发生。
            window.click("move-up-category:工具", cx);
            window.render_frame(cx);
        })
        .unwrap();
        assert_eq!(
            names(cx),
            ["工具", "工具/前端", "学习", "学习/书"],
            "按下灰按钮不该动任何东西"
        );

        cx.update_window(handle.into(), |_, window, cx| {
            window.click("move-down-category:工具", cx);
            window.render_frame(cx);
        })
        .unwrap();
        assert_eq!(
            names(cx),
            ["学习", "学习/书", "工具", "工具/前端"],
            "工具换到学习后面，它的子分类跟着一起走"
        );

        // 子分类只在同级里换：`工具/前端` 是独苗，两个方向都没有余地。
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("move-up-category:工具/前端", cx);
            window.render_frame(cx);
        })
        .unwrap();
        assert_eq!(names(cx), ["学习", "学习/书", "工具", "工具/前端"]);
    }

    /// 标签页是同一份视图的另一个实例，同一套按钮照样管用；顺手确认它挪的是
    /// 自己那张词表，没有把手伸到分类上去。
    #[gpui_kit::test]
    fn the_tags_page_moves_its_own_list(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);

        let library = cx.new(|_| seeded_library("move-tags"));
        let slot: Rc<RefCell<Option<Entity<TaxonomyView>>>> = Rc::new(RefCell::new(None));
        let handle = {
            let slot = slot.clone();
            let library = library.clone();
            cx.add_window(move |window, cx| {
                let view = cx.new(|cx| TaxonomyView::new(library, TaxonomyKind::Tag, window, cx));
                *slot.borrow_mut() = Some(view.clone());
                Shell { view }
            })
        };

        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.click("move-down-tag:cli", cx);
            window.render_frame(cx);
        })
        .unwrap();

        let (tags, categories) = {
            let view = slot.borrow().clone().expect("窗口已经建好");
            cx.update(|app| {
                let library = view.read(app).library.read(app);
                (
                    library.names(TaxonomyKind::Tag),
                    library.names(TaxonomyKind::Category),
                )
            })
        };
        assert_eq!(tags, ["待读", "cli"]);
        assert_eq!(
            categories,
            ["工具", "工具/前端", "学习", "学习/书"],
            "标签页的按钮不该动到分类"
        );
    }

    /// 「上一个兄弟」不是上一行：`开发 / 开发/前端 / 开发/后端` 里「开发/后端」
    /// 上面那一行是「前端」的子分类。这条纯函数就是那两个按钮亮不亮的依据。
    #[test]
    fn siblings_are_found_by_depth_not_by_the_line_above() {
        let row = |name: &str, depth: usize| NameRow {
            name: name.to_string(),
            leaf: name.to_string(),
            used_by: 0,
            depth,
            children: 0,
            has_prev: false,
            has_next: false,
        };
        let mut rows = vec![
            row("开发", 1),
            row("开发/前端", 2),
            row("开发/前端/框架", 3),
            row("开发/后端", 2),
            row("学习", 1),
        ];
        mark_siblings(TaxonomyKind::Category, &mut rows);

        let flags: Vec<(bool, bool)> = rows.iter().map(|row| (row.has_prev, row.has_next)).collect();
        assert_eq!(
            flags,
            [
                (false, true),  // 开发：后面是学习
                (false, true),  // 前端：前面没有，后面是后端
                (false, false), // 框架：独苗
                (true, false),  // 后端：前面是前端（不是上面那行的框架）
                (true, false),  // 学习：前面是开发
            ]
        );
    }
}

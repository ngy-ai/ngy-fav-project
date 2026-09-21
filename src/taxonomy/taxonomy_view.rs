//! 词表管理页：管词表本身。
//!
//! 一个词表一个页面，`kind` 决定这一页管分类还是标签——两边的规则完全一样，
//! 分开的只是入口。仓库上的归属在仓库页编辑，这里只管「有哪些名字」。改名和
//! 删除都会跟着改到仓库上，所以删除前先把会影响到几个仓库说清楚。

use gpui_kit::assets::IconName;
use gpui_kit::component::{
    ActiveTheme as _, Sizable as _, WindowExt as _, h_flex, v_flex,
    button::{Button, ButtonVariants as _},
    dialog::{DialogAction, DialogClose, DialogFooter},
    input::{Input, InputEvent, InputState},
    notification::Notification,
};
use gpui_kit::prelude::*;
use gpui_kit::*;

use crate::library::Library;
use crate::taxonomy::chip;
use crate::taxonomy::model::{self as tax, AddName, RenameName, TaxonomyKind};

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
        if self.kind.is_hierarchical() {
            library
                .category_rows()
                .into_iter()
                .map(|row| NameRow {
                    name: row.path,
                    leaf: row.leaf,
                    used_by: row.repos,
                    depth: row.depth,
                    children: row.children,
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
                })
                .collect()
        }
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

//! 「分类与标签」对话框：给一个仓库定一个分类、挂若干标签。
//!
//! 编辑的是草稿，点「保存」才写回 `Library`——中途反悔直接关窗，不留痕。
//! 草稿自己是个实体：`open_dialog` 的 builder 每帧都会被调用，状态只能放在
//! 实体里，否则每一帧都会被重置。
//!
//! 两边都能当场新建：新建的分类立刻选中，新建的标签立刻挂上。不然想给一个仓库
//! 归到一个还不存在的分类，就得先跑去另一个页面建好再回来。

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
use crate::repos::RepoId;
use crate::taxonomy::model::{self as tax, AddName, TaxonomyKind};

/// 打开编辑框。当前归属从 `Library` 现读，仓库已经不在了就直接不开。
pub fn open(
    library: Entity<Library>,
    repo_id: RepoId,
    full_name: String,
    window: &mut Window,
    cx: &mut App,
) {
    // 仓库上存的是分类节点 id，选框里要比对的是路径——从树上现取一份。
    let Some(assignment) = ({
        let library = library.read(cx);
        library.repo(&repo_id).map(|repo| RepoAssignment {
            repo_id: repo_id.clone(),
            category: library.repo_category_path(&repo_id),
            tags: repo.tags.clone(),
        })
    }) else {
        return;
    };

    let category_input = cx.new(|cx| {
        InputState::new(window, cx)
            .placeholder("新分类，可用 / 分层，如 开发/前端")
            .submit_on_enter(true)
    });
    let tag_input = cx.new(|cx| {
        InputState::new(window, cx)
            .placeholder("新标签，回车新建并挂上")
            .submit_on_enter(true)
    });
    let draft = cx.new(|cx| {
        AssignmentDraft::new(
            library,
            assignment,
            category_input,
            tag_input,
            window,
            cx,
        )
    });

    window.open_dialog(cx, move |dialog, _, _| {
        let draft = draft.clone();
        let on_confirm = draft.clone();

        dialog
            .title(format!("{full_name} · 分类与标签"))
            .w(px(560.))
            .child(draft)
            .footer(
                DialogFooter::new()
                    .child(DialogClose::new().child(
                        Button::new("cancel-taxonomy")
                            .label("取消")
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    ))
                    .child(
                        DialogAction::new()
                            .child(Button::new("confirm-taxonomy").label("保存").primary()),
                    ),
            )
            .on_ok(move |_, window, cx| {
                // `read` 借出的草稿不能同时再借 `cx` 去写盘，所以从 `update` 里走。
                if on_confirm.update(cx, |draft, cx| draft.commit(cx)) {
                    window.push_notification(Notification::success("分类与标签已保存"), cx);
                    true
                } else {
                    // 写着写着仓库被移除了：把窗口留着，让用户自己看清再决定。
                    window.push_notification(
                        Notification::error("保存失败：这个仓库可能已经被移除了"),
                        cx,
                    );
                    false
                }
            })
    });
}

/// 打开对话框时从仓库上抄下来的三样东西：改谁、现在是什么分类、现在挂哪些标签。
///
/// 打包成一条是因为它们总是一起出现——后面再想加「上次改过什么」之类的场景，
/// 也是往这里加一个字段，而不是给 `new` 再加一个参数。
struct RepoAssignment {
    repo_id: RepoId,
    category: Option<String>,
    tags: Vec<String>,
}

/// 编辑中的草稿。
struct AssignmentDraft {
    library: Entity<Library>,
    repo_id: RepoId,
    /// 选中的分类，`None` 表示未分类。
    category: Option<String>,
    tags: Vec<String>,
    category_input: Entity<InputState>,
    tag_input: Entity<InputState>,
    /// 名字不合法时的就地提示。对话框里弹通知会飘到别的窗口层去，不如写在输入框下面。
    category_error: Option<String>,
    tag_error: Option<String>,
    _subscriptions: Vec<Subscription>,
}

impl AssignmentDraft {
    fn new(
        library: Entity<Library>,
        assignment: RepoAssignment,
        category_input: Entity<InputState>,
        tag_input: Entity<InputState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let RepoAssignment {
            repo_id,
            category,
            tags,
        } = assignment;
        let mut subscriptions = Vec::new();
        subscriptions.push(cx.subscribe_in(
            &category_input,
            window,
            |this, _, event, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    this.create_category(window, cx);
                }
            },
        ));
        subscriptions.push(cx.subscribe_in(
            &tag_input,
            window,
            |this, _, event, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    this.create_tag(window, cx);
                }
            },
        ));

        Self {
            library,
            repo_id,
            category,
            tags,
            category_input,
            tag_input,
            category_error: None,
            tag_error: None,
            _subscriptions: subscriptions,
        }
    }

    /// 新建一个分类并直接选中它。
    fn create_category(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let raw = self.category_input.read(cx).value().to_string();
        match self.store_name(TaxonomyKind::Category, &raw, cx) {
            Some(name) => {
                self.category = Some(name);
                self.category_error = None;
                self.clear_input(TaxonomyKind::Category, window, cx);
            }
            None => self.category_error = Some(invalid_name_note()),
        }
        cx.notify();
    }

    /// 新建一个标签并直接挂上。
    fn create_tag(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let raw = self.tag_input.read(cx).value().to_string();
        match self.store_name(TaxonomyKind::Tag, &raw, cx) {
            Some(name) => {
                if !self.tags.iter().any(|tag| tax::same_name(tag, &name)) {
                    self.tags.push(name);
                }
                self.tag_error = None;
                self.clear_input(TaxonomyKind::Tag, window, cx);
            }
            None => self.tag_error = Some(invalid_name_note()),
        }
        cx.notify();
    }

    fn clear_input(&self, kind: TaxonomyKind, window: &mut Window, cx: &mut Context<Self>) {
        let input = match kind {
            TaxonomyKind::Category => &self.category_input,
            TaxonomyKind::Tag => &self.tag_input,
        };
        input.update(cx, |state, cx| state.set_value("", window, cx));
    }

    /// 把一个名字收进词表，返回词表里那份写法。
    ///
    /// 返回 `None` 表示这个名字不能用。词表里已有同名但写法不同时，返回的是词表
    /// 里的写法——名字的身份忽略大小写，认同一份。
    ///
    /// 分类接受 `开发/前端` 这样的路径，缺的上级会一起建上。
    fn store_name(&self, kind: TaxonomyKind, raw: &str, cx: &mut App) -> Option<String> {
        let cleaned = if kind.is_hierarchical() {
            tax::clean_path(raw)?
        } else {
            tax::clean_name(raw)?
        };

        self.library.update(cx, |library, cx| {
            if library.add_name(kind, &cleaned) == AddName::Added {
                cx.notify();
            }
        });

        self.library
            .read(cx)
            .names(kind)
            .iter()
            .find(|existing| tax::same_name(existing, &cleaned))
            .cloned()
    }

    /// 把草稿写回 `Library`。返回 `false` 表示没写成，调用方不要关窗。
    fn commit(&self, cx: &mut App) -> bool {
        self.library.update(cx, |library, cx| {
            let category_ok = library.set_repo_category(&self.repo_id, self.category.as_deref());
            let tags_ok = library.set_repo_tags(&self.repo_id, &self.tags);
            let ok = category_ok && tags_ok;
            if ok {
                cx.notify();
            }
            ok
        })
    }

    fn is_category(&self, name: &str) -> bool {
        self.category
            .as_deref()
            .is_some_and(|current| tax::same_name(current, name))
    }

    fn has_tag(&self, name: &str) -> bool {
        self.tags.iter().any(|tag| tax::same_name(tag, name))
    }

    /// 分类的单选项。`None` 是「不分类」——分类是单选，总得留一条退回去的路。
    fn category_choice(&self, value: Option<String>, cx: &mut Context<Self>) -> AnyElement {
        let (label, active) = match &value {
            Some(name) => (name.clone(), self.is_category(name)),
            None => ("不分类".to_string(), self.category.is_none()),
        };
        let id: ElementId = match &value {
            Some(name) => format!("pick-category:{name}").into(),
            None => "pick-category:none".into(),
        };

        let mut button = Button::new(id).label(label).small();
        button = if active { button.primary() } else { button.outline() };
        button
            .on_click(cx.listener(move |this, _, _, cx| {
                this.category = value.clone();
                cx.notify();
            }))
            .into_any_element()
    }

    /// 标签的多选项，再点一次摘掉。
    fn tag_choice(&self, name: String, cx: &mut Context<Self>) -> AnyElement {
        let active = self.has_tag(&name);
        let id: ElementId = format!("pick-tag:{name}").into();

        let mut button = Button::new(id).label(format!("#{name}")).small();
        button = if active { button.primary() } else { button.outline() };
        button
            .on_click(cx.listener(move |this, _, _, cx| {
                match this.tags.iter().position(|tag| tax::same_name(tag, &name)) {
                    Some(index) => {
                        this.tags.remove(index);
                    }
                    None => this.tags.push(name.clone()),
                }
                cx.notify();
            }))
            .into_any_element()
    }

    fn render_section(
        &self,
        kind: TaxonomyKind,
        placeholder_note: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // 主题借自 `cx`，先抄成值：后面还要拿 `cx` 造回调，借用不能一直挂着。
        let (muted, danger) = {
            let theme = cx.theme();
            (theme.muted_foreground, theme.danger)
        };
        let names: Vec<String> = {
            let library = self.library.read(cx);
            if kind.is_hierarchical() {
                // 按树序摆，父在子前面，扫一眼就知道谁在哪一支下面。
                library
                    .category_rows()
                    .into_iter()
                    .map(|row| row.path)
                    .collect()
            } else {
                library.names(kind).to_vec()
            }
        };
        let has_names = !names.is_empty();

        let (input, error) = match kind {
            TaxonomyKind::Category => (&self.category_input, self.category_error.clone()),
            TaxonomyKind::Tag => (&self.tag_input, self.tag_error.clone()),
        };

        // 分类是单选，所以先摆一个「不分类」；标签没有这个概念。
        let mut chips: Vec<AnyElement> = Vec::new();
        if kind == TaxonomyKind::Category {
            chips.push(self.category_choice(None, cx));
        }
        for name in names {
            chips.push(match kind {
                TaxonomyKind::Category => self.category_choice(Some(name), cx),
                TaxonomyKind::Tag => self.tag_choice(name, cx),
            });
        }

        let create = match kind {
            TaxonomyKind::Category => Button::new("create-category")
                .icon(IconName::Plus)
                .label("新建并选中"),
            TaxonomyKind::Tag => Button::new("create-tag")
                .icon(IconName::Plus)
                .label("新建并挂上"),
        }
        .outline()
        .small()
        .on_click(cx.listener(move |this, _, window, cx| match kind {
            TaxonomyKind::Category => this.create_category(window, cx),
            TaxonomyKind::Tag => this.create_tag(window, cx),
        }));

        v_flex()
            .gap_2()
            .child(
                h_flex()
                    .gap_3()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(kind.label()),
                    )
                    .child(div().text_xs().text_color(muted).child(placeholder_note.to_string())),
            )
            .child(
                h_flex()
                    .flex_wrap()
                    .gap_1()
                    .when(!has_names, |this| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(muted)
                                .child(format!("还没有{}，在下面新建", kind.label())),
                        )
                    })
                    .children(chips),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(div().flex_1().min_w_0().child(Input::new(input).small()))
                    .child(create),
            )
            .when_some(error, |this, error| {
                this.child(div().text_xs().text_color(danger).child(error))
            })
            .into_any_element()
    }
}

impl Render for AssignmentDraft {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let border = cx.theme().border;

        v_flex()
            .gap_4()
            .child(self.render_section(
                TaxonomyKind::Category,
                "只能选一个；选中上级就等于也属于它下面的分类",
                cx,
            ))
            .child(div().h(px(1.)).w_full().bg(border))
            .child(self.render_section(TaxonomyKind::Tag, "标签可以挂多个", cx))
    }
}

fn invalid_name_note() -> String {
    format!("名字不能为空，也不要超过 {} 个字", tax::MAX_NAME_CHARS)
}

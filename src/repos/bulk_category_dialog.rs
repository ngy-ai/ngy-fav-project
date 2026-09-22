//! 「整批改分类」对话框：给勾中的一批仓库换成同一个分类（或者都取消分类）。
//!
//! **只动分类，不碰标签。** 批量挂标签是另一件事，混在一起会让人以为「点了保存，
//! 标签怎么也没了」。
//!
//! 编辑的是草稿，点「保存」才写回 `Library`——中途反悔直接关窗，不留痕。草稿自己
//! 是个实体：`open_dialog` 的 builder 每帧都会被调用，状态只能放在实体里，否则每
//! 一帧都会被重置。
//!
//! 这批是**打开对话框那一刻定下来的**，中途不跟着列表的勾选变：用户在里面选分类的
//! 时候顺手去动了勾选，也仍然是同一批——看着对话框说「给 12 个仓库改分类」，结果改
//! 的是别的 12 个，那才是真的坑。

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

use crate::library::{CategoryRow, Library};
use crate::repos::RepoId;
use crate::shared::text::copyable_as;
use crate::taxonomy::model::{self as tax, AddName, TaxonomyKind};
use crate::taxonomy::picker;

/// 打开对话框。`ids` 一条都没有时什么都不做（没有可改的对象）。
///
/// `on_saved` 在写库成功之后调用一次，让列表把勾选清掉：这批已经处理完了，留着
/// 只会让人以为还没做。写成回调而不是让对话框去认识列表，是因为它本来就不该知道
/// 是谁打开的它。
pub fn open<F>(
    library: Entity<Library>,
    ids: Vec<RepoId>,
    on_saved: F,
    window: &mut Window,
    cx: &mut App,
) where
    F: Fn(&mut App) + Clone + 'static,
{
    if ids.is_empty() {
        return;
    }

    let category_input = cx.new(|cx| {
        InputState::new(window, cx)
            .placeholder("新分类，可用 / 分层，如 开发/前端")
            .submit_on_enter(true)
    });
    let draft = cx.new(|cx| BulkCategoryDraft::new(library, ids, category_input, window, cx));
    let count = draft.read(cx).ids.len();

    window.open_dialog(cx, move |dialog, _, _| {
        let draft = draft.clone();
        let on_confirm = draft.clone();
        let on_saved = on_saved.clone();

        dialog
            .title(format!("给 {count} 个仓库改分类"))
            .w(px(520.))
            .child(draft)
            .footer(
                DialogFooter::new()
                    .child(DialogClose::new().child(
                        Button::new("cancel-bulk-category")
                            .label("取消")
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    ))
                    .child(
                        DialogAction::new().child(
                            Button::new("confirm-bulk-category")
                                .label("保存")
                                .primary(),
                        ),
                    ),
            )
            .on_ok(move |_, window, cx| {
                // `read` 借出的草稿不能同时再借 `cx` 去写盘，所以从 `update` 里走。
                match on_confirm.update(cx, |draft, cx| draft.commit(cx)) {
                    Some(note) => {
                        window.push_notification(Notification::success(note), cx);
                        on_saved(cx);
                        true
                    }
                    None => {
                        // 分类写着写着被删了：把窗口留着，让用户自己看清再决定。
                        window.push_notification(
                            Notification::error("保存失败：这个分类可能已经被删掉了"),
                            cx,
                        );
                        false
                    }
                }
            })
    });
}

/// 交代这批是谁：「a/one、b/two 等 12 个」。
///
/// 全列出来在对话框里放不下，而一个名字都不提又让人不放心——那 12 个到底是哪 12 个。
fn preview(names: &[String]) -> String {
    match names {
        [] => "没有选中任何仓库".to_string(),
        [one] => one.clone(),
        [one, two] => format!("{one}、{two}"),
        [one, two, rest @ ..] => format!("{one}、{two} 等 {} 个", rest.len() + 2),
    }
}

/// 编辑中的草稿。
struct BulkCategoryDraft {
    library: Entity<Library>,
    /// 这次要改的那批仓库。打开时定下来，之后不再变。
    ids: Vec<RepoId>,
    /// 这批是谁，写在对话框里让用户一眼认出。
    preview: String,
    /// 选中的分类，`None` 表示不分类。
    category: Option<String>,
    category_input: Entity<InputState>,
    /// 名字不合法时的就地提示。对话框里弹通知会飘到别的窗口层去，不如写在输入框下面。
    category_error: Option<String>,
    _subscriptions: Vec<Subscription>,
}

impl BulkCategoryDraft {
    fn new(
        library: Entity<Library>,
        ids: Vec<RepoId>,
        category_input: Entity<InputState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        // 库里存的是小写 id，显示要用 GitHub 上的原始写法——逐个取一次，取不到
        // 的（列表上勾完又被删了）就退回 id 本身，别让它在提示里凭空消失。
        let names: Vec<String> = {
            let library = library.read(cx);
            ids.iter()
                .map(|id| {
                    library
                        .repo(id)
                        .map(|repo| repo.full_name.clone())
                        .unwrap_or_else(|| id.clone())
                })
                .collect()
        };

        let subscriptions = vec![cx.subscribe_in(
            &category_input,
            window,
            |this, _, event, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    this.create_category(window, cx);
                }
            },
        )];

        Self {
            library,
            ids,
            preview: preview(&names),
            category: None,
            category_input,
            category_error: None,
            _subscriptions: subscriptions,
        }
    }

    /// 新建一个分类并直接选中它。
    ///
    /// 没有这一条就得先跑去「分类」页建好再回来；而「这批仓库该归到哪」往往正是
    /// 在这一步才想清楚的。
    fn create_category(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let raw = self.category_input.read(cx).value().to_string();
        match self.store_name(&raw, cx) {
            Some(name) => {
                self.category = Some(name);
                self.category_error = None;
                self.category_input
                    .update(cx, |state, cx| state.set_value("", window, cx));
            }
            None => self.category_error = Some(invalid_name_note()),
        }
        cx.notify();
    }

    /// 把一个名字收进词表，返回词表里那份写法；`None` 表示这个名字不能用。
    fn store_name(&self, raw: &str, cx: &mut App) -> Option<String> {
        let cleaned = tax::clean_path(raw)?;

        self.library.update(cx, |library, cx| {
            if library.add_name(TaxonomyKind::Category, &cleaned) == AddName::Added {
                cx.notify();
            }
        });

        self.library
            .read(cx)
            .names(TaxonomyKind::Category)
            .iter()
            .find(|existing| tax::same_name(existing, &cleaned))
            .cloned()
    }

    /// 写回 `Library`。返回要说给用户听的那句话；`None` 表示没写成，调用方不要关窗。
    ///
    /// 消息由草稿自己拼：只有它同时知道选的是哪个分类，以及**真被改到的条数**——
    /// 勾选里混了已经删掉的仓库时，那个数会比勾的少，该说清楚。
    fn commit(&self, cx: &mut App) -> Option<String> {
        let changed = self.library.update(cx, |library, cx| {
            let changed = library.set_repos_category(&self.ids, self.category.as_deref());
            if changed.is_some() {
                cx.notify();
            }
            changed
        })?;

        let target = match self.category.as_deref() {
            Some(name) => format!("「{name}」"),
            None => "「不分类」".to_string(),
        };
        Some(if changed == self.ids.len() {
            format!("已把 {changed} 个仓库改到{target}")
        } else {
            format!(
                "已把 {changed} 个仓库改到{target}；另外 {} 个已经不在库里了",
                self.ids.len() - changed
            )
        })
    }
}

impl Render for BulkCategoryDraft {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (muted, border) = {
            let theme = cx.theme();
            (theme.muted_foreground, theme.border)
        };

        let categories: Vec<CategoryRow> = self.library.read(cx).category_rows();
        let has_names = !categories.is_empty();

        // 分类是单选，所以树最前面有一条「不分类」；选中项没有时亮的就是它。
        let rows = picker::category_choice_rows(categories, false);
        let selected = self
            .category
            .clone()
            .unwrap_or_else(|| picker::NONE_KEY.to_string());
        let draft = cx.entity();
        let chooser = picker::Picker::new(
            "bulk-category",
            IconName::FolderTree,
            self.category
                .clone()
                .unwrap_or_else(|| "不分类".to_string()),
        )
        .rows(rows)
        .selected(vec![selected])
        // 铺满整个对话框：它是个输入控件，不是排在行里的一颗按钮。
        .stretch()
        .on_pick(move |key, _window, cx| {
            // 交出来的是分类路径，唯一要翻译的是那个固定项。
            let value = (key != picker::NONE_KEY).then_some(key);
            draft.update(cx, |this, cx| {
                this.category = value;
                cx.notify();
            });
        })
        .into_element();

        v_flex()
            .gap_3()
            .child(
                v_flex()
                    .gap_1()
                    .child(div().text_sm().child(copyable_as(
                        "bulk-category-target",
                        format!("这次改的是：{}", self.preview),
                    )))
                    .child(div().text_xs().text_color(muted).child(copyable_as(
                        "bulk-category-note",
                        format!(
                            "这 {} 个仓库原来的分类都会被换成所选的这一个；标签不动。",
                            self.ids.len()
                        ),
                    ))),
            )
            .child(div().h(px(1.)).w_full().bg(border))
            .child(
                v_flex()
                    .gap_1()
                    .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child("新分类"))
                    .child(chooser)
                    .when(!has_names, |this| {
                        this.child(div().text_xs().text_color(muted).child(copyable_as(
                            "bulk-category-empty",
                            "还没有分类，在下面新建一个",
                        )))
                    })
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .child(Input::new(&self.category_input).small()),
                            )
                            .child(
                                Button::new("bulk-create-category")
                                    .icon(IconName::Plus)
                                    .label("新建并选中")
                                    .outline()
                                    .small()
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.create_category(window, cx)
                                    })),
                            ),
                    )
                    .when_some(self.category_error.clone(), |this, error| {
                        this.child(div().text_xs().text_color(cx.theme().danger).child(
                            copyable_as("bulk-category-error", error),
                        ))
                    }),
            )
    }
}

fn invalid_name_note() -> String {
    format!("名字不能为空，也不要超过 {} 个字", tax::MAX_NAME_CHARS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;
    use gpui_kit::TestAppContext;
    use gpui_kit::test::TestWindowExt as _;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    /// 一个在临时目录里、带两个仓库和一条分类的库。
    fn seeded_library() -> Library {
        // 同一毫秒里可能建好几个库，光靠时间戳会撞。
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default();
        let dir = std::env::temp_dir().join(format!(
            "ngy-fav-bulk-category-{}-{stamp}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));

        let mut library = Library::load_from(dir.join("library.db"));
        for name in ["a/one", "b/two"] {
            library.add_repo(crate::repos::model::Repo::new(name.to_string(), None, 1));
        }
        library.add_name(TaxonomyKind::Category, "开发/前端");
        library
    }

    #[test]
    fn the_preview_names_a_few_and_counts_the_rest() {
        assert_eq!(preview(&[]), "没有选中任何仓库");
        assert_eq!(preview(&["a/one".to_string()]), "a/one");
        assert_eq!(
            preview(&["a/one".to_string(), "b/two".to_string()]),
            "a/one、b/two"
        );
        assert_eq!(
            preview(&["a/one".to_string(), "b/two".to_string(), "c/three".to_string()]),
            "a/one、b/two 等 3 个",
            "列不下就列两个再报总数，一个名字都不提才让人不放心"
        );
    }

    /// 分类选框长在对话框正文里。这条测的就是那段正文（`BulkCategoryDraft`）：
    /// 点开按钮、点一行、菜单收起，最后看这一下有没有真的落到草稿上——保存时写进
    /// 库里的正是草稿。
    ///
    /// **没有连对话框外壳一起测**：`window.open_dialog` 的正文排在 `Root` 的
    /// deferred 层里，无头窗口按 id 找不到里面的元素。外壳是组件库的事。
    #[gpui_kit::test]
    fn picking_a_category_lands_on_the_draft(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);

        let library = cx.new(|_| seeded_library());
        let (draft, cx) = cx.add_window_view(move |window, cx: &mut Context<BulkCategoryDraft>| {
            let category_input = cx.new(|cx| InputState::new(window, cx));
            BulkCategoryDraft::new(
                library,
                vec!["a/one".to_string(), "b/two".to_string()],
                category_input,
                window,
                cx,
            )
        });

        cx.update(|window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("bulk-category-slot").is_some(), "分类得是个下拉框");
            assert!(
                window.try_find("bulk-category-menu").is_none(),
                "没点之前是收着的"
            );

            window.click("bulk-category-slot", cx);
            window.render_frame(cx);
            assert!(
                window.try_find("bulk-category-menu").is_some(),
                "点一下要展开分类树"
            );

            window.click("bulk-category-row:开发/前端", cx);
            window.render_frame(cx);
            assert!(
                window.try_find("bulk-category-menu").is_none(),
                "选完要自己收起来"
            );
            assert_eq!(
                draft.read(cx).category.as_deref(),
                Some("开发/前端"),
                "选中的分类要落到草稿上，保存时写的就是它"
            );
        });
    }

    /// 保存这一下要真的落库，而且要如实说改到了几个。
    #[gpui_kit::test]
    fn saving_writes_the_whole_batch_and_says_how_many_landed(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);

        let library = cx.new(|_| seeded_library());
        let (draft, cx) = cx.add_window_view({
            let library = library.clone();
            move |window, cx: &mut Context<BulkCategoryDraft>| {
                let category_input = cx.new(|cx| InputState::new(window, cx));
                BulkCategoryDraft::new(
                    library,
                    // 混一个库里没有的 id：它不该让整批失败，只该少改一条。
                    vec![
                        "a/one".to_string(),
                        "b/two".to_string(),
                        "查无此repo".to_string(),
                    ],
                    category_input,
                    window,
                    cx,
                )
            }
        });

        cx.update(|_, cx| {
            draft.update(cx, |draft, cx| {
                draft.category = Some("开发/前端".to_string());
                cx.notify();
            });
        });

        let note = cx.update(|_, cx| {
            draft
                .update(cx, |draft, cx| draft.commit(cx))
                .expect("分类在树上，该写成功")
        });
        assert_eq!(note, "已把 2 个仓库改到「开发/前端」；另外 1 个已经不在库里了");

        cx.update(|_, app| {
            let library = library.read(app);
            assert_eq!(library.repo_category_path("a/one").as_deref(), Some("开发/前端"));
            assert_eq!(library.repo_category_path("b/two").as_deref(), Some("开发/前端"));
        });
    }
}

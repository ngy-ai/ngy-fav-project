//! 下拉选择：收着是一个按钮，点开是一列（分类则是一棵缩进排出来的树）。
//!
//! 两处视图在用：仓库页的三个筛选（分类 / 语言 / 标签），和给仓库改分类时的选框。
//! 各处的数据与「选中项」的含义都不一样（筛选里「全部」「未分类」是固定项，
//! 改分类时只有「不分类」；标签还是一次能挂好几个），所以这一层只管两件事：
//! **把行摆出来**，以及**点一行之后把 key 交出去**。key 是什么意思由调用方解释——
//! 这里既不认识 `CategoryFilter`，也不认识仓库。
//!
//! 为什么分类是缩进的树而不是一排色块：路径写法（`AI Agent/Tools`）在折行的色块里
//! 读不出层级，而分类天生有上下级。所以树里的子分类只写自己那一段名字，层级交给
//! 缩进；全路径只在收起的按钮上出现，那是唯一需要它说清「选的是哪个」的地方。

use std::rc::Rc;

use gpui_kit::assets::IconName;
use gpui_kit::component::{
    ActiveTheme as _, Icon, Sizable as _, h_flex, v_flex,
    button::{Button, ButtonVariants as _},
    popover::Popover,
};
use gpui_kit::prelude::*;
use gpui_kit::*;

use crate::library::CategoryRow;
use crate::shared::text::copyable_as;

/// 下拉里的一行。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PickerRow {
    /// 这一行的身份，也是选中之后交出去的 key。**用名字不用下标**——分类增删
    /// 之后下标会错位，而下拉里点的永远是「当帧看到的那一行」。
    pub key: String,
    /// 显示名。子分类只写自己那一段：层级由缩进表达，路径写在行里是废话。
    pub label: String,
    /// 缩进层数，0 是顶层。分类最多 6 级，这个数不会大。
    pub depth: usize,
    /// 行右边的数字（筛选时是仓库数）。`None` 就不画。
    pub count: Option<usize>,
    /// 点这一行之后要不要把下拉收起来。
    ///
    /// 单选（分类、语言）一路收起；而标签一次能挂好几个，点一个收一次就没法连着
    /// 点了——所以多选的那些行留 `false`，只有「不限」这种收尾的行留 `true`。
    pub close: bool,
}

impl PickerRow {
    /// 摆在最前面的固定项：「全部」「未分类」「不限」。
    pub fn fixed(key: impl Into<String>, label: impl Into<String>, count: Option<usize>) -> Self {
        Self {
            key: key.into(),
            label: label.into(),
            depth: 0,
            count,
            close: true,
        }
    }

    /// 多选里的一行：点完留着，接着点下一个。
    pub fn keeping_open(mut self) -> Self {
        self.close = false;
        self
    }
}

/// 把分类树摊成行。顺序沿用 `CategoryRow` 的顺序（父在子前），**不做二次排序**——
/// 父子关系就是靠这个顺序读出来的，重排会把它们打散。
///
/// `show_count` 决定要不要带仓库数：筛选时那个数字就是「点下去会看到几条」，
/// 改分类时它跟这次操作毫无关系，摆着只会让人误以为在选筛选条件。
///
/// 空分类**也留着**：下拉框收起来不占地方，而一棵缺了几个节点的树看着像坏了；
/// 点进一个空分类看到空列表，正是「这个分类还没用上」最直观的回答。
pub fn category_rows(categories: Vec<CategoryRow>, show_count: bool) -> Vec<PickerRow> {
    categories
        .into_iter()
        .map(|category| PickerRow {
            key: category.path,
            label: category.leaf,
            depth: category.depth,
            count: show_count.then_some(category.repos),
            close: true,
        })
        .collect()
}

/// 选中一行之后的回调：把那一行的 key 交出去。
///
/// 起个别名是因为 `Rc<dyn Fn(..)>` 直接写在签名里太长，而这个类型在 `content`
/// 闭包里每行都要 clone 一次。
type OnPick = Rc<dyn Fn(String, &mut Window, &mut App)>;

/// 一个下拉选择框：收着是一个按钮，点开是一列行。
///
/// `prefix` 只用来生成元素 id（`{prefix}-slot` / `{prefix}-menu` / `{prefix}-row:{key}`）：
/// 页面上有多处用到它，写死就会互相认成同一个元素。
///
/// 点中一行要同时做两件事——把 key 交出去、把下拉收起来（行说了算，见
/// [`PickerRow::close`]）。收起只能落在 popover 自己的状态上，所以这里不去借
/// 调用方的 `Context`，而是靠一个普通回调把 key 送出去。
pub struct Picker {
    prefix: String,
    icon: IconName,
    label: String,
    rows: Vec<PickerRow>,
    /// 已选中的 key。单选时最多一个，多选时是一批。
    selected: Vec<String>,
    /// 按钮点亮：这一维上真加了条件，而不是「不限」。
    accent: bool,
    stretch: bool,
    on_pick: Option<OnPick>,
}

impl Picker {
    pub fn new(prefix: impl Into<String>, icon: IconName, label: impl Into<String>) -> Self {
        Self {
            prefix: prefix.into(),
            icon,
            label: label.into(),
            rows: Vec::new(),
            selected: Vec::new(),
            accent: false,
            stretch: false,
            on_pick: None,
        }
    }

    pub fn rows(mut self, rows: Vec<PickerRow>) -> Self {
        self.rows = rows;
        self
    }

    pub fn selected(mut self, keys: Vec<String>) -> Self {
        self.selected = keys;
        self
    }

    /// 当前不是「不限」时把按钮点亮，收着也看得出这一维筛上了。
    pub fn accent(mut self, accent: bool) -> Self {
        self.accent = accent;
        self
    }

    /// 按钮撑满一行（对话框里那一处）。
    pub fn stretch(mut self) -> Self {
        self.stretch = true;
        self
    }

    pub fn on_pick(mut self, on_pick: impl Fn(String, &mut Window, &mut App) + 'static) -> Self {
        self.on_pick = Some(Rc::new(on_pick));
        self
    }

    pub fn into_element(self) -> AnyElement {
        let Self {
            prefix,
            icon,
            label,
            rows,
            selected,
            accent,
            stretch,
            on_pick,
        } = self;

        let rows = Rc::new(rows);
        let selected = Rc::new(selected);
        // 没有回调时点一下什么都不做：纯渲染的场合（比如只截个图）不该炸。
        let on_pick: OnPick = on_pick.unwrap_or_else(|| Rc::new(|_, _, _| {}));
        let row_prefix = format!("{prefix}-row:");
        let menu_id: SharedString = format!("{prefix}-menu").into();

        let mut trigger = Button::new(format!("{prefix}-trigger"))
            .icon(icon)
            .label(label)
            .small();
        trigger = if accent {
            trigger.primary()
        } else {
            trigger.outline()
        };
        if stretch {
            trigger = trigger.w_full();
        }

        let popover = Popover::new(format!("{prefix}-popover"))
            .trigger(trigger)
            .content(move |_state, _window, cx| {
                let popover = cx.entity();
                let (muted, hover) = {
                    let theme = cx.theme();
                    (theme.muted_foreground, theme.list_hover)
                };

                let items: Vec<AnyElement> = rows
                    .iter()
                    .map(|row| {
                        let key = row.key.clone();
                        let row_id = format!("{row_prefix}{}", row.key);
                        let active = selected.iter().any(|it| it == &row.key);
                        let close = row.close;
                        let on_pick = on_pick.clone();
                        let popover = popover.clone();

                        h_flex()
                            .id(ElementId::from(row_id.clone()))
                            .w_full()
                            .items_center()
                            .gap_2()
                            .py_1()
                            .pr_2()
                            // 缩进挂在 padding 上而不是套一层 div：这样整行的悬停底色是通的。
                            .pl(px(4. + row.depth as f32 * 14.))
                            .rounded_sm()
                            .cursor_pointer()
                            .hover(move |style| style.bg(hover))
                            .on_click(move |_, window, cx| {
                                on_pick(key.clone(), window, cx);
                                if close {
                                    popover.update(cx, |state, cx| state.dismiss(window, cx));
                                }
                            })
                            .child(if active {
                                Icon::new(IconName::Check).size_3p5().into_any_element()
                            } else {
                                // 占位，让没选中的行和选中的行名字对得齐。
                                div().size_3p5().into_any_element()
                            })
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_sm()
                                    .child(copyable_as(format!("{row_id}-name"), row.label.clone())),
                            )
                            .when_some(row.count, |this, count| {
                                this.child(div().text_xs().text_color(muted).child(count.to_string()))
                            })
                            .test_support()
                            .into_any_element()
                    })
                    .collect();

                // 行多了要能滚。高度封顶，免得下拉比窗口还高。
                v_flex()
                    .id(menu_id.clone())
                    .w(px(240.))
                    .max_h(px(320.))
                    .overflow_y_scroll()
                    .children(items)
                    .test_support()
            });

        // 外面这层只为给它一个可观测的身份：`Button` 是 `RenderOnce`，挂不上 `test_support`，
        // 而测试要点开这个下拉就得先找到它。
        let mut slot = div()
            .id(SharedString::from(format!("{prefix}-slot")))
            .test_support()
            .child(popover);
        if stretch {
            slot = slot.w_full();
        }
        slot.into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;
    use gpui_kit::TestAppContext;
    use gpui_kit::test::TestWindowExt as _;
    use std::cell::RefCell;
    use std::rc::Rc;

    fn category(id: i64, path: &str, leaf: &str, depth: usize, repos: usize) -> CategoryRow {
        CategoryRow {
            id,
            // 拖拽排序才用得上；这里只摆下拉里的树，按路径取上级就够了。
            parent: path.rsplit_once('/').map(|(parent, _)| parent.to_string()),
            path: path.to_string(),
            leaf: leaf.to_string(),
            depth,
            children: 0,
            repos,
        }
    }

    #[test]
    fn a_row_writes_only_its_own_name_and_the_depth_says_the_rest() {
        let rows = category_rows(
            vec![
                category(1, "开发", "开发", 0, 5),
                category(2, "开发/前端", "前端", 1, 3),
                category(3, "开发/前端/构建", "构建", 2, 0),
            ],
            true,
        );
        let shown: Vec<(&str, usize, Option<usize>)> = rows
            .iter()
            .map(|row| (row.label.as_str(), row.depth, row.count))
            .collect();
        assert_eq!(
            shown,
            [
                ("开发", 0, Some(5)),
                ("前端", 1, Some(3)),
                ("构建", 2, Some(0)),
            ],
            "父在子前、子分类只写自己那一段、数量挂在行上，空分类也留着"
        );
        assert!(rows.iter().all(|row| row.close), "分类是单选，点完就该收起来");
    }

    #[test]
    fn the_count_is_left_out_when_it_means_nothing() {
        // 改分类时那个数字跟这次操作没关系，摆着只会让人以为是在选筛选条件。
        let rows = category_rows(vec![category(1, "开发", "开发", 0, 5)], false);
        assert_eq!(rows[0].count, None);
        assert_eq!(rows[0].key, "开发", "key 始终是路径——身份不跟着显示变");
    }

    /// 测试窗口的根：把下拉交出来的 key 记下来。
    struct Harness {
        picked: Rc<RefCell<Vec<String>>>,
        /// 多选那一份：行都留着不收。
        multi: bool,
    }

    impl Render for Harness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let picked = self.picked.clone();
            let rows = if self.multi {
                vec![
                    PickerRow::fixed("any", "不限", None),
                    PickerRow::fixed("cli", "cli", None).keeping_open(),
                    PickerRow::fixed("待读", "待读", None).keeping_open(),
                ]
            } else {
                vec![
                    PickerRow::fixed("none", "不分类", None),
                    PickerRow::fixed("开发", "开发", None),
                ]
            };

            v_flex().child(
                Picker::new("test-picker", IconName::FolderTree, "不分类")
                    .rows(rows)
                    .selected(vec!["none".to_string()])
                    .on_pick(move |key, _window, _cx| picked.borrow_mut().push(key))
                    .into_element(),
            )
        }
    }

    /// 这条盯的是**只有跑起来才说得清**的那一段：下拉点得开吗、点一行之后 key 真交
    /// 出来了吗、菜单自己收起来了吗。用它的两处（按分类筛选、给仓库改分类）靠的
    /// 就是这一层，所以两边的差别只剩「key 怎么解释」。
    #[gpui_kit::test]
    fn picking_a_row_hands_the_key_out_and_closes_the_menu(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);

        let picked = Rc::new(RefCell::new(Vec::new()));
        let handle = cx.add_window({
            let picked = picked.clone();
            move |_window, _cx| Harness {
                picked,
                multi: false,
            }
        });

        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(
                window.try_find("test-picker-menu").is_none(),
                "没点之前不该画出来"
            );

            window.click("test-picker-slot", cx);
            window.render_frame(cx);
            assert!(
                window.try_find("test-picker-menu").is_some(),
                "点一下按钮要展开"
            );

            window.click("test-picker-row:开发", cx);
            window.render_frame(cx);
            assert!(
                window.try_find("test-picker-menu").is_none(),
                "选完要自己收起来，不然会挡住下面的东西"
            );
        })
        .unwrap();

        assert_eq!(
            picked.borrow().as_slice(),
            ["开发"],
            "回调收到的是那一行的 key，不是下标"
        );
    }

    /// 多选（标签筛选）与单选的唯一区别就在这里：点一行**不**收起，可以接着点下一个；
    /// 点「不限」这种收尾的行才收。这个行为写在行上（`PickerRow::close`），
    /// 所以组件层完全不需要知道「多选」这个概念。
    #[gpui_kit::test]
    fn a_row_that_asks_to_stay_open_does_not_close_the_menu(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);

        let picked = Rc::new(RefCell::new(Vec::new()));
        let handle = cx.add_window({
            let picked = picked.clone();
            move |_window, _cx| Harness {
                picked,
                multi: true,
            }
        });

        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.click("test-picker-slot", cx);
            window.render_frame(cx);

            window.click("test-picker-row:cli", cx);
            window.render_frame(cx);
            assert!(
                window.try_find("test-picker-menu").is_some(),
                "多选的行点完要留着，不然选不了第二个"
            );

            window.click("test-picker-row:待读", cx);
            window.render_frame(cx);
            assert!(
                window.try_find("test-picker-menu").is_some(),
                "连着点第二个也不收"
            );

            window.click("test-picker-row:any", cx);
            window.render_frame(cx);
            assert!(
                window.try_find("test-picker-menu").is_none(),
                "「不限」是收尾的那一下，点完该收"
            );
        })
        .unwrap();

        assert_eq!(picked.borrow().as_slice(), ["cli", "待读", "any"]);
    }
}

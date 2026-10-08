# ngy-fav-project — 项目长期约定

GitHub 仓库收藏/发现桌面程序，gpui-kit（实际 0.6.4）。完整说明见根目录 `README.md`。
这里只记改代码时必须守住的约定与 README 不写的坑；GPUI 的通用套路在
`gpui-component-patterns` / `gpui-ui-testing` 两个技能里，不在这里重复。

## 分层

- **`src/library.rs` 是领域数据的唯一持有者**。SQLite 是权威、内存是镜像；视图不缓存仓库，
  只观察 `Entity<Library>`。
- **先写库，成功了再改内存**，一律走 `Library::write(|db| ...)`，失败记进 `save_state`
  （界面顶部横幅）。唯一例外是分类树：写成功后 `reload_categories()` 整批重读。
- **时间只从 `shared/clock.rs` 出**（全项目 `i64` Unix 秒）。**`shared/net.rs` 是唯一 HTTP 出口**
  （ureq 的阻塞调用放独立 OS 线程再桥回 async）；它关掉了 ureq 的 `http_status_as_error`
  ——开着的话 4xx/5xx 变 `Err`，**响应头连同正文全丢**，而撞配额时只有 403 上的
  `X-RateLimit-*` 能判断「是不是配额、几时恢复」。**解析出 0 条要返回 `Err`**，
  空 `Vec` 会被上层读成「本轮无新项目」。**`RunPlan` 是配置快照**：一轮开始时读成值，
  途中改设置不影响本轮。
- **`shared/store.rs` 只管「数据放哪」**，读写归 `src/db/`。

## 发现源与解析（`src/github/` + `src/discovery/`）

- **发现源的身份就是 URL**，指向哪一类页面完全从地址形状推（`github/source.rs::SourcePage`
  = `Trending` | `Topic { slug }`），**不额外持久化「类型」字段**——存了就会出现「改了地址
  忘了改类型」，然后拿 trending 的选择器去套 topics 页面。**加一类页面要同时动三处**：
  `SourcePage::of`（认不认）、`discovery::model::validate_source_url`（添加时就拒掉认不出的
  地址，别留到运行时每轮报「结构可能已经变化」）、`sources_view::suggest_name`（默认名字口径）。
- 两类页面 DOM 互不相干，解析器分开（`trending.rs` / `topic.rs`）；**条目形状与取文本的工具
  共用 `entry.rs`**（`RepoEntry` / `repo_from_href` / `parse_count` / `element_text`），
  别在两个解析器里各写一份。**解析出 0 条一律 `Err`**，空 `Vec` 会被读成「本轮无新项目」。
- **发现源自带的标签也从地址推**（`source::tags_for_source`：topics → `[slug]`，trending → 空），
  **没有「源 → 标签」映射表**。落点是 `runner::absorb_entries`：标签**先 `add_name` 建进词表**
  （`add_repo` 是拿名字换 id，词表外的名字被静默丢掉），再挂到**这一轮新入库**的仓库上
  （`AlreadyPresent` 的一个字段都不碰，否则用户摘掉的标签下一轮会长回来）。建不出来要报错。
  **`add_repo` 会把 `repo.tags` 按词表过滤并换成词表里的写法**——它不替你建名字，只保证
  内存镜像里不会留一个「库里没有、重启就消失、还点不着」的标签。
- **别拿 `tmp-` 前缀的工具类当选择器**：GitHub 正把 `mr-3` / `px-3` 逐个改成 `tmp-mr-3` /
  `tmp-px-3`，同一张卡片上两种混着出现。靠结构（`article` / `h3` / `p`）与语义标记
  （`itemprop="programmingLanguage"`、`js-social-count`）更稳。
- topics 卡片的**星数计数器显示的是缩写**（`248k`），完整数字只在 `title` / `aria-label` 上；
  直接读文本会得到一个**看着像真数据的错数**。trending 的星标链接写的是完整数字。
- topics 卡片下方那排 tab（Code / Issues / Pull requests）里 `Code` 也指向 `/owner/repo`，
  **仓库链接必须限定在 `h3` 里**，否则每张卡片解析出两条。
- **改选择器时另拿一份整页 HTML 自己过一遍**：单测里的真实片段只能证明选择器写对了，
  证明不了它在整页上不会多匹配。抓样本用 `curl -sL -A "<浏览器 UA>"`。

## 存储（`src/db/`）

- 读表收 `&Connection`（入口 `Db::open` / `open_in_memory`），事务用 `Db::transaction`
  （内部 `unchecked_transaction`，因为连接是 `&self`）。
- **`PRAGMA foreign_keys = ON` 必须在连接上执行**，否则分类的级联删除完全失效。
- **`SELECT_COLUMNS` 与 `read_row` 的列下标必须一起改**；`ORDER_BY` 引用 `r.` 别名，用它的语句
  必须 `FROM repos r`（漏了是运行期 `no such column`）。**`name_key` 列存 Rust 侧小写化的名字**
  （sqlite 的 `lower()` 只认 ASCII）。**`PRAGMA user_version` 是 schema 版本**，
  比程序新就硬失败、不写入。
- **`read()` 把错误吞成 `None`**，写错 SQL 表现为「功能静默失灵」而不是报错；加查询先跑真实 SQL。

## 分类与标签（`src/taxonomy/`）

- **分类是 `parent_id` 真树**（仓库上 `Repo.category_id: Option<i64>`）；**标签**扁平
  （`Repo.tags: Vec<String>`，标签表有自己的 id）。**名字里的 `/` 只是输入语法**（逐级解析成节点，
  缺的上级顺手建，最多 6 级）——**父子全靠 `parent_id`，别从名字推断**。
- **名字即身份**：比较忽略大小写（`model::same_name`），存第一次出现的写法。名字/路径语法
  （`model.rs`）与树操作（`tree.rs`）是纯函数，**先加测试再改**。**词表只有四个入口**：
  `Library::{add_name, rename_name, remove_name, move_name}`；别在视图里直接改词表，
  `set_repo_category` / `set_repo_tags` 拒绝词表外的名字（返回 `false` 且什么都不改）。
- **同级按 `position` 排**（不按名字码点）。`move_name(kind, name, ±1)` 换的是**同级**邻居；
  写库时把整段同级 `position` 重写成 `0..n`——老数据里可能有并列值，只交换两条会被 id 序盖回去，
  看着像「点了没反应」。**改名 = 改名 + 移动**（填完整路径，改到自己下级 → `Circular`），
  所以别另做「移动分类」功能。**删分类连子树一起删**，仓库 `category_id` 置空
  （外键 `ON DELETE SET NULL`）。
- **拖拽排序走 `place_name(kind, name, target, before)`**，一步落到目标旁边
  （`place_category` / `place_tag`）；落点只认同级（跨层 → `MoveName::NotSibling`，一条都不改）。
  `CategoryRow.parent` 就是给「谁跟谁同级」用的，从 `parent_id` 出、不从 path 切。
- **两个管理页**（`Page::Categories` / `Page::Tags`）是同一个 `TaxonomyView` 的两份实例，构造时传
  `TaxonomyKind`；别按 kind 存两份状态。**列表行的分类色块挂第一行**（`List` 要求所有行等高）。
- 子分类**只写自己那一段名字**，层级交给缩进；空分类留在树里（缺节点的树看着像坏了）；
  计数是**整棵子树**的条数。

### 选东西用的下拉（`taxonomy/picker.rs`）

- **一个 builder：`Picker::new(prefix, icon, label).rows(..).selected(..).accent(..).stretch()
  .on_pick(..).into_element()`**。五处在用：筛选条的分类/语言/标签、单个仓库改分类、整批改分类。
  这层只做两件事——把行摆出来（`PickerRow`）、点一行之后把 **key** 交出去；**key 是什么意思由
  调用方解释**，别把 `CategoryFilter` 或仓库带进来。
- **`PickerRow.close` 决定点完收不收**（`keeping_open()` 造多选的行），picker 层没有「多选模式」
  这个概念。`selected: Vec<String>`，单选就是 0/1 个。
- **元素 id 由 prefix 生成**：`{prefix}-slot` / `{prefix}-menu` / `{prefix}-row:{key}`。现用 prefix：
  `repo-category-filter` / `repo-language-filter` / `repo-tag-filter` / `assign-category` /
  `bulk-category`。
- 单选分类用的行统一从 `picker::category_choice_rows(categories, false)` 出；最前面那条「不分类」
  的 key 是 `picker::NONE_KEY`，两处对话框共用，别各定一个常量。

## 列表与分页（`src/repos/`）

- 形状在 `repos/model.rs`：`RepoQuery` / `RepoQuerySpec` / `RepoPage`，分页是 `LIMIT/OFFSET` +
  单独 `COUNT(*)`。**翻页要有稳定全序**：`ORDER BY r.discovered_at DESC, r.full_name ASC`
  （少了后半段，同一条记录可能出现在两页里）。页码越界会回退（`Library::query_repos`），
  别在视图里再钳一次。**`CategoryScope::Subtrees(vec![])` 编译成 `WHERE 0`**。
- **筛选三个维度**：分类（单选，选上级 = 整棵子树）、语言（`Option<String>`）、标签
  （`Vec<String>`，**多选、都要有**）；维度之间是**且**。SQL 在 `db/repos.rs::build_filter`：
  语言 `r.language = ?` **精确相等**（用 `LIKE` 会把 `Gosu` 当 `Go` 捞出来），标签**每个一个 `EXISTS`**。
- **筛选条上的计数一律全库口径**（`Library::{category_rows, language_rows, tag_rows}` 从内存镜像
  数出来），不是「在当前条件下还剩几条」；语言按数量降序、标签按词表顺序。
- **标签选择集合按 `name_key` 排序存**（`delegate.set_tags`）：否则「先点 A 后点 B」与反过来是
  两份不相等的条件，「条件没变就不重查」失效，SQL 参数顺序也成了看运气。
- **失效的筛选项在查库之前摘掉**（`ReposView::sync_page`：分类被删、语言不再存在、标签被摘），
  并回第一页。`delegate.has_facet_filters()` 和「清除」按钮管的必须是同一件事（它不动搜索词）。
- **`list_delegate.rs` 是纯状态**（不碰数据库）；**`accept_page` 返回「变没变」，变了才
  `cx.notify()`**——无条件通知会转不停。
- **勾选（`checked: BTreeSet<RepoId>`）跟 `selected` 是两件事**：后者是详情面板正在看的那一个。
  勾选**跨页、跨筛选条件地留着**，只有仓库真被删了才摘（`retain_checked`，在 `sync_page` 里做）。
- **「全选」选的是筛出来的整批，不是这一页**：`matching: Vec<RepoId>` 由视图查
  `Library::repo_ids_matching`（不带 `LIMIT` 的 `SELECT id`）塞进 delegate，只用来算那个方框；
  再点一次「全选」摘的是**匹配的那些**，不是整个勾选集合。
- **批量改分类：`Library::set_repos_category(&ids, Option<&str>) -> Option<usize>`**，库里是一条
  `db::repos::set_category_bulk`（`UPDATE ... WHERE id IN (...)`，套在 `Db::transaction` 里；
  **空 ids 要在拼 SQL 前拦住，sqlite 的 `IN ()` 是语法错误**），返回**真被改到的条数**。
  内存镜像一趟遍历改完，别逐个 `find`。
- **批量对话框的 `on_saved` 是回调**（`F: Fn(&mut App) + Clone + 'static`）：对话框不该认识列表，
  而「保存完把勾选清掉」得由列表自己做。`open_dialog` 的 builder 是 `Fn`、每帧都调，
  所以捕获的东西必须 `Clone`。**`add_repo_dialog` 的 `on_added` 同一套路**（多一个
  `&mut Window`，因为要调 `ListState::set_selected_index` / `scroll_to_selected_item`）。
- **列表的选中按「行号」记**（`ListState::set_selected_index`，行号只对当前这一页有意义），
  而视图对外说的是 id：**按 id 选中要先用 `delegate.position_of(id)` 换算行号**，换不到
  （被筛掉了）就只把 `delegate.set_selected(Some(id))` 落下去——详情面板照样看它，列表里
  没有一行可高亮。**手工添加完的那个仓库也要这样选中**（`ReposView::select_added`：
  回第一页 → 取回那一页 → 换算行号 → 选中并滚到它；**不碰筛选条件**）。
- **测「加完要选中」别走 `add_repo_dialog::submit`**：它会给新仓库拉一次元数据（真联网）。
  直接调它交出去的那一步（`select_added_callback`）。

## 界面（只记项目里定下来的，通用套路见技能）

- **要按 id 找的元素必须挂 `.test_support()`**（不开特性时是恒等函数）；`Button` 这类 `RenderOnce`
  组件挂不上，要包一层 `div`。**`open_dialog` 的正文排在 `Root` 的 deferred 层里，无头窗口按 id
  找不到**（`has_active_dialog` 为真但元素不可观测）——要测就把那段正文实体直接当窗口根。
- **三栏（侧栏 / 仓库列表 / 详情）都能拖宽**，两处都是 `h_resizable(..).with_state(&Entity<..>)` +
  `resizable_panel()`：`app-shell`（侧栏 168~360，拖到 168 以下松手 = 自动收起，展开回来还是
  拖出来的宽度）与 `repos-split`（列表 240~560）。固定宽度那一栏必须 `.flex_none()`。
  宽度只活在这次运行里，**没有写进配置文件**。
- **侧栏那两处绕开组件库的写法别改回去**：收起/展开不能用「reset 再 resize」（`reset_panel`
  会把 `size_range` 弄成 `0..0`，紧接着设宽度会被夹成 0），现在是收起直接 `resize_panel(0, 48)`、
  展开只 `reset_panel` 并把宽度交给渲染时的 `.size()`；`Sidebar` 的宽度写 `w_full()` 而不是
  像素值（像素值会启用组件自带的宽度过渡动画，拖起来像橡皮筋；代价是收起/展开没有滑动过渡）。
  把手只有分割线左边 4px、`on_resize` 用 `Rc<Cell<f32>>` 回传宽度——原因与细节见技能 §10。

## 验证与构建

- **沙箱里的 cargo 可能卡在文件锁上**，别干等（`Blocking waiting for file lock` + rustc 的 CPU
  增量恒为 0 = 等锁，不是慢）；清掉抢锁的进程后改用宿主 PowerShell 跑。**跑完一定要杀进程**，
  残留进程占着 exe，下次 `cargo build` 报 LNK1104，而 `tasklist` 都看不见它。
- 造临时数据用 `NGY_FAV_HOME` 指到临时目录；核对库里写了什么，用 python 的 `sqlite3` **只读**
  （`mode=ro`）打开看表。
- **`cargo fmt` 不要跑**：项目本来就不是 rustfmt 干净的，新代码手工对齐周围风格。
- **同一条消息里只改一个文件一次**：对同一文件并发发多个 `Edit` 会有改动被吞掉（都报 success，
  磁盘上只剩一部分），改完 `grep` 复核。
- 图标由 `tools/make_icon.py` 生成，`build.rs` 自扫 `Windows Kits\10\bin` 找 rc.exe
  （别退回 winresource 的默认探测，它调 `reg.exe`，而 reg.exe 在本机黑名单里）。

## 相关技能

`gpui-component-patterns`（界面套路与坑）、`gpui-ui-testing`（无头窗口测试配方）、
`app-data-relocation`（数据目录可搬）、`gpui-theming`、`gpui-gui-automation`、
`gpui-app-icon`、`http-rate-limit-breaker`。

# ngy-fav-project — 项目长期约定

GitHub 仓库收藏/发现桌面程序，gpui-kit（实际 0.6.4）。完整说明见根目录 `README.md`，
这里只记改代码时必须守住的约定，以及 README 里不写的坑。

## 分层约定

- **`src/library.rs` 是领域数据的唯一持有者**，SQLite 是权威、内存是镜像。
  视图不缓存仓库副本，只观察 `Entity<Library>`；列表按页从库里取。
- **先写库，成功了再改内存**。所有修改走 `Library::write(|db| ...)`，失败记进
  `save_state`（界面顶部横幅报错）。反过来做会留下「只在内存里、下次启动就没」的假象。
  唯一的例外是分类树：写成功后 `reload_categories()` 整批重读（表小，增量容易漏）。
- **时间只从 `src/shared/clock.rs` 出**，全项目一律 `i64` Unix 秒。不要直接调 `SystemTime`。
- **`src/shared/net.rs` 是唯一的 HTTP 出口**。它把 `ureq` 的阻塞调用放到独立 OS 线程再桥回 async，
  所以不占 GPUI 共享后台线程池。新增网络请求走它，别直接 `ureq`。
- **`net.rs` 的 agent 关掉了 ureq 的 `http_status_as_error`**（默认是开的）。开着的话 4xx/5xx
  会在 `ureq` 内部变成 `Err(StatusCode)`，**响应头连同正文全丢**——撞配额时那个 403 上的
  `X-RateLimit-Remaining` / `X-RateLimit-Reset` 正是唯一能判断「是不是配额、几时恢复」的依据。
  所以非 2xx 也走 `Ok(response)`，由 `blocking_get` 统一读头读正文，再自己转成
  `HttpError::Status { code, url, rate_limit_remaining, rate_limit_reset, body }`。
  **重定向不受影响**（`run.rs` 里按 `status().is_redirection()` 单独处理，与这个开关无关）。
- **解析与网络分离**。`github/trending.rs`、`github/repo_meta.rs` 的解析都是纯函数
  （字符串进、结构体出），测试用真实抓下来的 HTML 片段，不联网。
- **解析出 0 条要返回 `Err`**，不能返回空 `Vec`——空列表会被上层误读成「本轮无新项目」。
- **`RunPlan` 是配置快照**。一轮开始时把配置读成值，抓取途中用户改设置不影响本轮。
- **`shared/store.rs` 只管「数据放哪」**。读写归 `src/db/`（SQLite 给事务与 WAL），
  它现在只剩目录解析与 `archive_file`（旧文件改名留档）。**不要再往那里加写文件的函数。**

## 存储（`src/db/`）

- **读表的函数收 `&Connection` 而不是 `&Db`**，入口是 `Db::open` / `open_in_memory`；
  事务用 `Db::transaction`（内部 `unchecked_transaction`，因为连接是 `&self`）。
- **`PRAGMA foreign_keys = ON` 必须在连接上执行**，否则分类树的级联删除完全失效。
- **`SELECT_COLUMNS` 与 `read_row` 的列下标必须一起改**；`ORDER_BY` 引用了 `r.` 别名，
  用它的查询语句必须 `FROM repos r`（漏别名 → 运行期 `no such column`）。
- **`name_key` 列存 Rust 侧小写化的名字**（sqlite 的 `lower()` 只认 ASCII），比较走它。
- **`PRAGMA user_version` 是 schema 版本**，比程序新就 `SchemaTooNew` 硬失败、不写入。
- **`read()` 会把错误吞成 `None`**，所以写错 SQL 会表现为「功能静默失灵」而不是报错。
  加查询时先在测试里跑一遍真实 SQL。
- 旧 `library.json` **只在全新空库时导入一次**，成功后 `archive_file` 改名留档。
  库里有数据就完全不碰它。

## 分类与标签（`src/taxonomy/`）

- **分类是 `parent_id` 真树**（`CategoryTree`），仓库上是 `Repo.category_id: Option<i64>`；
  **标签**是扁平的 `Repo.tags: Vec<String>`，词表里带稳定 `id`。归属上是单向的：
  一个仓库最多一个分类，可以挂多个标签。
- **名字即身份**：比较忽略大小写（`taxonomy::model::same_name`），存第一次出现的写法。
  名字与路径语法（`clean_name/same_name/clean_path/segments/join_path/hue_seed/name_key`）
  都在 `taxonomy/model.rs`，是纯函数，先加测试再改。
- **词表操作只有三个入口**：`Library::{add_name, rename_name, remove_name}`。
  别在视图里直接改 `data.categories`（那个结构已经不存在了）。
- **`set_repo_category` / `set_repo_tags` 拒绝词表外的名字**（返回 `false` 且什么都不改）。
  界面上的「新建」是「先 `add_name` 再挂上」，所以正常路径碰不到拒绝。
- **`name_key` 比较在 SQL 里做**：同级唯一是 `UNIQUE(ifnull(parent_id,0), name_key)`。
- **列表行的分类色块挂第一行**。`List` 要求所有行等高，行数不能随内容变（见 list_delegate 注释）。

### 树的规矩

- **分类名里可以带 `/`，但那只是输入语法**。界面上填 `开发/前端`，落到库里是一条
  逐级解析出来的路径（缺的上级顺手建），分隔符 `/`，最多 6 级。**别再从名字里推断父子关系**。
- **父子的关系全是 `parent_id`**，纯函数在 `taxonomy/tree.rs`：`segments / path / depth /
  subtree / is_within / find_path / sibling_named / flatten / insert / set_name / set_parent /
  remove_subtree`。`CategoryTree::nodes` 恒按 `(position, id)` 排好，所以 `children()` 天生有序。
- **同级按 `position`（建立顺序）排，不按名字码点**。新建接在末尾，改名不换位置
  （`rename_category` 只在 `parent != current_parent` 时才调 `move_category`）。
- **改名 = 改名 + 移动**：改名框填完整路径，`开发/前端` → `工具/前端` 就是挪子树；
  改成自己的下级 → `RenameName::Circular`。所以别另做「移动分类」功能。
- **删除分类连子树一起删**，仓库的 `category_id` 被置空（外键 `ON DELETE SET NULL`）。
  `CategoryRow.repos` / `child_count` 都按子树算。
- **`name_key` 索引 + 外键是唯一的完整性保障**，界面上的判断都只是提前拦一下。
- **两个管理页**：`Page::Categories` / `Page::Tags` 是同一个 `TaxonomyView` 的两份实例，
  构造时传 `TaxonomyKind`。视图里不要再按 kind 存两份输入框。

## 列表与分页（`src/repos/`）

- **查询的形状在 `repos/model.rs`**：`RepoQuery`（界面要什么）/ `RepoQuerySpec`（拼进 SQL 的条件）/
  `RepoPage`（一页 + total + 页码）。分页是 `LIMIT/OFFSET` + 单独 `COUNT(*)`。
- **翻页要有稳定全序**：`ORDER BY r.discovered_at DESC, r.full_name ASC`。少了后半段，
  同一条记录可能出现在两页里。
- **页码越界会回退**（`Library::query_repos` 里重查最后一页），别在视图里另做一套钳制。
- **`CategoryScope::Subtrees(vec![])` 编译成 `WHERE 0`**：筛选一个已不存在的分类宁可空着，
  也不要静默退回「全部」。
- **`list_delegate.rs` 不碰数据库，是纯状态**（`query` + 一页 `page` + 选中项），
  查库是视图的事（`ReposView::sync_page`）。**`accept_page` 返回「变没变」，变了才
  `cx.notify()`**——无条件通知会让「通知 → 观察者再查一次 → 再通知」转不停。
  把规则放这儿是为了能用普通 `#[test]` 测（没有 GPUI 上下文）。

## GPUI 界面上的两个固定套路

- **对话框的状态必须自己是个实体**。`window.open_dialog` 的 builder 是 `Fn`，每帧都会被
  调用——在里面 `cx.new` 会每帧重建状态。现有做法：`open()` 里先 `cx.new` 出草稿实体
  （`repos/taxonomy_dialog.rs::AssignmentDraft` 最完整），builder 里只 clone 句柄。
- **借用**：`entity.read(cx).method(cx)` 编译不过（E0502）。要写盘/改状态就走
  `entity.update(cx, |this, cx| this.commit(cx))`。
  另外 `cx.theme()` 借的是 `&cx`，要在造回调之前把颜色抄成值（`Hsla` 是 Copy）。

## 后台调度（`src/discovery/runner.rs`）

- 巡检每 5 秒一次，判断交给纯函数 `decide(TickState, now)`，触发条件有测试兜着。
- **两种触发不能合并**：`DueKind::Full`（抓发现源）与 `DueKind::Metadata`（只补元数据）。
  合并过一次，后果是每 60 秒重抓 trending 并覆盖成功记录。
- **匿名 API 60 次/小时是硬约束**。任何新的定期请求都要有退避，否则会把配额烧光。
- 源码改动后要 `cargo clippy --all-targets` 无警告 + `cargo test` 全绿。

### 配额用完就整轮停（`src/github/rate_limit.rs`）

- **判据是三条，不是「看 403」**：`remaining == Some(0)` / 状态码 429 /
  403 **且正文提到 `rate limit`**（二级限流不带配额头）。光看 403 会把「令牌没权限、
  仓库被下架」也判成配额用完，那些等多久都不会好。
- **闸门 `RateLimitGate` 内部是 `Rc<Cell<Option<i64>>>`**，clone 共享状态——`run_plan`
  里 clone 进各 future 的那份改了，`runner` 持有的那份也跟着变。只在 GPUI 单线程执行器里
  访问，不要换成原子量或锁。
- **撞上的那一刻要 `trip`，不是只在轮内 `break`**。只 break 的话下一轮巡检（5 秒后）
  又会起一轮、又打一遍。`decide` 开头就先看 `rate_limit_until`，没恢复就返回 `None`。
- **轮内请求在发出去之前要问一次闸门**（metadata 那批在做，且闸门一旦拉下，
  还没启动的 future 就停在那儿）。已经在飞的拦不回来，上界是 `METADATA_CONCURRENCY`。
- **`blocked_until` 会 clamp 到 1 小时**，并滤掉过期的 `reset`——中间设备改写响应头
  或时钟偏差都不该把发现功能永久锁死。
- **`trip` 合并时先滤掉过期的旧值再取 max**，否则一个已过期的远期值会「接住」新的一次拉闸。
- **配额掐断的轮不算「无进展」**，不要叠到 `metadata_stall_rounds` 退避上——否则恢复
  时刻到了还要再多等一轮。
- **`run_now` / `refresh_metadata_now` 会先 `clear()` 闸门**：用户主动点时多半刚填了令牌
  （配额 60 → 5000）。真没恢复，第一个请求 403 会立刻再拉上，代价一个请求。
- 界面侧：`rate_limit_until()` 有值时**不要**再显示「下次自动运行」——那个时间到了也不会跑。
  两个值占同一行位置。闸门开合那一刻靠 `sync_rate_limit_flag()` 补一次 `notify`，
  否则恢复时刻到了但没活干时，界面会一直挂着那句过期的「已停止联网」。

## 验证

- **界面截不到图**（`Screenshot` 只看得到宿主桌面，画布不在上面）。但 windows-mcp 的
  `Process` 能列出沙箱里这个应用进程、`App`（按窗口标题 switch/resize）也认得到窗口，
  所以「进程起没起来、有没有 panic、数据文件有没有被改写」都能查。
- 造临时数据用 `NGY_FAV_HOME` 指到临时目录，别碰 `%APPDATA%` 里的真实数据。
- 后台跑 GUI 应用要用 `run_in_background`，普通 `&` 会随 shell 退出被杀。
- **跑完一定要杀进程**（windows-mcp `Process` mode=kill）。残留进程占着
  `target\debug\ngy-fav-project.exe`，下次 `cargo build` 报 LNK1104「无法打开文件」，
  而 `tasklist` / `Get-Process` 都看不见它。
- 想核对库里到底写了什么，用 python 的 `sqlite3` **只读**打开（`mode=ro`）看
  `categories` / `repos` / `repo_tags`，比在界面上翻快得多。
- **要判断本机 IP 的匿名配额是不是真的用完了**，直接用 python 打一次
  `https://api.github.com/repos/<owner>/<repo>`（带上项目的 UA），打印状态码与
  `x-ratelimit-remaining` / `x-ratelimit-reset`。比猜快，而且能区分「程序坏了」
  与「配额真的没了」——本机是共享出口 IP，很容易就是后者。
- **HTTP 层的行为（尤其「非 2xx 还带不带头」）用本机 stub 服务测**，别 mock 传输层：
  要验证的正是传输层交出了什么。`shared/net.rs` 的 `spawn_stub` 是个只回一次固定响应的
  `TcpListener`，用 `Connection: close` 结束正文，省得手算 `Content-Length`。
- `cargo fmt` 不要跑：项目本来就不是 rustfmt 干净的（100 多处 diff，多是既有文件）。
  新代码手工对齐周围风格即可。
- **同一条消息里只改一个文件一次。** 对同一个文件并发发多个 `Edit`，会有改动被吞掉
  （每次都报 success，磁盘上只留下一部分）。改完用 `grep` 复核对不对，别只信成功回执。

## 图标与构建

- `assets/icon.ico` 由 `tools/make_icon.py` 生成（需要 Pillow，装在
  `C:\Users\admin\.workbuddy\binaries\python\envs\default`）。改图标 = 改脚本再跑一次。
- `build.rs` 负责嵌入。**别退回 winresource 的默认探测**——它调 `reg.exe` 查注册表，
  而 reg.exe 在本机安全策略黑名单里。现有实现自己扫 `Windows Kits\10\bin` 找 rc.exe。
- **`rerun-if-changed` 必须指向 `assets` 目录而非 `assets/icon.ico`**：
  监视当时不存在的文件会把「缺失」写进指纹，图标后来生成了也不重跑。
- 嵌 exe 是唯一办法：gpui 的 `WindowOptions::icon` 只对 X11 生效。
- 完整流程（生成 / 预览 / 嵌入 / 验证）已在技能 **`gpui-app-icon`**（用户级）里，
  含四个实测脚本，新项目直接照它做。

## 测试模块的固定写法

`use gpui_kit::*` 会把 GPUI 的 `#[gpui::test]` 带进来盖掉 std 的 `#[test]`，所以：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;   // 恢复 std 的 #[test]
}
```

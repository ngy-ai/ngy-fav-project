# ngy-fav-project

一个管 GitHub 仓库收藏的小程序：手工添加或从 trending 自动发现，顺带把仓库的描述和创建时间补齐。
桌面界面用 [gpui-kit](https://crates.io/crates/gpui-kit) 写。

```bash
cargo run
```

## 它做四件事

**项目管理** — 每个仓库记这几个字段：仓库名（`owner/repo`）、地址、原始描述（抓自仓库页 About）、
汉语描述（人工填，可选）、发现时间、创建时间。汉语描述填了就在列表和详情里优先显示。

**分类与标签** — 一个仓库最多属于**一个**分类，可以挂**任意多个**标签。分类是单选，
所以详情面板里画成一个色块（颜色由名字算出来，同一个分类到哪都是同一个颜色）；
标签横着铺一排。列表按分类筛选，搜索框也能命中分类名和标签名。

**仓库发现** — 发现源就是一条 URL，规则是「返回结果和 trending 页面长得一样」。默认带一个
`https://github.com/trending?since=weekly&spoken_language_code=`。可以加多个（换语言、换时间窗）。

**后台任务** — 每 5 秒巡检一次，到点就跑一轮发现并入库，已存在的仓库跳过。

## 后台任务什么时候动

巡检分两种，别把它们混在一起：

| 触发 | 条件 | 干什么 |
| --- | --- | --- |
| 完整运行 | 距上次抓源 ≥ `interval_minutes` | 抓所有启用的源 → 顺带补一批元数据 |
| 元数据补全 | 没到间隔，但有仓库缺描述/创建时间，且距上次 ≥ 重试间隔 | **只**补元数据，不碰发现源 |

元数据补全单独成轮是有原因的：早先它和完整运行挤在一起，于是每 60 秒就重抓一次 trending，
还把上一次「新增 21 个」的成功记录盖成一次超时失败。

重试间隔默认 60 秒，**连续一轮一个字段都没补上就翻倍**（60 → 120 → 240 → …，封顶 15 分钟）。
GitHub 匿名接口每小时只有 60 次，固定节奏重试几分钟就能把它烧光，之后整点前都补不动。

一轮里同时补 4 个仓库（`METADATA_CONCURRENCY`）。串行补 21 个实测要四十多秒，
全程「运行中」把界面锁住，用户点什么都被拒；并发不改变请求总数，对配额没有额外代价。

## 模块分层

```
src/
  main.rs              启动：初始化 gpui-kit、开窗、建 Library 与 DiscoveryRunner
  app.rs               AppShell：侧栏导航四页 + Root overlay
  library.rs           领域数据唯一持有者：内存状态 + 落盘
  shared/
    net.rs             阻塞 HTTP（ureq）桥成 async，走独立 OS 线程
    store.rs           数据目录解析 + 原子写
    clock.rs           时间戳唯一出口（全项目存 i64 Unix 秒）
  github/
    trending.rs        trending 页面解析（纯函数）
    repo_meta.rs       About 描述 + 创建时间，两路并行抓
  repos/
    model.rs           Repo 领域模型 + 输入解析
    list_delegate.rs   列表 delegate（搜索 + 分类筛选 + 选中态）
    repos_view.rs      仓库页
    add_repo_dialog.rs / edit_description_dialog.rs / taxonomy_dialog.rs
  taxonomy/
    model.rs           名字规则（清洗、比较、去重、颜色种子），纯函数
    chip.rs            分类与标签的色块
    taxonomy_view.rs   分类与标签页（管词表）
  discovery/
    model.rs           发现源、运行记录、设置
    runner.rs          后台调度（巡检、计划、执行）
    sources_view.rs    发现页
  settings/
    settings_view.rs   设置页
build.rs               构建时把 assets/icon.ico 嵌进 exe
assets/icon.ico        应用图标
tools/make_icon.py     图标生成脚本（改图标时才需要跑）
```

分层的几条约定：

- **网络与解析分离**。`github/trending.rs` 和 `repo_meta.rs` 里的解析都是纯函数，
  拿字符串进、拿结构体出，所以能直接单测（测试用的是真实抓下来的 HTML 片段）。
- **`Library` 是数据的唯一持有者**。视图不缓存仓库，只观察它。分类与标签的词表同样住在
  `Library` 里，仓库上只存名字。
- **`RunPlan` 是快照**。一轮开始时把配置读成值，抓取途中用户改设置不影响本轮。
- **时间只从 `shared/clock.rs` 出**，一律 `i64` Unix 秒。

## 分类与标签的几条规矩

词表（有哪些分类、有哪些标签）和归属（哪个仓库属于谁）是分开的两件事：词表由「分类与标签」
页管，归属在仓库详情里点「编辑」设置。

- **名字就是身份**。比较时忽略大小写（`AI` 和 `ai` 是同一个），存下来的是第一次出现的写法。
  名字里的连续空白会被压成一个空格，超过 40 个字算误输入。
- **一个仓库最多一个分类**。分类是单选，所以必有「不分类」这一项；删掉一个分类，用它的仓库
  回到未分类，**仓库本身不会被删**。
- **标签可以挂任意多个**，顺序按词表排，不按点击顺序——同一组标签在任何仓库上看起来都一样。
- **改名会跟着改引用**。改分类名/标签名时，所有用到它的仓库会一起改，不用手工重挂。
- **归属只认词表里的名字**：往仓库上写一个词表里没有的名字会被拒绝（返回 `false`，什么都不改）。
  界面上的「新建」走的是「先加词表、再挂上」，所以正常路径碰不到这个拒绝。
- **加载时会对齐一次**（`Library::normalize_taxonomy`）。手改过 `library.json` 时，仓库引用了
  词表里没有的名字会被收进词表（只增不减，不丢信息），不能用的名字从仓库上摘掉，标签去重。
  对齐结果只改内存，落盘要等下一次真正的修改。

## 数据文件

默认 `%APPDATA%\ngy-fav-project\library.json`（macOS/Linux 走 `Application Support` / `XDG_CONFIG_HOME`）。
设 `NGY_FAV_HOME` 可以整个换掉目录。

```jsonc
{
  "repos": [{
    "full_name": "coder/coder",          // 身份，去重按它的小写
    "url": "https://github.com/coder/coder",
    "description": "…",                  // 抓来的原始描述
    "description_zh": "…",               // 人工填的，显示时优先
    "discovered_at": 1789899213,
    "created_at": 1637006815,            // 可能为 null，等后台补
    "source": "周榜 · 全部语言",
    "language": "Go",
    "stars": 38039,
    "category": "开发工具",              // 单个，null 表示未分类
    "tags": ["cli", "待读"]              // 多个，顺序按下面的词表排
  }],
  "categories": ["开发工具", "学习"],     // 词表，顺序就是界面上的顺序
  "tags": ["cli", "待读"],
  "sources": [{
    "name": "周榜 · 全部语言",
    "url": "https://github.com/trending?since=weekly&spoken_language_code=",
    "enabled": true,
    "last_run": { "started_at": 0, "finished_at": 0, "found": 21, "added": 21, "skipped": 0, "error": null }
  }],
  "settings": {
    "auto_discover": true,
    "interval_minutes": 180,
    "metadata_batch": 8,
    "github_token": null
  }
}
```

加分类标签之前写下的文件没有 `categories` / `tags` / `category` / `tags` 这几个字段，
一样能读进来（缺的当空处理）。

写入走「临时文件 + rename」，所以中途断电不会留下半个 JSON。
文件损坏时会被改名成 `library.corrupt-<时间戳>.json` 留档，然后从空库起步，不会静默丢数据。

## 图标

`assets/icon.ico` 是提交进仓库的成品，`build.rs` 在构建时把它嵌进可执行文件。
Windows 的窗口与任务栏图标取自 exe 的资源段（gpui 的 `WindowOptions::icon` 只对 X11 生效），
所以必须在构建这一层做。同一份资源里还写了文件属性用的版本信息，版本号跟着 `Cargo.toml` 走。

只改代码不用管这里。要换图标就编辑脚本再跑一次：

```bash
python tools/make_icon.py     # 依赖 Pillow
```

脚本在 4 倍尺寸上绘制再降采样，所以边缘自带抗锯齿，不必手工描边。

两个踩过的坑：`build.rs` 自己扫 `Windows Kits\10\bin` 找 `rc.exe`，不走 winresource 默认的注册表查询
（那条路会调 `reg.exe`，在受限环境里会被安全策略直接拦掉）；找不到 SDK 时只发 warning 不阻断构建——
图标是装饰，不该让整个项目编译不过。

## 已知约束

- **匿名接口 60 次/小时**。一个仓库要消耗 1 次（拿创建时间）。21 个仓库要等 3 轮才补完。
  在「设置」里填一个 GitHub 令牌后是 5000 次/小时，界面会显示剩余配额。
- **trending 页面结构变了会报错**，不会静默返回空列表——空列表会被上层误读成「本轮无新项目」。
- 解析规则对着当前页面写（`article.Box-row`），GitHub 改版就需要跟着改。

## 测试

```bash
cargo test                                  # 71 个，离线
cargo test probe_real_metadata -- --ignored --nocapture   # 联网探针，打印每个仓库的耗时与失败原因
```

联网探针默认 skip（会消耗配额），用来回答「补全为什么这么慢」这类只有真实网络能回答的问题。

界面本身在开发环境里截不到（沙箱看不到应用的窗口），所以界面这一层没有自动化验证：
能被测试盖住的部分都挤进了纯函数——名字规则、词表增删改、归属与筛选逻辑都在 `cargo test` 里。

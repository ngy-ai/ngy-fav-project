# ngy-fav-project — 项目长期约定

GitHub 仓库收藏/发现桌面程序，gpui-kit（实际 0.6.4）。完整说明见根目录 `README.md`，
这里只记改代码时必须守住的约定，以及 README 里不写的坑。

## 分层约定

- **`src/library.rs` 是领域数据的唯一持有者**。视图不缓存仓库副本，只观察 `Entity<Library>`。
- **时间只从 `src/shared/clock.rs` 出**，全项目一律 `i64` Unix 秒。不要直接调 `SystemTime`。
- **`src/shared/net.rs` 是唯一的 HTTP 出口**。它把 `ureq` 的阻塞调用放到独立 OS 线程再桥回 async，
  所以不占 GPUI 共享后台线程池。新增网络请求走它，别直接 `ureq`。
- **解析与网络分离**。`github/trending.rs`、`github/repo_meta.rs` 的解析都是纯函数
  （字符串进、结构体出），测试用真实抓下来的 HTML 片段，不联网。
- **解析出 0 条要返回 `Err`**，不能返回空 `Vec`——空列表会被上层误读成「本轮无新项目」。
- **`RunPlan` 是配置快照**。一轮开始时把配置读成值，抓取途中用户改设置不影响本轮。
- **落盘走 `shared/store.rs::write_atomic`**（临时文件 + rename）。数据损坏时改名留档，
  不要让读取失败变成「从空库起步且不告诉用户」。

## 分类与标签（`src/taxonomy/`）

- **归属是单向的**：`Repo.category: Option<String>` 是单个，`Repo.tags: Vec<String>` 是多个。
  词表（`LibraryData.categories/tags`）和归属分开存，仓库上存的是**名字本身**，不是 id。
- **名字即身份**：比较忽略大小写（`taxonomy::model::same_name`），存第一次出现的写法。
  所有名字规则（清洗/比较/去重/色相）都在 `taxonomy/model.rs` 里，是纯函数，先加测试再改。
- **词表操作只有三个入口**：`Library::{add_name, rename_name, remove_name}`，改名与删除
  会自动改到仓库上的引用（`repoint_references`）。别在视图里直接改 `data.categories`。
- **`set_repo_category` / `set_repo_tags` 拒绝词表外的名字**（返回 `false` 且什么都不改）。
  界面上的「新建」是「先 `add_name` 再挂上」，所以正常路径碰不到拒绝。
- **`normalize_taxonomy` 只在加载时跑，且只改内存**（不偷偷改写手写的 JSON）：把仓库引用的
  未登记名字收进词表、摘掉不能用的名字、标签去重并按词表顺序排。
- **列表行的分类色块挂第一行**。`List` 要求所有行等高，行数不能随内容变（见 list_delegate 注释）。

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

## 验证

- 界面无法在本环境截图（`Get-Process` 看不到沙箱里的应用进程）。
  验证后台行为靠轮询 `%APPDATA%\ngy-fav-project\library.json`，见今日日志。
- 后台跑 GUI 应用要用 `run_in_background`，普通 `&` 会随 shell 退出被杀。

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

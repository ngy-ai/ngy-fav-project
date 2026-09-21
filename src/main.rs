//! GitHub 仓库管理与发现工具。
//!
//! 分层：`shared`（时间、网络、文件）→ `github`（抓取与解析）→
//! `repos` / `taxonomy` / `discovery` / `settings`（领域与界面）→ `app`（外壳）。

mod app;
pub mod db;
pub mod discovery;
pub mod github;
pub mod library;
pub mod repos;
pub mod settings;
pub mod shared;
pub mod taxonomy;

use gpui_kit::component::Root;
use gpui_kit::*;

use crate::app::AppShell;
use crate::discovery::DiscoveryRunner;
use crate::library::Library;

fn main() {
    // `AllAssets` 而不是 `Assets`：默认资源包只含 101 个组件图标，
    // 而导航用到的 Radar / Play / RefreshCw 之类在完整目录里。
    gpui_kit::application()
        .with_assets(gpui_kit::assets::AllAssets)
        .run(|cx| {
            gpui_kit::init(cx);
            cx.set_window_appearance(Some(WindowAppearance::Light));

            cx.spawn(async move |cx| {
                cx.open_window(
                    WindowOptions {
                        window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                            point(px(120.), px(90.)),
                            size(px(1180.), px(760.)),
                        ))),
                        titlebar: Some(TitlebarOptions {
                            title: Some("GitHub 收藏".into()),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                    |window, cx| {
                        // 领域数据与后台任务都在这一层创建：三个页面共用同一份。
                        let library = cx.new(|_| Library::load());
                        let runner = cx.new(|cx| DiscoveryRunner::new(library.clone(), cx));
                        let shell = cx.new(|cx| AppShell::new(library, runner, window, cx));
                        cx.new(|cx| Root::new(shell, window, cx))
                    },
                )
                .expect("打开窗口失败");
            })
            .detach();
        });
}

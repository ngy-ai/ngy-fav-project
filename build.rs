//! 把 `assets/icon.ico` 嵌进可执行文件。
//!
//! Windows 的窗口与任务栏图标取自 exe 的资源段——gpui 的 `WindowOptions::icon`
//! 只对 X11 生效，所以只能在构建这一层嵌。
//!
//! 嵌入失败只发 warning、不中断构建：图标是装饰，不该因为机器上找不到 `rc.exe`
//! 就让整个项目编译不过。但也不能装作没事，warning 会出现在构建输出里。

// 只在 Windows 上用到；放到外面会在别的平台变成未使用导入。
#[cfg(windows)]
use std::path::{Path, PathBuf};

fn main() {
    // 监视整个 assets 目录，不要只写 assets/icon.ico。
    //
    // 监视单个文件时，如果首次构建它还不存在，cargo 会把「缺失」记进指纹，
    // 之后再生成图标也不会重跑本脚本——表现是图标永远嵌不进去，而构建输出里
    // 只有那条旧警告（cargo 重放上次的 warning），`cargo build` 一秒内 Finished。
    // 监视目录则没这个问题：目录里新增文件同样会触发重跑。
    println!("cargo:rerun-if-changed=assets");

    #[cfg(windows)]
    embed_icon();
}

#[cfg(windows)]
fn embed_icon() {
    let icon = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("assets")
        .join("icon.ico");
    // 版本号跟着 Cargo.toml 走，别在这儿写死一份会过期的东西。
    let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.1.0".to_string());

    let mut resource = winresource::WindowsResource::new();
    resource.set_icon(&icon.to_string_lossy());
    resource.set("FileDescription", "GitHub 仓库管理与发现工具");
    resource.set("ProductName", "GitHub 收藏");
    resource.set("FileVersion", &format!("{version}.0"));
    resource.set("ProductVersion", &format!("{version}.0"));

    let Some(rc_dir) = find_rc_dir() else {
        println!(
            "cargo:warning=没找到 Windows SDK 里的 rc.exe，跳过嵌入图标；\
             设 RC_PATH 指向 rc.exe 可以绕过自动查找"
        );
        return;
    };
    resource.set_toolkit_path(&rc_dir.to_string_lossy());

    if let Err(error) = resource.compile() {
        println!("cargo:warning=嵌入图标失败，可执行文件将没有图标：{error}");
    }
}

/// 找到装着 `rc.exe` 的那个目录。
///
/// 不走 winresource 默认那条路（它调 `reg.exe` 查注册表拿 SDK 根目录）：
/// 直接扫 `Windows Kits\10\bin` 下的版本目录，取版本号最大的一个。
/// 这样不依赖注册表，也不会碰上被安全策略拦下的程序。
#[cfg(windows)]
fn find_rc_dir() -> Option<PathBuf> {
    // 用户显式指定了就照办。
    if let Some(rc) = std::env::var_os("RC_PATH") {
        return PathBuf::from(rc).parent().map(Path::to_path_buf);
    }

    const ROOTS: [&str; 2] = [
        r"C:\Program Files (x86)\Windows Kits\10\bin",
        r"C:\Program Files\Windows Kits\10\bin",
    ];

    for root in ROOTS {
        let Ok(entries) = std::fs::read_dir(root) else {
            continue;
        };
        // 目录名形如 10.0.26100.0，字典序就是版本序。
        let mut versions: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|dir| dir.join("x64").join("rc.exe").is_file())
            .collect();
        versions.sort();
        if let Some(latest) = versions.pop() {
            return Some(latest.join("x64"));
        }
    }

    None
}

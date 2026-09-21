//! 数据文件的存放位置与原子写入。

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// 应用目录名，同时用于数据目录与配置目录。
const APP_DIR: &str = "ngy-fav-project";

/// 覆盖数据目录的环境变量（测试与多份数据副本用）。
const HOME_ENV: &str = "NGY_FAV_HOME";

/// 数据目录。按平台约定选择，优先尊重环境变量。
pub fn data_dir() -> PathBuf {
    if let Some(custom) = std::env::var_os(HOME_ENV).filter(|value| !value.is_empty()) {
        return PathBuf::from(custom);
    }

    #[cfg(target_os = "windows")]
    {
        if let Some(appdata) = std::env::var_os("APPDATA") {
            return PathBuf::from(appdata).join(APP_DIR);
        }
    }

    #[cfg(target_os = "macos")]
    {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home)
                .join("Library/Application Support")
                .join(APP_DIR);
        }
    }

    if let Some(config) = std::env::var_os("XDG_CONFIG_HOME").filter(|value| !value.is_empty()) {
        return PathBuf::from(config).join(APP_DIR);
    }
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join(".config").join(APP_DIR);
    }

    PathBuf::from(".").join(APP_DIR)
}

/// 仓库库数据文件。
pub fn default_library_path() -> PathBuf {
    data_dir().join("library.json")
}

/// 读取文本；文件不存在返回 `Ok(None)`，其余 IO 错误原样上报。
pub fn read_text(path: &Path) -> io::Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// 先写临时文件再 `rename`，避免进程在写一半时被终止留下半个 JSON。
pub fn write_atomic(path: &Path, contents: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let temp = path.with_extension("json.tmp");
    fs::write(&temp, contents)?;

    // Windows 的 rename 不能覆盖已存在的文件。
    if let Err(error) = fs::rename(&temp, path) {
        if path.exists() {
            fs::remove_file(path)?;
            fs::rename(&temp, path)?;
        } else {
            let _ = fs::remove_file(&temp);
            return Err(error);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_replaces_existing_content() {
        let dir = std::env::temp_dir().join("ngy-fav-store-test");
        let path = dir.join("library.json");

        write_atomic(&path, "{\"a\":1}").expect("首次写入");
        assert_eq!(read_text(&path).unwrap().unwrap(), "{\"a\":1}");

        write_atomic(&path, "{\"a\":2}").expect("覆盖写入");
        assert_eq!(read_text(&path).unwrap().unwrap(), "{\"a\":2}");

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_file_is_not_an_error() {
        let path = std::env::temp_dir().join("ngy-fav-does-not-exist/library.json");
        assert!(read_text(&path).unwrap().is_none());
    }
}

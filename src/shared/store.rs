//! 数据放在哪里。
//!
//! 数据只有一份，在一个 SQLite 库里（见 [`default_db_path`]）。这里只管位置，
//! 读写本身归 `db` 模块——落盘的一次性保障（事务、WAL）由 SQLite 给，不再自己
//! 写临时文件再改名。
//!
//! 唯一还需要动文件的地方是[留档](archive_file)：迁移过的旧 `library.json` 不能
//! 悄悄删掉，改名留一份，用户哪天想找回什么还有地方看。

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::shared::clock::now_unix;

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

/// 数据库文件。仓库、分类树、标签、发现源与设置都在里面。
pub fn default_db_path() -> PathBuf {
    data_dir().join("library.db")
}

/// 把一个文件改名留档，名字里带上时间戳。
///
/// 用于迁移过的旧数据文件：新库已经接手了，但旧文件是用户的数据，删掉就没有了，
/// 留一份改过名的在旁边最稳妥。
pub fn archive_file(path: &Path, tag: &str) -> io::Result<PathBuf> {
    let stem = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().to_string())
        .unwrap_or_else(|| "backup".to_string());
    let archived = path.with_file_name(format!("{stem}.{tag}-{}.json", now_unix()));
    fs::rename(path, &archived)?;
    Ok(archived)
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn archiving_renames_instead_of_deleting() {
        let dir = std::env::temp_dir().join(format!("ngy-fav-store-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("library.json");
        fs::write(&path, "{\"repos\":[]}").unwrap();

        let archived = archive_file(&path, "legacy").expect("改名留档");
        assert!(!path.exists(), "原名让出来了");
        assert!(archived.exists(), "内容还在，只是换了个名字");
        assert!(
            archived.file_name().unwrap().to_string_lossy().contains("legacy"),
            "名字里带上留档的原因：{archived:?}"
        );

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_database_lives_beside_the_rest_of_the_data() {
        let path = default_db_path();
        assert_eq!(path.file_name().unwrap(), "library.db");
        assert_eq!(path.parent(), Some(data_dir().as_path()));
    }
}

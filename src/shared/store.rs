//! 数据放在哪里。
//!
//! 数据只有一份，在一个 SQLite 库里（见 [`db_path`]）。这里管两件事：**位置**，
//! 以及**位置的记录**。
//!
//! 位置分成两层，这是数据能搬走的前提：
//!
//! - **配置目录**：固定不动（Windows 上是 `%APPDATA%\ngy-fav-project`），
//!   里面只有一个几字节的 `location.json`。
//! - **数据目录**：默认就是配置目录，但用户可以在设置里改到别的盘。改完之后
//!   `location.json` 里记着新位置，数据本身搬过去。
//!
//! 一旦数据可以搬走，就必须有一个**不会跟着搬**的地方记住「搬到哪去了」，
//! 否则下次启动根本找不到库。那就是配置目录存在的全部理由。
//!
//! 库的读写归 `db` 模块——落盘的一次性保障（事务、WAL）由 SQLite 给。这里
//! 唯一会动文件的地方是[留档](archive_file)：迁移过的旧 `library.json` 不能
//! 悄悄删掉，改名留一份。

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::shared::clock::now_unix;
use crate::shared::relocate::MoveStrategy;

/// 应用目录名，同时用于数据目录与配置目录的默认值。
const APP_DIR: &str = "ngy-fav-project";

/// 覆盖配置目录的环境变量（测试与多份数据副本用）。
const HOME_ENV: &str = "NGY_FAV_HOME";

/// 数据库文件名。
pub const DB_FILE: &str = "library.db";

/// 旧版数据文件名。
pub const LEGACY_FILE: &str = "library.json";

/// 记住数据放在哪的配置文件名。
const LOCATION_FILE: &str = "location.json";

/// 配置目录。它自己永远不动，数据搬到哪个盘都不影响它。
pub fn config_dir() -> PathBuf {
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

/// 位置配置文件的路径。
pub fn location_path() -> PathBuf {
    config_dir().join(LOCATION_FILE)
}

/// `location.json` 的内容。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Location {
    /// 数据目录。`None` 表示没改过，就用配置目录本身。
    pub data_dir: Option<PathBuf>,
    /// 一次没能当场做完的搬家：文件还在 `from`，配置已经指向新位置，
    /// 下次启动接着搬。
    pub pending_move: Option<PendingMove>,
}

/// 还没做完的一次搬家。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingMove {
    pub from: PathBuf,
    pub strategy: MoveStrategy,
}

/// 读位置配置。
///
/// 文件不在、读不出来、内容不像话，一律当「没改过」：一个坏掉的指针文件不该
/// 让程序连默认位置的数据都打不开——那才是真把数据弄丢。
pub fn read_location_from(path: &Path) -> Location {
    fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// 写位置配置。
pub fn write_location_to(path: &Path, location: &Location) -> io::Result<()> {
    if let Some(parent) = path.parent().filter(|parent| !parent.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    let text = serde_json::to_string_pretty(location).unwrap_or_else(|_| "{}".to_string());
    fs::write(path, text)
}

pub fn read_location() -> Location {
    read_location_from(&location_path())
}

pub fn write_location(location: &Location) -> io::Result<()> {
    write_location_to(&location_path(), location)
}

/// 从「配置目录 + 位置配置」推出数据目录。
///
/// 默认位置就是配置目录本身：没改过位置的人，数据一直在老地方，升级不该让它搬家。
pub fn data_dir_from(config: &Path, location: &Location) -> PathBuf {
    match &location.data_dir {
        Some(dir) if !dir.as_os_str().is_empty() => dir.clone(),
        _ => config.to_path_buf(),
    }
}

/// 当前的数据目录。
pub fn data_dir() -> PathBuf {
    data_dir_from(&config_dir(), &read_location())
}

/// 某个目录里的库文件。
pub fn db_path(dir: &Path) -> PathBuf {
    dir.join(DB_FILE)
}

/// 默认位置（或配置指定的位置）上的库文件。
pub fn default_db_path() -> PathBuf {
    db_path(&data_dir())
}

/// 这个目录里有没有库文件。用来区分「新位置是空的」和「那边已经有数据了」。
pub fn has_database(dir: &Path) -> bool {
    dir.join(DB_FILE).is_file()
}

/// 目录里属于本程序的数据文件：主库、WAL 与共享内存、旧版与留档的 JSON。
///
/// 只认自己写下的名字（`library` 开头，且只由字母数字与 `.` `-` `_` 组成），
/// 免得把用户顺手放在同一个目录里的别的东西一起搬走。
pub fn data_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        .filter(|path| {
            path.file_name().is_some_and(|name| {
                let name = name.to_string_lossy();
                // `library.` 开头的才是自己写的：`library2.db` 或 `library 笔记`
                // 都是别人放在这儿的东西，不该被我们搬走。
                name.starts_with("library.")
                    && name
                        .chars()
                        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_'))
            })
        })
        .collect();
    files.sort();
    files
}

/// 丢掉目录里的 WAL 与共享内存文件。
///
/// 这两个文件只对「生成它们的那一次会话」有意义。当我们要把**另一个库**盖到这个
/// 目录上来的时候，如果不先丢掉，下一次打开会拿一份配不上的 WAL 去恢复，那是真
/// 会毁数据的。它们本来就是临时文件，删掉不损失任何已提交的数据。
pub fn discard_sidecars(dir: &Path) -> io::Result<()> {
    for path in data_files(dir) {
        let Some(name) = path.file_name().map(|name| name.to_string_lossy().to_string()) else {
            continue;
        };
        if name.ends_with("-wal") || name.ends_with("-shm") {
            fs::remove_file(&path)?;
        }
    }
    Ok(())
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

    fn temp_dir(name: &str) -> PathBuf {
        use std::time::{SystemTime, UNIX_EPOCH};
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default();
        let dir = std::env::temp_dir().join(format!(
            "ngy-fav-store-{name}-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn archiving_renames_instead_of_deleting() {
        let dir = temp_dir("archive");
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
        let dir = temp_dir("layout");
        assert_eq!(db_path(&dir).file_name().unwrap(), DB_FILE);
        assert_eq!(db_path(&dir).parent(), Some(dir.as_path()));

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_fresh_install_keeps_the_database_where_it_always_was() {
        let config = temp_dir("default-dir");
        assert_eq!(
            data_dir_from(&config, &Location::default()),
            config,
            "没改过位置的人，数据不该因为这次升级搬走"
        );

        let moved = Location {
            data_dir: Some(PathBuf::from("D:/ngy-fav")),
            pending_move: None,
        };
        assert_eq!(data_dir_from(&config, &moved), PathBuf::from("D:/ngy-fav"));

        fs::remove_dir_all(&config).ok();
    }

    #[test]
    fn the_location_round_trips_and_a_broken_one_means_default() {
        let dir = temp_dir("location");
        let path = dir.join(LOCATION_FILE);

        let location = Location {
            data_dir: Some(PathBuf::from("D:/数据/ngy")),
            pending_move: Some(PendingMove {
                from: PathBuf::from("C:/旧位置"),
                strategy: MoveStrategy::Merge,
            }),
        };
        write_location_to(&path, &location).unwrap();
        assert_eq!(read_location_from(&path), location);

        fs::write(&path, "{ 这不是 json").unwrap();
        assert_eq!(read_location_from(&path), Location::default());

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn moving_the_data_leaves_the_config_file_behind() {
        // 配置目录与数据目录是两回事：数据搬走了，指针文件还得在原地等着被读到。
        let config = temp_dir("config");
        let data = temp_dir("data");
        let location = Location {
            data_dir: Some(data.clone()),
            pending_move: None,
        };

        let path = config.join(LOCATION_FILE);
        write_location_to(&path, &location).unwrap();
        assert!(path.exists());
        assert!(!data.join(LOCATION_FILE).exists(), "指针不该跟着数据走");

        fs::remove_dir_all(&config).ok();
        fs::remove_dir_all(&data).ok();
    }

    #[test]
    fn only_this_programs_files_are_considered_data() {
        let dir = temp_dir("files");
        for name in [
            "library.db",
            "library.db-wal",
            "library.db-shm",
            "library.json",
            "library.legacy-1700000000.json",
        ] {
            fs::write(dir.join(name), "x").unwrap();
        }
        // 用户顺手放在旁边的别的东西不算。
        fs::write(dir.join("我的笔记.txt"), "x").unwrap();
        fs::write(dir.join("library2.db"), "x").unwrap();

        let names: Vec<String> = data_files(&dir)
            .iter()
            .map(|path| path.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert_eq!(
            names,
            [
                "library.db",
                "library.db-shm",
                "library.db-wal",
                "library.json",
                "library.legacy-1700000000.json"
            ]
        );

        // 目录不存在时不该炸，只当没有。
        assert!(data_files(&dir.join("没有这个目录")).is_empty());

        fs::remove_dir_all(&dir).ok();
    }
}

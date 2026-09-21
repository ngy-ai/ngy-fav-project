//! 把数据文件从一个目录搬到另一个目录。
//!
//! 「换数据位置」最底层的动作就是它。这里只动文件，不管配置也不管内存，所以
//! 每一种情形都能单独造出来测：跨盘、目标已有同名文件、目标目录还不存在。
//!
//! 一个文件怎么算搬成功：**源没了、目标在**。同盘时改名一步到位；跨盘改名会
//! 失败（Windows 上是 `ERROR_NOT_SAME_DEVICE`），退回「复制 + 删源」。复制成功
//! 却删不掉源文件的那种情况要单独记账——那正是「新旧位置同时存在」，不能让
//! 用户以为已经搬干净了。

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// 新位置已经有数据时，用户挑的处理办法。
///
/// 三种都**不会**把两边的数据都弄丢：`UseNewOnly` 一个文件都不动，
/// `Overwrite` 只盖同名文件，`Merge` 先把旧的并进去再删旧的。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MoveStrategy {
    /// 只用新位置的数据：旧数据原地不动，留在那儿当备份。
    UseNewOnly,
    /// 把旧数据搬过去，盖掉新位置已有的同名文件。
    Overwrite,
    /// 把旧数据并进新位置的库，然后把旧文件删掉。
    Merge,
}

impl MoveStrategy {
    /// 三种策略。界面上按这个顺序摆，从「最不破坏」到「最彻底」。
    pub const ALL: [MoveStrategy; 3] = [Self::UseNewOnly, Self::Overwrite, Self::Merge];

    /// 界面上那个按钮写的字。
    pub fn label(self) -> &'static str {
        match self {
            Self::UseNewOnly => "仅用新位置",
            Self::Overwrite => "移动并覆盖",
            Self::Merge => "移动并合并",
        }
    }

    /// 一句话说清这个选择的后果，摆在按钮下面。
    pub fn hint(self) -> &'static str {
        match self {
            Self::UseNewOnly => "不搬任何文件，直接改用新位置的数据；旧数据留在原处，可以自己删。",
            Self::Overwrite => "把当前数据搬到新位置，盖掉那边已有的同名文件，那边的数据不再保留。",
            Self::Merge => "把当前数据并进新位置的库，之后再删掉旧文件；同一个仓库以新位置那份为准。",
        }
    }
}

/// 搬完之后的账。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MoveReport {
    /// 落在新位置的文件。
    pub moved: Vec<PathBuf>,
    /// 因为目标已有同名文件而没动的源文件。
    pub skipped: Vec<PathBuf>,
    /// 复制过去了、源文件却没删掉的：两边都在。
    pub left_behind: Vec<PathBuf>,
}

impl MoveReport {
    pub fn is_empty(&self) -> bool {
        self.moved.is_empty() && self.skipped.is_empty() && self.left_behind.is_empty()
    }
}

/// 把一批文件搬到 `to` 目录。目标目录不存在就建出来。
///
/// `overwrite = false` 时目标已有的文件原地不动，记进 [`MoveReport::skipped`]：
/// 「两边都有」该怎么办是上面的决定，这里不替它做主。
pub fn move_files(files: &[PathBuf], to: &Path, overwrite: bool) -> io::Result<MoveReport> {
    fs::create_dir_all(to)?;

    let mut report = MoveReport::default();
    for file in files {
        let Some(name) = file.file_name() else {
            continue;
        };
        let destination = to.join(name);
        // 本来就在目标目录里（同一个目录搬到自己），没必要碰。
        if destination == *file {
            continue;
        }
        if destination.exists() && !overwrite {
            report.skipped.push(file.clone());
            continue;
        }

        match move_one(file, &destination)? {
            LeftBehind::No => report.moved.push(destination),
            LeftBehind::Yes => {
                report.moved.push(destination);
                report.left_behind.push(file.clone());
            }
        }
    }
    Ok(report)
}

/// 删掉一批文件，返回真的删掉了的那些。
///
/// 只在数据**已经**安全地躺在别处之后才用：这是「移动并合并」的最后一步，
/// 那时旧文件里的东西已经并进新库了。
pub fn remove_files(files: &[PathBuf]) -> io::Result<Vec<PathBuf>> {
    let mut gone = Vec::new();
    for file in files {
        fs::remove_file(file)?;
        gone.push(file.clone());
    }
    Ok(gone)
}

enum LeftBehind {
    No,
    Yes,
}

/// 搬一个文件。先试改名（同盘一瞬间的事），跨盘再退回复制加删除。
fn move_one(from: &Path, to: &Path) -> io::Result<LeftBehind> {
    if fs::rename(from, to).is_ok() {
        return Ok(LeftBehind::No);
    }
    // rename 失败的原因不止跨盘（也可能是权限），所以复制这一步失败就如实报错，
    // 不把它当成「那就算了」。
    fs::copy(from, to)?;
    match fs::remove_file(from) {
        Ok(()) => Ok(LeftBehind::No),
        Err(_) => Ok(LeftBehind::Yes),
    }
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
            "ngy-fav-relocate-{name}-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn moving_takes_the_file_away_from_the_old_place() {
        let from = temp_dir("move-from");
        let to = temp_dir("move-to");
        let source = from.join("library.db");
        fs::write(&source, "库").unwrap();

        let report = move_files(std::slice::from_ref(&source), &to, true).unwrap();

        assert!(!source.exists(), "搬走之后原地不该还留着");
        assert_eq!(fs::read_to_string(to.join("library.db")).unwrap(), "库");
        assert_eq!(report.moved, vec![to.join("library.db")]);
        assert!(report.left_behind.is_empty());

        fs::remove_dir_all(&from).ok();
        fs::remove_dir_all(&to).ok();
    }

    #[test]
    fn an_existing_file_is_skipped_unless_overwriting_was_asked_for() {
        let from = temp_dir("keep-from");
        let to = temp_dir("keep-to");
        let source = from.join("library.db");
        fs::write(&source, "旧的").unwrap();
        fs::write(to.join("library.db"), "那边本来就有的").unwrap();

        let kept = move_files(std::slice::from_ref(&source), &to, false).unwrap();
        assert_eq!(kept.skipped, vec![source.clone()]);
        assert!(kept.moved.is_empty());
        assert!(source.exists(), "没搬就不能把源删了");
        assert_eq!(fs::read_to_string(to.join("library.db")).unwrap(), "那边本来就有的");

        let replaced = move_files(std::slice::from_ref(&source), &to, true).unwrap();
        assert_eq!(replaced.moved, vec![to.join("library.db")]);
        assert!(!source.exists());
        assert_eq!(fs::read_to_string(to.join("library.db")).unwrap(), "旧的");

        fs::remove_dir_all(&from).ok();
        fs::remove_dir_all(&to).ok();
    }

    #[test]
    fn the_destination_directory_is_created_on_demand() {
        let from = temp_dir("create-from");
        let parent = temp_dir("create-parent");
        let to = parent.join("更深/一层");
        let source = from.join("library.db");
        fs::write(&source, "库").unwrap();

        move_files(&[source], &to, true).unwrap();
        assert!(to.join("library.db").exists(), "缺的目录要自己建出来");

        fs::remove_dir_all(&from).ok();
        fs::remove_dir_all(&parent).ok();
    }

    /// 搬不动的文件必须报错，而不是记一笔「搬过了」就走。
    #[test]
    fn a_missing_source_is_an_error_not_a_silent_success() {
        let from = temp_dir("missing-from");
        let to = temp_dir("missing-to");
        let ghost = from.join("library.db");

        assert!(move_files(&[ghost], &to, true).is_err());

        fs::remove_dir_all(&from).ok();
        fs::remove_dir_all(&to).ok();
    }

    #[test]
    fn every_strategy_says_what_it_does() {
        for strategy in [
            MoveStrategy::UseNewOnly,
            MoveStrategy::Overwrite,
            MoveStrategy::Merge,
        ] {
            assert!(!strategy.label().is_empty());
            assert!(!strategy.hint().is_empty());
        }
        // 存进配置文件之后要能读回来，否则「重启后再搬」会认不出当初选的是哪个。
        let text = serde_json::to_string(&MoveStrategy::Merge).unwrap();
        assert_eq!(
            serde_json::from_str::<MoveStrategy>(&text).unwrap(),
            MoveStrategy::Merge
        );
    }
}

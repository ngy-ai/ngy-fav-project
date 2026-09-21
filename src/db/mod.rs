//! SQLite 存储层。
//!
//! 整个应用只有一个连接，读写都发生在主线程上——`Library` 是 GPUI 里的单线程
//! 实体，后台抓取回到主线程之后才落库，所以不需要连接池，也不存在跨线程问题。
//!
//! 分成几块：`schema` 管表结构与版本迁移，`repos` / `taxonomy` / `sources`
//! 各自管一组表的读写，`legacy` 负责把旧版 `library.json` 导进来。

pub mod legacy;
pub mod repos;
pub mod schema;
pub mod sources;
pub mod taxonomy;

use std::path::Path;

use rusqlite::Connection;

/// 存储层错误。
///
/// 刻意做成一个显式枚举而不是 `anyhow`：调用方要分开处理「库是更新版本的程序写的」
/// 和「这一次写失败了」，前者不能当成一次普通的写盘失败吞掉。
#[derive(Debug)]
pub enum DbError {
    Sqlite(rusqlite::Error),
    Io(std::io::Error),
    /// 待导入的文件内容不是一个能认的旧版数据文件。
    Format(String),
    /// 库文件的结构版本比本程序认识的还新。
    SchemaTooNew { found: i64, supported: i64 },
}

impl std::fmt::Display for DbError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Sqlite(error) => write!(f, "{error}"),
            Self::Io(error) => write!(f, "{error}"),
            Self::Format(reason) => write!(f, "{reason}"),
            Self::SchemaTooNew { found, supported } => write!(
                f,
                "数据库结构版本是 {found}，本程序只认到 {supported}；请升级程序再打开"
            ),
        }
    }
}

impl From<rusqlite::Error> for DbError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error)
    }
}

pub type DbResult<T> = Result<T, DbError>;

/// 一个数据库连接。
pub struct Db {
    conn: Connection,
}

impl Db {
    /// 打开（必要时创建）一个库文件。
    pub fn open(path: &Path) -> DbResult<Self> {
        if let Some(parent) = path.parent().filter(|parent| !parent.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).map_err(DbError::Io)?;
        }
        Self::prepare(Connection::open(path)?)
    }

    /// 内存库，测试用。
    pub fn open_in_memory() -> DbResult<Self> {
        Self::prepare(Connection::open_in_memory()?)
    }

    fn prepare(conn: Connection) -> DbResult<Self> {
        // 外键默认是关的，而分类树的级联删除完全靠它——不打开就等于没有约束。
        // WAL + NORMAL：提交仍然写日志，只是不做额外的目录同步，够用且快得多。
        conn.execute_batch(
            "PRAGMA foreign_keys = ON;
             PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;",
        )?;
        schema::migrate(&conn)?;
        Ok(Self { conn })
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    /// 在一个事务里做一批改动。中途出错整批回滚，内存不会拿到一个只改了一半的结果。
    pub fn transaction<T>(
        &self,
        f: impl FnOnce(&rusqlite::Transaction<'_>) -> DbResult<T>,
    ) -> DbResult<T> {
        let tx = self.conn.unchecked_transaction()?;
        let value = f(&tx)?;
        tx.commit()?;
        Ok(value)
    }
}

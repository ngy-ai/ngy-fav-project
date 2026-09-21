//! 表结构与版本迁移。
//!
//! 版本号存在 sqlite 自己的 `user_version` 里，不额外建一张版本表——它本来就是
//! 为这件事准备的。`migrate` 只做「从 0 建到当前版本」，日后加字段再往上叠。

use rusqlite::Connection;

use super::{DbError, DbResult};

/// 当前表结构版本。
pub const SCHEMA_VERSION: i64 = 1;

/// 建表语句。全部 `IF NOT EXISTS`，重复执行是安全的。
///
/// 几点刻意的地方：
///
/// - 分类是**一棵真树**：`parent_id` 自引用，`ON DELETE CASCADE` 让「删分类连子分类
///   一起删」由数据库保证，而不是靠应用一层层扫。
/// - `name_key` 存小写后的名字，唯一索引建在它上面。sqlite 的 `lower()` 只认 ASCII，
///   而这里的名字可能是任意 Unicode，所以小写化在 Rust 里做，库里只存结果。
/// - 仓库引用分类用 `category_id`，不是分类名：改分类名/挪动子树都不必回头改仓库，
///   这正是把分类做成树换来的好处。
/// - 分类被删时 `ON DELETE SET NULL` 把仓库退回未分类，仓库本身不会被删。
/// - 标签是扁平的，仓库与标签的多对多落在 `repo_tags` 上。
const DDL: &str = r#"
CREATE TABLE IF NOT EXISTS categories (
    id        INTEGER PRIMARY KEY AUTOINCREMENT,
    parent_id INTEGER REFERENCES categories(id) ON DELETE CASCADE,
    name      TEXT    NOT NULL,
    name_key  TEXT    NOT NULL,
    position  INTEGER NOT NULL DEFAULT 0
);

-- parent_id 是 NULL 的根分类：sqlite 的唯一索引里 NULL 互不相等，
-- 不套一层 ifnull 的话两个同名根分类能同时存在。
CREATE UNIQUE INDEX IF NOT EXISTS categories_sibling
    ON categories(ifnull(parent_id, 0), name_key);

CREATE TABLE IF NOT EXISTS tags (
    id       INTEGER PRIMARY KEY AUTOINCREMENT,
    name     TEXT    NOT NULL,
    name_key TEXT    NOT NULL,
    position INTEGER NOT NULL DEFAULT 0
);

CREATE UNIQUE INDEX IF NOT EXISTS tags_name ON tags(name_key);

CREATE TABLE IF NOT EXISTS repos (
    id             TEXT PRIMARY KEY,
    full_name      TEXT    NOT NULL,
    url            TEXT    NOT NULL,
    description    TEXT,
    description_zh TEXT,
    discovered_at  INTEGER NOT NULL,
    created_at     INTEGER,
    source         TEXT,
    language       TEXT,
    stars          INTEGER,
    category_id    INTEGER REFERENCES categories(id) ON DELETE SET NULL
);

-- 列表默认按发现时间倒序翻页，这个索引就是给它用的。
CREATE INDEX IF NOT EXISTS repos_discovered
    ON repos(discovered_at DESC, full_name);

CREATE TABLE IF NOT EXISTS repo_tags (
    repo_id TEXT    NOT NULL REFERENCES repos(id) ON DELETE CASCADE,
    tag_id  INTEGER NOT NULL REFERENCES tags(id)  ON DELETE CASCADE,
    PRIMARY KEY (repo_id, tag_id)
);

CREATE INDEX IF NOT EXISTS repo_tags_tag ON repo_tags(tag_id);

CREATE TABLE IF NOT EXISTS sources (
    url         TEXT PRIMARY KEY,
    name        TEXT    NOT NULL,
    enabled     INTEGER NOT NULL DEFAULT 1,
    position    INTEGER NOT NULL DEFAULT 0,
    started_at  INTEGER,
    finished_at INTEGER,
    found       INTEGER,
    added       INTEGER,
    skipped     INTEGER,
    error       TEXT
);

CREATE TABLE IF NOT EXISTS settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
"#;

pub fn migrate(conn: &Connection) -> DbResult<()> {
    let found: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if found > SCHEMA_VERSION {
        // 硬失败而不是硬着头皮打开：结构不认识的时候照常写，会把用户的库改坏。
        return Err(DbError::SchemaTooNew {
            found,
            supported: SCHEMA_VERSION,
        });
    }
    if found == SCHEMA_VERSION {
        return Ok(());
    }

    conn.execute_batch(DDL)?;
    conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrate_creates_every_table_and_is_repeatable() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        // 第二次跑不能报错——每次打开库都会走一遍。
        migrate(&conn).unwrap();

        for table in [
            "categories",
            "tags",
            "repos",
            "repo_tags",
            "sources",
            "settings",
        ] {
            let count: i64 = conn
                .query_row(
                    "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                    [table],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(count, 1, "缺少表 {table}");
        }
    }

    #[test]
    fn a_newer_schema_is_refused_instead_of_silently_written() {
        let conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "user_version", SCHEMA_VERSION + 1)
            .unwrap();

        match migrate(&conn) {
            Err(DbError::SchemaTooNew { found, supported }) => {
                assert_eq!(found, SCHEMA_VERSION + 1);
                assert_eq!(supported, SCHEMA_VERSION);
            }
            other => panic!("期望拒绝更新版本的库，得到 {other:?}"),
        }
    }

    #[test]
    fn deleting_a_category_cascades_to_its_subtree_and_uncategorizes_repos() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        migrate(&conn).unwrap();

        conn.execute(
            "INSERT INTO categories (id, parent_id, name, name_key) VALUES (1, NULL, '开发', '开发')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO categories (id, parent_id, name, name_key) VALUES (2, 1, '前端', '前端')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO repos (id, full_name, url, discovered_at, category_id)
             VALUES ('a/one', 'a/one', 'https://github.com/a/one', 1, 2)",
            [],
        )
        .unwrap();

        conn.execute("DELETE FROM categories WHERE id = 1", []).unwrap();

        let categories: i64 = conn
            .query_row("SELECT count(*) FROM categories", [], |row| row.get(0))
            .unwrap();
        assert_eq!(categories, 0, "子分类应当跟着删掉");

        let repo: Option<i64> = conn
            .query_row("SELECT category_id FROM repos WHERE id = 'a/one'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(repo, None, "仓库回到未分类，而不是被删掉");
    }

    #[test]
    fn sibling_names_are_unique_but_different_parents_may_repeat() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        migrate(&conn).unwrap();

        conn.execute(
            "INSERT INTO categories (id, parent_id, name, name_key) VALUES (1, NULL, '开发', '开发')",
            [],
        )
        .unwrap();
        // 同名根分类必须被挡住。
        assert!(
            conn.execute(
                "INSERT INTO categories (parent_id, name, name_key) VALUES (NULL, '开发', '开发')",
                [],
            )
            .is_err()
        );
        // 不同上级下面可以同名。
        conn.execute(
            "INSERT INTO categories (parent_id, name, name_key) VALUES (1, '开发', '开发')",
            [],
        )
        .unwrap();
        // 同一个上级下面再来一个同名的不行。
        assert!(
            conn.execute(
                "INSERT INTO categories (parent_id, name, name_key) VALUES (1, '开发', '开发')",
                [],
            )
            .is_err()
        );
    }
}

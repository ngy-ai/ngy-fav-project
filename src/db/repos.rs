//! 仓库表的读写。
//!
//! 分页、搜索、按分类筛选都在这里落成 SQL：列表只取当前这一页，不必把整个库
//! 读进内存再筛。`Library` 负责把「分类名」这类东西解析成 id，这里只管拼条件。

use std::collections::HashMap;

use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension, params, params_from_iter};

use super::DbResult;
use crate::repos::model::{CategoryScope, Repo, RepoId, RepoQuerySpec};

/// 读仓库时的列顺序，`read_row` 按下标取值，两处必须一起改。
const SELECT_COLUMNS: &str = "r.full_name, r.url, r.description, r.description_zh, \
     r.discovered_at, r.created_at, r.source, r.language, r.stars, r.category_id";

/// 列表的排序：发现时间倒序，同一秒入库的按名字排——翻页需要一个稳定的全序，
/// 否则同一条记录可能在两页里各出现一次。
const ORDER_BY: &str = "ORDER BY r.discovered_at DESC, r.full_name ASC";

fn read_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Repo> {
    Ok(Repo {
        full_name: row.get(0)?,
        url: row.get(1)?,
        description: row.get(2)?,
        description_zh: row.get(3)?,
        discovered_at: row.get(4)?,
        created_at: row.get(5)?,
        source: row.get(6)?,
        language: row.get(7)?,
        stars: row.get::<_, Option<i64>>(8)?.map(|value| value as u64),
        category_id: row.get(9)?,
        tags: Vec::new(),
    })
}

/// 把一批仓库的标签一次性取出来补上。
///
/// 单独一次查询而不是在仓库查询里 `join` 出来：标签的顺序由词表决定，
/// 用 `group_concat` 拼字符串就没法保证顺序了。
fn attach_tags(conn: &Connection, repos: &mut [Repo]) -> DbResult<()> {
    if repos.is_empty() {
        return Ok(());
    }

    let placeholders = vec!["?"; repos.len()].join(",");
    let sql = format!(
        "SELECT rt.repo_id, t.name FROM repo_tags rt
         JOIN tags t ON t.id = rt.tag_id
         WHERE rt.repo_id IN ({placeholders})
         ORDER BY t.position, t.id"
    );
    let ids: Vec<String> = repos.iter().map(Repo::id).collect();

    let mut by_repo: HashMap<String, Vec<String>> = HashMap::new();
    let mut statement = conn.prepare(&sql)?;
    let rows = statement.query_map(params_from_iter(ids.iter()), |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    for row in rows {
        let (repo_id, tag) = row?;
        by_repo.entry(repo_id).or_default().push(tag);
    }

    for repo in repos.iter_mut() {
        repo.tags = by_repo.remove(&repo.id()).unwrap_or_default();
    }
    Ok(())
}

/// 读出整个库。启动时用一次，之后所有查询都走分页那条路。
pub fn load_all(conn: &Connection) -> DbResult<Vec<Repo>> {
    let sql = format!("SELECT {SELECT_COLUMNS} FROM repos r {ORDER_BY}");
    let mut statement = conn.prepare(&sql)?;
    let rows = statement.query_map([], read_row)?;
    let mut repos: Vec<Repo> = rows.collect::<rusqlite::Result<_>>()?;
    attach_tags(conn, &mut repos)?;
    Ok(repos)
}

pub fn count(conn: &Connection) -> DbResult<usize> {
    let total: i64 = conn.query_row("SELECT count(*) FROM repos", [], |row| row.get(0))?;
    Ok(total as usize)
}

/// 取一页仓库，附带「符合条件的总数」。
pub fn query_page(conn: &Connection, spec: &RepoQuerySpec) -> DbResult<(Vec<Repo>, usize)> {
    let (where_clause, filter_params) = build_filter(spec);

    let total: i64 = conn.query_row(
        &format!("SELECT count(*) FROM repos r {where_clause}"),
        params_from_iter(filter_params.iter()),
        |row| row.get(0),
    )?;

    let mut page_params = filter_params;
    page_params.push(Value::Integer(spec.limit as i64));
    page_params.push(Value::Integer(spec.offset as i64));

    let sql = format!(
        "SELECT {SELECT_COLUMNS} FROM repos r {where_clause} {ORDER_BY} LIMIT ? OFFSET ?"
    );
    let mut statement = conn.prepare(&sql)?;
    let rows = statement.query_map(params_from_iter(page_params.iter()), read_row)?;
    let mut repos: Vec<Repo> = rows.collect::<rusqlite::Result<_>>()?;
    attach_tags(conn, &mut repos)?;

    Ok((repos, total as usize))
}

/// 把筛选条件拼成 `WHERE` 子句与对应的位置参数。
///
/// 只用无名 `?`：sqlite 的编号参数可以复用，但那样参数集合就必须跟着编号走，
/// 混着 `params_from_iter` 容易错位。重复的值多传几次更不容易写错。
fn build_filter(spec: &RepoQuerySpec) -> (String, Vec<Value>) {
    let mut conditions: Vec<String> = Vec::new();
    let mut params: Vec<Value> = Vec::new();

    match &spec.category {
        CategoryScope::All => {}
        CategoryScope::Uncategorized => conditions.push("r.category_id IS NULL".to_string()),
        CategoryScope::Subtrees(ids) => {
            if ids.is_empty() {
                // 筛选的是一个已经不存在的分类。宁可一条都不匹配，也不要静默退回「全部」——
                // 那会让用户以为筛选没生效。
                conditions.push("0".to_string());
            } else {
                let list = vec!["?"; ids.len()].join(",");
                conditions.push(format!("r.category_id IN ({list})"));
                params.extend(ids.iter().map(|id| Value::Integer(*id)));
            }
        }
    }

    if let Some(language) = &spec.language {
        // 精确相等而不是 LIKE：语言名是枚举值，`Go` 不该把 `Gosu` 也捞进来。
        conditions.push("r.language = ?".to_string());
        params.push(Value::Text(language.clone()));
    }

    for tag in &spec.tags {
        // 每个标签一个 EXISTS，几个标签就是「几个都得挂上」。比 `IN` 再数一遍
        // 更好读，参数顺序也跟标签顺序一一对得上。
        conditions.push(
            "EXISTS (SELECT 1 FROM repo_tags rt JOIN tags t ON t.id = rt.tag_id \
             WHERE rt.repo_id = r.id AND t.name_key = ?)"
                .to_string(),
        );
        params.push(Value::Text(tag.clone()));
    }

    if let Some(pattern) = &spec.search {
        let mut parts = vec![
            "r.full_name LIKE ? ESCAPE '\\'".to_string(),
            "ifnull(r.description, '') LIKE ? ESCAPE '\\'".to_string(),
            "ifnull(r.description_zh, '') LIKE ? ESCAPE '\\'".to_string(),
            "ifnull(r.language, '') LIKE ? ESCAPE '\\'".to_string(),
            "EXISTS (SELECT 1 FROM repo_tags rt JOIN tags t ON t.id = rt.tag_id \
             WHERE rt.repo_id = r.id AND t.name_key LIKE ? ESCAPE '\\')"
                .to_string(),
        ];
        if !spec.search_categories.is_empty() {
            let list = vec!["?"; spec.search_categories.len()].join(",");
            parts.push(format!("r.category_id IN ({list})"));
        }
        conditions.push(format!("({})", parts.join(" OR ")));

        for _ in 0..5 {
            params.push(Value::Text(pattern.clone()));
        }
        params.extend(spec.search_categories.iter().map(|id| Value::Integer(*id)));
    }

    if conditions.is_empty() {
        (String::new(), params)
    } else {
        (format!("WHERE {}", conditions.join(" AND ")), params)
    }
}

/// 把用户输入的搜索词收成一个 `LIKE` 模式。
///
/// `%` 和 `_` 在 `LIKE` 里是通配符，必须先转义——否则搜 `a_b` 会顺带把 `axb`
/// 也搜出来，搜 `50%` 更是把整个库都捞回来。调用方负责先把词转成小写。
pub fn like_pattern(raw: &str) -> String {
    let mut pattern = String::with_capacity(raw.len() + 2);
    pattern.push('%');
    for ch in raw.chars() {
        if matches!(ch, '%' | '_' | '\\') {
            pattern.push('\\');
        }
        pattern.push(ch);
    }
    pattern.push('%');
    pattern
}

/// 还缺描述或创建时间的仓库，最新的排在前面。
pub fn pending_metadata(conn: &Connection, limit: usize) -> DbResult<Vec<RepoId>> {
    let mut statement = conn.prepare(&format!(
        "SELECT id FROM repos r
         WHERE trim(ifnull(description, '')) = '' OR created_at IS NULL
         {ORDER_BY} LIMIT ?"
    ))?;
    let rows = statement.query_map([limit as i64], |row| row.get(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn count_pending_metadata(conn: &Connection) -> DbResult<usize> {
    let total: i64 = conn.query_row(
        "SELECT count(*) FROM repos
         WHERE trim(ifnull(description, '')) = '' OR created_at IS NULL",
        [],
        |row| row.get(0),
    )?;
    Ok(total as usize)
}

/// 插入一个仓库与它的标签。已经存在时什么都不做并返回 `false`。
pub fn insert(conn: &Connection, repo: &Repo, tag_ids: &[i64]) -> DbResult<bool> {
    let changed = conn.execute(
        "INSERT OR IGNORE INTO repos
            (id, full_name, url, description, description_zh, discovered_at,
             created_at, source, language, stars, category_id)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            repo.id(),
            repo.full_name,
            repo.url,
            repo.description.as_deref(),
            repo.description_zh.as_deref(),
            repo.discovered_at,
            repo.created_at,
            repo.source.as_deref(),
            repo.language.as_deref(),
            repo.stars.map(|stars| stars as i64),
            repo.category_id,
        ],
    )?;
    if changed == 0 {
        return Ok(false);
    }
    set_tags(conn, &repo.id(), tag_ids)?;
    Ok(true)
}

pub fn delete(conn: &Connection, id: &str) -> DbResult<bool> {
    Ok(conn.execute("DELETE FROM repos WHERE id = ?", [id])? > 0)
}

/// 写入人工填写的汉语描述；`None` 表示清除。
pub fn set_description_zh(conn: &Connection, id: &str, text: Option<&str>) -> DbResult<bool> {
    let changed = conn.execute(
        "UPDATE repos SET description_zh = ? WHERE id = ?",
        params![text, id],
    )?;
    Ok(changed > 0)
}

/// 给出仓库定分类；`None` 表示移回未分类。
pub fn set_category(conn: &Connection, id: &str, category_id: Option<i64>) -> DbResult<bool> {
    let changed = conn.execute(
        "UPDATE repos SET category_id = ? WHERE id = ?",
        params![category_id, id],
    )?;
    Ok(changed > 0)
}

/// 覆盖仓库的标签集合，顺序由调用方按词表排好。
pub fn set_tags(conn: &Connection, id: &str, tag_ids: &[i64]) -> DbResult<()> {
    conn.execute("DELETE FROM repo_tags WHERE repo_id = ?", [id])?;
    let mut statement = conn.prepare("INSERT INTO repo_tags (repo_id, tag_id) VALUES (?, ?)")?;
    for tag_id in tag_ids {
        statement.execute(params![id, tag_id])?;
    }
    Ok(())
}

/// 用抓回来的元数据补齐字段。**只填空缺**，手工整理过的描述不该被一次刷新冲掉。
///
/// 返回是否真的写进了至少一个字段——调用方用它判断这一轮有没有进展。
pub fn fill_metadata(
    conn: &Connection,
    id: &str,
    description: Option<&str>,
    created_at: Option<i64>,
    stars: Option<u64>,
) -> DbResult<bool> {
    let changed = conn.execute(
        "UPDATE repos SET
             description = CASE
                 WHEN trim(ifnull(description, '')) = '' THEN ?2 ELSE description END,
             created_at  = CASE WHEN created_at IS NULL THEN ?3 ELSE created_at END,
             stars       = CASE WHEN ?4 IS NULL THEN stars ELSE ?4 END
         WHERE id = ?1",
        params![
            id,
            description,
            created_at,
            stars.map(|stars| stars as i64)
        ],
    )?;
    Ok(changed > 0)
}

/// 仓库是否存在。
pub fn exists(conn: &Connection, id: &str) -> DbResult<bool> {
    let found: Option<i64> = conn
        .query_row("SELECT 1 FROM repos WHERE id = ?", [id], |row| row.get(0))
        .optional()?;
    Ok(found.is_some())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;

    fn db() -> Db {
        Db::open_in_memory().unwrap()
    }

    fn repo(name: &str, discovered_at: i64) -> Repo {
        Repo::new(name.to_string(), None, discovered_at)
    }

    fn spec() -> RepoQuerySpec {
        RepoQuerySpec {
            search: None,
            category: CategoryScope::All,
            search_categories: Vec::new(),
            language: None,
            tags: Vec::new(),
            offset: 0,
            limit: 10,
        }
    }

    #[test]
    fn insert_is_idempotent_and_tags_round_trip() {
        let db = db();
        let conn = db.conn();

        conn.execute(
            "INSERT INTO tags (id, name, name_key, position) VALUES (1, 'cli', 'cli', 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO tags (id, name, name_key, position) VALUES (2, '待读', '待读', 1)",
            [],
        )
        .unwrap();

        let mut first = repo("Coder/Coder", 1);
        first.tags = vec!["cli".to_string(), "待读".to_string()];
        assert!(insert(conn, &first, &[1, 2]).unwrap());
        assert!(!insert(conn, &repo("coder/coder", 2), &[]).unwrap(), "同名不算新增");

        let all = load_all(conn).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].full_name, "Coder/Coder", "保留首次入库的写法");
        assert_eq!(all[0].tags, ["cli".to_string(), "待读".to_string()]);
    }

    #[test]
    fn page_query_filters_and_counts() {
        let db = db();
        let conn = db.conn();
        for (name, at) in [("a/one", 1), ("b/two", 2), ("c/three", 3)] {
            insert(conn, &repo(name, at), &[]).unwrap();
        }

        let (first, total) = query_page(
            conn,
            &RepoQuerySpec {
                limit: 2,
                ..spec()
            },
        )
        .unwrap();
        assert_eq!(total, 3, "总数是符合条件的全部，不是这一页");
        assert_eq!(first.len(), 2);
        assert_eq!(first[0].full_name, "c/three", "最新的在最前");

        let (second, _) = query_page(
            conn,
            &RepoQuerySpec {
                offset: 2,
                limit: 2,
                ..spec()
            },
        )
        .unwrap();
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].full_name, "a/one");
    }

    #[test]
    fn search_hits_name_and_reaches_tags() {
        let db = db();
        let conn = db.conn();
        conn.execute(
            "INSERT INTO tags (id, name, name_key, position) VALUES (1, 'cli', 'cli', 0)",
            [],
        )
        .unwrap();

        let mut tagged = repo("coder/coder", 1);
        insert(conn, &tagged, &[]).unwrap();
        tagged = repo("rust-lang/rust", 2);
        insert(conn, &tagged, &[1]).unwrap();

        let (rows, total) = query_page(
            conn,
            &RepoQuerySpec {
                search: Some("%coder%".to_string()),
                ..spec()
            },
        )
        .unwrap();
        assert_eq!(total, 1);
        assert_eq!(rows[0].full_name, "coder/coder");

        let (rows, _) = query_page(
            conn,
            &RepoQuerySpec {
                search: Some("%cli%".to_string()),
                ..spec()
            },
        )
        .unwrap();
        assert_eq!(rows[0].full_name, "rust-lang/rust", "标签也能搜到");
    }

    /// 三个仓库，语言分别是 Rust / Rust / Go，用来验证语言筛选。
    fn three_languages(conn: &Connection) {
        for (name, language) in [("a/one", "Rust"), ("b/two", "Rust"), ("c/three", "Go")] {
            let mut repo = repo(name, 1);
            repo.language = Some(language.to_string());
            insert(conn, &repo, &[]).unwrap();
        }
    }

    #[test]
    fn language_matches_the_whole_name_and_nothing_else() {
        let db = db();
        let conn = db.conn();
        three_languages(conn);
        // 名字带前缀的那种：筛 Go 不该把 Gosu 也捞进来。
        let mut gosu = repo("d/four", 1);
        gosu.language = Some("Gosu".to_string());
        insert(conn, &gosu, &[]).unwrap();

        let (rows, total) = query_page(
            conn,
            &RepoQuerySpec {
                language: Some("Go".to_string()),
                ..spec()
            },
        )
        .unwrap();
        assert_eq!(total, 1);
        assert_eq!(rows[0].full_name, "c/three");

        let (rows, _) = query_page(
            conn,
            &RepoQuerySpec {
                language: Some("Rust".to_string()),
                ..spec()
            },
        )
        .unwrap();
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn several_tags_must_all_be_present() {
        let db = db();
        let conn = db.conn();
        for (id, name) in [(1, "cli"), (2, "待读")] {
            conn.execute(
                "INSERT INTO tags (id, name, name_key, position) VALUES (?1, ?2, ?2, ?1)",
                params![id, name],
            )
            .unwrap();
        }
        insert(conn, &repo("a/both", 1), &[1, 2]).unwrap();
        insert(conn, &repo("b/only-cli", 2), &[1]).unwrap();
        insert(conn, &repo("c/neither", 3), &[]).unwrap();

        let (rows, total) = query_page(
            conn,
            &RepoQuerySpec {
                tags: vec!["cli".to_string(), "待读".to_string()],
                ..spec()
            },
        )
        .unwrap();
        assert_eq!(total, 1, "两个标签都要有，不是有一个就算");
        assert_eq!(rows[0].full_name, "a/both");

        let (rows, _) = query_page(
            conn,
            &RepoQuerySpec {
                tags: vec!["cli".to_string()],
                ..spec()
            },
        )
        .unwrap();
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn tag_comparison_ignores_case_because_the_key_is_lowercased() {
        let db = db();
        let conn = db.conn();
        conn.execute(
            "INSERT INTO tags (id, name, name_key, position) VALUES (1, 'CLI', 'cli', 0)",
            [],
        )
        .unwrap();
        insert(conn, &repo("a/one", 1), &[1]).unwrap();

        let (rows, _) = query_page(
            conn,
            &RepoQuerySpec {
                tags: vec!["cli".to_string()],
                ..spec()
            },
        )
        .unwrap();
        assert_eq!(rows.len(), 1, "词表里存的是 CLI，筛选用小写也得对上");
    }

    #[test]
    fn an_unknown_language_or_tag_matches_nothing_instead_of_everything() {
        // 语言被改名、标签被删掉之后，筛选项就不该悄悄退回「全部」——那会让用户
        // 以为筛选没生效。跟分类那个空子树是同一个道理。
        let db = db();
        let conn = db.conn();
        three_languages(conn);

        let (rows, _) = query_page(
            conn,
            &RepoQuerySpec {
                language: Some("Rustlang".to_string()),
                tags: vec!["没有这个标签".to_string()],
                ..spec()
            },
        )
        .unwrap();
        assert!(rows.is_empty());
    }

    #[test]
    fn an_empty_subtree_scope_matches_nothing() {        let db = db();
        let conn = db.conn();
        insert(conn, &repo("a/one", 1), &[]).unwrap();

        let (rows, total) = query_page(
            conn,
            &RepoQuerySpec {
                category: CategoryScope::Subtrees(Vec::new()),
                ..spec()
            },
        )
        .unwrap();
        assert!(rows.is_empty());
        assert_eq!(total, 0);
    }

    #[test]
    fn undeleted_category_returns_the_repo_to_uncategorized() {
        let db = db();
        let conn = db.conn();
        conn.execute(
            "INSERT INTO categories (id, parent_id, name, name_key) VALUES (1, NULL, '工具', '工具')",
            [],
        )
        .unwrap();
        let mut one = repo("a/one", 1);
        one.category_id = Some(1);
        insert(conn, &one, &[]).unwrap();
        insert(conn, &repo("b/two", 2), &[]).unwrap();

        assert!(set_category(conn, "a/one", None).unwrap());
        let (rows, _) = query_page(
            conn,
            &RepoQuerySpec {
                category: CategoryScope::Uncategorized,
                ..spec()
            },
        )
        .unwrap();
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn metadata_fill_only_touches_missing_fields() {
        let db = db();
        let conn = db.conn();
        let mut one = repo("a/one", 1);
        one.description = Some("原有描述".to_string());
        insert(conn, &one, &[]).unwrap();

        assert!(fill_metadata(conn, "a/one", Some("新描述"), Some(42), Some(7)).unwrap());

        let stored = &load_all(conn).unwrap()[0];
        assert_eq!(stored.description.as_deref(), Some("原有描述"), "不覆盖已有描述");
        assert_eq!(stored.created_at, Some(42));
        assert_eq!(stored.stars, Some(7));
    }

    #[test]
    fn pending_metadata_skips_blank_descriptions() {
        let db = db();
        let conn = db.conn();

        let mut blank = repo("a/one", 1);
        blank.description = Some("   ".to_string());
        insert(conn, &blank, &[]).unwrap();

        let mut done = repo("b/two", 2);
        done.description = Some("有描述".to_string());
        done.created_at = Some(1);
        insert(conn, &done, &[]).unwrap();

        assert_eq!(pending_metadata(conn, 10).unwrap(), vec!["a/one".to_string()]);
        assert_eq!(count_pending_metadata(conn).unwrap(), 1);
    }

    #[test]
    fn deleting_a_repo_takes_its_tag_links() {
        let db = db();
        let conn = db.conn();
        conn.execute(
            "INSERT INTO tags (id, name, name_key, position) VALUES (1, 'cli', 'cli', 0)",
            [],
        )
        .unwrap();
        insert(conn, &repo("a/one", 1), &[1]).unwrap();

        assert!(delete(conn, "a/one").unwrap());
        let links: i64 = conn
            .query_row("SELECT count(*) FROM repo_tags", [], |row| row.get(0))
            .unwrap();
        assert_eq!(links, 0);
        assert!(!exists(conn, "a/one").unwrap());
    }
}

//! 分类树与标签表的读写。
//!
//! 分类在这里就是一棵真树：每个节点知道自己挂在哪一级下面。父子关系不靠名字里
//! 的斜杠去猜，所以改一个分类的名字只动它自己，挪一棵子树只改一个 `parent_id`。

use rusqlite::{Connection, OptionalExtension, params};

use super::DbResult;
use crate::taxonomy::model::{Tag, name_key};
use crate::taxonomy::tree::CategoryNode;

/// 读出整棵分类树。顺序由 `position` 决定，同级之间稳定。
pub fn load_categories(conn: &Connection) -> DbResult<Vec<CategoryNode>> {
    let mut statement = conn.prepare(
        "SELECT id, parent_id, name, position FROM categories ORDER BY position, id",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(CategoryNode {
            id: row.get(0)?,
            parent: row.get(1)?,
            name: row.get(2)?,
            position: row.get(3)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// 在指定上级下面按名字找一个分类，忽略大小写。`parent` 为 `None` 表示找顶层。
pub fn find_child(conn: &Connection, parent: Option<i64>, name: &str) -> DbResult<Option<i64>> {
    let found = conn
        .query_row(
            "SELECT id FROM categories WHERE parent_id IS ? AND name_key = ?",
            params![parent, name_key(name)],
            |row| row.get(0),
        )
        .optional()?;
    Ok(found)
}

/// 按名字找一个标签，忽略大小写。
pub fn find_tag(conn: &Connection, name: &str) -> DbResult<Option<i64>> {
    let found = conn
        .query_row(
            "SELECT id FROM tags WHERE name_key = ?",
            params![name_key(name)],
            |row| row.get(0),
        )
        .optional()?;
    Ok(found)
}

/// 新建一个分类节点，返回它的 id。位置排在同级最后。
pub fn insert_category(conn: &Connection, parent: Option<i64>, name: &str) -> DbResult<i64> {
    let position: i64 = conn.query_row(
        "SELECT ifnull(max(position), -1) + 1 FROM categories WHERE parent_id IS ?",
        params![parent],
        |row| row.get(0),
    )?;
    conn.execute(
        "INSERT INTO categories (parent_id, name, name_key, position) VALUES (?, ?, ?, ?)",
        params![parent, name, name_key(name), position],
    )?;
    Ok(conn.last_insert_rowid())
}

/// 只改分类自己的名字。上级与子节点都不动——这正是树结构带来的好处。
pub fn rename_category(conn: &Connection, id: i64, name: &str) -> DbResult<()> {
    conn.execute(
        "UPDATE categories SET name = ?, name_key = ? WHERE id = ?",
        params![name, name_key(name), id],
    )?;
    Ok(())
}

/// 把一棵子树挂到别处；`None` 表示变成顶层分类。
pub fn move_category(conn: &Connection, id: i64, parent: Option<i64>) -> DbResult<()> {
    let position: i64 = conn.query_row(
        "SELECT ifnull(max(position), -1) + 1 FROM categories WHERE parent_id IS ? AND id <> ?",
        params![parent, id],
        |row| row.get(0),
    )?;
    conn.execute(
        "UPDATE categories SET parent_id = ?, position = ? WHERE id = ?",
        params![parent, position, id],
    )?;
    Ok(())
}

/// 删除一个分类。外键的级联会带走它的整棵子树，仓库则回到未分类。
pub fn delete_category(conn: &Connection, id: i64) -> DbResult<()> {
    conn.execute("DELETE FROM categories WHERE id = ?", [id])?;
    Ok(())
}

/// 读出标签词表，顺序就是界面上的顺序。
pub fn load_tags(conn: &Connection) -> DbResult<Vec<Tag>> {
    let mut statement = conn.prepare("SELECT id, name FROM tags ORDER BY position, id")?;
    let rows = statement.query_map([], |row| {
        Ok(Tag {
            id: row.get(0)?,
            name: row.get(1)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn insert_tag(conn: &Connection, name: &str) -> DbResult<i64> {
    let position: i64 = conn.query_row(
        "SELECT ifnull(max(position), -1) + 1 FROM tags",
        [],
        |row| row.get(0),
    )?;
    conn.execute(
        "INSERT INTO tags (name, name_key, position) VALUES (?, ?, ?)",
        params![name, name_key(name), position],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn rename_tag(conn: &Connection, id: i64, name: &str) -> DbResult<()> {
    conn.execute(
        "UPDATE tags SET name = ?, name_key = ? WHERE id = ?",
        params![name, name_key(name), id],
    )?;
    Ok(())
}

/// 删除标签。`repo_tags` 上的关联由级联清掉，仓库本身不受影响。
pub fn delete_tag(conn: &Connection, id: i64) -> DbResult<()> {
    conn.execute("DELETE FROM tags WHERE id = ?", [id])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;

    fn db() -> Db {
        Db::open_in_memory().unwrap()
    }

    #[test]
    fn categories_form_a_tree_that_survives_a_round_trip() {
        let db = db();
        let conn = db.conn();

        let dev = insert_category(conn, None, "开发").unwrap();
        let front = insert_category(conn, Some(dev), "前端").unwrap();
        let back = insert_category(conn, Some(dev), "后端").unwrap();
        insert_category(conn, None, "学习").unwrap();

        let nodes = load_categories(conn).unwrap();
        assert_eq!(nodes.len(), 4);
        assert_eq!(nodes[0].id, dev);
        assert_eq!(nodes[1].id, front, "同级按插入顺序");
        assert_eq!(nodes[2].id, back);
        assert_eq!(nodes[3].name, "学习");
        assert_eq!(nodes[1].parent, Some(dev));
    }

    #[test]
    fn renaming_only_touches_the_node_itself() {
        let db = db();
        let conn = db.conn();
        let dev = insert_category(conn, None, "开发").unwrap();
        let front = insert_category(conn, Some(dev), "前端").unwrap();

        rename_category(conn, dev, "工具").unwrap();

        let tree = crate::taxonomy::tree::CategoryTree::from_nodes(load_categories(conn).unwrap());
        assert_eq!(tree.path(dev).as_deref(), Some("工具"));
        assert_eq!(
            tree.path(front).as_deref(),
            Some("工具/前端"),
            "子分类跟着走，但一行数据都没改"
        );
    }

    #[test]
    fn moving_a_subtree_only_changes_its_parent() {
        let db = db();
        let conn = db.conn();
        let dev = insert_category(conn, None, "开发").unwrap();
        let tools = insert_category(conn, None, "工具").unwrap();
        let front = insert_category(conn, Some(dev), "前端").unwrap();

        move_category(conn, front, Some(tools)).unwrap();

        let tree = crate::taxonomy::tree::CategoryTree::from_nodes(load_categories(conn).unwrap());
        assert_eq!(tree.path(front).as_deref(), Some("工具/前端"));
    }

    #[test]
    fn deleting_a_category_takes_its_subtree() {
        let db = db();
        let conn = db.conn();
        let dev = insert_category(conn, None, "开发").unwrap();
        insert_category(conn, Some(dev), "前端").unwrap();
        insert_category(conn, Some(dev), "后端").unwrap();
        insert_category(conn, None, "学习").unwrap();

        delete_category(conn, dev).unwrap();

        let nodes = load_categories(conn).unwrap();
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].name, "学习");
    }

    #[test]
    fn tags_keep_their_order_and_ids() {
        let db = db();
        let conn = db.conn();
        let cli = insert_tag(conn, "cli").unwrap();
        let later = insert_tag(conn, "待读").unwrap();

        let tags = load_tags(conn).unwrap();
        assert_eq!(tags[0].id, cli);
        assert_eq!(tags[1].id, later);
        assert_eq!(tags[1].name, "待读", "保留首次的写法");

        rename_tag(conn, later, "稍后读").unwrap();
        assert_eq!(load_tags(conn).unwrap()[1].name, "稍后读");

        delete_tag(conn, cli).unwrap();
        assert_eq!(load_tags(conn).unwrap().len(), 1);
    }

    #[test]
    fn tag_names_are_unique_case_insensitively() {
        let db = db();
        let conn = db.conn();
        insert_tag(conn, "AI").unwrap();
        assert!(insert_tag(conn, "ai").is_err(), "大小写不同还是同一个标签");
    }
}

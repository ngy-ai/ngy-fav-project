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

/// 在同级里把某个分类往前/往后挪一格。`delta` 为负往前、为正往后。
///
/// **只动同级**：`开发/前端` 往前挪，换的是它上面那个兄弟的位置，不会跑到别的
/// 上级底下去。返回 `false` 表示已经在头或尾，没有可挪的余地，这时一条都没改。
pub fn reorder_category(conn: &Connection, id: i64, delta: i64) -> DbResult<bool> {
    let Some(parent) = category_parent(conn, id)? else {
        return Ok(false);
    };

    let mut ids = ordered_ids(
        conn,
        "SELECT id FROM categories WHERE parent_id IS ?1 ORDER BY position, id",
        params![parent],
    )?;
    if !shifted(&mut ids, id, delta) {
        return Ok(false);
    }
    rewrite_positions(conn, "categories", &ids)?;
    Ok(true)
}

/// 标签是扁平的，整张表就是它的同级。
pub fn reorder_tag(conn: &Connection, id: i64, delta: i64) -> DbResult<bool> {
    let mut ids = ordered_ids(conn, "SELECT id FROM tags ORDER BY position, id", [])?;
    if !shifted(&mut ids, id, delta) {
        return Ok(false);
    }
    rewrite_positions(conn, "tags", &ids)?;
    Ok(true)
}

/// 把某个分类放到同级里另一个分类的前面/后面。
///
/// 和 `reorder_category` 只挪一格不同，这里给的是**目标**：中间隔了几格都不用管，
/// 一步到位——拖拽落点说的就是「放到它旁边」。目标必须与被挪的那个同级，跨层换
/// 上级是「改名改路径」的事，排序里不做，所以这种情况返回 `false` 且一条都不改。
pub fn place_category(conn: &Connection, id: i64, target: i64, before: bool) -> DbResult<bool> {
    if id == target {
        return Ok(false);
    }
    let Some(parent) = category_parent(conn, id)? else {
        return Ok(false);
    };
    let Some(target_parent) = category_parent(conn, target)? else {
        return Ok(false);
    };
    if parent != target_parent {
        return Ok(false);
    }

    let mut ids = ordered_ids(
        conn,
        "SELECT id FROM categories WHERE parent_id IS ?1 ORDER BY position, id",
        params![parent],
    )?;
    if !placed(&mut ids, id, target, before) {
        return Ok(false);
    }
    rewrite_positions(conn, "categories", &ids)?;
    Ok(true)
}

/// 标签是扁平的，整张表就是它的同级。
pub fn place_tag(conn: &Connection, id: i64, target: i64, before: bool) -> DbResult<bool> {
    if id == target {
        return Ok(false);
    }
    let mut ids = ordered_ids(conn, "SELECT id FROM tags ORDER BY position, id", [])?;
    if !placed(&mut ids, id, target, before) {
        return Ok(false);
    }
    rewrite_positions(conn, "tags", &ids)?;
    Ok(true)
}

/// 某个分类挂在谁下面。外面那层 `None` 是「库里没有这个 id」，内层的 `None` 是顶层
/// ——`parent_id` 本来就允许是 NULL，所以这里非套两层不可。
fn category_parent(conn: &Connection, id: i64) -> DbResult<Option<Option<i64>>> {
    Ok(conn
        .query_row("SELECT parent_id FROM categories WHERE id = ?", [id], |row| {
            row.get(0)
        })
        .optional()?)
}

/// 把 `id` 从 `ids` 里摘出来，插到 `target` 的前面/后面。两者缺一就返回 `false`，
/// 此时 `ids` 一格都没动。
fn placed(ids: &mut Vec<i64>, id: i64, target: i64, before: bool) -> bool {
    let Some(from) = ids.iter().position(|it| *it == id) else {
        return false;
    };
    let moved = ids.remove(from);
    let Some(at) = ids.iter().position(|it| *it == target) else {
        // 摘掉自己之后目标不见了，只可能是 `id == target`——调用方已经拦过，
        // 这里只是把摘掉的放回去，别留下半截状态。
        ids.insert(from, moved);
        return false;
    };
    ids.insert(if before { at } else { at + 1 }, moved);
    true
}

/// 读出一串 id，顺序就是现在界面上的顺序。
fn ordered_ids(conn: &Connection, sql: &str, params: impl rusqlite::Params) -> DbResult<Vec<i64>> {
    let mut statement = conn.prepare(sql)?;
    let rows = statement.query_map(params, |row| row.get(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// 把 `id` 在 `ids` 里挪 `delta` 格。挪不动（已经在两头、或者压根不在里面）时
/// 返回 `false`，此时 `ids` 一格都没动。
fn shifted(ids: &mut [i64], id: i64, delta: i64) -> bool {
    let Some(at) = ids.iter().position(|it| *it == id) else {
        return false;
    };
    let target = at as i64 + delta;
    if target < 0 || target as usize >= ids.len() {
        return false;
    }
    ids.swap(at, target as usize);
    true
}

/// 按 `ids` 的顺序把整段同级的 `position` 重写成 0..n。
///
/// 不是只交换两条 `position` 就完事：老数据里同一个上级下面可能有好几条 `position`
/// 相同（早期导入、手工改过库），只换两条的话下一次排序又按 id 把它们摆回去，
/// 看起来像「点了没反应」。整段重写一遍，顺序才算真定下来。
fn rewrite_positions(conn: &Connection, table: &str, ids: &[i64]) -> DbResult<()> {
    // 表名是这里写死的字面量，不是外面传进来的东西。
    let sql = format!("UPDATE {table} SET position = ?1 WHERE id = ?2");
    let mut statement = conn.prepare(&sql)?;
    for (position, id) in ids.iter().enumerate() {
        statement.execute(params![position as i64, id])?;
    }
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

    /// 整棵树的路径顺序，铺平了看最直观。
    fn paths(conn: &Connection) -> Vec<String> {
        crate::taxonomy::tree::CategoryTree::from_nodes(load_categories(conn).unwrap())
            .flatten()
            .into_iter()
            .map(|node| node.path)
            .collect()
    }

    /// 按路径找 id。要传 id 的地方先用它，测试里就不用一路拿着 `insert` 的返回值了。
    fn id_of(conn: &Connection, path: &str) -> i64 {
        crate::taxonomy::tree::CategoryTree::from_nodes(load_categories(conn).unwrap())
            .find_path(path)
            .unwrap_or_else(|| panic!("没有「{path}」这个分类"))
    }

    fn tag_names(conn: &Connection) -> Vec<String> {
        load_tags(conn)
            .unwrap()
            .into_iter()
            .map(|tag| tag.name)
            .collect()
    }

    /// 往前后挪一格换的是**同级**里的邻居。子分类挪到头也不会跑到顶层去——
    /// 同级按 `parent_id` 划，跟层级没有关系。
    #[test]
    fn siblings_move_within_their_own_level() {
        let db = db();
        let conn = db.conn();
        let dev = insert_category(conn, None, "开发").unwrap();
        insert_category(conn, None, "学习").unwrap();
        insert_category(conn, Some(dev), "前端").unwrap();
        let back = insert_category(conn, Some(dev), "后端").unwrap();

        assert_eq!(
            paths(conn),
            ["开发", "开发/前端", "开发/后端", "学习"],
            "一开始是建立的顺序"
        );

        assert!(reorder_category(conn, back, -1).unwrap(), "后端往前换到前端前面");
        assert_eq!(paths(conn), ["开发", "开发/后端", "开发/前端", "学习"]);

        // 顶层两条：把「学习」往前挪，换的是「开发」，不是某个子分类。
        let study = crate::taxonomy::tree::CategoryTree::from_nodes(
            load_categories(conn).unwrap(),
        )
        .find_path("学习")
        .expect("刚刚建过");
        assert!(reorder_category(conn, study, -1).unwrap());
        assert_eq!(
            paths(conn),
            ["学习", "开发", "开发/后端", "开发/前端"],
            "顶层换了位，子分类跟着上级走"
        );
    }

    /// 已经在两头就没有余地了：返回 `false` 且一条都不改。
    #[test]
    fn moving_past_the_edge_changes_nothing() {
        let db = db();
        let conn = db.conn();
        let first = insert_category(conn, None, "开发").unwrap();
        insert_category(conn, None, "学习").unwrap();
        let only = insert_category(conn, Some(first), "前端").unwrap();

        assert!(!reorder_category(conn, first, -1).unwrap(), "它已经在最前面");
        assert!(!reorder_category(conn, only, 1).unwrap(), "独苗也没得挪");
        assert!(!reorder_category(conn, only, -1).unwrap());
        assert!(!reorder_category(conn, 9999, 1).unwrap(), "不在库里");
        assert_eq!(paths(conn), ["开发", "开发/前端", "学习"]);
    }

    /// 拖拽给的落点是「放到谁旁边」，不是「挪几格」：一次到位，中间隔着几个都行。
    #[test]
    fn placing_lands_next_to_the_target_in_one_step() {
        let db = db();
        let conn = db.conn();
        insert_category(conn, None, "开发").unwrap();
        insert_category(conn, None, "学习").unwrap();
        insert_category(conn, None, "工具").unwrap();

        let study = id_of(conn, "学习");
        // 「学习」拖到「工具」后面（从中间一次挪到末尾）。
        assert!(place_category(conn, study, id_of(conn, "工具"), false).unwrap());
        assert_eq!(paths(conn), ["开发", "工具", "学习"]);

        // 再拖到最前面。
        assert!(place_category(conn, study, id_of(conn, "开发"), true).unwrap());
        assert_eq!(paths(conn), ["学习", "开发", "工具"]);
    }

    /// 落在别层就什么都不做：跨层换的是上级，那是「改名改路径」的活。
    /// 子分类之间照样能拖——同级看的是 `parent_id`，跟深度无关。
    #[test]
    fn placing_refuses_to_cross_levels() {
        let db = db();
        let conn = db.conn();
        let dev = insert_category(conn, None, "开发").unwrap();
        insert_category(conn, Some(dev), "前端").unwrap();
        insert_category(conn, Some(dev), "后端").unwrap();
        insert_category(conn, None, "学习").unwrap();

        let back = id_of(conn, "开发/后端");
        assert!(place_category(conn, back, id_of(conn, "开发/前端"), true).unwrap());
        assert_eq!(paths(conn), ["开发", "开发/后端", "开发/前端", "学习"]);

        assert!(
            !place_category(conn, back, id_of(conn, "学习"), true).unwrap(),
            "二级拖到顶层：不同级"
        );
        assert!(
            !place_category(conn, dev, id_of(conn, "开发/前端"), true).unwrap(),
            "反过来也一样"
        );
        assert!(!place_category(conn, dev, dev, true).unwrap(), "拖到自己身上");
        assert!(!place_category(conn, 9999, dev, false).unwrap(), "不在库里");
        assert_eq!(
            paths(conn),
            ["开发", "开发/后端", "开发/前端", "学习"],
            "不接受的那些一次都没改"
        );
    }

    /// 老数据里同一个上级下可能有好几条 `position` 相同——只交换两条的话，
    /// 下次排序又会按 id 摆回去，看起来像「点了没反应」。所以整段重写一遍。
    #[test]
    fn duplicated_positions_are_normalized_while_moving() {
        let db = db();
        let conn = db.conn();
        let dev = insert_category(conn, None, "开发").unwrap();
        let mut ids = Vec::new();
        for name in ["a", "b", "c"] {
            conn.execute(
                "INSERT INTO categories (parent_id, name, name_key, position) \
                 VALUES (?1, ?2, ?2, 0)",
                params![dev, name],
            )
            .unwrap();
            ids.push(conn.last_insert_rowid());
        }

        // position 全相同时的顺序是 id 序：a, b, c。
        assert_eq!(paths(conn), ["开发", "开发/a", "开发/b", "开发/c"]);
        assert!(reorder_category(conn, ids[2], -1).unwrap(), "c 往前一格");

        assert_eq!(
            paths(conn),
            ["开发", "开发/a", "开发/c", "开发/b"],
            "真的换了，没有被 id 序盖回去"
        );
        let positions: Vec<i64> = {
            let mut statement = conn
                .prepare("SELECT position FROM categories WHERE parent_id = ?1 ORDER BY position, id")
                .unwrap();
            statement
                .query_map([dev], |row| row.get(0))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap()
        };
        assert_eq!(positions, [0, 1, 2], "整段重写成 0..n，不再有并列");
    }

    /// 标签是扁平的，整张表就是它的同级。
    #[test]
    fn tags_move_in_the_flat_list() {
        let db = db();
        let conn = db.conn();
        let cli = insert_tag(conn, "cli").unwrap();
        let later = insert_tag(conn, "待读").unwrap();

        assert!(reorder_tag(conn, cli, 1).unwrap());
        let names: Vec<String> = load_tags(conn)
            .unwrap()
            .into_iter()
            .map(|tag| tag.name)
            .collect();
        assert_eq!(names, ["待读", "cli"]);

        assert!(!reorder_tag(conn, cli, 1).unwrap(), "已经在最后了");
        assert!(reorder_tag(conn, later, 1).unwrap(), "换回来");
        assert_eq!(load_tags(conn).unwrap()[0].name, "cli");
        assert!(!reorder_tag(conn, 9999, -1).unwrap(), "不在库里");
    }

    /// 标签没有层级，整张表就是同级，拖到谁旁边就是谁旁边。
    #[test]
    fn placing_tags_lands_next_to_the_target() {
        let db = db();
        let conn = db.conn();
        let cli = insert_tag(conn, "cli").unwrap();
        let later = insert_tag(conn, "待读").unwrap();
        let tools = insert_tag(conn, "工具").unwrap();

        // cli 拖到「工具」后面：从开头一次挪到末尾。
        assert!(place_tag(conn, cli, tools, false).unwrap());
        assert_eq!(tag_names(conn), ["待读", "工具", "cli"]);

        // 再拖到最前面。
        assert!(place_tag(conn, cli, later, true).unwrap());
        assert_eq!(tag_names(conn), ["cli", "待读", "工具"]);

        assert!(!place_tag(conn, cli, cli, true).unwrap(), "拖到自己身上");
        assert!(!place_tag(conn, 9999, later, true).unwrap(), "不在库里");
        assert_eq!(tag_names(conn), ["cli", "待读", "工具"], "不接受的一次都没改");
    }
}

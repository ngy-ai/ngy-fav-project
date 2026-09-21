//! 内存里的分类树。
//!
//! 树上的一级知道自己挂在哪一级下面，父子关系是**存出来的**，不是从名字里的斜杠
//! 猜出来的。所以改一个分类的名字只影响它自己，挪一棵子树只改一个 `parent_id`——
//! 改名与移动不再需要顺着名字去改写一串引用。
//!
//! 库里那棵才是权威；这里这份是它的内存镜像，每次写库成功之后跟着同步。
//! 全是纯函数，不碰 `Library` 也不碰界面，好测。

use std::collections::HashSet;

use super::model::{MAX_DEPTH, PATH_SEPARATOR, same_name};

/// 树上的一级。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CategoryNode {
    pub id: i64,
    /// 上级节点的 id。`None` 表示它是顶层分类。
    pub parent: Option<i64>,
    pub name: String,
    /// 同级之间的排序依据。排序只认它和 id，不认名字的码点——
    /// 用户摆出来的顺序不该被一次重命名打乱。
    pub position: i64,
}

/// 铺平之后的一行，供界面按缩进画成树。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FlatCategory {
    pub id: i64,
    /// 从顶层到它的完整路径，例如 `开发/前端`。
    pub path: String,
    /// 它自己叫什么。缩进已经说明了上级是谁，界面上只需要画这个名字。
    pub leaf: String,
    /// 层级，顶层是 1。
    pub depth: usize,
    /// 直接子分类数量。
    pub children: usize,
}

#[derive(Clone, Debug, Default)]
pub struct CategoryTree {
    /// 按 (position, id) 排好的一串节点。顺序保持住，子节点取出来就是有序的。
    nodes: Vec<CategoryNode>,
}

impl CategoryTree {
    pub fn from_nodes(mut nodes: Vec<CategoryNode>) -> Self {
        nodes.sort_by(|a, b| a.position.cmp(&b.position).then_with(|| a.id.cmp(&b.id)));
        Self { nodes }
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn get(&self, id: i64) -> Option<&CategoryNode> {
        self.nodes.iter().find(|node| node.id == id)
    }

    pub fn contains(&self, id: i64) -> bool {
        self.get(id).is_some()
    }

    /// 直接子节点，按既有顺序。
    pub fn children(&self, parent: Option<i64>) -> Vec<&CategoryNode> {
        self.nodes
            .iter()
            .filter(|node| node.parent == parent)
            .collect()
    }

    pub fn child_count(&self, id: i64) -> usize {
        self.nodes.iter().filter(|node| node.parent == Some(id)).count()
    }

    /// 从顶层到它的每一级名字。
    pub fn segments(&self, id: i64) -> Vec<String> {
        let mut names = Vec::new();
        let mut current = Some(id);
        // 上限只是防御：外键保证了树上没有环，正常永远走不满。
        for _ in 0..=MAX_DEPTH {
            let Some(node) = current.and_then(|current| self.get(current)) else {
                break;
            };
            names.push(node.name.clone());
            current = node.parent;
        }
        names.reverse();
        names
    }

    /// 完整路径，例如 `开发/前端`。节点不在树上时返回 `None`。
    pub fn path(&self, id: i64) -> Option<String> {
        self.get(id)
            .map(|_| self.segments(id).join(&PATH_SEPARATOR.to_string()))
    }

    /// 层级，顶层是 1。
    pub fn depth(&self, id: i64) -> usize {
        self.segments(id).len()
    }

    /// 它自己和它下面全部后代的 id。
    pub fn subtree(&self, id: i64) -> Vec<i64> {
        let mut collected = Vec::new();
        let mut pending = vec![id];
        while let Some(current) = pending.pop() {
            if !self.contains(current) {
                continue;
            }
            collected.push(current);
            pending.extend(self.children(Some(current)).into_iter().map(|child| child.id));
        }
        collected
    }

    /// `id` 是不是 `ancestor` 本身或它的后代。
    pub fn is_within(&self, id: i64, ancestor: i64) -> bool {
        let mut current = Some(id);
        for _ in 0..=MAX_DEPTH {
            match current {
                Some(node_id) if node_id == ancestor => return true,
                Some(node_id) => current = self.get(node_id).and_then(|node| node.parent),
                None => return false,
            }
        }
        false
    }

    /// 按路径找节点。逐级比较忽略大小写。
    pub fn find_path(&self, path: &str) -> Option<i64> {
        let mut parent = None;
        let mut found = None;

        for segment in path.split(PATH_SEPARATOR) {
            let segment = segment.trim();
            if segment.is_empty() {
                return None;
            }
            let node = self
                .children(parent)
                .into_iter()
                .find(|node| same_name(&node.name, segment))?;
            parent = Some(node.id);
            found = Some(node.id);
        }
        found
    }

    /// 同一级下面有没有同名的另一个节点。`except` 用来在改名/移动时排除自己。
    pub fn sibling_named(&self, parent: Option<i64>, name: &str, except: Option<i64>) -> Option<i64> {
        self.children(parent)
            .into_iter()
            .find(|node| Some(node.id) != except && same_name(&node.name, name))
            .map(|node| node.id)
    }

    /// 铺平成「父在前、子紧随其后」的一串，供界面按 `depth` 缩进。
    ///
    /// 只影响显示，不改树本身。走不到的节点也会补在最后——正常情况下外键保证
    /// 不会有游离节点，但界面宁可把它们画在明面上，也不要悄悄吞掉。
    pub fn flatten(&self) -> Vec<FlatCategory> {
        let mut flat = Vec::new();
        let mut visited = HashSet::new();

        for root in self.children(None) {
            self.flatten_into(root.id, 1, &mut flat, &mut visited);
        }
        for node in &self.nodes {
            if !visited.contains(&node.id) {
                self.flatten_into(node.id, 1, &mut flat, &mut visited);
            }
        }
        flat
    }

    fn flatten_into(
        &self,
        id: i64,
        depth: usize,
        flat: &mut Vec<FlatCategory>,
        visited: &mut HashSet<i64>,
    ) {
        if !visited.insert(id) {
            return;
        }
        let Some(node) = self.get(id) else {
            return;
        };

        flat.push(FlatCategory {
            id,
            path: self.segments(id).join(&PATH_SEPARATOR.to_string()),
            leaf: node.name.clone(),
            depth,
            children: self.child_count(id),
        });

        for child in self.children(Some(id)) {
            self.flatten_into(child.id, depth + 1, flat, visited);
        }
    }

    // ---- 写库成功之后同步内存 ----

    pub fn insert(&mut self, node: CategoryNode) {
        self.nodes.push(node);
        self.nodes
            .sort_by(|a, b| a.position.cmp(&b.position).then_with(|| a.id.cmp(&b.id)));
    }

    pub fn set_name(&mut self, id: i64, name: &str) {
        if let Some(node) = self.nodes.iter_mut().find(|node| node.id == id) {
            node.name = name.to_string();
        }
    }

    pub fn set_parent(&mut self, id: i64, parent: Option<i64>, position: i64) {
        if let Some(node) = self.nodes.iter_mut().find(|node| node.id == id) {
            node.parent = parent;
            node.position = position;
        }
        self.nodes
            .sort_by(|a, b| a.position.cmp(&b.position).then_with(|| a.id.cmp(&b.id)));
    }

    /// 连带整棵子树一起摘掉。
    pub fn remove_subtree(&mut self, id: i64) {
        let doomed = self.subtree(id);
        self.nodes.retain(|node| !doomed.contains(&node.id));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: i64, parent: Option<i64>, name: &str, position: i64) -> CategoryNode {
        CategoryNode {
            id,
            parent,
            name: name.to_string(),
            position,
        }
    }

    /// 开发(1) ─ 前端(2) ─ 框架(4)
    ///         └ 后端(3)
    /// 学习(5)
    fn sample() -> CategoryTree {
        CategoryTree::from_nodes(vec![
            node(1, None, "开发", 0),
            node(2, Some(1), "前端", 0),
            node(3, Some(1), "后端", 1),
            node(4, Some(2), "框架", 0),
            node(5, None, "学习", 1),
        ])
    }

    #[test]
    fn paths_are_read_off_the_tree_not_off_the_names() {
        let tree = sample();
        assert_eq!(tree.path(4).as_deref(), Some("开发/前端/框架"));
        assert_eq!(tree.path(1).as_deref(), Some("开发"));
        assert_eq!(tree.depth(4), 3);
        assert_eq!(tree.depth(5), 1);
        assert_eq!(tree.path(99), None);
    }

    #[test]
    fn a_path_may_not_contain_a_separator_to_fake_a_level() {
        // 名字里带斜杠也不会被当成层级——层级只由 parent 决定。
        let tree = CategoryTree::from_nodes(vec![node(1, None, "开发/前端", 0)]);
        assert_eq!(tree.depth(1), 1);
        assert_eq!(tree.path(1).as_deref(), Some("开发/前端"));
    }

    #[test]
    fn subtrees_and_membership() {
        let tree = sample();
        let mut subtree = tree.subtree(1);
        subtree.sort();
        assert_eq!(subtree, vec![1, 2, 3, 4]);

        assert!(tree.is_within(4, 1));
        assert!(tree.is_within(1, 1), "自己算在自己这棵子树里");
        assert!(!tree.is_within(5, 1));
        assert!(!tree.is_within(1, 4), "方向反过来不是");
    }

    #[test]
    fn find_path_walks_level_by_level_ignoring_case() {
        let tree = sample();
        assert_eq!(tree.find_path("开发/前端"), Some(2));
        assert_eq!(tree.find_path("开发"), Some(1));
        assert_eq!(tree.find_path("dev/frontend"), None);
        assert_eq!(tree.find_path("开发/前端/框架"), Some(4));

        // 名字里像前缀不算命中，得整段一样。
        let tree = CategoryTree::from_nodes(vec![
            node(1, None, "开发", 0),
            node(2, None, "开发工具", 1),
        ]);
        assert_eq!(tree.find_path("开发工具"), Some(2));
    }

    #[test]
    fn sibling_collision_is_detected_per_level() {
        let tree = sample();
        // 不同上级下面可以同名。
        assert_eq!(tree.sibling_named(Some(3), "前端", None), None);
        // 同一个上级下面不行；排除自己之后可以。
        assert_eq!(tree.sibling_named(Some(1), "前端", None), Some(2));
        assert_eq!(tree.sibling_named(Some(1), "前端", Some(2)), None);
        // 顶层也一样。
        assert_eq!(tree.sibling_named(None, "学习", None), Some(5));
    }

    #[test]
    fn flatten_puts_children_right_after_their_parent() {
        let tree = sample();
        let flat: Vec<(i64, usize)> = tree
            .flatten()
            .into_iter()
            .map(|row| (row.id, row.depth))
            .collect();
        assert_eq!(flat, vec![(1, 1), (2, 2), (4, 3), (3, 2), (5, 1)]);

        let row = &tree.flatten()[2];
        assert_eq!(row.path, "开发/前端/框架");
        assert_eq!(row.leaf, "框架");
        assert_eq!(row.children, 0);
    }

    #[test]
    fn ordering_follows_position_not_the_name() {
        // 名字反过来排，顺序仍按 position 走。
        let tree = CategoryTree::from_nodes(vec![
            node(1, None, "zzz", 0),
            node(2, None, "aaa", 1),
        ]);
        let names: Vec<String> = tree.flatten().into_iter().map(|row| row.leaf).collect();
        assert_eq!(names, ["zzz", "aaa"]);
    }

    #[test]
    fn a_detached_node_is_still_drawn_instead_of_vanishing() {
        // 上级不在树上（正常由外键挡着，这里只是防御）。
        let tree = CategoryTree::from_nodes(vec![
            node(1, None, "开发", 0),
            node(2, Some(99), "孤儿", 1),
        ]);
        let ids: Vec<i64> = tree.flatten().into_iter().map(|row| row.id).collect();
        assert_eq!(ids, vec![1, 2]);
    }

    #[test]
    fn mutations_keep_the_invariants() {
        let mut tree = sample();
        tree.insert(node(6, Some(3), "嵌入式", 0));
        assert_eq!(tree.path(6).as_deref(), Some("开发/后端/嵌入式"));

        tree.set_name(1, "工具");
        assert_eq!(
            tree.path(4).as_deref(),
            Some("工具/前端/框架"),
            "改名之后后代自动跟着新路径"
        );

        tree.set_parent(2, None, 2);
        assert_eq!(tree.path(4).as_deref(), Some("前端/框架"), "挪子树只改一个 parent");

        tree.remove_subtree(2);
        assert_eq!(
            tree.len(),
            4,
            "摘掉的是它自己和它的后代（2、4），别的都不动"
        );
        assert!(!tree.contains(4));
        assert!(tree.contains(3));
    }
}

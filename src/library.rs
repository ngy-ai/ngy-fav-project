//! 领域数据的唯一持有者。
//!
//! 数据存在 SQLite 里（见 `db` 模块）。这里同时持有一份内存镜像——界面渲染、
//! 后台调度读的都是它，不必每次都问数据库要。两边的同步规则只有一条：
//!
//! **先写库，成功了再改内存。**
//!
//! 反过来做就会在写失败时留下一份只存在于内存里的假象，用户看着改好了，
//! 下次打开又变回去。所以每个修改方法都走 [`Library::write`]：库写成功才继续。
//!
//! 分类树是唯一的例外：树上的操作（建节点、改名、挪子树、删子树）会连带改动
//! 一批节点，逐个去猜内存该怎么变既啰嗦又容易漏，所以写成功之后直接从库里
//! 重读一遍——那张表只有几十行，重读比维护增量便宜。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::db::{self, Db};
use crate::discovery::model::{
    DiscoverySettings, DiscoverySource, SourceRun, default_sources,
};
use crate::repos::model::{
    CategoryFilter, CategoryScope, Repo, RepoId, RepoPage, RepoQuery, RepoQuerySpec, RepoRow,
};
use crate::shared::store;
use crate::taxonomy::model::{self as tax, AddName, RenameName, Tag, TaxonomyKind};
use crate::taxonomy::tree::{CategoryTree, FlatCategory};

/// 入库结果。调用方需要知道是新增还是重复，好把结果说给用户听。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddOutcome {
    Added,
    AlreadyPresent,
}

/// 最近一次写库的状态。写失败必须能被看见——用户改了汉语描述却存不下，
/// 是必须当场知道的坏消息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SaveState {
    /// 这次启动还没写过。
    Untouched,
    Saved,
    Failed(String),
}

/// 「分类」页要画的一行：树上铺平的位置，加上仓库数。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CategoryRow {
    pub id: i64,
    pub path: String,
    pub leaf: String,
    pub depth: usize,
    /// 直接子分类数量。
    pub children: usize,
    /// 属于这个分类的仓库数，**含它的整棵子树**——「开发」底下挂了多少，
    /// 把「开发/前端」的算上才说得通。
    pub repos: usize,
}

/// 从库里读回来的一整份数据。
struct Snapshot {
    repos: Vec<Repo>,
    categories: CategoryTree,
    tags: Vec<Tag>,
    sources: Vec<DiscoverySource>,
    settings: DiscoverySettings,
}

pub struct Library {
    repos: Vec<Repo>,
    categories: CategoryTree,
    tags: Vec<Tag>,
    sources: Vec<DiscoverySource>,
    settings: DiscoverySettings,
    /// 数据库连接。打不开时为 `None`：应用还能起来看，但任何改动都会如实报失败，
    /// 而不是假装写进去了。
    db: Option<Db>,
    path: PathBuf,
    /// 启动时遇到的问题，在界面上如实展示。
    load_warning: Option<String>,
    /// 启动时想告诉用户的事（例如「已经帮你把旧数据迁过来了」）。
    load_note: Option<String>,
    save_state: SaveState,
}

impl Library {
    /// 从默认位置加载。
    pub fn load() -> Self {
        Self::load_from(store::default_db_path())
    }

    /// 从指定的数据库文件加载。旧版 `library.json` 按同目录同名去找。
    pub fn load_from(path: PathBuf) -> Self {
        let legacy_path = path.with_file_name("library.json");
        let mut library = Self {
            repos: Vec::new(),
            categories: CategoryTree::default(),
            tags: Vec::new(),
            sources: Vec::new(),
            settings: DiscoverySettings::default(),
            db: None,
            path,
            load_warning: None,
            load_note: None,
            save_state: SaveState::Untouched,
        };

        let db = match Db::open(&library.path) {
            Ok(db) => db,
            Err(error) => {
                library.load_warning = Some(format!(
                    "打开数据库 {} 失败：{error}。本次启动读不到也写不了数据。",
                    library.path.display()
                ));
                return library;
            }
        };

        match db::sources::is_empty(db.conn()) {
            Ok(true) => library.seed_empty_database(&db, &legacy_path),
            Ok(false) => {}
            Err(error) => {
                library.load_warning = Some(format!("读取数据库失败：{error}"));
            }
        }

        match read_snapshot(&db) {
            Ok(snapshot) => library.apply(snapshot),
            Err(error) => library.push_warning(format!("读取数据失败：{error}")),
        }

        library.db = Some(db);
        library
    }

    /// 全新的库：要么导入旧版 JSON，要么给一个默认发现源。
    fn seed_empty_database(&mut self, db: &Db, legacy_path: &Path) {
        if !legacy_path.exists() {
            for source in default_sources() {
                if let Err(error) = db::sources::insert_source(db.conn(), &source) {
                    self.push_warning(format!("写入默认发现源失败：{error}"));
                }
            }
            return;
        }

        match db::legacy::import(db, legacy_path) {
            Ok(report) => {
                let archived = match store::archive_file(legacy_path, "legacy") {
                    Ok(archived) => format!("，原文件已留档为 {}", archived.display()),
                    Err(error) => format!("；旧文件改名留档失败：{error}"),
                };
                self.load_note = Some(format!(
                    "已把 library.json 迁移进数据库：{} 个仓库、{} 个分类、{} 个标签、{} 个发现地址{}。",
                    report.repos, report.categories, report.tags, report.sources, archived
                ));
            }
            Err(error) => self.push_warning(format!(
                "导入 {} 失败：{error}。本次以空库启动，旧文件保持原样。",
                legacy_path.display()
            )),
        }
    }

    fn apply(&mut self, snapshot: Snapshot) {
        self.repos = snapshot.repos;
        self.categories = snapshot.categories;
        self.tags = snapshot.tags;
        self.sources = snapshot.sources;
        self.settings = snapshot.settings;
    }

    fn push_warning(&mut self, message: String) {
        self.load_warning = Some(match self.load_warning.take() {
            Some(existing) => format!("{existing}；{message}"),
            None => message,
        });
    }

    // ---- 状态 ----

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load_warning(&self) -> Option<&str> {
        self.load_warning.as_deref()
    }

    pub fn load_note(&self) -> Option<&str> {
        self.load_note.as_deref()
    }

    pub fn save_state(&self) -> &SaveState {
        &self.save_state
    }

    /// 写库。成功返回结果；失败返回 `None`，并把原因记进 `save_state`。
    fn write<T>(&mut self, f: impl FnOnce(&Db) -> db::DbResult<T>) -> Option<T> {
        let outcome = match self.db.as_ref() {
            Some(db) => f(db).map_err(|error| error.to_string()),
            None => Err("数据库没有打开，这次改动不会保存".to_string()),
        };
        match outcome {
            Ok(value) => {
                self.save_state = SaveState::Saved;
                Some(value)
            }
            Err(message) => {
                self.save_state = SaveState::Failed(message);
                None
            }
        }
    }

    /// 读库。读失败只能返回 `None`（读不到就是读不到），启动时的读取失败会
    /// 单独记进 `load_warning`。
    fn read<T>(&self, f: impl FnOnce(&Db) -> db::DbResult<T>) -> Option<T> {
        self.db.as_ref().and_then(|db| f(db).ok())
    }

    fn reload_categories(&mut self) {
        if let Some(nodes) = self.read(|db| db::taxonomy::load_categories(db.conn())) {
            self.categories = CategoryTree::from_nodes(nodes);
        }
    }

    fn reload_tags(&mut self) {
        if let Some(tags) = self.read(|db| db::taxonomy::load_tags(db.conn())) {
            self.tags = tags;
        }
    }

    /// 整批重读仓库。只在「分类删除」这类会连带改动一批仓库的操作之后用。
    fn reload_repos(&mut self) {
        if let Some(repos) = self.read(|db| db::repos::load_all(db.conn())) {
            self.repos = repos;
        }
    }

    // ---- 仓库 ----

    pub fn repo_count(&self) -> usize {
        self.repos.len()
    }

    pub fn repo(&self, id: &str) -> Option<&Repo> {
        self.repos.iter().find(|repo| repo.id() == id)
    }

    /// 仓库所属分类的完整路径。
    pub fn repo_category_path(&self, id: &str) -> Option<String> {
        let repo = self.repo(id)?;
        repo.category_id.and_then(|node| self.categories.path(node))
    }

    /// 加入一个仓库。已经存在时什么都不改，返回 `AlreadyPresent`。
    pub fn add_repo(&mut self, repo: Repo) -> AddOutcome {
        let id = repo.id();
        if self.repo(&id).is_some() {
            return AddOutcome::AlreadyPresent;
        }

        let tag_ids = self.tag_ids_for(&repo.tags);
        let Some(inserted) = self.write(|db| db::repos::insert(db.conn(), &repo, &tag_ids)) else {
            // 写不进去就不能进内存，否则界面上的列表会多出一个下次启动就消失的仓库。
            return AddOutcome::AlreadyPresent;
        };
        if !inserted {
            return AddOutcome::AlreadyPresent;
        }
        self.repos.insert(0, repo);
        AddOutcome::Added
    }

    pub fn remove_repo(&mut self, id: &str) -> bool {
        let Some(removed) = self.write(|db| db::repos::delete(db.conn(), id)) else {
            return false;
        };
        if !removed {
            return false;
        }
        self.repos.retain(|repo| repo.id() != id);
        true
    }

    /// 写入人工填写的汉语描述。空白输入等同于清除。
    pub fn set_description_zh(&mut self, id: &str, text: Option<String>) -> bool {
        let cleaned = text
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());

        let Some(written) = self.write(|db| {
            db::repos::set_description_zh(db.conn(), id, cleaned.as_deref())
        }) else {
            return false;
        };
        if !written {
            return false;
        }
        if let Some(repo) = self.repos.iter_mut().find(|repo| repo.id() == id) {
            repo.description_zh = cleaned;
        }
        true
    }

    /// 用抓回来的元数据补齐字段。
    ///
    /// 只填空缺，不覆盖已有内容——手工整理过的描述不该被一次刷新冲掉。
    pub fn fill_repo_metadata(
        &mut self,
        id: &str,
        description: Option<String>,
        created_at: Option<i64>,
        stars: Option<u64>,
    ) -> bool {
        let Some(written) = self.write(|db| {
            db::repos::fill_metadata(
                db.conn(),
                id,
                description.as_deref(),
                created_at,
                stars,
            )
        }) else {
            return false;
        };
        if !written {
            return false;
        }

        // 库里那条 UPDATE 只填空缺，内存也要跟着按同样的规则改，否则两边会漂。
        if let Some(repo) = self.repos.iter_mut().find(|repo| repo.id() == id) {
            if repo.description.is_none() {
                repo.description = description;
            }
            if repo.created_at.is_none() {
                repo.created_at = created_at;
            }
            if stars.is_some() {
                repo.stars = stars;
            }
        }
        true
    }

    /// 还需要补元数据的仓库，最新的排在前面——刚出现在列表里的项目先补齐最有感知。
    pub fn repos_pending_metadata(&self, limit: usize) -> Vec<RepoId> {
        self.read(|db| db::repos::pending_metadata(db.conn(), limit))
            .unwrap_or_default()
    }

    /// 缺少元数据的仓库数量，用于在界面上说明「还有多少没补全」。
    pub fn pending_metadata_count(&self) -> usize {
        self.read(|db| db::repos::count_pending_metadata(db.conn()))
            .unwrap_or_default()
    }

    /// 还有仓库缺描述或创建时间。
    pub fn has_pending_metadata(&self) -> bool {
        self.pending_metadata_count() > 0
    }

    /// 分页查一页仓库。搜索与分类筛选都在 SQL 里做，不必把整个库读进内存。
    ///
    /// 读库失败时返回一个空页——列表看起来是空的，而不是悄悄显示上一次的陈旧结果。
    pub fn query_repos(&self, query: &RepoQuery) -> RepoPage {
        let mut page = self.query_page_once(query);
        // 结果变少之后页码可能落在范围外（比如站在第 3 页时筛掉了一半仓库）。
        // 退回最后一页，别让用户对着一片空白还要自己点回去。
        if page.total > 0 && page.rows.is_empty() && query.page > 0 {
            let clamped = RepoQuery {
                page: page.page_count().saturating_sub(1),
                ..query.clone()
            };
            page = self.query_page_once(&clamped);
        }
        page
    }

    fn query_page_once(&self, query: &RepoQuery) -> RepoPage {
        let spec = self.build_spec(query);
        let Some((repos, total)) = self.read(|db| db::repos::query_page(db.conn(), &spec)) else {
            return RepoPage {
                rows: Vec::new(),
                total: 0,
                page: query.page,
                page_size: query.page_size,
            };
        };

        RepoPage {
            rows: repos.iter().map(|repo| self.to_row(repo)).collect(),
            total,
            page: query.page,
            page_size: query.page_size.max(1),
        }
    }

    fn to_row(&self, repo: &Repo) -> RepoRow {
        RepoRow {
            id: repo.id(),
            full_name: repo.full_name.clone(),
            subtitle: repo
                .display_description()
                .map(str::to_string)
                .unwrap_or_else(|| "暂无描述，等待补全".to_string()),
            language: repo.language.clone(),
            pending_metadata: repo.has_pending_metadata(),
            category_path: repo.category_id.and_then(|node| self.categories.path(node)),
            category_leaf: repo
                .category_id
                .and_then(|node| self.categories.get(node))
                .map(|node| node.name.clone()),
            tags: repo.tags.clone(),
        }
    }

    /// 把界面上的筛选条件翻译成 SQL 能用的形状。
    fn build_spec(&self, query: &RepoQuery) -> RepoQuerySpec {
        let needle = query.search.trim().to_lowercase();
        let search = (!needle.is_empty()).then(|| db::repos::like_pattern(&needle));

        let category = match &query.category {
            CategoryFilter::All => CategoryScope::All,
            CategoryFilter::Uncategorized => CategoryScope::Uncategorized,
            // 选中上级就等于选中它整棵子树。
            CategoryFilter::Named(path) => match self.categories.find_path(path) {
                Some(node) => CategoryScope::Subtrees(self.categories.subtree(node)),
                None => CategoryScope::Subtrees(Vec::new()),
            },
        };

        let page_size = query.page_size.max(1);
        RepoQuerySpec {
            search_categories: if search.is_some() {
                self.categories_matching(&needle)
            } else {
                Vec::new()
            },
            search,
            category,
            offset: query.page * page_size,
            limit: page_size,
        }
    }

    /// 路径里含这个词的分类节点，连同它们的后代——搜「开发」要能搜到
    /// 归在「开发/前端」下的仓库。
    fn categories_matching(&self, needle_lower: &str) -> Vec<i64> {
        let mut ids = Vec::new();
        for node in self.categories.flatten() {
            if node.path.to_lowercase().contains(needle_lower) {
                ids.extend(self.categories.subtree(node.id));
            }
        }
        ids.sort_unstable();
        ids.dedup();
        ids
    }

    // ---- 分类与标签 ----

    pub fn categories(&self) -> &CategoryTree {
        &self.categories
    }

    pub fn tags(&self) -> &[Tag] {
        &self.tags
    }

    /// 词表内容，顺序就是界面上的显示顺序。分类按树序铺平。
    pub fn names(&self, kind: TaxonomyKind) -> Vec<String> {
        match kind {
            TaxonomyKind::Category => self
                .categories
                .flatten()
                .into_iter()
                .map(|node| node.path)
                .collect(),
            TaxonomyKind::Tag => self.tags.iter().map(|tag| tag.name.clone()).collect(),
        }
    }

    /// 「分类」页要画的整棵树：每一级带上自己的仓库数。
    pub fn category_rows(&self) -> Vec<CategoryRow> {
        let mut direct: HashMap<i64, usize> = HashMap::new();
        for repo in &self.repos {
            if let Some(node) = repo.category_id {
                *direct.entry(node).or_default() += 1;
            }
        }

        self.categories
            .flatten()
            .into_iter()
            .map(|flat: FlatCategory| {
                let repos = self
                    .categories
                    .subtree(flat.id)
                    .into_iter()
                    .filter_map(|id| direct.get(&id).copied())
                    .sum();
                CategoryRow {
                    id: flat.id,
                    path: flat.path,
                    leaf: flat.leaf,
                    depth: flat.depth,
                    children: flat.children,
                    repos,
                }
            })
            .collect()
    }

    /// 往词表里加一个名字。
    ///
    /// 分类接受 `父/子` 这样的路径，**缺的上级会一起补上**：写 `开发/前端` 时
    /// 树上没有 `开发` 就顺手建一个，否则前端会挂在一条缺了根的枝上。
    pub fn add_name(&mut self, kind: TaxonomyKind, raw: &str) -> AddName {
        match kind {
            TaxonomyKind::Category => self.add_category(raw),
            TaxonomyKind::Tag => self.add_tag(raw),
        }
    }

    fn add_category(&mut self, raw: &str) -> AddName {
        let Some(path) = tax::clean_path(raw) else {
            return AddName::Invalid;
        };
        if self.categories.find_path(&path).is_some() {
            return AddName::AlreadyPresent;
        }

        // 先只看不动：把能解析出来的上级走到底，剩下的是要新建的。
        let mut parent = None;
        let mut missing: Vec<String> = Vec::new();
        for segment in tax::segments(&path) {
            if !missing.is_empty() {
                missing.push(segment);
                continue;
            }
            match self.categories.sibling_named(parent, &segment, None) {
                Some(existing) => parent = Some(existing),
                None => missing.push(segment),
            }
        }

        let outcome = self.write(|db| {
            db.transaction(|tx| {
                for segment in &missing {
                    parent = Some(db::taxonomy::insert_category(tx, parent, segment)?);
                }
                Ok(())
            })
        });
        if outcome.is_none() {
            return AddName::Failed;
        }

        self.reload_categories();
        AddName::Added
    }

    fn add_tag(&mut self, raw: &str) -> AddName {
        let Some(name) = tax::clean_name(raw) else {
            return AddName::Invalid;
        };
        if self.tag_named(&name).is_some() {
            return AddName::AlreadyPresent;
        }

        if self
            .write(|db| db::taxonomy::insert_tag(db.conn(), &name))
            .is_none()
        {
            return AddName::Failed;
        }
        self.reload_tags();
        AddName::Added
    }

    /// 改名字，并让已经用上它的地方跟着变。
    ///
    /// 分类这里填的是**完整路径**，所以改路径就等于移动整棵子树
    /// （`开发/前端` → `工具/前端`），「挪到别的分类底下」不需要另做一个功能。
    /// 因为仓库引用的是节点 id，改名与移动都不会碰到仓库那一行。
    pub fn rename_name(&mut self, kind: TaxonomyKind, old: &str, new: &str) -> RenameName {
        match kind {
            TaxonomyKind::Category => self.rename_category(old, new),
            TaxonomyKind::Tag => self.rename_tag(old, new),
        }
    }

    fn rename_category(&mut self, old: &str, new: &str) -> RenameName {
        let Some(cleaned) = tax::clean_path(new) else {
            return RenameName::Invalid;
        };
        let Some(id) = self.categories.find_path(old) else {
            return RenameName::NotFound;
        };

        let segments = tax::segments(&cleaned);
        let Some((leaf, parents)) = segments.split_last() else {
            return RenameName::Invalid;
        };

        // 先只看不动：把新上级解析到底，同时挡住「挪进自己的下级」。
        let mut parent = None;
        let mut missing: Vec<String> = Vec::new();
        for segment in parents {
            if !missing.is_empty() {
                missing.push(segment.clone());
                continue;
            }
            match self.categories.sibling_named(parent, segment, None) {
                Some(existing) => {
                    if self.categories.is_within(existing, id) {
                        // `开发` 改成 `开发/前端` 会让它成为自己的后代，树会断成一个环。
                        return RenameName::Circular;
                    }
                    parent = Some(existing);
                }
                // 缺的这一级连同它下面的都会新建，不可能落在旧子树里。
                None => missing.push(segment.clone()),
            }
        }

        let current = self.categories.get(id).expect("刚刚还在树上");
        if missing.is_empty()
            && current.parent == parent
            && tax::same_name(&current.name, leaf)
        {
            // 只有大小写或空白变了的输入算同一个名字，原有写法留着。
            return RenameName::Unchanged;
        }
        if missing.is_empty() && self.categories.sibling_named(parent, leaf, Some(id)).is_some() {
            return RenameName::NameTaken;
        }
        let current_parent = current.parent;

        let outcome = self.write(|db| {
            db.transaction(|tx| {
                // 缺的上级一起建出来，父节点顺着往下走。
                for segment in &missing {
                    parent = Some(db::taxonomy::insert_category(tx, parent, segment)?);
                }
                // 上级真的换了才挪。只谈改名时若顺手挪一下，它会被挤到同级的末尾——
                // 改个名字不该同时换位置。
                if parent != current_parent {
                    db::taxonomy::move_category(tx, id, parent)?;
                }
                db::taxonomy::rename_category(tx, id, leaf)?;
                Ok(())
            })
        });
        if outcome.is_none() {
            return RenameName::Failed;
        }

        self.reload_categories();
        RenameName::Renamed
    }

    fn rename_tag(&mut self, old: &str, new: &str) -> RenameName {
        let Some(cleaned) = tax::clean_name(new) else {
            return RenameName::Invalid;
        };
        let Some(tag) = self.tag_named(old) else {
            return RenameName::NotFound;
        };
        if tax::same_name(&tag.name, &cleaned) {
            return RenameName::Unchanged;
        }
        if self.tag_named(&cleaned).is_some() {
            return RenameName::NameTaken;
        }

        let id = tag.id;
        let previous = tag.name.clone();
        if self
            .write(|db| db::taxonomy::rename_tag(db.conn(), id, &cleaned))
            .is_none()
        {
            return RenameName::Failed;
        }

        self.reload_tags();
        // 仓库上存的是标签名字，改名要跟着改到每一处引用。
        for repo in &mut self.repos {
            for tag in repo.tags.iter_mut() {
                if tax::same_name(tag, &previous) {
                    *tag = cleaned.clone();
                }
            }
        }
        RenameName::Renamed
    }

    /// 从词表里删掉一个名字。返回受影响的仓库数量，调用方据此说明后果。
    ///
    /// 分类是**连子树一起删**：删掉 `开发` 之后它下面的分类也都不在了，那些仓库
    /// 回到未分类，但仓库本身一个都不会被移除。
    pub fn remove_name(&mut self, kind: TaxonomyKind, name: &str) -> usize {
        match kind {
            TaxonomyKind::Category => self.remove_category(name),
            TaxonomyKind::Tag => self.remove_tag(name),
        }
    }

    fn remove_category(&mut self, name: &str) -> usize {
        let Some(id) = self.categories.find_path(name) else {
            return 0;
        };
        let affected = self.repo_count_in_subtree(id);

        if self
            .write(|db| db::taxonomy::delete_category(db.conn(), id))
            .is_none()
        {
            return 0;
        }

        // 级联已经把仓库的 category_id 置空了，整批重读最省事也最不会漏。
        self.reload_categories();
        self.reload_repos();
        affected
    }

    fn remove_tag(&mut self, name: &str) -> usize {
        let Some(tag) = self.tag_named(name) else {
            return 0;
        };
        let id = tag.id;
        let affected = self.repo_count_using(TaxonomyKind::Tag, name);

        if self
            .write(|db| db::taxonomy::delete_tag(db.conn(), id))
            .is_none()
        {
            return 0;
        }

        self.reload_tags();
        for repo in &mut self.repos {
            repo.tags.retain(|tag| !tax::same_name(tag, name));
        }
        affected
    }

    /// 有多少个仓库正在用这个名字。分类算上它的后代——「开发」底下挂了多少仓库，
    /// 加上「开发/前端」「开发/后端」的才说得通。
    pub fn repo_count_using(&self, kind: TaxonomyKind, name: &str) -> usize {
        match kind {
            TaxonomyKind::Category => match self.categories.find_path(name) {
                Some(id) => self.repo_count_in_subtree(id),
                None => 0,
            },
            TaxonomyKind::Tag => self
                .repos
                .iter()
                .filter(|repo| repo.tags.iter().any(|tag| tax::same_name(tag, name)))
                .count(),
        }
    }

    fn repo_count_in_subtree(&self, id: i64) -> usize {
        let nodes = self.categories.subtree(id);
        self.repos
            .iter()
            .filter(|repo| {
                repo.category_id
                    .is_some_and(|node| nodes.contains(&node))
            })
            .count()
    }

    /// 还没归类的仓库数量。
    pub fn uncategorized_count(&self) -> usize {
        self.repos
            .iter()
            .filter(|repo| repo.category_id.is_none())
            .count()
    }

    /// 给仓库定分类；`None` 表示移出分类（回到未分类）。
    ///
    /// 名字必须已经在树上——要新建就先 `add_name`。不在树上时返回 `false`
    /// 且什么都不改：静默挂到一个不存在的节点上，会让两处数据永远对不上。
    pub fn set_repo_category(&mut self, id: &str, category: Option<&str>) -> bool {
        let node = match category {
            None => None,
            Some(name) => match self.categories.find_path(name) {
                Some(node) => Some(node),
                None => return false,
            },
        };

        let Some(written) = self.write(|db| db::repos::set_category(db.conn(), id, node)) else {
            return false;
        };
        if !written {
            return false;
        }
        if let Some(repo) = self.repos.iter_mut().find(|repo| repo.id() == id) {
            repo.category_id = node;
        }
        true
    }

    /// 覆盖仓库的标签集合，同样要求所有名字都已经在词表里。
    ///
    /// 存下来的顺序取词表顺序，而不是点击顺序：同一组标签在任何仓库上看起来都
    /// 一样，也不会因为先点谁后点谁而有两种排法。
    pub fn set_repo_tags(&mut self, id: &str, tags: &[String]) -> bool {
        if tags.iter().any(|name| self.tag_named(name).is_none()) {
            return false;
        }

        let ordered: Vec<String> = self
            .tags
            .iter()
            .filter(|tag| tags.iter().any(|name| tax::same_name(name, &tag.name)))
            .map(|tag| tag.name.clone())
            .collect();
        let tag_ids: Vec<i64> = self
            .tags
            .iter()
            .filter(|tag| tags.iter().any(|name| tax::same_name(name, &tag.name)))
            .map(|tag| tag.id)
            .collect();

        // 仓库不在内存里就先拦下：`set_tags` 只回报「写成功没有」，它不区分
        // 「仓库不存在」和「这些标签本来就都在」——那两种都是一样的行数。
        if self.repo(id).is_none() {
            return false;
        }

        if self.write(|db| db::repos::set_tags(db.conn(), id, &tag_ids)).is_none() {
            return false;
        }

        if let Some(repo) = self.repos.iter_mut().find(|repo| repo.id() == id) {
            repo.tags = ordered;
        }
        true
    }

    fn tag_named(&self, name: &str) -> Option<&Tag> {
        self.tags
            .iter()
            .find(|tag| tax::same_name(&tag.name, name))
    }

    /// 把标签名字换成 id，顺序按词表。仓库上出现的名字都必须在词表里。
    fn tag_ids_for(&self, names: &[String]) -> Vec<i64> {
        self.tags
            .iter()
            .filter(|tag| names.iter().any(|name| tax::same_name(name, &tag.name)))
            .map(|tag| tag.id)
            .collect()
    }

    // ---- 发现源 ----

    pub fn sources(&self) -> &[DiscoverySource] {
        &self.sources
    }

    pub fn enabled_sources(&self) -> impl Iterator<Item = &DiscoverySource> {
        self.sources.iter().filter(|source| source.enabled)
    }

    pub fn source(&self, url: &str) -> Option<&DiscoverySource> {
        self.sources.iter().find(|source| source.url == url)
    }

    /// 同一个地址只保留一份，避免重复抓取同一个榜单。
    pub fn add_source(&mut self, source: DiscoverySource) -> AddOutcome {
        if self.source(&source.url).is_some() {
            return AddOutcome::AlreadyPresent;
        }

        let Some(inserted) = self.write(|db| db::sources::insert_source(db.conn(), &source)) else {
            return AddOutcome::AlreadyPresent;
        };
        if !inserted {
            return AddOutcome::AlreadyPresent;
        }
        self.sources.push(source);
        AddOutcome::Added
    }

    pub fn remove_source(&mut self, url: &str) -> bool {
        let Some(removed) = self.write(|db| db::sources::delete_source(db.conn(), url)) else {
            return false;
        };
        if !removed {
            return false;
        }
        self.sources.retain(|source| source.url != url);
        true
    }

    pub fn set_source_enabled(&mut self, url: &str, enabled: bool) -> bool {
        let Some(written) = self.write(|db| db::sources::set_source_enabled(db.conn(), url, enabled))
        else {
            return false;
        };
        if !written {
            return false;
        }
        if let Some(source) = self.sources.iter_mut().find(|source| source.url == url) {
            source.enabled = enabled;
        }
        true
    }

    pub fn record_source_run(&mut self, url: &str, run: SourceRun) -> bool {
        let Some(written) = self.write(|db| db::sources::record_source_run(db.conn(), url, &run))
        else {
            return false;
        };
        if !written {
            return false;
        }
        if let Some(source) = self.sources.iter_mut().find(|source| source.url == url) {
            source.last_run = Some(run);
        }
        true
    }

    // ---- 设置 ----

    pub fn settings(&self) -> &DiscoverySettings {
        &self.settings
    }

    pub fn update_settings(&mut self, settings: DiscoverySettings) {
        if self
            .write(|db| db::sources::save_settings(db.conn(), &settings))
            .is_none()
        {
            return;
        }
        self.settings = settings;
    }

    /// 上次自动运行的起点：全部发现源里最近一次运行时间。
    pub fn last_auto_run_at(&self) -> Option<i64> {
        self.sources
            .iter()
            .filter_map(|source| source.last_run.as_ref())
            .map(|run| run.finished_at)
            .max()
    }
}

fn read_snapshot(db: &Db) -> db::DbResult<Snapshot> {
    let conn = db.conn();
    Ok(Snapshot {
        repos: db::repos::load_all(conn)?,
        categories: CategoryTree::from_nodes(db::taxonomy::load_categories(conn)?),
        tags: db::taxonomy::load_tags(conn)?,
        sources: db::sources::load_sources(conn)?,
        settings: db::sources::load_settings(conn)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::DbError;
    use crate::repos::model::DEFAULT_PAGE_SIZE;

    /// 让 `DbError` 在测试里也能直接 `unwrap`，省掉一层 `map_err`。
    impl DbError {
        fn message(&self) -> String {
            self.to_string()
        }
    }

    /// 每次调用都新建一个目录：固定目录名会让「第二次跑测试」读到上一次写下的数据，
    /// 于是断言开始飘。
    fn unique_dir(name: &str) -> PathBuf {
        use std::time::{SystemTime, UNIX_EPOCH};
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default();
        let dir = std::env::temp_dir().join(format!(
            "ngy-fav-library-test-{name}-{}-{stamp}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn temp_library(name: &str) -> Library {
        Library::load_from(unique_dir(name).join("library.db"))
    }

    fn library_with_repos(name: &str) -> Library {
        let mut library = temp_library(name);
        for repo in ["a/one", "b/two", "c/three"] {
            library.add_repo(Repo::new(repo.to_string(), None, 1));
        }
        library.add_name(TaxonomyKind::Category, "工具");
        library.add_name(TaxonomyKind::Category, "学习");
        library.add_name(TaxonomyKind::Tag, "cli");
        library.add_name(TaxonomyKind::Tag, "待读");
        library
    }

    fn query() -> RepoQuery {
        RepoQuery::default()
    }

    // ---- 基础 ----

    #[test]
    fn fresh_library_gets_the_default_source() {
        let library = temp_library("fresh");
        assert_eq!(library.repo_count(), 0);
        assert_eq!(library.sources().len(), 1);
        assert!(library.sources()[0].url.contains("/trending"));
        assert!(library.path().extension().is_some_and(|ext| ext == "db"));
    }

    #[test]
    fn everything_survives_a_reopen() {
        let dir = unique_dir("reopen");
        let path = dir.join("library.db");

        let mut library = Library::load_from(path.clone());
        let mut repo = Repo::new("coder/coder".to_string(), Some("周榜".to_string()), 99);
        repo.description_zh = Some("汉语描述".to_string());
        library.add_repo(repo);
        library.add_name(TaxonomyKind::Category, "开发/前端");
        assert!(library.set_repo_category("coder/coder", Some("开发/前端")));
        library.add_name(TaxonomyKind::Tag, "cli");
        assert!(library.set_repo_tags("coder/coder", &["cli".to_string()]));

        let reloaded = Library::load_from(path);
        assert_eq!(reloaded.load_warning(), None);
        assert_eq!(reloaded.repo_count(), 1);
        assert_eq!(
            reloaded.repo("coder/coder").unwrap().description_zh.as_deref(),
            Some("汉语描述")
        );
        assert_eq!(
            reloaded.repo_category_path("coder/coder").as_deref(),
            Some("开发/前端")
        );
        assert_eq!(reloaded.repo("coder/coder").unwrap().tags, ["cli".to_string()]);
        assert_eq!(reloaded.names(TaxonomyKind::Category), ["开发".to_string(), "开发/前端".to_string()]);
    }

    #[test]
    fn adding_the_same_repo_twice_changes_nothing() {
        let mut library = temp_library("dup");
        let first = Repo::new("Coder/Coder".to_string(), None, 1);

        assert_eq!(library.add_repo(first), AddOutcome::Added);
        assert_eq!(
            library.add_repo(Repo::new("coder/coder".to_string(), None, 2)),
            AddOutcome::AlreadyPresent
        );
        assert_eq!(library.repo_count(), 1);
        // 保留的是先入库的那条，它的发现时间没被后一次覆盖。
        assert_eq!(library.repo("coder/coder").unwrap().discovered_at, 1);
    }

    #[test]
    fn metadata_fill_does_not_overwrite_existing_values() {
        let mut library = temp_library("fill");
        let mut repo = Repo::new("coder/coder".to_string(), None, 1);
        repo.description = Some("原描述".to_string());
        library.add_repo(repo);

        library.fill_repo_metadata(
            "coder/coder",
            Some("新描述".to_string()),
            Some(42),
            Some(7),
        );

        let stored = library.repo("coder/coder").unwrap();
        assert_eq!(stored.description.as_deref(), Some("原描述"));
        assert_eq!(stored.created_at, Some(42));
        assert_eq!(stored.stars, Some(7));
    }

    #[test]
    fn description_edits_round_trip_and_blank_clears_it() {
        let mut library = temp_library("zh");
        library.add_repo(Repo::new("a/one".to_string(), None, 1));

        assert!(library.set_description_zh("a/one", Some("  汉语  ".to_string())));
        assert_eq!(
            library.repo("a/one").unwrap().description_zh.as_deref(),
            Some("汉语")
        );

        assert!(library.set_description_zh("a/one", Some("   ".to_string())));
        assert_eq!(library.repo("a/one").unwrap().description_zh, None);

        // 不存在的仓库不能谎报成功。
        assert!(!library.set_description_zh("z/none", Some("x".to_string())));
    }

    #[test]
    fn removing_a_repo_takes_it_out_of_the_database_too() {
        let mut library = library_with_repos("remove-repo");
        assert!(library.remove_repo("a/one"));
        assert!(!library.remove_repo("a/one"), "已经没了");

        let path = library.path().to_path_buf();
        let reloaded = Library::load_from(path);
        assert_eq!(reloaded.repo_count(), 2);
        assert!(reloaded.repo("a/one").is_none());
    }

    #[test]
    fn metadata_backfill_is_capped_and_newest_first() {
        let mut library = temp_library("pending");
        for (name, discovered_at) in [("a/one", 1), ("b/two", 2), ("c/three", 3)] {
            library.add_repo(Repo::new(name.to_string(), None, discovered_at));
        }

        let pending = library.repos_pending_metadata(2);
        assert_eq!(pending, vec!["c/three".to_string(), "b/two".to_string()]);
        assert_eq!(library.pending_metadata_count(), 3, "总数不受批次上限影响");
    }

    // ---- 分类与标签 ----

    #[test]
    fn names_are_deduplicated_case_insensitively() {
        let mut library = temp_library("add-name");

        assert_eq!(library.add_name(TaxonomyKind::Category, "  工具 "), AddName::Added);
        assert_eq!(library.add_name(TaxonomyKind::Category, "工具"), AddName::AlreadyPresent);
        assert_eq!(library.add_name(TaxonomyKind::Category, "TOOL"), AddName::Added);
        assert_eq!(library.add_name(TaxonomyKind::Category, "tool"), AddName::AlreadyPresent);
        assert_eq!(library.add_name(TaxonomyKind::Category, "   "), AddName::Invalid);

        assert_eq!(
            library.names(TaxonomyKind::Category),
            ["工具".to_string(), "TOOL".to_string()]
        );
        assert!(library.names(TaxonomyKind::Tag).is_empty(), "分类不该串到标签里");
    }

    #[test]
    fn a_repo_has_at_most_one_category_but_many_tags() {
        let mut library = library_with_repos("assign");

        assert!(library.set_repo_category("a/one", Some("工具")));
        assert!(library.set_repo_tags("a/one", &["cli".to_string(), "待读".to_string()]));

        assert_eq!(library.repo_category_path("a/one").as_deref(), Some("工具"));
        assert_eq!(
            library.repo("a/one").unwrap().tags,
            ["cli".to_string(), "待读".to_string()]
        );

        // 再定一次分类是替换，不是追加。
        assert!(library.set_repo_category("a/one", Some("学习")));
        assert_eq!(library.repo_category_path("a/one").as_deref(), Some("学习"));

        // 移出分类回到未分类。
        assert!(library.set_repo_category("a/one", None));
        assert_eq!(library.repo_category_path("a/one"), None);
        assert_eq!(library.uncategorized_count(), 3);
    }

    #[test]
    fn tags_are_stored_in_vocabulary_order_not_click_order() {
        let mut library = library_with_repos("tag-order");
        assert!(library.set_repo_tags("a/one", &["待读".to_string(), "cli".to_string()]));

        assert_eq!(
            library.repo("a/one").unwrap().tags,
            ["cli".to_string(), "待读".to_string()],
            "顺序由词表决定，跟先点谁无关"
        );
    }

    #[test]
    fn names_outside_the_vocabulary_are_refused() {
        let mut library = library_with_repos("unknown-name");

        assert!(!library.set_repo_category("a/one", Some("没建过的分类")));
        assert_eq!(library.repo("a/one").unwrap().category_id, None);

        assert!(!library.set_repo_tags("a/one", &["刚编的标签".to_string()]));
        assert!(library.repo("a/one").unwrap().tags.is_empty());

        // 仓库不存在时也不能谎报成功。
        assert!(!library.set_repo_category("z/none", Some("工具")));
        assert!(!library.set_repo_tags("z/none", &[]));
    }

    #[test]
    fn renaming_a_category_needs_no_bookkeeping_on_the_repos() {
        let mut library = library_with_repos("rename");
        assert!(library.set_repo_category("a/one", Some("工具")));
        assert!(library.set_repo_category("b/two", Some("工具")));

        assert_eq!(
            library.rename_name(TaxonomyKind::Category, "工具", "开发工具"),
            RenameName::Renamed
        );

        assert_eq!(
            library.repo_category_path("a/one").as_deref(),
            Some("开发工具")
        );
        assert_eq!(
            library.repo_category_path("b/two").as_deref(),
            Some("开发工具")
        );
        assert_eq!(
            library.names(TaxonomyKind::Category),
            ["开发工具".to_string(), "学习".to_string()]
        );
    }

    #[test]
    fn renaming_a_tag_follows_into_the_repos_and_keeps_the_order() {
        let mut library = library_with_repos("rename-tag");
        assert!(library.set_repo_tags("a/one", &["cli".to_string(), "待读".to_string()]));

        assert_eq!(
            library.rename_name(TaxonomyKind::Tag, "待读", "稍后读"),
            RenameName::Renamed
        );

        assert_eq!(
            library.repo("a/one").unwrap().tags,
            ["cli".to_string(), "稍后读".to_string()]
        );
    }

    #[test]
    fn rename_reports_the_cases_it_cannot_do() {
        let mut library = library_with_repos("rename-outcome");

        assert_eq!(
            library.rename_name(TaxonomyKind::Category, "工具", "学习"),
            RenameName::NameTaken
        );
        assert_eq!(
            library.rename_name(TaxonomyKind::Category, "没有这个", "随便"),
            RenameName::NotFound
        );
        assert_eq!(
            library.rename_name(TaxonomyKind::Category, "工具", "  "),
            RenameName::Invalid
        );
        // 只改大小写算同一个名字，原有写法保留。
        assert_eq!(
            library.rename_name(TaxonomyKind::Category, "工具", "  工具 "),
            RenameName::Unchanged
        );
        assert_eq!(
            library.names(TaxonomyKind::Category),
            ["工具".to_string(), "学习".to_string()]
        );
    }

    #[test]
    fn removing_a_category_uncategorizes_its_repos() {
        let mut library = library_with_repos("remove-category");
        assert!(library.set_repo_category("a/one", Some("工具")));
        assert!(library.set_repo_category("b/two", Some("工具")));

        assert_eq!(library.repo_count_using(TaxonomyKind::Category, "工具"), 2);
        assert_eq!(library.remove_name(TaxonomyKind::Category, "工具"), 2);

        assert_eq!(library.names(TaxonomyKind::Category), ["学习".to_string()]);
        assert_eq!(library.repo_category_path("a/one"), None);
        assert_eq!(library.repo_category_path("b/two"), None);
        assert_eq!(library.uncategorized_count(), 3);
        assert_eq!(library.remove_name(TaxonomyKind::Category, "工具"), 0, "已经没了");
    }

    #[test]
    fn removing_a_tag_only_detaches_it_from_the_repos() {
        let mut library = library_with_repos("remove-tag");
        assert!(library.set_repo_category("a/one", Some("工具")));
        assert!(library.set_repo_tags("a/one", &["cli".to_string(), "待读".to_string()]));

        assert_eq!(library.remove_name(TaxonomyKind::Tag, "待读"), 1);

        let repo = library.repo("a/one").unwrap();
        assert_eq!(repo.tags, ["cli".to_string()], "其它标签和分类都不受影响");
        assert_eq!(library.repo_category_path("a/one").as_deref(), Some("工具"));
    }

    // ---- 分类的树 ----

    #[test]
    fn adding_a_nested_category_brings_its_ancestors_along() {
        let mut library = temp_library("add-nested");

        assert_eq!(
            library.add_name(TaxonomyKind::Category, "开发/前端/框架"),
            AddName::Added
        );

        assert_eq!(
            library.names(TaxonomyKind::Category),
            ["开发".to_string(), "开发/前端".to_string(), "开发/前端/框架".to_string()],
            "缺的上级要一起补上，否则树是断的"
        );
        assert_eq!(
            library.add_name(TaxonomyKind::Category, "开发/前端"),
            AddName::AlreadyPresent
        );
        assert_eq!(library.categories().len(), 3);
    }

    #[test]
    fn sibling_names_are_refused_but_the_same_name_may_reappear_below() {
        let mut library = temp_library("siblings");
        library.add_name(TaxonomyKind::Category, "开发");
        library.add_name(TaxonomyKind::Category, "开发/前端");
        library.add_name(TaxonomyKind::Category, "工具/前端");

        // 两条不同的枝上可以有同名的一级。
        let rows = library.category_rows();
        assert!(rows.iter().any(|row| row.path == "工具/前端"));

        // 同一个上级下面不行。
        assert_eq!(
            library.add_name(TaxonomyKind::Category, "开发/前端"),
            AddName::AlreadyPresent
        );
    }

    #[test]
    fn category_rows_carry_depth_children_and_subtree_counts() {
        let mut library = temp_library("rows");
        for repo in ["a/one", "b/two", "c/three"] {
            library.add_repo(Repo::new(repo.to_string(), None, 1));
        }
        library.add_name(TaxonomyKind::Category, "开发");
        library.add_name(TaxonomyKind::Category, "开发/前端");
        library.add_name(TaxonomyKind::Category, "开发工具");
        assert!(library.set_repo_category("a/one", Some("开发")));
        assert!(library.set_repo_category("b/two", Some("开发/前端")));
        assert!(library.set_repo_category("c/three", Some("开发工具")));

        let rows = library.category_rows();
        let dev = rows.iter().find(|row| row.path == "开发").unwrap();
        assert_eq!(dev.depth, 1);
        assert_eq!(dev.children, 1);
        assert_eq!(dev.repos, 2, "上级算上整棵子树");

        let front = rows.iter().find(|row| row.path == "开发/前端").unwrap();
        assert_eq!(front.depth, 2);
        assert_eq!(front.leaf, "前端");
        assert_eq!(front.repos, 1);

        let sibling = rows.iter().find(|row| row.path == "开发工具").unwrap();
        assert_eq!(
            sibling.repos, 1,
            "名字只是前缀相同，不算同一棵树"
        );
    }

    #[test]
    fn renaming_a_parent_moves_its_whole_subtree() {
        let mut library = temp_library("rename-subtree");
        for name in ["开发", "开发/前端", "开发/后端", "学习"] {
            library.add_name(TaxonomyKind::Category, name);
        }
        library.add_repo(Repo::new("a/one".to_string(), None, 1));
        assert!(library.set_repo_category("a/one", Some("开发/前端")));

        assert_eq!(
            library.rename_name(TaxonomyKind::Category, "开发", "工具"),
            RenameName::Renamed
        );

        assert_eq!(
            library.names(TaxonomyKind::Category),
            [
                "工具".to_string(),
                "工具/前端".to_string(),
                "工具/后端".to_string(),
                "学习".to_string()
            ]
        );
        assert_eq!(library.repo_category_path("a/one").as_deref(), Some("工具/前端"));
    }

    #[test]
    fn renaming_onto_a_new_parent_moves_the_subtree_and_creates_the_parent() {
        let mut library = temp_library("move-subtree");
        library.add_name(TaxonomyKind::Category, "开发");
        library.add_name(TaxonomyKind::Category, "开发/前端");
        library.add_name(TaxonomyKind::Category, "工具");
        library.add_repo(Repo::new("a/one".to_string(), None, 1));
        assert!(library.set_repo_category("a/one", Some("开发/前端")));

        assert_eq!(
            library.rename_name(TaxonomyKind::Category, "开发/前端", "工具/前端"),
            RenameName::Renamed
        );

        assert_eq!(
            library.names(TaxonomyKind::Category),
            ["开发".to_string(), "工具".to_string(), "工具/前端".to_string()],
            "改后的写法留在原位置，树没有多出一条"
        );
        assert_eq!(library.repo_category_path("a/one").as_deref(), Some("工具/前端"));

        // 缺的上级也会顺手建出来。
        assert_eq!(
            library.rename_name(TaxonomyKind::Category, "工具/前端", "学习/前端"),
            RenameName::Renamed
        );
        assert_eq!(
            library.names(TaxonomyKind::Category),
            [
                "开发".to_string(),
                "工具".to_string(),
                "学习".to_string(),
                "学习/前端".to_string()
            ],
            "同级按建立顺序排：学习是这次改名才建出来的，排在工具后面"
        );
    }

    #[test]
    fn renaming_refuses_to_nest_a_category_inside_itself() {
        let mut library = temp_library("rename-cycle");
        for name in ["开发", "开发/前端"] {
            library.add_name(TaxonomyKind::Category, name);
        }

        assert_eq!(
            library.rename_name(TaxonomyKind::Category, "开发", "开发/前端/更前面"),
            RenameName::Circular
        );
        assert_eq!(
            library.names(TaxonomyKind::Category),
            ["开发".to_string(), "开发/前端".to_string()],
            "拒绝之后树必须原样"
        );
    }

    #[test]
    fn removing_a_parent_takes_the_subtree_and_uncategorizes_those_repos() {
        let mut library = temp_library("remove-subtree");
        for repo in ["a/one", "b/two", "c/three"] {
            library.add_repo(Repo::new(repo.to_string(), None, 1));
        }
        for name in ["开发", "开发/前端", "学习"] {
            library.add_name(TaxonomyKind::Category, name);
        }
        assert!(library.set_repo_category("a/one", Some("开发")));
        assert!(library.set_repo_category("b/two", Some("开发/前端")));
        assert!(library.set_repo_category("c/three", Some("学习")));

        assert_eq!(library.remove_name(TaxonomyKind::Category, "开发"), 2);

        assert_eq!(library.names(TaxonomyKind::Category), ["学习".to_string()]);
        assert_eq!(library.repo_category_path("a/one"), None);
        assert_eq!(library.repo_category_path("b/two"), None);
        assert_eq!(
            library.repo_category_path("c/three").as_deref(),
            Some("学习"),
            "别的分支不受影响"
        );
    }

    #[test]
    fn removing_a_leaf_leaves_its_parent_alone() {
        let mut library = temp_library("remove-leaf");
        for name in ["开发", "开发/前端"] {
            library.add_name(TaxonomyKind::Category, name);
        }
        library.add_repo(Repo::new("a/one".to_string(), None, 1));
        assert!(library.set_repo_category("a/one", Some("开发/前端")));

        assert_eq!(library.remove_name(TaxonomyKind::Category, "开发/前端"), 1);

        assert_eq!(library.names(TaxonomyKind::Category), ["开发".to_string()]);
        assert_eq!(library.repo_category_path("a/one"), None);
    }

    // ---- 分页 ----

    #[test]
    fn paging_splits_the_list_and_reports_the_full_total() {
        let mut library = temp_library("paging");
        for index in 0..7 {
            library.add_repo(Repo::new(format!("a/repo-{index}"), None, index));
        }

        let first = library.query_repos(&RepoQuery {
            page: 0,
            page_size: 3,
            ..query()
        });
        assert_eq!(first.total, 7, "总数是整个结果集，不是这一页");
        assert_eq!(first.rows.len(), 3);
        assert_eq!(first.page_count(), 3);
        assert_eq!(first.rows[0].id, "a/repo-6", "最新的在最前");

        let last = library.query_repos(&RepoQuery {
            page: 2,
            page_size: 3,
            ..query()
        });
        assert_eq!(last.rows.len(), 1);
        assert_eq!(last.rows[0].id, "a/repo-0");
        assert!(!last.has_next());
    }

    #[test]
    fn a_page_beyond_the_end_falls_back_instead_of_showing_nothing() {
        let mut library = temp_library("paging-clamp");
        for index in 0..4 {
            library.add_repo(Repo::new(format!("a/repo-{index}"), None, index));
        }

        let clamped = library.query_repos(&RepoQuery {
            page: 9,
            page_size: 3,
            ..query()
        });
        assert_eq!(clamped.page, 1, "退回最后一页");
        assert_eq!(clamped.rows.len(), 1);

        // 一条都没有时不该陷入来回重查。
        let empty = library.query_repos(&RepoQuery {
            page: 3,
            page_size: 3,
            ..query()
        });
        assert!(empty.rows.is_empty() || empty.page == 1);
    }

    #[test]
    fn search_and_category_filter_narrow_the_total_too() {
        let mut library = temp_library("paging-filter");
        for (name, category) in [
            ("a/coder", Some("工具")),
            ("b/rust", Some("开发/前端")),
            ("c/other", None),
        ] {
            library.add_repo(Repo::new(name.to_string(), None, 1));
            if let Some(category) = category {
                library.add_name(TaxonomyKind::Category, category);
                assert!(library.set_repo_category(name, Some(category)));
            }
        }
        library.add_name(TaxonomyKind::Tag, "cli");
        assert!(library.set_repo_tags("b/rust", &["cli".to_string()]));

        let by_name = library.query_repos(&RepoQuery {
            search: "coder".to_string(),
            ..query()
        });
        assert_eq!(by_name.total, 1);
        assert_eq!(by_name.rows[0].id, "a/coder");

        let by_tag = library.query_repos(&RepoQuery {
            search: "cli".to_string(),
            ..query()
        });
        assert_eq!(by_tag.rows[0].id, "b/rust", "标签也能搜到");

        // 选中「开发」要连带它子树里的仓库。
        let by_tree = library.query_repos(&RepoQuery {
            category: CategoryFilter::Named("开发".to_string()),
            ..query()
        });
        assert_eq!(by_tree.total, 1);
        assert_eq!(by_tree.rows[0].id, "b/rust");

        let uncategorized = library.query_repos(&RepoQuery {
            category: CategoryFilter::Uncategorized,
            ..query()
        });
        assert_eq!(uncategorized.total, 1);
        assert_eq!(uncategorized.rows[0].id, "c/other");

        // 一个已经不存在的分类：宁可什么都不匹配，也不要静默退回「全部」。
        let gone = library.query_repos(&RepoQuery {
            category: CategoryFilter::Named("没有这个".to_string()),
            ..query()
        });
        assert_eq!(gone.total, 0);
    }

    #[test]
    fn the_rows_carry_the_category_path_for_display() {
        let mut library = temp_library("row-shape");
        library.add_repo(Repo::new("a/one".to_string(), None, 1));
        library.add_name(TaxonomyKind::Category, "开发/前端");
        assert!(library.set_repo_category("a/one", Some("开发/前端")));

        let page = library.query_repos(&RepoQuery {
            page_size: DEFAULT_PAGE_SIZE,
            ..query()
        });
        let row = &page.rows[0];
        assert_eq!(row.category_path.as_deref(), Some("开发/前端"));
        assert_eq!(row.category_leaf.as_deref(), Some("前端"), "行里只画末级名");
    }

    #[test]
    fn search_wildcards_are_escaped() {
        let mut library = temp_library("like-escape");
        library.add_repo(Repo::new("a/one".to_string(), None, 1));
        library.add_repo(Repo::new("a/two".to_string(), None, 2));

        // `_` 在 LIKE 里是单字符通配符，不转义的话 "a/one" 会连 "a/two" 一起命中。
        let page = library.query_repos(&RepoQuery {
            search: "a_one".to_string(),
            ..query()
        });
        assert_eq!(page.total, 0);
    }

    // ---- 发现源与设置 ----

    #[test]
    fn sources_and_settings_round_trip() {
        let dir = unique_dir("sources");
        let path = dir.join("library.db");
        let mut library = Library::load_from(path.clone());

        let source = DiscoverySource::new("月榜", "https://github.com/trending?since=monthly");
        assert_eq!(library.add_source(source), AddOutcome::Added);
        assert_eq!(
            library.add_source(DiscoverySource::new(
                "月榜",
                "https://github.com/trending?since=monthly"
            )),
            AddOutcome::AlreadyPresent
        );
        assert!(library.set_source_enabled("https://github.com/trending?since=monthly", false));

        let settings = DiscoverySettings {
            auto_discover: false,
            interval_minutes: 45,
            metadata_batch: 3,
            github_token: Some("ghp_x".to_string()),
        };
        library.update_settings(settings.clone());

        let reloaded = Library::load_from(path);
        assert_eq!(reloaded.sources().len(), 2);
        assert!(!reloaded.sources()[1].enabled);
        assert_eq!(reloaded.settings(), &settings);
    }

    #[test]
    fn the_last_auto_run_is_the_newest_of_all_sources() {
        let mut library = temp_library("last-run");
        let url = library.sources()[0].url.clone();
        library.record_source_run(
            &url,
            SourceRun {
                started_at: 1,
                finished_at: 500,
                found: 20,
                added: 20,
                skipped: 0,
                error: None,
            },
        );
        assert_eq!(library.last_auto_run_at(), Some(500));

        let path = library.path().to_path_buf();
        let reloaded = Library::load_from(path);
        assert_eq!(reloaded.last_auto_run_at(), Some(500));
        assert_eq!(reloaded.sources()[0].last_run.as_ref().unwrap().added, 20);
    }

    // ---- 故障路径 ----

    #[test]
    fn a_database_from_a_newer_version_is_reported_not_written() {
        let dir = unique_dir("too-new");
        let path = dir.join("library.db");
        {
            let db = Db::open(&path).unwrap();
            db.conn()
                .pragma_update(None, "user_version", db::schema::SCHEMA_VERSION + 1)
                .unwrap();
        }

        let library = Library::load_from(path);
        let warning = library.load_warning().expect("必须报告打不开");
        assert!(warning.contains("版本"), "得到：{warning}");
        assert_eq!(library.repo_count(), 0);
    }

    #[test]
    fn a_broken_legacy_file_leaves_the_database_empty_and_says_so() {
        let dir = unique_dir("legacy-broken");
        std::fs::write(dir.join("library.json"), "{ 这不是 json").unwrap();

        let library = Library::load_from(dir.join("library.db"));
        let warning = library.load_warning().expect("必须报告导入失败");
        assert!(warning.contains("导入"), "得到：{warning}");
        assert_eq!(library.repo_count(), 0, "坏文件不能变成半个库");

        // 旧文件保持原样，没有被改名也没有被删。
        assert!(dir.join("library.json").exists());
    }

    #[test]
    fn an_empty_database_imports_the_legacy_file_once_and_archives_it() {
        let dir = unique_dir("legacy-import");
        std::fs::write(
            dir.join("library.json"),
            r#"{
                "repos": [
                    {
                        "full_name": "coder/coder",
                        "discovered_at": 7,
                        "category": "开发/前端",
                        "tags": ["cli"]
                    }
                ],
                "categories": ["开发/前端"],
                "tags": ["cli"]
            }"#,
        )
        .unwrap();

        let library = Library::load_from(dir.join("library.db"));
        assert_eq!(library.load_warning(), None);
        let note = library.load_note().expect("要说明迁移了什么");
        assert!(note.contains("1 个仓库"), "得到：{note}");
        assert_eq!(library.repo_count(), 1);
        assert_eq!(
            library.repo_category_path("coder/coder").as_deref(),
            Some("开发/前端")
        );

        // 旧文件被改名留档，不会在下次启动时又被导一遍。
        assert!(!dir.join("library.json").exists());
        let archived = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().contains("legacy-"))
            .count();
        assert_eq!(archived, 1);

        // 再打开一次不该重复导入，也不该再报迁移。
        let again = Library::load_from(dir.join("library.db"));
        assert_eq!(again.load_note(), None);
        assert_eq!(again.repo_count(), 1);
    }

    #[test]
    fn a_database_that_already_has_data_ignores_the_legacy_file() {
        let dir = unique_dir("legacy-skip");
        let path = dir.join("library.db");

        let mut library = Library::load_from(path.clone());
        library.add_repo(Repo::new("a/one".to_string(), None, 1));

        // 后来才冒出来的旧文件不该盖掉已经在用的库。
        std::fs::write(
            dir.join("library.json"),
            r#"{"repos": [{"full_name": "b/two", "discovered_at": 2}]}"#,
        )
        .unwrap();

        let reloaded = Library::load_from(path);
        assert_eq!(reloaded.load_note(), None);
        assert_eq!(reloaded.repo_count(), 1);
        assert!(reloaded.repo("b/two").is_none());
        assert!(dir.join("library.json").exists(), "旧文件不该被动过");
    }

    /// `DbError` 得能变成一个给人看的句子——写库失败时它就是界面上那句提示。
    #[test]
    fn database_errors_have_a_readable_message() {
        let error = DbError::Format("文件读不懂".to_string());
        assert_eq!(error.message(), "文件读不懂");
    }
}

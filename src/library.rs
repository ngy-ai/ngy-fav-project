//! 领域数据的唯一持有者。
//!
//! 仓库、发现源、后台任务配置存在同一个 JSON 文件里，由一个 `Entity<Library>`
//! 统一读写。各个功能页只通过 `Entity<Library>` 观察它，不各自缓存副本——
//! 否则「后台发现了新仓库」和「列表里看到的仓库」会各说各话。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::discovery::model::{DiscoverySettings, DiscoverySource, SourceRun, default_sources};
use crate::repos::model::{Repo, RepoId};
use crate::shared::clock::now_unix;
use crate::shared::store;
use crate::taxonomy::model::{self as tax, AddName, RenameName, TaxonomyKind};

/// 落盘的数据形状。刻意保持扁平，没有多余的包装层。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LibraryData {
    #[serde(default)]
    pub repos: Vec<Repo>,
    /// 分类词表。名字就是身份，仓库上用同名的字符串引用它。
    #[serde(default)]
    pub categories: Vec<String>,
    /// 标签词表。
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub sources: Vec<DiscoverySource>,
    #[serde(default)]
    pub settings: DiscoverySettings,
}

/// 入库结果。调用方需要知道是新增还是重复，好把结果说给用户听。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddOutcome {
    Added,
    AlreadyPresent,
}

/// 最近一次写盘的状态。写盘失败必须能被看见——用户改了汉语描述却存不下，
/// 是必须当场知道的坏消息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SaveState {
    /// 尚未写过盘。
    Untouched,
    Saved,
    Failed(String),
}

pub struct Library {
    data: LibraryData,
    path: PathBuf,
    /// 启动时读取数据文件遇到的问题，在界面上如实展示。
    load_warning: Option<String>,
    save_state: SaveState,
}

impl Library {
    /// 从默认位置加载。
    ///
    /// 文件不存在 → 空库 + 默认发现源（首次启动的正常路径）。
    /// 文件损坏 → 把它改名留档，再以空库启动，并把原因带出去。
    pub fn load() -> Self {
        Self::load_from(store::default_library_path())
    }

    pub fn load_from(path: PathBuf) -> Self {
        let mut library = Self {
            data: LibraryData {
                settings: DiscoverySettings::default(),
                ..LibraryData::default()
            },
            path,
            load_warning: None,
            save_state: SaveState::Untouched,
        };

        match store::read_text(&library.path) {
            Ok(None) => {
                library.data.sources = default_sources();
            }
            Ok(Some(text)) => match serde_json::from_str::<LibraryData>(&text) {
                Ok(data) => {
                    library.data = data;
                    // 手改过文件时词表和仓库上的引用可能对不上，加载后先对齐一次。
                    // 只增不减，所以不会把谁的数据弄丢。
                    library.normalize_taxonomy();
                }
                Err(error) => {
                    let backup = library.quarantine_corrupt_file();
                    library.load_warning = Some(match backup {
                        Some(path) => format!(
                            "数据文件无法解析（{error}），已备份为 {}，本次以空库启动",
                            path.display()
                        ),
                        None => format!("数据文件无法解析（{error}），本次以空库启动"),
                    });
                    library.data.sources = default_sources();
                }
            },
            Err(error) => {
                library.load_warning = Some(format!("读取数据文件失败：{error}"));
                library.data.sources = default_sources();
            }
        }

        library
    }

    /// 损坏的文件不删，改名留档——里面可能有用户手写的汉语描述。
    fn quarantine_corrupt_file(&self) -> Option<PathBuf> {
        let backup = self
            .path
            .with_file_name(format!("library.corrupt-{}.json", now_unix()));
        std::fs::rename(&self.path, &backup).ok().map(|_| backup)
    }

    pub fn data(&self) -> &LibraryData {
        &self.data
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load_warning(&self) -> Option<&str> {
        self.load_warning.as_deref()
    }

    pub fn save_state(&self) -> &SaveState {
        &self.save_state
    }

    /// 立刻把当前数据写盘。
    ///
    /// 刻意同步执行：文件只有几十到几百 KB，耗时在毫秒级；换成后台线程就没法
    /// 把写失败回报给界面，而「看起来保存了其实没保存」正是最该避免的一种坏。
    pub fn persist(&mut self) {
        let json = match serde_json::to_string_pretty(&self.data) {
            Ok(json) => json,
            Err(error) => {
                self.save_state = SaveState::Failed(format!("序列化失败：{error}"));
                return;
            }
        };

        self.save_state = match store::write_atomic(&self.path, &json) {
            Ok(()) => SaveState::Saved,
            Err(error) => SaveState::Failed(format!("写入 {} 失败：{error}", self.path.display())),
        };
    }

    // ---- 仓库 ----

    /// 按发现时间倒序，最新的在最前。
    pub fn repos_newest_first(&self) -> Vec<&Repo> {
        let mut repos: Vec<&Repo> = self.data.repos.iter().collect();
        repos.sort_by(|a, b| {
            b.discovered_at
                .cmp(&a.discovered_at)
                .then_with(|| a.full_name.cmp(&b.full_name))
        });
        repos
    }

    pub fn repo(&self, id: &str) -> Option<&Repo> {
        self.data.repos.iter().find(|repo| repo.id() == id)
    }

    /// 加入一个仓库。已经存在时什么都不改，返回 `AlreadyPresent`。
    pub fn add_repo(&mut self, repo: Repo) -> AddOutcome {
        let id = repo.id();
        if self.data.repos.iter().any(|existing| existing.id() == id) {
            return AddOutcome::AlreadyPresent;
        }
        self.data.repos.push(repo);
        AddOutcome::Added
    }

    pub fn remove_repo(&mut self, id: &str) -> bool {
        let before = self.data.repos.len();
        self.data.repos.retain(|repo| repo.id() != id);
        self.data.repos.len() != before
    }

    /// 写入人工填写的汉语描述。空白输入等同于清除。
    pub fn set_description_zh(&mut self, id: &str, text: Option<String>) -> bool {
        let cleaned = text
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        match self.data.repos.iter_mut().find(|repo| repo.id() == id) {
            Some(repo) => {
                repo.description_zh = cleaned;
                true
            }
            None => false,
        }
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
        let Some(repo) = self.data.repos.iter_mut().find(|repo| repo.id() == id) else {
            return false;
        };

        if repo.description.is_none() {
            repo.description = description;
        }
        if repo.created_at.is_none() {
            repo.created_at = created_at;
        }
        if let Some(stars) = stars {
            repo.stars = Some(stars);
        }
        true
    }

    /// 还需要补元数据的仓库，最新的排在前面——刚出现在列表里的项目先补齐最有感知。
    pub fn repos_pending_metadata(&self, limit: usize) -> Vec<RepoId> {
        self.repos_newest_first()
            .into_iter()
            .filter(|repo| repo.has_pending_metadata())
            .take(limit)
            .map(Repo::id)
            .collect()
    }

    // ---- 分类与标签 ----

    /// 词表内容，顺序就是界面上的显示顺序。
    pub fn names(&self, kind: TaxonomyKind) -> &[String] {
        match kind {
            TaxonomyKind::Category => &self.data.categories,
            TaxonomyKind::Tag => &self.data.tags,
        }
    }

    fn names_mut(&mut self, kind: TaxonomyKind) -> &mut Vec<String> {
        match kind {
            TaxonomyKind::Category => &mut self.data.categories,
            TaxonomyKind::Tag => &mut self.data.tags,
        }
    }

    /// 往词表里加一个名字。
    pub fn add_name(&mut self, kind: TaxonomyKind, raw: &str) -> AddName {
        let Some(name) = tax::clean_name(raw) else {
            return AddName::Invalid;
        };
        if tax::contains(self.names(kind), &name) {
            return AddName::AlreadyPresent;
        }
        self.names_mut(kind).push(name);
        AddName::Added
    }

    /// 改名字，并把它在仓库上的引用一起改掉。
    pub fn rename_name(&mut self, kind: TaxonomyKind, old: &str, new: &str) -> RenameName {
        let Some(cleaned) = tax::clean_name(new) else {
            return RenameName::Invalid;
        };
        let Some(index) = tax::index_of(self.names(kind), old) else {
            return RenameName::NotFound;
        };
        if tax::same_name(old, &cleaned) {
            // 只有大小写或空白变了的输入算同一个名字，原有写法留着。
            return RenameName::Unchanged;
        }
        if tax::contains(self.names(kind), &cleaned) {
            return RenameName::NameTaken;
        }

        let previous = self.names_mut(kind)[index].clone();
        self.names_mut(kind)[index] = cleaned.clone();
        self.repoint_references(kind, &previous, Some(&cleaned));
        RenameName::Renamed
    }

    /// 从词表里删掉一个名字，并把它在仓库上的引用一起摘掉。
    /// 返回受影响的仓库数量，调用方据此说明后果。
    pub fn remove_name(&mut self, kind: TaxonomyKind, name: &str) -> usize {
        if tax::index_of(self.names(kind), name).is_none() {
            return 0;
        }
        let affected = self.repo_count_using(kind, name);
        self.names_mut(kind)
            .retain(|existing| !tax::same_name(existing, name));
        self.repoint_references(kind, name, None);
        affected
    }

    /// 有多少个仓库正在用这个名字。
    pub fn repo_count_using(&self, kind: TaxonomyKind, name: &str) -> usize {
        self.data
            .repos
            .iter()
            .filter(|repo| match kind {
                TaxonomyKind::Category => repo
                    .category
                    .as_deref()
                    .is_some_and(|current| tax::same_name(current, name)),
                TaxonomyKind::Tag => repo.tags.iter().any(|tag| tax::same_name(tag, name)),
            })
            .count()
    }

    /// 还没归类的仓库数量。
    pub fn uncategorized_count(&self) -> usize {
        self.data
            .repos
            .iter()
            .filter(|repo| repo.category.is_none())
            .count()
    }

    /// 给仓库定分类；`None` 表示移出分类（回到未分类）。
    ///
    /// 名字必须已经在词表里——要新建就先 `add_name`。不在词表里时返回 `false`
    /// 且什么都不改：静默塞进一个词表里没有的名字，会让两处数据永远对不上。
    pub fn set_repo_category(&mut self, id: &str, category: Option<&str>) -> bool {
        let stored = match category {
            None => None,
            Some(name) => match self
                .data
                .categories
                .iter()
                .find(|existing| tax::same_name(existing, name))
            {
                // 存词表里的写法，别让大小写漂出去。
                Some(existing) => Some(existing.clone()),
                None => return false,
            },
        };

        match self.data.repos.iter_mut().find(|repo| repo.id() == id) {
            Some(repo) => {
                repo.category = stored;
                true
            }
            None => false,
        }
    }

    /// 覆盖仓库的标签集合，同样要求所有名字都已经在词表里。
    ///
    /// 存下来的顺序取词表顺序，而不是点击顺序：同一组标签在任何仓库上看起来都
    /// 一样，也不会因为先点谁后点谁而有两种排法。
    pub fn set_repo_tags(&mut self, id: &str, tags: &[String]) -> bool {
        if tags.iter().any(|name| !tax::contains(&self.data.tags, name)) {
            return false;
        }
        let ordered: Vec<String> = self
            .data
            .tags
            .iter()
            .filter(|existing| tags.iter().any(|name| tax::same_name(existing, name)))
            .cloned()
            .collect();

        match self.data.repos.iter_mut().find(|repo| repo.id() == id) {
            Some(repo) => {
                repo.tags = ordered;
                true
            }
            None => false,
        }
    }

    /// 把一个名字在所有仓库上的引用换成另一个；`None` 表示摘掉引用。
    fn repoint_references(&mut self, kind: TaxonomyKind, from: &str, to: Option<&str>) {
        for repo in &mut self.data.repos {
            match kind {
                TaxonomyKind::Category => {
                    let matched = repo
                        .category
                        .as_deref()
                        .is_some_and(|current| tax::same_name(current, from));
                    if matched {
                        repo.category = to.map(str::to_string);
                    }
                }
                // 标签是就地改写而不是删了再加：位置保持不动，就还是词表的顺序。
                TaxonomyKind::Tag => match to {
                    Some(to) => {
                        for tag in repo.tags.iter_mut() {
                            if tax::same_name(tag, from) {
                                *tag = to.to_string();
                            }
                        }
                    }
                    None => repo.tags.retain(|tag| !tax::same_name(tag, from)),
                },
            }
        }
    }

    /// 让词表和仓库上的引用重新对齐。为手改过的 `library.json` 兜底：
    ///
    /// - 仓库引用了词表里没有的名字 → 收进词表（只增不减，不会丢信息）
    /// - 不能用（空白、超长）的名字 → 从仓库上摘掉
    /// - 词表自身去重；标签去重并按词表顺序排列
    ///
    /// 返回是否真的动过数据。加载时调用，落盘要等下一次真正的修改。
    pub fn normalize_taxonomy(&mut self) -> bool {
        // 先把每个仓库「想要」什么看全，再统一改——改到一半再回头读容易漏。
        let wanted: Vec<(RepoId, Option<String>, Vec<String>)> = self
            .data
            .repos
            .iter()
            .map(|repo| {
                (
                    repo.id(),
                    repo.category.as_deref().and_then(tax::clean_name),
                    repo.tags
                        .iter()
                        .filter_map(|tag| tax::clean_name(tag))
                        .collect(),
                )
            })
            .collect();

        let mut changed = tax::dedupe(&mut self.data.categories);
        changed |= tax::dedupe(&mut self.data.tags);

        for (_, category, tags) in &wanted {
            if let Some(category) = category {
                changed |= tax::push_new(&mut self.data.categories, category);
            }
            for tag in tags {
                changed |= tax::push_new(&mut self.data.tags, tag);
            }
        }

        for (id, category, tags) in wanted {
            let before = self.snapshot_taxonomy(&id);
            let category_ok = self.set_repo_category(&id, category.as_deref());
            let tags_ok = self.set_repo_tags(&id, &tags);
            if !category_ok || !tags_ok || self.snapshot_taxonomy(&id) != before {
                changed = true;
            }
        }

        changed
    }

    fn snapshot_taxonomy(&self, id: &str) -> Option<(Option<String>, Vec<String>)> {
        self.repo(id)
            .map(|repo| (repo.category.clone(), repo.tags.clone()))
    }

    // ---- 发现源 ----

    pub fn sources(&self) -> &[DiscoverySource] {
        &self.data.sources
    }

    pub fn enabled_sources(&self) -> impl Iterator<Item = &DiscoverySource> {
        self.data.sources.iter().filter(|source| source.enabled)
    }

    pub fn source(&self, url: &str) -> Option<&DiscoverySource> {
        self.data.sources.iter().find(|source| source.url == url)
    }

    /// 同一个地址只保留一份，避免重复抓取同一个榜单。
    pub fn add_source(&mut self, source: DiscoverySource) -> AddOutcome {
        if self.data.sources.iter().any(|existing| existing.url == source.url) {
            return AddOutcome::AlreadyPresent;
        }
        self.data.sources.push(source);
        AddOutcome::Added
    }

    pub fn remove_source(&mut self, url: &str) -> bool {
        let before = self.data.sources.len();
        self.data.sources.retain(|source| source.url != url);
        self.data.sources.len() != before
    }

    pub fn set_source_enabled(&mut self, url: &str, enabled: bool) -> bool {
        match self.data.sources.iter_mut().find(|source| source.url == url) {
            Some(source) => {
                source.enabled = enabled;
                true
            }
            None => false,
        }
    }

    pub fn record_source_run(&mut self, url: &str, run: SourceRun) -> bool {
        match self.data.sources.iter_mut().find(|source| source.url == url) {
            Some(source) => {
                source.last_run = Some(run);
                true
            }
            None => false,
        }
    }

    // ---- 设置 ----

    pub fn settings(&self) -> &DiscoverySettings {
        &self.data.settings
    }

    pub fn update_settings(&mut self, settings: DiscoverySettings) {
        self.data.settings = settings;
    }

    /// 还有仓库缺描述或创建时间。
    pub fn has_pending_metadata(&self) -> bool {
        self.data
            .repos
            .iter()
            .any(|repo| repo.has_pending_metadata())
    }

    /// 缺少元数据的仓库数量，用于在界面上说明「还有多少没补全」。
    pub fn pending_metadata_count(&self) -> usize {
        self.data
            .repos
            .iter()
            .filter(|repo| repo.has_pending_metadata())
            .count()
    }

    /// 上次自动运行的起点：全部发现源里最近一次运行时间。
    pub fn last_auto_run_at(&self) -> Option<i64> {
        self.data
            .sources
            .iter()
            .filter_map(|source| source.last_run.as_ref())
            .map(|run| run.finished_at)
            .max()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 每次调用都新建一个目录：固定目录名会让「第二次跑测试」读到上一次写下的数据，
    /// 于是断言开始飘。
    fn unique_dir(name: &str) -> std::path::PathBuf {
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
        Library::load_from(unique_dir(name).join("library.json"))
    }

    #[test]
    fn fresh_library_gets_the_default_source() {
        let library = temp_library("fresh");
        assert!(library.data().repos.is_empty());
        assert_eq!(library.sources().len(), 1);
        assert!(library.sources()[0].url.contains("/trending"));
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
        assert_eq!(library.data().repos.len(), 1);
        // 保留的是先入库的那条，它的发现时间没被后一次覆盖。
        assert_eq!(library.data().repos[0].discovered_at, 1);
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
    fn persist_round_trips_through_the_file() {
        let mut library = temp_library("roundtrip");
        let mut repo = Repo::new("coder/coder".to_string(), Some("周榜".to_string()), 99);
        repo.description_zh = Some("汉语描述".to_string());
        library.add_repo(repo);
        library.persist();
        assert_eq!(library.save_state(), &SaveState::Saved);

        let path = library.path().to_path_buf();
        let reloaded = Library::load_from(path);
        assert_eq!(reloaded.load_warning(), None);
        assert_eq!(reloaded.data().repos.len(), 1);
        assert_eq!(
            reloaded.data().repos[0].description_zh.as_deref(),
            Some("汉语描述")
        );
        assert_eq!(reloaded.source("https://github.com/trending?since=weekly&spoken_language_code=").map(|s| s.name.clone()), Some("周榜 · 全部语言".to_string()));
    }

    #[test]
    fn corrupt_file_is_quarantined_and_reported() {
        let dir = unique_dir("corrupt");
        let path = dir.join("library.json");
        std::fs::write(&path, "{ this is not json").unwrap();

        let mut library = Library::load_from(path.clone());

        assert!(library.load_warning().is_some(), "必须报告读取失败");
        assert!(!path.exists(), "损坏的文件应当被改名移走");

        let backups: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().contains("corrupt"))
            .collect();
        assert_eq!(backups.len(), 1, "损坏的文件应当被留档而不是丢弃");
        // 留档是为了「里面可能有手写的汉语描述」，所以内容必须原样保留。
        assert_eq!(
            std::fs::read_to_string(backups[0].path()).unwrap(),
            "{ this is not json"
        );

        // 隔离之后原来的路径必须还能用来写盘。
        library.persist();
        assert_eq!(library.save_state(), &SaveState::Saved);
        assert!(path.exists());
    }

    #[test]
    fn metadata_backfill_is_capped_and_newest_first() {
        let mut library = temp_library("pending");
        for (name, discovered_at) in [("a/one", 1), ("b/two", 2), ("c/three", 3)] {
            library.add_repo(Repo::new(name.to_string(), None, discovered_at));
        }

        let pending = library.repos_pending_metadata(2);
        assert_eq!(pending, vec!["c/three".to_string(), "b/two".to_string()]);
    }

    // ---- 分类与标签 ----

    /// 一个装了三个仓库、两个分类、两个标签的库，用于下面几条测试。
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

    #[test]
    fn names_are_deduplicated_case_insensitively() {
        let mut library = temp_library("add-name");

        assert_eq!(library.add_name(TaxonomyKind::Category, "  工具 "), AddName::Added);
        assert_eq!(library.add_name(TaxonomyKind::Category, "工具"), AddName::AlreadyPresent);
        assert_eq!(library.add_name(TaxonomyKind::Category, "TOOL"), AddName::Added);
        assert_eq!(library.add_name(TaxonomyKind::Category, "tool"), AddName::AlreadyPresent);
        assert_eq!(library.add_name(TaxonomyKind::Category, "   "), AddName::Invalid);

        assert_eq!(library.names(TaxonomyKind::Category), ["工具".to_string(), "TOOL".to_string()]);
        assert!(library.names(TaxonomyKind::Tag).is_empty(), "分类不该串到标签里");
    }

    #[test]
    fn a_repo_has_at_most_one_category_but_many_tags() {
        let mut library = library_with_repos("assign");

        assert!(library.set_repo_category("a/one", Some("工具")));
        assert!(library.set_repo_tags("a/one", &["cli".to_string(), "待读".to_string()]));

        let repo = library.repo("a/one").unwrap();
        assert_eq!(repo.category.as_deref(), Some("工具"));
        assert_eq!(repo.tags, ["cli".to_string(), "待读".to_string()]);

        // 再定一次分类是替换，不是追加。
        assert!(library.set_repo_category("a/one", Some("学习")));
        assert_eq!(library.repo("a/one").unwrap().category.as_deref(), Some("学习"));

        // 移出分类回到未分类。
        assert!(library.set_repo_category("a/one", None));
        assert_eq!(library.repo("a/one").unwrap().category, None);
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
        assert_eq!(library.repo("a/one").unwrap().category, None);

        assert!(!library.set_repo_tags("a/one", &["刚编的标签".to_string()]));
        assert!(library.repo("a/one").unwrap().tags.is_empty());

        // 仓库不存在时也不能谎报成功。
        assert!(!library.set_repo_category("z/none", Some("工具")));
        assert!(!library.set_repo_tags("z/none", &[]));
    }

    #[test]
    fn renaming_a_category_follows_into_the_repos() {
        let mut library = library_with_repos("rename");
        assert!(library.set_repo_category("a/one", Some("工具")));
        assert!(library.set_repo_category("b/two", Some("工具")));

        assert_eq!(
            library.rename_name(TaxonomyKind::Category, "工具", "开发工具"),
            RenameName::Renamed
        );

        assert_eq!(library.repo("a/one").unwrap().category.as_deref(), Some("开发工具"));
        assert_eq!(library.repo("b/two").unwrap().category.as_deref(), Some("开发工具"));
        assert_eq!(library.names(TaxonomyKind::Category), ["开发工具".to_string(), "学习".to_string()]);
    }

    #[test]
    fn renaming_a_tag_follows_into_the_repos_and_keeps_the_order() {
        let mut library = library_with_repos("rename-tag");
        assert!(library.set_repo_tags("a/one", &["cli".to_string(), "待读".to_string()]));

        assert_eq!(library.rename_name(TaxonomyKind::Tag, "待读", "稍后读"), RenameName::Renamed);

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
        assert_eq!(library.names(TaxonomyKind::Category), ["工具".to_string(), "学习".to_string()]);
    }

    #[test]
    fn removing_a_category_uncategorizes_its_repos() {
        let mut library = library_with_repos("remove-category");
        assert!(library.set_repo_category("a/one", Some("工具")));
        assert!(library.set_repo_category("b/two", Some("工具")));

        assert_eq!(library.repo_count_using(TaxonomyKind::Category, "工具"), 2);
        assert_eq!(library.remove_name(TaxonomyKind::Category, "工具"), 2);

        assert_eq!(library.names(TaxonomyKind::Category), ["学习".to_string()]);
        assert_eq!(library.repo("a/one").unwrap().category, None);
        assert_eq!(library.repo("b/two").unwrap().category, None);
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
        assert_eq!(repo.category.as_deref(), Some("工具"));
    }

    #[test]
    fn normalize_adopts_names_that_only_the_repos_know() {
        let dir = unique_dir("normalize");
        let path = dir.join("library.json");
        // 手改过的文件：词表是空的，仓库却已经有分类和标签，还带大小写与重复。
        let hand_written = r#"{
                "repos": [
                    {
                        "full_name": "a/one",
                        "url": "https://github.com/a/one",
                        "discovered_at": 1,
                        "category": " 工具 ",
                        "tags": ["cli", "CLI", "待读"]
                    }
                ]
            }"#;
        std::fs::write(&path, hand_written).unwrap();

        let library = Library::load_from(path.clone());

        assert_eq!(library.load_warning(), None);
        assert_eq!(library.names(TaxonomyKind::Category), ["工具".to_string()]);
        assert_eq!(
            library.names(TaxonomyKind::Tag),
            ["cli".to_string(), "待读".to_string()],
            "重复的标签只收一次"
        );

        let repo = library.repo("a/one").unwrap();
        assert_eq!(repo.category.as_deref(), Some("工具"), "首尾空白要被收掉");
        assert_eq!(repo.tags, ["cli".to_string(), "待读".to_string()]);

        // 对齐只发生在内存里：启动不该偷偷改写用户手写的文件。
        assert_eq!(std::fs::read_to_string(&path).unwrap(), hand_written);
    }

    #[test]
    fn normalize_drops_names_that_cannot_be_used() {
        let dir = unique_dir("normalize-invalid");
        let path = dir.join("library.json");
        std::fs::write(
            &path,
            r#"{
                "repos": [
                    {
                        "full_name": "a/one",
                        "url": "https://github.com/a/one",
                        "discovered_at": 1,
                        "category": "   ",
                        "tags": ["", "  "]
                    }
                ]
            }"#,
        )
        .unwrap();

        let library = Library::load_from(path);

        assert_eq!(library.repo("a/one").unwrap().category, None);
        assert!(library.repo("a/one").unwrap().tags.is_empty());
        assert!(library.names(TaxonomyKind::Category).is_empty());
        assert!(library.names(TaxonomyKind::Tag).is_empty());
    }

    #[test]
    fn normalize_reports_whether_it_changed_anything() {
        let mut library = temp_library("normalize-noop");
        library.add_repo(Repo::new("a/one".to_string(), None, 1));
        assert!(library.add_name(TaxonomyKind::Category, "工具") == AddName::Added);
        assert!(library.set_repo_category("a/one", Some("工具")));

        assert!(!library.normalize_taxonomy(), "已经对齐的数据不该被再改一次");

        let path = library.path().to_path_buf();
        library.persist();
        let reloaded = Library::load_from(path);
        assert_eq!(reloaded.repo("a/one").unwrap().category.as_deref(), Some("工具"));
        assert_eq!(reloaded.names(TaxonomyKind::Category), ["工具".to_string()]);
    }

    #[test]
    fn taxonomy_survives_a_round_trip_through_the_file() {
        let mut library = library_with_repos("taxonomy-roundtrip");
        assert!(library.set_repo_category("a/one", Some("工具")));
        assert!(library.set_repo_tags("a/one", &["cli".to_string()]));
        library.persist();
        assert_eq!(library.save_state(), &SaveState::Saved);

        let path = library.path().to_path_buf();
        let reloaded = Library::load_from(path);

        assert_eq!(reloaded.names(TaxonomyKind::Category), ["工具".to_string(), "学习".to_string()]);
        assert_eq!(reloaded.names(TaxonomyKind::Tag), ["cli".to_string(), "待读".to_string()]);
        let repo = reloaded.repo("a/one").unwrap();
        assert_eq!(repo.category.as_deref(), Some("工具"));
        assert_eq!(repo.tags, ["cli".to_string()]);
    }
}

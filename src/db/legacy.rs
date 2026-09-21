//! 旧版 `library.json` 的导入。
//!
//! 老版本把全部数据放在一个 JSON 文件里，仓库用分类名（路径）引用分类，标签也是
//! 名字。这里把那份文件读进来翻译成新库的形状：分类按路径补齐成一棵树，仓库换成
//! `category_id`，标签换成 id 关联。
//!
//! 只在**全新的空库**上跑一次。之后旧文件会被改名留档，不会再次参与。

use std::path::Path;

use serde::Deserialize;

use super::{Db, DbResult, repos, sources, taxonomy};
use crate::discovery::model::{DiscoverySettings, DiscoverySource};
use crate::repos::model::Repo;
use crate::taxonomy::model as tax;

#[derive(Debug, Default, Deserialize)]
struct LegacyLibrary {
    #[serde(default)]
    repos: Vec<LegacyRepo>,
    #[serde(default)]
    categories: Vec<String>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    sources: Vec<DiscoverySource>,
    #[serde(default)]
    settings: DiscoverySettings,
}

/// 旧版的仓库记录。分类是路径字符串，标签是名字。
#[derive(Debug, Default, Deserialize)]
struct LegacyRepo {
    #[serde(default)]
    full_name: String,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    description_zh: Option<String>,
    #[serde(default)]
    discovered_at: i64,
    #[serde(default)]
    created_at: Option<i64>,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    language: Option<String>,
    #[serde(default)]
    stars: Option<u64>,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
}

/// 导入的账目，用来如实告诉用户搬过来了多少东西。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImportReport {
    pub repos: usize,
    pub categories: usize,
    pub tags: usize,
    pub sources: usize,
}

/// 把一个旧版 JSON 文件导进空库。整批在一个事务里，中途失败不会留下半份数据。
pub fn import(db: &Db, path: &Path) -> DbResult<ImportReport> {
    let text = std::fs::read_to_string(path).map_err(super::DbError::Io)?;
    let legacy: LegacyLibrary = serde_json::from_str(&text).map_err(|error| {
        super::DbError::Format(format!("不是一份能认的旧版数据文件：{error}"))
    })?;

    db.transaction(|tx| {
        // 词表先建全，仓库再往上挂。仓库上出现过的名字也要收进来——旧文件里
        // 词表与实际用到的名字本来就可能对不上（词表是加载时补的）。
        for name in &legacy.categories {
            ensure_category(tx, name)?;
        }
        for legacy_repo in &legacy.repos {
            if let Some(path) = legacy_repo.category.as_deref() {
                ensure_category(tx, path)?;
            }
        }
        for name in &legacy.tags {
            ensure_tag(tx, name)?;
        }
        for legacy_repo in &legacy.repos {
            for name in &legacy_repo.tags {
                ensure_tag(tx, name)?;
            }
        }

        let vocabulary = taxonomy::load_tags(tx)?;
        let mut imported_repos = 0usize;

        for legacy_repo in &legacy.repos {
            if legacy_repo.full_name.trim().is_empty() {
                continue;
            }
            let mut repo = Repo::new(
                legacy_repo.full_name.trim().to_string(),
                legacy_repo.source.clone(),
                legacy_repo.discovered_at,
            );
            if let Some(url) = legacy_repo.url.clone().filter(|url| !url.trim().is_empty()) {
                repo.url = url;
            }
            repo.description = legacy_repo.description.clone();
            repo.description_zh = legacy_repo.description_zh.clone();
            repo.created_at = legacy_repo.created_at;
            repo.language = legacy_repo.language.clone();
            repo.stars = legacy_repo.stars;
            repo.category_id = match legacy_repo.category.as_deref() {
                Some(path) => ensure_category(tx, path)?,
                None => None,
            };

            // 标签按词表顺序取 id，和运行时写进去的排法一致。
            let wanted: Vec<String> = legacy_repo
                .tags
                .iter()
                .filter_map(|name| tax::clean_name(name))
                .collect();
            let tag_ids: Vec<i64> = vocabulary
                .iter()
                .filter(|tag| wanted.iter().any(|name| tax::same_name(name, &tag.name)))
                .map(|tag| tag.id)
                .collect();

            if repos::insert(tx, &repo, &tag_ids)? {
                imported_repos += 1;
            }
        }

        for source in &legacy.sources {
            sources::insert_source(tx, source)?;
        }
        sources::save_settings(tx, &legacy.settings)?;

        Ok(ImportReport {
            repos: imported_repos,
            categories: taxonomy::load_categories(tx)?.len(),
            tags: vocabulary.len(),
            sources: legacy.sources.len(),
        })
    })
}

/// 按路径找到分类节点，缺哪一级就补哪一级。
fn ensure_category(conn: &rusqlite::Connection, path: &str) -> DbResult<Option<i64>> {
    let Some(cleaned) = tax::clean_path(path) else {
        return Ok(None);
    };

    let mut parent = None;
    for segment in tax::segments(&cleaned) {
        parent = Some(match taxonomy::find_child(conn, parent, &segment)? {
            Some(existing) => existing,
            None => taxonomy::insert_category(conn, parent, &segment)?,
        });
    }
    Ok(parent)
}

/// 按名字找到标签，没有就建一个。
fn ensure_tag(conn: &rusqlite::Connection, name: &str) -> DbResult<Option<i64>> {
    let Some(cleaned) = tax::clean_name(name) else {
        return Ok(None);
    };
    if let Some(existing) = taxonomy::find_tag(conn, &cleaned)? {
        return Ok(Some(existing));
    }
    Ok(Some(taxonomy::insert_tag(conn, &cleaned)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::taxonomy::tree::CategoryTree;

    /// 一份加了分类标签之后的旧版文件。
    const LEGACY: &str = r#"{
        "repos": [
            {
                "full_name": "Coder/Coder",
                "url": "https://github.com/Coder/Coder",
                "description": "云开发环境",
                "description_zh": "汉语描述",
                "discovered_at": 1789899213,
                "created_at": 1637006815,
                "source": "周榜 · 全部语言",
                "language": "Go",
                "stars": 38039,
                "category": " 开发／前端 ",
                "tags": ["cli", "CLI", "待读"]
            },
            {
                "full_name": "rust-lang/rust",
                "discovered_at": 1789899214,
                "category": null,
                "tags": []
            }
        ],
        "categories": ["开发/前端/框架"],
        "tags": ["cli", "待读"],
        "sources": [
            {
                "name": "周榜 · 全部语言",
                "url": "https://github.com/trending?since=weekly&spoken_language_code=",
                "enabled": true,
                "last_run": { "started_at": 0, "finished_at": 1789899200, "found": 21, "added": 21, "skipped": 0, "error": null }
            }
        ],
        "settings": { "auto_discover": false, "interval_minutes": 90, "metadata_batch": 4, "github_token": null }
    }"#;

    fn write_legacy(dir: &Path) -> std::path::PathBuf {
        let path = dir.join("library.json");
        std::fs::write(&path, LEGACY).unwrap();
        path
    }

    fn temp_dir(name: &str) -> std::path::PathBuf {
        use std::time::{SystemTime, UNIX_EPOCH};
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default();
        let dir = std::env::temp_dir().join(format!(
            "ngy-fav-legacy-{name}-{}-{stamp}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn importing_translates_paths_and_names_into_ids() {
        let dir = temp_dir("import");
        let db = Db::open(&dir.join("library.db")).unwrap();
        let report = import(&db, &write_legacy(&dir)).unwrap();

        assert_eq!(report.repos, 2);
        assert_eq!(report.sources, 1);

        let tree = CategoryTree::from_nodes(taxonomy::load_categories(db.conn()).unwrap());
        // 路径里的全角斜杠要收干净，缺的上级要补上。
        let front = tree.find_path("开发/前端").expect("分类应当在树上");
        assert_eq!(tree.path(front).as_deref(), Some("开发/前端"));
        assert_eq!(
            tree.path(tree.find_path("开发/前端/框架").unwrap()).as_deref(),
            Some("开发/前端/框架")
        );

        let loaded = repos::load_all(db.conn()).unwrap();
        let coder = loaded.iter().find(|repo| repo.full_name == "Coder/Coder").unwrap();
        assert_eq!(coder.category_id, Some(front), "仓库挂到了分类节点上");
        assert_eq!(coder.tags, ["cli".to_string(), "待读".to_string()], "重复的标签只留一份");

        let rust = loaded.iter().find(|repo| repo.full_name == "rust-lang/rust").unwrap();
        assert_eq!(rust.category_id, None);
        assert_eq!(rust.url, "https://github.com/rust-lang/rust", "缺地址时补一个");

        let tags = taxonomy::load_tags(db.conn()).unwrap();
        assert_eq!(tags.len(), 2);

        let settings = sources::load_settings(db.conn()).unwrap();
        assert!(!settings.auto_discover);
        assert_eq!(settings.interval_minutes, 90);
        assert_eq!(sources::load_sources(db.conn()).unwrap().len(), 1);
    }

    #[test]
    fn importing_the_same_file_twice_does_not_double_anything() {
        let dir = temp_dir("import-twice");
        let db = Db::open(&dir.join("library.db")).unwrap();
        let path = write_legacy(&dir);

        import(&db, &path).unwrap();
        let second = import(&db, &path).unwrap();

        assert_eq!(second.repos, 0, "已经在库里的仓库不会再进一遍");
        assert_eq!(repos::load_all(db.conn()).unwrap().len(), 2);
        assert_eq!(taxonomy::load_categories(db.conn()).unwrap().len(), 3);
    }

    #[test]
    fn a_file_with_nothing_usable_imports_nothing() {
        let dir = temp_dir("import-empty");
        let path = dir.join("library.json");
        std::fs::write(&path, "{}").unwrap();

        let db = Db::open(&dir.join("library.db")).unwrap();
        let report = import(&db, &path).unwrap();
        assert_eq!(report, ImportReport::default());
        assert!(repos::load_all(db.conn()).unwrap().is_empty());
    }

    #[test]
    fn broken_json_is_reported_instead_of_half_imported() {
        let dir = temp_dir("import-broken");
        let path = dir.join("library.json");
        std::fs::write(&path, "{ 这不是 json").unwrap();

        let db = Db::open(&dir.join("library.db")).unwrap();
        assert!(import(&db, &path).is_err());
        assert!(repos::load_all(db.conn()).unwrap().is_empty());
    }
}

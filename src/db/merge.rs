//! 把两个库并成一个。
//!
//! 「移动并合并」用的就是这里：新旧位置各有一份数据，谁也不想丢，于是把旧的那份
//! 并进新的。
//!
//! 合并的规矩只有一条，但它决定了所有细节：**新位置是主人**。
//!
//! - 同一个仓库（`full_name` 小写后相同）两边都有时，**新位置那份原样保留**——
//!   包括它的分类、标签、汉语描述。旧的那份只贡献新位置没有的仓库。
//! - 分类按**路径**合并：新位置已有 `开发/前端` 就直接用那个节点，没有才逐级建。
//!   按路径而不是按 id，是因为两个库的自增 id 各算各的，id 之间没有任何关系。
//! - 标签按名字合并（忽略大小写），保留新位置那边的写法。
//! - 发现源按 URL 合并，它本来就是按 URL 去重的。
//! - **设置不动**：令牌、间隔、批次都留着新位置的。旧位置那份偏好不该悄悄盖掉
//!   你现在用的这份。
//!
//! 整个合并在一个事务里，中途失败什么都不落。

use std::collections::HashMap;

use rusqlite::Connection;

use super::{Db, DbResult, repos, sources, taxonomy};
use crate::taxonomy::model as tax;
use crate::taxonomy::tree::CategoryTree;

/// 并进来多少东西。用来如实告诉用户结果。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MergeReport {
    /// 新位置原来没有、这次并进来的仓库数。
    pub repos: usize,
    /// 两边都有、留着新位置那份的仓库数。
    pub repos_kept: usize,
    /// 新建出来的分类数（新位置已经有的不算）。
    pub categories: usize,
    /// 新建出来的标签数。
    pub tags: usize,
    /// 新增的发现地址数。
    pub sources: usize,
}

/// 把 `src` 的内容并进 `dst`。`dst` 已有的数据不会被改动。
pub fn merge_into(dst: &Db, src: &Db) -> DbResult<MergeReport> {
    // 先把源库读成值。这样整个事务里只碰目标库一个连接，出错也好回滚。
    let source_categories = taxonomy::load_categories(src.conn())?;
    let source_tags = taxonomy::load_tags(src.conn())?;
    let source_repos = repos::load_all(src.conn())?;
    let source_sources = sources::load_sources(src.conn())?;

    // 铺平成「父在子前」的顺序，逐条对到目标树上：父先对上，子才找得到落脚处。
    let tree = CategoryTree::from_nodes(source_categories);

    dst.transaction(|tx| {
        // ---- 分类：按路径逐级对上，缺的建出来 ----
        let mut category_map: HashMap<i64, i64> = HashMap::new();
        let mut new_categories = 0usize;
        for flat in tree.flatten() {
            let (id, created) = ensure_category(tx, &flat.path)?;
            if let Some(id) = id {
                if created {
                    new_categories += 1;
                }
                category_map.insert(flat.id, id);
            }
        }

        // ---- 标签：按名字对上 ----
        let mut new_tags = 0usize;
        for tag in &source_tags {
            if taxonomy::find_tag(tx, &tag.name)?.is_none() {
                taxonomy::insert_tag(tx, &tag.name)?;
                new_tags += 1;
            }
        }

        // 目标这边合并之后的词表，用来把仓库上的标签名字换成目标库的 id。
        let vocabulary = taxonomy::load_tags(tx)?;
        let tag_id_by_key: HashMap<String, i64> = vocabulary
            .iter()
            .map(|tag| (tax::name_key(&tag.name), tag.id))
            .collect();

        // ---- 仓库：新位置已有的原样不动，只补它没有的 ----
        let mut added = 0usize;
        let mut kept = 0usize;
        for mut repo in source_repos {
            repo.category_id = repo.category_id.and_then(|id| category_map.get(&id).copied());
            let tag_ids: Vec<i64> = repo
                .tags
                .iter()
                .filter_map(|name| tag_id_by_key.get(&tax::name_key(name)).copied())
                .collect();

            if repos::insert(tx, &repo, &tag_ids)? {
                added += 1;
            } else {
                kept += 1;
            }
        }

        // ---- 发现源：按 URL 去重 ----
        let mut new_sources = 0usize;
        for source in &source_sources {
            if sources::insert_source(tx, source)? {
                new_sources += 1;
            }
        }

        Ok(MergeReport {
            repos: added,
            repos_kept: kept,
            categories: new_categories,
            tags: new_tags,
            sources: new_sources,
        })
    })
}

/// 按路径找到分类节点，缺哪一级补哪一级。返回值里的 `bool` 表示整条路径上
/// 有没有新建过节点。
fn ensure_category(conn: &Connection, path: &str) -> DbResult<(Option<i64>, bool)> {
    let Some(cleaned) = tax::clean_path(path) else {
        return Ok((None, false));
    };

    let mut parent = None;
    let mut created = false;
    for segment in tax::segments(&cleaned) {
        parent = Some(match taxonomy::find_child(conn, parent, &segment)? {
            Some(existing) => existing,
            None => {
                created = true;
                taxonomy::insert_category(conn, parent, &segment)?
            }
        });
    }
    Ok((parent, created))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::model::{DiscoverySettings, DiscoverySource};
    use crate::repos::model::Repo;

    fn db() -> Db {
        Db::open_in_memory().unwrap()
    }

    /// 「新位置」的库：一个仓库、一棵分类、一个标签、一个发现源。
    fn target() -> Db {
        let db = db();
        let conn = db.conn();
        let mut repo = Repo::new("a/one".to_string(), None, 10);
        repo.description_zh = Some("新位置写的".to_string());
        repos::insert(conn, &repo, &[]).unwrap();

        let dev = taxonomy::insert_category(conn, None, "开发").unwrap();
        let front = taxonomy::insert_category(conn, Some(dev), "前端").unwrap();
        repos::set_category(conn, "a/one", Some(front)).unwrap();

        let cli = taxonomy::insert_tag(conn, "cli").unwrap();
        repos::set_tags(conn, "a/one", &[cli]).unwrap();

        sources::insert_source(
            conn,
            &DiscoverySource::new("周榜", "https://example.com/week"),
        )
        .unwrap();
        sources::save_settings(
            conn,
            &DiscoverySettings {
                interval_minutes: 45,
                ..DiscoverySettings::default()
            },
        )
        .unwrap();
        db
    }

    /// 「旧位置」的库：一个同名但内容不同的仓库、一个自己的仓库、一棵更深的分类、
    /// 一个新标签、另一个发现源。
    fn source() -> Db {
        let db = db();
        let conn = db.conn();
        let mut same = Repo::new("A/One".to_string(), None, 1);
        same.description_zh = Some("旧位置写的".to_string());
        repos::insert(conn, &same, &[]).unwrap();

        let mut other = Repo::new("b/two".to_string(), None, 2);
        other.language = Some("Rust".to_string());
        repos::insert(conn, &other, &[]).unwrap();

        let dev = taxonomy::insert_category(conn, None, "开发").unwrap();
        let front = taxonomy::insert_category(conn, Some(dev), "前端").unwrap();
        let framework = taxonomy::insert_category(conn, Some(front), "框架").unwrap();
        repos::set_category(conn, "b/two", Some(framework)).unwrap();
        repos::set_category(conn, "A/One", Some(framework)).unwrap();

        let later = taxonomy::insert_tag(conn, "待读").unwrap();
        repos::set_tags(conn, "b/two", &[later]).unwrap();

        sources::insert_source(
            conn,
            &DiscoverySource::new("月榜", "https://example.com/month"),
        )
        .unwrap();
        sources::save_settings(
            conn,
            &DiscoverySettings {
                interval_minutes: 5,
                ..DiscoverySettings::default()
            },
        )
        .unwrap();
        db
    }

    #[test]
    fn merging_brings_over_what_is_missing_and_keeps_what_is_there() {
        let dst = target();
        let src = source();

        let report = merge_into(&dst, &src).unwrap();
        assert_eq!(report.repos, 1, "只有 b/two 是新的");
        assert_eq!(report.repos_kept, 1, "a/one 两边都有，留着新位置那份");
        assert_eq!(report.categories, 1, "只在旧库里有的「框架」要建出来");
        assert_eq!(report.tags, 1, "「待读」是新标签");
        assert_eq!(report.sources, 1, "只有月榜是新的");

        let repos_in = repos::load_all(dst.conn()).unwrap();
        assert_eq!(repos_in.len(), 2);

        let same = repos_in.iter().find(|repo| repo.full_name == "a/one").unwrap();
        assert_eq!(
            same.description_zh.as_deref(),
            Some("新位置写的"),
            "两边都有的仓库，新位置那份说了算"
        );
        assert_eq!(same.discovered_at, 10, "发现时间也没有被旧的改掉");

        let other = repos_in.iter().find(|repo| repo.full_name == "b/two").unwrap();
        assert_eq!(other.language.as_deref(), Some("Rust"), "旧库的元数据要带过来");
        assert_eq!(other.discovered_at, 2);
        // 分类按路径对上：旧库的「开发/前端/框架」挂到了新库那棵树上。
        let tree = CategoryTree::from_nodes(taxonomy::load_categories(dst.conn()).unwrap());
        assert_eq!(
            tree.path(other.category_id.expect("要挂上分类")).as_deref(),
            Some("开发/前端/框架")
        );
        assert_eq!(other.tags, ["待读".to_string()]);
    }

    #[test]
    fn a_second_merge_changes_nothing() {
        let dst = target();
        let src = source();

        merge_into(&dst, &src).unwrap();
        let again = merge_into(&dst, &src).unwrap();

        assert_eq!(again.repos, 0, "第二次没有新东西可并");
        assert_eq!(again.repos_kept, 2);
        assert_eq!(again.categories, 0);
        assert_eq!(again.tags, 0);
        assert_eq!(again.sources, 0);
        assert_eq!(repos::load_all(dst.conn()).unwrap().len(), 2);
        assert_eq!(taxonomy::load_categories(dst.conn()).unwrap().len(), 3);
    }

    #[test]
    fn the_settings_of_the_new_place_survive() {
        let dst = target();
        let src = source();
        merge_into(&dst, &src).unwrap();

        let settings = sources::load_settings(dst.conn()).unwrap();
        assert_eq!(settings.interval_minutes, 45, "旧位置的偏好不该盖掉现在的");

        let urls: Vec<String> = sources::load_sources(dst.conn())
            .unwrap()
            .into_iter()
            .map(|source| source.url)
            .collect();
        assert_eq!(urls.len(), 2, "两个发现源都在");
    }

    #[test]
    fn merging_reports_nothing_when_the_source_is_empty() {
        let dst = target();
        let empty = db();

        let report = merge_into(&dst, &empty).unwrap();
        assert_eq!(report, MergeReport::default());
        assert_eq!(repos::load_all(dst.conn()).unwrap().len(), 1);
    }

    /// 两棵同名的树要合成一棵，而不是各自长着一份「开发」。
    #[test]
    fn shared_category_paths_land_on_one_tree() {
        let dst = target();
        let src = source();
        merge_into(&dst, &src).unwrap();

        let tree = CategoryTree::from_nodes(taxonomy::load_categories(dst.conn()).unwrap());
        assert_eq!(tree.children(None).len(), 1, "顶层只有一个「开发」");
        assert!(
            tree.find_path("开发/前端/框架").is_some(),
            "旧库那条枝要接在同一棵树上"
        );
    }

    /// 词表这一层不该因为合并多出重复项。
    #[test]
    fn the_vocabulary_has_no_duplicates_after_merging() {
        let dst = target();
        let src = db();

        // 旧库里同一个标签换了个大小写；仓库也挂着它。
        let cli = taxonomy::insert_tag(src.conn(), "CLI").unwrap();
        let mut repo = Repo::new("b/two".to_string(), None, 3);
        repo.tags = vec!["CLI".to_string()];
        repos::insert(src.conn(), &repo, &[cli]).unwrap();

        let report = merge_into(&dst, &src).unwrap();
        assert_eq!(report.tags, 0, "CLI 与 cli 是同一个标签");

        let tags = taxonomy::load_tags(dst.conn()).unwrap();
        assert_eq!(tags.len(), 1, "CLI 对上了已有的 cli，不该再多一个");
        assert_eq!(tags[0].name, "cli", "保留新位置的写法");

        let merged = repos::load_all(dst.conn())
            .unwrap()
            .into_iter()
            .find(|repo| repo.full_name == "b/two")
            .unwrap();
        assert_eq!(merged.tags, ["cli".to_string()]);
    }
}

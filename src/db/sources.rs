//! 发现源与设置的读写。
//!
//! 发现源的身份就是 URL（和仓库用 `full_name`、标签用名字是同一套做法），
//! 所以它直接当主键，不需要再维护一个可能对不上的自增 id。运行记录摊平在
//! 同一个表里：一个源只有一个「最近一次」，没必要再立一张表。

use rusqlite::{Connection, params};

use super::DbResult;
use crate::discovery::model::{DiscoverySettings, DiscoverySource, SourceRun};

/// 设置项在 `settings` 表里的键名。
const AUTO_DISCOVER: &str = "auto_discover";
const INTERVAL_MINUTES: &str = "interval_minutes";
const METADATA_BATCH: &str = "metadata_batch";
const GITHUB_TOKEN: &str = "github_token";

pub fn load_sources(conn: &Connection) -> DbResult<Vec<DiscoverySource>> {
    let mut statement = conn.prepare(
        "SELECT name, url, enabled, started_at, finished_at, found, added, skipped, error
         FROM sources ORDER BY position, url",
    )?;
    let rows = statement.query_map([], |row| {
        let finished_at: Option<i64> = row.get(4)?;
        // `finished_at` 有没有值就代表这个源跑没跑过，其余几个字段是那一次的账目。
        let last_run = finished_at.map(|finished_at| SourceRun {
            started_at: row.get::<_, Option<i64>>(3).ok().flatten().unwrap_or(0),
            finished_at,
            found: row.get::<_, Option<i64>>(5).ok().flatten().unwrap_or(0) as usize,
            added: row.get::<_, Option<i64>>(6).ok().flatten().unwrap_or(0) as usize,
            skipped: row.get::<_, Option<i64>>(7).ok().flatten().unwrap_or(0) as usize,
            error: row.get(8).ok().flatten(),
        });
        Ok(DiscoverySource {
            name: row.get(0)?,
            url: row.get(1)?,
            enabled: row.get::<_, i64>(2)? != 0,
            last_run,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// 新增一个发现源。同一个 URL 只留一份，重复时返回 `false`。
pub fn insert_source(conn: &Connection, source: &DiscoverySource) -> DbResult<bool> {
    let position: i64 = conn.query_row(
        "SELECT ifnull(max(position), -1) + 1 FROM sources",
        [],
        |row| row.get(0),
    )?;
    let changed = conn.execute(
        "INSERT OR IGNORE INTO sources (url, name, enabled, position) VALUES (?, ?, ?, ?)",
        params![source.url, source.name, i64::from(source.enabled), position],
    )?;
    Ok(changed > 0)
}

pub fn delete_source(conn: &Connection, url: &str) -> DbResult<bool> {
    Ok(conn.execute("DELETE FROM sources WHERE url = ?", [url])? > 0)
}

pub fn set_source_enabled(conn: &Connection, url: &str, enabled: bool) -> DbResult<bool> {
    let changed = conn.execute(
        "UPDATE sources SET enabled = ? WHERE url = ?",
        params![i64::from(enabled), url],
    )?;
    Ok(changed > 0)
}

/// 记下一次运行的账。源不存在时返回 `false`。
pub fn record_source_run(conn: &Connection, url: &str, run: &SourceRun) -> DbResult<bool> {
    let changed = conn.execute(
        "UPDATE sources SET started_at = ?, finished_at = ?, found = ?, added = ?, skipped = ?,
                            error = ?
         WHERE url = ?",
        params![
            run.started_at,
            run.finished_at,
            run.found as i64,
            run.added as i64,
            run.skipped as i64,
            run.error.as_deref(),
            url,
        ],
    )?;
    Ok(changed > 0)
}

pub fn load_settings(conn: &Connection) -> DbResult<DiscoverySettings> {
    let mut settings = DiscoverySettings::default();
    let mut statement = conn.prepare("SELECT key, value FROM settings")?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;

    for row in rows {
        let (key, value) = row?;
        match key.as_str() {
            AUTO_DISCOVER => settings.auto_discover = value == "1" || value == "true",
            INTERVAL_MINUTES => {
                if let Ok(minutes) = value.parse() {
                    settings.interval_minutes = minutes;
                }
            }
            METADATA_BATCH => {
                if let Ok(batch) = value.parse() {
                    settings.metadata_batch = batch;
                }
            }
            // 空串等同于没设置，否则清空令牌之后会留下一个 `Some("")`。
            GITHUB_TOKEN => {
                let trimmed = value.trim();
                settings.github_token = (!trimmed.is_empty()).then(|| trimmed.to_string());
            }
            _ => {}
        }
    }
    Ok(settings)
}

pub fn save_settings(conn: &Connection, settings: &DiscoverySettings) -> DbResult<()> {
    let entries = [
        (AUTO_DISCOVER, i64::from(settings.auto_discover).to_string()),
        (INTERVAL_MINUTES, settings.interval_minutes.to_string()),
        (METADATA_BATCH, settings.metadata_batch.to_string()),
        (
            GITHUB_TOKEN,
            settings.github_token.clone().unwrap_or_default(),
        ),
    ];

    let mut statement = conn.prepare(
        "INSERT INTO settings (key, value) VALUES (?, ?)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    )?;
    for (key, value) in entries {
        statement.execute(params![key, value])?;
    }
    Ok(())
}

/// 库里有没有任何数据。用来判断「这是一次全新的启动，该不该导入旧 JSON」。
///
/// 设置也算：只要用户动过一次设置，这个库就已经在用了，不该再被一份陈旧的
/// `library.json` 盖上去。
pub fn is_empty(conn: &Connection) -> DbResult<bool> {
    let count: i64 = conn.query_row(
        "SELECT (SELECT count(*) FROM repos)
              + (SELECT count(*) FROM sources)
              + (SELECT count(*) FROM categories)
              + (SELECT count(*) FROM tags)
              + (SELECT count(*) FROM settings)",
        [],
        |row| row.get(0),
    )?;
    Ok(count == 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;

    fn db() -> Db {
        Db::open_in_memory().unwrap()
    }

    #[test]
    fn sources_keep_their_order_and_run_records() {
        let db = db();
        let conn = db.conn();

        let first = DiscoverySource::new("周榜", "https://github.com/trending");
        let second = DiscoverySource::new("月榜", "https://github.com/trending?since=monthly");
        assert!(insert_source(conn, &first).unwrap());
        assert!(insert_source(conn, &second).unwrap());
        assert!(!insert_source(conn, &first).unwrap(), "同一个地址只留一份");

        let run = SourceRun {
            started_at: 10,
            finished_at: 20,
            found: 21,
            added: 21,
            skipped: 0,
            error: None,
        };
        assert!(record_source_run(conn, &first.url, &run).unwrap());

        let sources = load_sources(conn).unwrap();
        assert_eq!(sources.len(), 2);
        assert_eq!(sources[0].url, first.url);
        assert_eq!(sources[0].last_run.as_ref().map(|run| run.added), Some(21));
        assert_eq!(sources[1].last_run, None, "没跑过的源不该凭空有记录");

        assert!(set_source_enabled(conn, &second.url, false).unwrap());
        assert!(!load_sources(conn).unwrap()[1].enabled);

        assert!(delete_source(conn, &first.url).unwrap());
        assert_eq!(load_sources(conn).unwrap().len(), 1);
    }

    #[test]
    fn a_failed_run_is_stored_as_a_failed_run() {
        let db = db();
        let conn = db.conn();
        let source = DiscoverySource::new("周榜", "https://github.com/trending");
        insert_source(conn, &source).unwrap();

        record_source_run(
            conn,
            &source.url,
            &SourceRun {
                started_at: 1,
                finished_at: 2,
                found: 0,
                added: 0,
                skipped: 0,
                error: Some("超时".to_string()),
            },
        )
        .unwrap();

        let stored = &load_sources(conn).unwrap()[0];
        assert_eq!(
            stored.last_run.as_ref().and_then(|run| run.error.as_deref()),
            Some("超时")
        );
    }

    #[test]
    fn settings_round_trip_and_blank_token_means_absent() {
        let db = db();
        let conn = db.conn();

        assert!(load_settings(conn).unwrap().auto_discover, "没有记录时用默认值");

        let settings = DiscoverySettings {
            auto_discover: false,
            interval_minutes: 45,
            metadata_batch: 3,
            github_token: Some("ghp_x".to_string()),
        };
        save_settings(conn, &settings).unwrap();
        assert_eq!(load_settings(conn).unwrap(), settings);

        // 改一项不能把别的项冲掉。
        save_settings(
            conn,
            &DiscoverySettings {
                github_token: None,
                ..settings
            },
        )
        .unwrap();
        let reloaded = load_settings(conn).unwrap();
        assert_eq!(reloaded.github_token, None);
        assert_eq!(reloaded.interval_minutes, 45);
        assert!(!reloaded.auto_discover);
    }

    #[test]
    fn emptiness_is_about_data_not_about_the_file_existing() {
        let db = db();
        let conn = db.conn();
        assert!(is_empty(conn).unwrap());

        insert_source(conn, &DiscoverySource::new("周榜", "https://github.com/trending")).unwrap();
        assert!(!is_empty(conn).unwrap());

        save_settings(conn, &DiscoverySettings::default()).unwrap();
        assert!(!is_empty(conn).unwrap(), "只写了配置也算开过库");
    }
}

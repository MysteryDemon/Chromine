use std::path::Path;

use anyhow::{Context, Result};
use chromine_shared::{RecordSource, is_valid_guid};

use crate::downloads::{download_key, write_completion_record};

pub fn chrome_time_to_unix_ms(t: i64) -> i64 {
    const EPOCH_DELTA_MS: i64 = 11_644_473_600_000;
    if t <= 0 { 0 } else { t / 1000 - EPOCH_DELTA_MS }
}

pub struct ReconcileReport {
    pub examined: usize,
    pub recovered: usize,
}

pub fn reconcile(profile_dir: &Path, incoming: &Path, completed_dir: &Path) -> Result<ReconcileReport> {
    let history = profile_dir.join("Default").join("History");
    if !history.exists() {
        return Ok(ReconcileReport { examined: 0, recovered: 0 });
    }
    let tmp = tempdir_in_tmp()?;
    let copy = tmp.join("History");
    std::fs::copy(&history, &copy).context("copy History")?;
    let journal = profile_dir.join("Default").join("History-journal");
    if journal.exists() {
        let _ = std::fs::copy(&journal, tmp.join("History-journal"));
    }
    let result = scan(&copy, incoming, completed_dir);
    let _ = std::fs::remove_dir_all(&tmp);
    result
}

fn tempdir_in_tmp() -> Result<std::path::PathBuf> {
    let dir =
        std::env::temp_dir().join(format!("chromine-history-{}-{}", std::process::id(), chromine_shared::now_ms()));
    std::fs::create_dir(&dir)?;
    Ok(dir)
}

fn scan(db: &Path, incoming: &Path, completed_dir: &Path) -> Result<ReconcileReport> {
    let conn = rusqlite::Connection::open(db).context("open History copy")?;
    let mut stmt = conn.prepare(
        "SELECT d.id, d.start_time, d.target_path, d.received_bytes, d.end_time, c.url, d.referrer
         FROM downloads d LEFT JOIN downloads_url_chains c ON c.id = d.id AND c.chain_index = 0
         WHERE d.state = 1 ORDER BY d.id",
    )?;
    let rows = stmt.query_map([], |r| {
        let web = |i: usize| -> rusqlite::Result<Option<String>> {
            Ok(r.get::<_, Option<String>>(i)?.filter(|u| u.starts_with("https://") || u.starts_with("http://")))
        };
        Ok((
            (
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, i64>(4)?,
            ),
            web(5)?.or(web(6)?),
        ))
    })?;
    let referenced: std::collections::HashSet<String> = std::fs::read_dir(completed_dir)
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| std::fs::read(e.path()).ok())
                .filter_map(|b| serde_json::from_slice::<chromine_shared::CompletedDownload>(&b).ok())
                .map(|r| r.file_path)
                .collect()
        })
        .unwrap_or_default();
    let mut report = ReconcileReport { examined: 0, recovered: 0 };
    for row in rows {
        let ((id, start_time, target, received, end_time), source_url) = row?;
        report.examined += 1;
        let key = download_key(id, chrome_time_to_unix_ms(start_time));
        if !is_valid_guid(&key) || completed_dir.join(format!("{key}.json")).exists() || referenced.contains(&target) {
            continue;
        }
        let path = Path::new(&target);
        if !chromine_shared::fsutil::is_direct_child(incoming, path) {
            continue;
        }
        let Ok(meta) = chromine_shared::fsutil::regular_file_lstat(path) else { continue };
        if meta.len() as i64 != received {
            continue;
        }
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_string();
        match write_completion_record(
            incoming,
            completed_dir,
            &key,
            &name,
            path,
            (RecordSource::History, source_url.as_deref()),
            chrome_time_to_unix_ms(end_time),
        ) {
            Ok(_) => {
                tracing::info!(%key, path = %target, "recovered completed download from Chrome history");
                report.recovered += 1;
            }
            Err(e) => tracing::warn!(%key, error = %e, "could not recover download from history"),
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    const START: i64 = 13_407_000_000_123_456;

    fn make_history(profile: &Path, rows: &[(i64, &str, i64, i64)]) {
        std::fs::create_dir_all(profile.join("Default")).unwrap();
        let c = rusqlite::Connection::open(profile.join("Default/History")).unwrap();
        c.execute_batch(
            "CREATE TABLE downloads (id INTEGER PRIMARY KEY, guid VARCHAR NOT NULL, current_path LONGVARCHAR NOT NULL,
             target_path LONGVARCHAR NOT NULL, start_time INTEGER NOT NULL, received_bytes INTEGER NOT NULL,
             total_bytes INTEGER NOT NULL, state INTEGER NOT NULL, end_time INTEGER NOT NULL,
             referrer VARCHAR NOT NULL DEFAULT '');
             CREATE TABLE downloads_url_chains (id INTEGER NOT NULL, chain_index INTEGER NOT NULL, url LONGVARCHAR NOT NULL,
             PRIMARY KEY (id, chain_index));
             INSERT INTO downloads_url_chains VALUES (1, 0, 'https://example.com/get/done'), (1, 1, 'https://cdn.example.net/done.zip?sig=1');",
        )
        .unwrap();
        for (id, path, size, state) in rows {
            c.execute(
                "INSERT INTO downloads (id, guid, current_path, target_path, start_time, received_bytes, total_bytes, state, end_time)
                 VALUES (?1, 'g', ?2, ?2, ?5, ?3, ?3, ?4, ?5 + 5000000)",
                rusqlite::params![id, path, size, state, START],
            )
            .unwrap();
        }
    }

    #[test]
    fn recovers_only_complete_present_matching_files() {
        let d = tempfile::tempdir().unwrap();
        let incoming = d.path().join("incoming");
        let completed = d.path().join("completed");
        std::fs::create_dir_all(&incoming).unwrap();
        std::fs::create_dir_all(&completed).unwrap();
        std::fs::write(incoming.join("done.zip"), b"12345").unwrap();
        std::fs::write(incoming.join("resized.zip"), b"123").unwrap();
        std::fs::write(incoming.join("already.zip"), b"1").unwrap();
        std::fs::write(incoming.join("pending.zip"), b"22").unwrap();
        let start_ms = chrome_time_to_unix_ms(START);
        std::fs::write(completed.join(format!("4-{start_ms}.json")), b"{}").unwrap();
        let p = |n: &str| incoming.join(n).to_string_lossy().into_owned();
        let other = chromine_shared::CompletedDownload {
            schema: 1,
            guid: "99-1".into(),
            suggested_filename: "pending.zip".into(),
            file_path: p("pending.zip"),
            size: 2,
            dev: 0,
            ino: 0,
            completed_at_ms: 0,
            source: RecordSource::Extension,
            source_url: None,
        };
        std::fs::write(completed.join("99-1.json"), serde_json::to_vec(&other).unwrap()).unwrap();
        make_history(
            &d.path().join("profile"),
            &[
                (1, &p("done.zip"), 5, 1),
                (2, &p("cancelled.zip"), 5, 2),
                (3, &p("resized.zip"), 5, 1),
                (4, &p("already.zip"), 1, 1),
                (5, &p("claimed-and-gone.zip"), 5, 1),
                (6, "/etc/passwd", 5, 1),
                (7, &p("pending.zip"), 2, 1),
            ],
        );
        let r = reconcile(&d.path().join("profile"), &incoming, &completed).unwrap();
        assert_eq!(r.examined, 6);
        assert_eq!(r.recovered, 1);
        let rec: chromine_shared::CompletedDownload =
            serde_json::from_slice(&std::fs::read(completed.join(format!("1-{start_ms}.json"))).unwrap()).unwrap();
        assert_eq!(rec.source, RecordSource::History);
        assert_eq!(rec.source_url.as_deref(), Some("https://example.com/get/done"));
        assert_eq!(rec.size, 5);
        assert!(rec.completed_at_ms > 1_600_000_000_000);
        assert_eq!(start_ms, (START / 1000) - 11_644_473_600_000);
    }

    #[test]
    fn missing_history_is_fine() {
        let d = tempfile::tempdir().unwrap();
        let r = reconcile(d.path(), d.path(), d.path()).unwrap();
        assert_eq!(r.examined, 0);
    }
}

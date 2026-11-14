use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use chromine_shared::{
    CompletedDownload, MAX_RECORD_BYTES, SCHEMA_VERSION, fsutil, is_partial_download_name, is_valid_guid, now_ms,
};
use notify::{RecursiveMode, Watcher};
use serde::Serialize;
use tokio::sync::{Notify, mpsc, watch};
use tokio_util::sync::CancellationToken;

use crate::db::{Db, NewJob, state};
use crate::settings::Live;

#[derive(Clone)]
pub struct Ingest {
    pub db: Db,
    pub incoming: PathBuf,
    pub completed: PathBuf,
    pub jobs_dir: PathBuf,
    pub upload_wake: Arc<Notify>,
    pub settings: watch::Receiver<Live>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum RecordOutcome {
    Queued(String),
    Duplicate,
    Missing(String),
    Rejected(String),
}

#[derive(Debug, Clone, Serialize)]
pub struct Orphan {
    pub name: String,
    pub size_bytes: u64,
    pub modified_at: i64,
}

pub const STAGED_NAME: &str = "data";

pub fn clean_display_name(raw: &str) -> String {
    let mut s: String =
        raw.chars().map(|c| if c == '/' || c == '\\' { '_' } else { c }).filter(|c| !c.is_control()).collect();
    s = s.trim().to_string();
    if s.len() > 255 {
        let mut cut = 255;
        while !s.is_char_boundary(cut) {
            cut -= 1;
        }
        s.truncate(cut);
    }
    if s.is_empty() || s == "." || s == ".." { "download.bin".to_string() } else { s }
}

pub fn clean_source_url(raw: &str) -> Option<String> {
    let mut url = url::Url::parse(raw.trim()).ok()?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none_or(str::is_empty) {
        return None;
    }
    url.set_username("").ok()?;
    url.set_password(None).ok()?;
    Some(String::from(url)).filter(|u| u.len() <= 2048)
}

impl Ingest {
    pub fn staged_path(&self, job_id: &str) -> PathBuf {
        self.jobs_dir.join(job_id).join(STAGED_NAME)
    }

    pub fn read_record(path: &Path) -> Result<CompletedDownload> {
        let (file, meta) = fsutil::open_regular_nofollow(path)?;
        if meta.len() > MAX_RECORD_BYTES {
            bail!("record too large");
        }
        let rec: CompletedDownload =
            serde_json::from_reader(std::io::BufReader::new(file)).context("malformed record")?;
        if rec.schema != SCHEMA_VERSION {
            bail!("unsupported record schema {}", rec.schema);
        }
        if !is_valid_guid(&rec.guid) {
            bail!("invalid GUID");
        }
        let expected = format!("{}.json", rec.guid);
        if path.file_name().and_then(|n| n.to_str()) != Some(expected.as_str()) {
            bail!("record name does not match its GUID");
        }
        Ok(rec)
    }

    pub async fn process_record(&self, record_path: &Path) -> Result<RecordOutcome> {
        let rec = match Self::read_record(record_path) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(record = %record_path.display(), error = %e, "ignoring invalid completion record");
                quarantine(record_path);
                return Ok(RecordOutcome::Rejected(e.to_string()));
            }
        };
        if let Some(existing) = self.db.get_by_guid(&rec.guid).await? {
            if existing.file_state != state::FILE_CLAIMING {
                remove_record(record_path);
            }
            return Ok(RecordOutcome::Duplicate);
        }
        let file_path = PathBuf::from(&rec.file_path);
        let display = if rec.suggested_filename.trim().is_empty() {
            file_path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
        } else {
            rec.suggested_filename.clone()
        };
        let original = clean_display_name(&display);

        if !fsutil::is_direct_child(&self.incoming, &file_path) {
            remove_record(record_path);
            return Ok(RecordOutcome::Rejected(format!("{} is outside the download directory", rec.file_path)));
        }
        let chrome_name = file_path.file_name().unwrap().to_string_lossy().into_owned();
        if is_partial_download_name(&chrome_name) {
            remove_record(record_path);
            return Ok(RecordOutcome::Rejected("partial download".into()));
        }

        let id = uuid::Uuid::new_v4().to_string();
        let staged = self.staged_path(&id);
        let job = NewJob {
            id: id.clone(),
            download_guid: rec.guid.clone(),
            source: match rec.source {
                chromine_shared::RecordSource::Extension => "extension".into(),
                chromine_shared::RecordSource::Cdp => "cdp".into(),
                chromine_shared::RecordSource::History => "history".into(),
                chromine_shared::RecordSource::YtDlp => "ytdlp".into(),
            },
            source_url: rec.source_url.as_deref().and_then(clean_source_url),
            original_filename: original.clone(),
            chrome_filename: chrome_name,
            size_bytes: rec.size as i64,
            file_dev: rec.dev as i64,
            file_ino: rec.ino as i64,
            incoming_path: rec.file_path.clone(),
            staged_path: staged.to_string_lossy().into_owned(),
            download_completed_at: rec.completed_at_ms,
        };
        if !self.db.insert_claiming(&job, now_ms()).await? {
            remove_record(record_path);
            return Ok(RecordOutcome::Duplicate);
        }
        let outcome = self.claim(&id, &file_path, &staged, rec.dev, rec.ino, rec.size).await?;
        remove_record(record_path);
        if let RecordOutcome::Queued(_) = &outcome {
            tracing::info!(job = %id, file = %original, size = rec.size, "download queued for upload");
            self.upload_wake.notify_one();
        }
        Ok(outcome)
    }

    async fn claim(&self, id: &str, from: &Path, to: &Path, dev: u64, ino: u64, size: u64) -> Result<RecordOutcome> {
        let from = from.to_path_buf();
        let to = to.to_path_buf();
        let res = tokio::task::spawn_blocking(move || claim_file(&from, &to, dev, ino, size)).await?;
        match res {
            Ok(()) => {
                self.stage(id).await?;
                Ok(RecordOutcome::Queued(id.to_string()))
            }
            Err(e) => {
                let msg = format!("The completed download could not be claimed: {e}");
                tracing::error!(job = %id, error = %e, "claim failed");
                self.db.mark_file_missing(id, &msg, now_ms()).await?;
                Ok(RecordOutcome::Missing(msg))
            }
        }
    }

    async fn stage(&self, id: &str) -> Result<()> {
        let upload = self.settings.borrow().filecloud;
        self.db.mark_staged(id, upload, now_ms()).await
    }

    pub async fn recover_claims(&self) -> Result<()> {
        for job in self.db.claiming_jobs().await? {
            let staged = PathBuf::from(&job.staged_path);
            let incoming = PathBuf::from(&job.incoming_path);
            let matches = |p: &Path| {
                fsutil::regular_file_lstat(p)
                    .map(|m| m.dev() as i64 == job.file_dev && m.ino() as i64 == job.file_ino)
                    .unwrap_or(false)
            };
            if matches(&staged) {
                self.stage(&job.id).await?;
            } else if matches(&incoming) {
                let _ = self
                    .claim(&job.id, &incoming, &staged, job.file_dev as u64, job.file_ino as u64, job.size_bytes as u64)
                    .await?;
            } else {
                self.db
                    .mark_file_missing(&job.id, "The file disappeared before it could be claimed.", now_ms())
                    .await?;
            }
        }
        Ok(())
    }

    pub async fn scan(&self) -> Result<usize> {
        let dir = self.completed.clone();
        let paths = tokio::task::spawn_blocking(move || list_records(&dir)).await??;
        let mut n = 0;
        for p in paths {
            match self.process_record(&p).await {
                Ok(RecordOutcome::Queued(_)) => n += 1,
                Ok(_) => {}
                Err(e) => tracing::error!(record = %p.display(), error = %e, "processing record failed"),
            }
        }
        Ok(n)
    }

    pub fn orphans(&self) -> Vec<Orphan> {
        let mut referenced = std::collections::HashSet::new();
        if let Ok(paths) = list_records(&self.completed) {
            for p in paths {
                if let Ok(r) = Self::read_record(&p) {
                    referenced.insert(r.file_path);
                }
            }
        }
        let mut out = Vec::new();
        let Ok(rd) = std::fs::read_dir(&self.incoming) else { return out };
        let now = now_ms();
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || is_partial_download_name(&name) {
                continue;
            }
            let Ok(meta) = e.metadata() else { continue };
            if !meta.file_type().is_file() {
                continue;
            }
            let modified = meta.mtime() * 1000;
            if now - modified < 120_000 {
                continue;
            }
            if referenced.contains(&e.path().to_string_lossy().into_owned()) {
                continue;
            }
            out.push(Orphan { name, size_bytes: meta.len(), modified_at: modified });
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    pub async fn import_orphan(&self, name: &str) -> Result<String> {
        if name.is_empty() || name.contains('/') || name.starts_with('.') || is_partial_download_name(name) {
            bail!("invalid file name");
        }
        let path = self.incoming.join(name);
        if !fsutil::is_direct_child(&self.incoming, &path) {
            bail!("invalid file name");
        }
        let meta = fsutil::regular_file_lstat(&path).context("file not found or not a regular file")?;
        let id = uuid::Uuid::new_v4().to_string();
        let staged = self.staged_path(&id);
        let job = NewJob {
            id: id.clone(),
            download_guid: format!("manual-{}", uuid::Uuid::new_v4()),
            source: "manual".into(),
            source_url: None,
            original_filename: clean_display_name(name),
            chrome_filename: name.to_string(),
            size_bytes: meta.len() as i64,
            file_dev: meta.dev() as i64,
            file_ino: meta.ino() as i64,
            incoming_path: path.to_string_lossy().into_owned(),
            staged_path: staged.to_string_lossy().into_owned(),
            download_completed_at: meta.mtime() * 1000,
        };
        self.db.insert_claiming(&job, now_ms()).await?;
        match self.claim(&id, &path, &staged, meta.dev(), meta.ino(), meta.len()).await? {
            RecordOutcome::Queued(_) => {
                self.upload_wake.notify_one();
                Ok(id)
            }
            other => bail!("import failed: {other:?}"),
        }
    }

    pub async fn run(self, shutdown: CancellationToken) -> Result<()> {
        self.recover_claims().await?;
        let n = self.scan().await?;
        if n > 0 {
            tracing::info!(queued = n, "startup scan queued completed downloads");
        }
        let (tx, mut rx) = mpsc::unbounded_channel::<PathBuf>();
        let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            if let Ok(ev) = res {
                for p in ev.paths {
                    let _ = tx.send(p);
                }
            }
        })?;
        if let Err(e) = watcher.watch(&self.completed, RecursiveMode::NonRecursive) {
            tracing::warn!(error = %e, "filesystem notifications unavailable; relying on periodic scans");
        }
        let mut tick = tokio::time::interval(Duration::from_secs(30));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => break,
                Some(p) = rx.recv() => {
                    let is_record = p.extension().and_then(|e| e.to_str()) == Some("json")
                        && !p.file_name().and_then(|n| n.to_str()).unwrap_or("").starts_with('.');
                    if is_record && p.exists()
                        && let Err(e) = self.process_record(&p).await {
                            tracing::error!(record = %p.display(), error = %e, "processing record failed");
                        }
                }
                _ = tick.tick() => {
                    if let Err(e) = self.scan().await {
                        tracing::error!(error = %e, "periodic scan failed");
                    }
                }
            }
        }
        Ok(())
    }
}

fn list_records(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut v = Vec::new();
    for e in std::fs::read_dir(dir).with_context(|| format!("cannot read {}", dir.display()))?.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.ends_with(".json") && !name.starts_with('.') {
            v.push(e.path());
        }
    }
    v.sort();
    Ok(v)
}

fn remove_record(path: &Path) {
    if let Err(e) = std::fs::remove_file(path)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(record = %path.display(), error = %e, "cannot remove processed record");
    }
}

fn quarantine(path: &Path) {
    let Some(name) = path.file_name() else { return };
    let target = path.with_file_name(format!(".rejected-{}", name.to_string_lossy()));
    if std::fs::rename(path, &target).is_err() {
        remove_record(path);
    }
}

fn claim_file(from: &Path, to: &Path, dev: u64, ino: u64, size: u64) -> Result<()> {
    let meta = fsutil::regular_file_lstat(from).with_context(|| format!("{} is missing", from.display()))?;
    if meta.dev() != dev || meta.ino() != ino {
        bail!("{} was replaced by a different file", from.display());
    }
    if meta.len() != size {
        bail!("size changed after completion ({} != {})", meta.len(), size);
    }
    let dir = to.parent().context("staged path has no parent")?;
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(dir)
        .or_else(|e| if e.kind() == std::io::ErrorKind::AlreadyExists { Ok(()) } else { Err(e) })?;
    std::fs::rename(from, to).context("rename into job directory failed")?;
    let (_f, after) = fsutil::open_regular_nofollow(to)?;
    if after.dev() != dev || after.ino() != ino {
        bail!("claimed file does not match the completed download");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_links_are_web_addresses_without_passwords() {
        assert_eq!(
            clean_source_url(" https://user:secret@example.com/a b.zip?x=1#top ").as_deref(),
            Some("https://example.com/a%20b.zip?x=1#top")
        );
        assert_eq!(clean_source_url("http://example.com").as_deref(), Some("http://example.com/"));
        for bad in [
            "javascript:alert(1)",
            "blob:https://example.com/1",
            "data:text/plain,x",
            "file:///etc/passwd",
            "ftp://example.com/x",
            "not a url",
            "",
        ] {
            assert_eq!(clean_source_url(bad), None, "{bad}");
        }
        assert_eq!(clean_source_url(&format!("https://example.com/{}", "a".repeat(2100))), None);
    }

    #[test]
    fn display_names_are_cleaned() {
        assert_eq!(clean_display_name("report.zip"), "report.zip");
        assert_eq!(clean_display_name("../../etc/passwd"), ".._.._etc_passwd");
        assert_eq!(clean_display_name("a\nb\u{0}c.txt"), "abc.txt");
        assert_eq!(clean_display_name("   "), "download.bin");
        assert_eq!(clean_display_name(".."), "download.bin");
        assert!(clean_display_name(&"é".repeat(300)).len() <= 255);
    }
}

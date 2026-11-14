use std::collections::{HashMap, HashSet, VecDeque};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use chromine_shared::{
    ActiveDownload, CompletedDownload, RecordSource, RejectedDownload, SCHEMA_VERSION, fsutil,
    is_partial_download_name, is_valid_guid, now_ms,
};

pub const FINISHED_FILE_MODE: u32 = 0o640;
pub const RECORD_FILE_MODE: u32 = 0o640;

pub fn download_key(id: i64, start_ms: i64) -> String {
    format!("{id}-{start_ms}")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Cancel { key: String, reason: String },
}

pub struct Tracker {
    incoming: std::path::PathBuf,
    completed_dir: std::path::PathBuf,
    min_free: u64,
    active: HashMap<String, ActiveDownload>,
    recorded: HashSet<String>,
    rejections: VecDeque<RejectedDownload>,
    paused_low_disk: bool,
    pub last_error: Option<String>,
}

fn base_name(path: &str) -> String {
    Path::new(path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

impl Tracker {
    pub fn new(incoming: std::path::PathBuf, completed_dir: std::path::PathBuf, min_free: u64) -> Self {
        Tracker {
            incoming,
            completed_dir,
            min_free,
            active: HashMap::new(),
            recorded: HashSet::new(),
            rejections: VecDeque::new(),
            paused_low_disk: false,
            last_error: None,
        }
    }

    pub fn active(&self) -> Vec<ActiveDownload> {
        let mut v: Vec<_> = self.active.values().cloned().collect();
        v.sort_by_key(|d| d.started_at_ms);
        v
    }

    pub fn retain_active(&mut self, in_progress: &HashSet<String>) {
        self.active.retain(|key, d| {
            let keep = in_progress.contains(key);
            if !keep {
                tracing::info!(%key, file = %d.filename, "download no longer in progress in Chrome; no longer shown");
            }
            keep
        });
    }

    pub fn rejections(&self) -> Vec<RejectedDownload> {
        self.rejections.iter().cloned().collect()
    }

    pub fn paused(&self) -> bool {
        self.paused_low_disk
    }

    fn reject(&mut self, filename: &str, reason: String) {
        tracing::warn!(file = %filename, %reason, "download refused");
        self.rejections.push_back(RejectedDownload { filename: filename.to_string(), reason, at_ms: now_ms() });
        while self.rejections.len() > 20 {
            self.rejections.pop_front();
        }
    }

    pub fn on_disk_sample(&mut self, available: u64) -> Vec<Action> {
        let low = available < self.min_free;
        if low != self.paused_low_disk {
            if low {
                tracing::warn!(available, min_free = self.min_free, "free space below minimum; refusing downloads");
            } else {
                tracing::info!(available, "free space recovered; downloads allowed again");
            }
        }
        self.paused_low_disk = low;
        if !low {
            return Vec::new();
        }
        let victims: Vec<(String, String)> = self.active.iter().map(|(k, d)| (k.clone(), d.filename.clone())).collect();
        let mut actions = Vec::new();
        for (key, name) in victims {
            let reason = format!(
                "Cancelled: free disk space ({}) fell below the configured minimum ({})",
                human(available),
                human(self.min_free)
            );
            self.reject(&name, reason.clone());
            self.active.remove(&key);
            actions.push(Action::Cancel { key, reason });
        }
        actions
    }

    pub fn on_started(&mut self, key: &str, path: &str, total: u64, available: u64) -> Vec<Action> {
        if !is_valid_guid(key) {
            self.last_error = Some("Chrome reported a download with an invalid ID".into());
            return Vec::new();
        }
        let filename = base_name(path);
        if available < self.min_free {
            self.paused_low_disk = true;
            let reason = format!("Refused: only {} free, minimum is {}", human(available), human(self.min_free));
            self.reject(&filename, reason.clone());
            return vec![Action::Cancel { key: key.to_string(), reason }];
        }
        tracing::info!(%key, file = %filename, "download started");
        self.active.insert(
            key.to_string(),
            ActiveDownload {
                guid: key.to_string(),
                filename,
                received_bytes: 0,
                total_bytes: total,
                started_at_ms: now_ms(),
            },
        );
        self.check_fits(key, 0, total, available)
    }

    pub fn on_progress(&mut self, key: &str, path: &str, received: u64, total: u64, available: u64) -> Vec<Action> {
        if !self.active.contains_key(key) {
            if self.recorded.contains(key) || !is_valid_guid(key) {
                return Vec::new();
            }
            let actions = self.on_started(key, path, total, available);
            if !actions.is_empty() {
                return actions;
            }
        }
        let Some(d) = self.active.get_mut(key) else { return Vec::new() };
        if !path.is_empty() {
            d.filename = base_name(path);
        }
        d.received_bytes = received;
        d.total_bytes = total;
        self.check_fits(key, received, total, available)
    }

    fn check_fits(&mut self, key: &str, received: u64, total: u64, available: u64) -> Vec<Action> {
        if total <= received {
            return Vec::new();
        }
        let remaining = total - received;
        if available >= remaining.saturating_add(self.min_free) {
            return Vec::new();
        }
        let name = self.active.remove(key).map(|d| d.filename).unwrap_or_default();
        let reason = format!(
            "Cancelled: needs {} more but only {} is free above the {} minimum",
            human(remaining),
            human(available.saturating_sub(self.min_free)),
            human(self.min_free)
        );
        self.reject(&name, reason.clone());
        vec![Action::Cancel { key: key.to_string(), reason }]
    }

    pub fn on_interrupted(&mut self, key: &str, error: Option<&str>) {
        if let Some(d) = self.active.remove(key) {
            tracing::info!(%key, file = %d.filename, error = error.unwrap_or(""), "download cancelled or interrupted; nothing to upload");
        }
    }

    pub fn on_completed(
        &mut self,
        key: &str,
        path: &str,
        size: u64,
        replay: bool,
        source_url: Option<&str>,
    ) -> Option<CompletedDownload> {
        self.active.remove(key);
        if replay {
            let record = self.completed_dir.join(format!("{key}.json"));
            let p = Path::new(path);
            let still_waiting = !self.recorded.contains(key)
                && !record.exists()
                && fsutil::regular_file_lstat(p).map(|m| size == 0 || m.len() == size).unwrap_or(false);
            if !still_waiting {
                self.recorded.insert(key.to_string());
                return None;
            }
        }
        let name = base_name(path);
        match write_completion_record(
            &self.incoming,
            &self.completed_dir,
            key,
            &name,
            Path::new(path),
            (RecordSource::Extension, source_url),
            now_ms(),
        ) {
            Ok(r) => {
                tracing::info!(key = %r.guid, path = %r.file_path, size = r.size, "download completed; record written");
                self.recorded.insert(key.to_string());
                Some(r)
            }
            Err(e) => {
                tracing::error!(%key, error = %e, "completed download was not recorded");
                self.last_error = Some(format!("{name}: {e}"));
                self.reject(&name, format!("Not queued for upload: {e}"));
                self.recorded.insert(key.to_string());
                None
            }
        }
    }
}

pub fn write_completion_record(
    incoming: &Path,
    completed_dir: &Path,
    key: &str,
    suggested: &str,
    path: &Path,
    (source, source_url): (RecordSource, Option<&str>),
    completed_at_ms: i64,
) -> Result<CompletedDownload> {
    if !is_valid_guid(key) {
        bail!("invalid download ID");
    }
    if path.as_os_str().is_empty() {
        return Err(anyhow!("Chrome did not report where the file was saved"));
    }
    if !fsutil::is_direct_child(incoming, path) {
        bail!(
            "saved outside the download folder ({}); only files saved in {} are uploaded",
            path.display(),
            incoming.display()
        );
    }
    let name = path.file_name().and_then(|n| n.to_str()).ok_or_else(|| anyhow!("file name is not valid UTF-8"))?;
    if is_partial_download_name(name) {
        bail!("file still has a partial-download name");
    }
    let meta = fsutil::regular_file_lstat(path).with_context(|| format!("cannot inspect {}", path.display()))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(FINISHED_FILE_MODE))
        .with_context(|| format!("cannot set permissions on {}", path.display()))?;
    let record = CompletedDownload {
        schema: SCHEMA_VERSION,
        guid: key.to_string(),
        suggested_filename: suggested.chars().take(1024).collect(),
        file_path: path.to_string_lossy().into_owned(),
        size: meta.len(),
        dev: meta.dev(),
        ino: meta.ino(),
        completed_at_ms,
        source,
        source_url: source_url.filter(|u| u.len() <= 4096).map(str::to_string),
    };
    let bytes = serde_json::to_vec_pretty(&record)?;
    fsutil::atomic_write(completed_dir, &format!("{key}.json"), &bytes, RECORD_FILE_MODE)
        .context("cannot write completion record")?;
    Ok(record)
}

pub fn human(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 { format!("{bytes} B") } else { format!("{v:.1} {}", UNITS[i]) }
}

#[cfg(test)]
mod tests {
    use super::*;

    const K1: &str = "1-1793743839882";
    const K2: &str = "2-1793743840001";
    const BIG: u64 = 1 << 40;

    fn setup() -> (tempfile::TempDir, Tracker) {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("incoming")).unwrap();
        std::fs::create_dir_all(d.path().join("events/completed")).unwrap();
        let t = Tracker::new(d.path().join("incoming"), d.path().join("events/completed"), 1000);
        (d, t)
    }

    fn records(d: &tempfile::TempDir) -> usize {
        std::fs::read_dir(d.path().join("events/completed")).unwrap().count()
    }

    #[test]
    fn completed_download_writes_record() {
        let (d, mut t) = setup();
        let f = d.path().join("incoming/report.zip");
        std::fs::write(&f, b"hello").unwrap();
        t.on_started(K1, "", 5, BIG);
        assert_eq!(t.active().len(), 1);
        t.on_progress(K1, f.to_str().unwrap(), 5, 5, BIG);
        assert_eq!(t.active()[0].filename, "report.zip");
        let rec = t.on_completed(K1, f.to_str().unwrap(), 5, false, Some("https://example.com/a.zip")).unwrap();
        assert_eq!(rec.source_url.as_deref(), Some("https://example.com/a.zip"));
        assert!(t.active().is_empty());
        assert_eq!(rec.guid, K1);
        assert_eq!(rec.size, 5);
        assert_eq!(rec.suggested_filename, "report.zip");
        assert_eq!(rec.source, RecordSource::Extension);
        assert!(d.path().join(format!("events/completed/{K1}.json")).exists());
        let mode = std::fs::metadata(&f).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, FINISHED_FILE_MODE);
    }

    #[test]
    fn interrupted_downloads_write_nothing() {
        let (d, mut t) = setup();
        std::fs::write(d.path().join("incoming/a.bin.crdownload"), b"partial").unwrap();
        t.on_started(K1, "", 10, BIG);
        t.on_progress(K1, d.path().join("incoming/a.bin").to_str().unwrap(), 3, 10, BIG);
        assert_eq!(t.active()[0].received_bytes, 3);
        t.on_interrupted(K1, Some("USER_CANCELED"));
        assert!(t.active().is_empty());
        assert_eq!(records(&d), 0);
    }

    #[test]
    fn rejects_paths_outside_incoming_symlinks_and_partials() {
        let (d, mut t) = setup();
        let outside = d.path().join("elsewhere.zip");
        std::fs::write(&outside, b"x").unwrap();
        assert!(t.on_completed(K1, outside.to_str().unwrap(), 1, false, None).is_none());
        assert!(t.rejections().last().unwrap().reason.contains("outside the download folder"));

        let link = d.path().join("incoming/link.zip");
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        assert!(t.on_completed(K2, link.to_str().unwrap(), 1, false, None).is_none());

        let traversal = format!("{}/incoming/../elsewhere.zip", d.path().display());
        assert!(t.on_completed("3-1", &traversal, 1, false, None).is_none());

        let partial = d.path().join("incoming/a.zip.crdownload");
        std::fs::write(&partial, b"x").unwrap();
        assert!(t.on_completed("4-1", partial.to_str().unwrap(), 1, false, None).is_none());

        assert!(t.on_completed("5-1", "", 1, false, None).is_none());
        let ok = d.path().join("incoming/ok.zip");
        std::fs::write(&ok, b"x").unwrap();
        assert!(t.on_completed("../../x", ok.to_str().unwrap(), 1, false, None).is_none());
        assert_eq!(records(&d), 0);
    }

    #[test]
    fn same_name_downloads_get_separate_records() {
        let (d, mut t) = setup();
        let a = d.path().join("incoming/report.zip");
        let b = d.path().join("incoming/report (1).zip");
        std::fs::write(&a, b"a").unwrap();
        std::fs::write(&b, b"bb").unwrap();
        t.on_completed(K1, a.to_str().unwrap(), 1, false, None).unwrap();
        t.on_completed(K2, b.to_str().unwrap(), 2, false, None).unwrap();
        assert_eq!(records(&d), 2);
    }

    #[test]
    fn replayed_completions_only_record_files_still_waiting() {
        let (d, mut t) = setup();
        assert!(t.on_completed(K1, d.path().join("incoming/gone.zip").to_str().unwrap(), 5, true, None).is_none());
        assert!(t.rejections().is_empty());
        let f = d.path().join("incoming/waiting.zip");
        std::fs::write(&f, b"12345").unwrap();
        assert!(t.on_completed(K2, f.to_str().unwrap(), 5, true, None).is_some());
        assert!(t.on_completed(K2, f.to_str().unwrap(), 5, true, None).is_none());
        assert!(t.on_completed("9-9", f.to_str().unwrap(), 999, true, None).is_none());
        assert_eq!(records(&d), 1);
    }

    #[test]
    fn low_disk_refuses_and_cancels() {
        let (_d, mut t) = setup();
        let a = t.on_started(K1, "", 0, 500);
        assert!(matches!(&a[..], [Action::Cancel { key, .. }] if key == K1));
        assert!(t.paused());
        assert_eq!(t.rejections().len(), 1);

        t.on_disk_sample(10_000);
        assert!(!t.paused());
        let a = t.on_started(K2, "/x/huge.iso", 9_500, 10_000);
        assert!(matches!(&a[..], [Action::Cancel { .. }]));

        assert!(t.on_started("3-3", "", 0, 10_000).is_empty());
        let a = t.on_disk_sample(900);
        assert_eq!(a.len(), 1);
        assert!(t.active().is_empty());
    }
}

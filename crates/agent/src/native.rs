use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use chromine_shared::{MonitorState, fsutil, now_ms};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::downloads::{Action, Tracker, download_key};

pub const HOST_NAME: &str = "com.chromine.agent";
const MAX_MESSAGE: usize = 1024 * 1024;

pub fn is_native_host_invocation(first_arg: &str) -> bool {
    first_arg.starts_with("chrome-extension://")
}

#[derive(Debug, Clone)]
pub struct HostConfig {
    pub download_dir: PathBuf,
    pub min_free_disk_bytes: u64,
    pub state_file: PathBuf,
}

impl HostConfig {
    pub fn from_env() -> Result<HostConfig> {
        let get = |k: &str, d: &str| std::env::var(k).ok().filter(|v| !v.is_empty()).unwrap_or_else(|| d.to_string());
        Ok(HostConfig {
            download_dir: PathBuf::from(get("DOWNLOAD_DIR", "/downloads")),
            min_free_disk_bytes: get("MIN_FREE_DISK_BYTES", "2147483648").parse().context("MIN_FREE_DISK_BYTES")?,
            state_file: PathBuf::from(get("MONITOR_STATE_FILE", crate::config::DEFAULT_MONITOR_STATE_FILE)),
        })
    }
}

#[derive(Debug, Deserialize)]
struct Item {
    id: i64,
    #[serde(default)]
    start_ms: i64,
    #[serde(default)]
    filename: String,
    #[serde(default)]
    received: u64,
    #[serde(default)]
    total: u64,
    #[serde(default)]
    file_size: u64,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    url: String,
    #[serde(default)]
    referrer: String,
}

#[derive(Debug, Deserialize)]
struct Snapshot {
    downloads: Vec<Item>,
}

#[derive(Debug, Deserialize)]
struct Message {
    event: String,
    download: Item,
    #[serde(default)]
    replay: bool,
}

pub async fn read_message<R: AsyncRead + Unpin>(r: &mut R) -> std::io::Result<Option<Value>> {
    let mut len = [0u8; 4];
    match r.read_exact(&mut len).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let n = u32::from_ne_bytes(len) as usize;
    if n > MAX_MESSAGE {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "native message too large"));
    }
    let mut buf = vec![0u8; n];
    r.read_exact(&mut buf).await?;
    serde_json::from_slice(&buf).map(Some).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

pub async fn read_next<R: AsyncRead + Unpin>(mut r: R) -> (R, std::io::Result<Option<Value>>) {
    let msg = read_message(&mut r).await;
    (r, msg)
}

pub async fn write_message<W: AsyncWrite + Unpin>(w: &mut W, v: &Value) -> std::io::Result<()> {
    let body = serde_json::to_vec(v)?;
    w.write_all(&(body.len() as u32).to_ne_bytes()).await?;
    w.write_all(&body).await?;
    w.flush().await
}

pub struct Monitor {
    cfg: HostConfig,
    tracker: Tracker,
    ids: HashMap<String, i64>,
    connected_since_ms: i64,
}

impl Monitor {
    pub fn new(cfg: HostConfig) -> Monitor {
        let incoming = cfg.download_dir.join(chromine_shared::INCOMING_SUBDIR);
        let completed = cfg.download_dir.join(chromine_shared::EVENTS_SUBDIR).join(chromine_shared::COMPLETED_SUBDIR);
        let tracker = Tracker::new(incoming, completed, cfg.min_free_disk_bytes);
        Monitor { cfg, tracker, ids: HashMap::new(), connected_since_ms: now_ms() }
    }

    fn available(&self) -> u64 {
        fsutil::disk_space(&self.cfg.download_dir).map(|d| d.available_bytes).unwrap_or(u64::MAX)
    }

    fn commands(&self, actions: Vec<Action>) -> Vec<Value> {
        actions
            .into_iter()
            .filter_map(|a| match a {
                Action::Cancel { key, reason } => {
                    tracing::warn!(%key, %reason, "cancelling download");
                    self.ids.get(&key).map(|id| json!({ "cmd": "cancel", "id": id }))
                }
            })
            .collect()
    }

    pub fn handle(&mut self, msg: &Value) -> Vec<Value> {
        if msg.get("event").and_then(Value::as_str) == Some("snapshot") {
            return self.snapshot(msg);
        }
        let m: Message = match serde_json::from_value(msg.clone()) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!(error = %e, "ignoring malformed message from the extension");
                return Vec::new();
            }
        };
        let d = &m.download;
        let key = download_key(d.id, d.start_ms);
        self.ids.insert(key.clone(), d.id);
        let available = self.available();
        let actions = match m.event.as_str() {
            "created" => self.tracker.on_started(&key, &d.filename, d.total, available),
            "progress" => self.tracker.on_progress(&key, &d.filename, d.received, d.total, available),
            "complete" => {
                let size = if d.file_size > 0 { d.file_size } else { d.received };
                let source = [&d.url, &d.referrer].into_iter().find(|u| !u.is_empty()).map(String::as_str);
                self.tracker.on_completed(&key, &d.filename, size, m.replay, source);
                Vec::new()
            }
            "interrupted" => {
                self.tracker.on_interrupted(&key, d.error.as_deref());
                Vec::new()
            }
            other => {
                tracing::debug!(event = other, "ignoring unknown event");
                Vec::new()
            }
        };
        self.commands(actions)
    }

    fn snapshot(&mut self, msg: &Value) -> Vec<Value> {
        let snap: Snapshot = match serde_json::from_value(msg.clone()) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "ignoring malformed download list from the extension");
                return Vec::new();
            }
        };
        let available = self.available();
        let mut in_progress = HashSet::new();
        let mut actions = Vec::new();
        for d in &snap.downloads {
            let key = download_key(d.id, d.start_ms);
            self.ids.insert(key.clone(), d.id);
            actions.extend(self.tracker.on_progress(&key, &d.filename, d.received, d.total, available));
            in_progress.insert(key);
        }
        self.tracker.retain_active(&in_progress);
        self.commands(actions)
    }

    pub fn disk_tick(&mut self) -> Vec<Value> {
        let available = self.available();
        let actions = self.tracker.on_disk_sample(available);
        self.commands(actions)
    }

    pub fn state(&self) -> MonitorState {
        MonitorState {
            updated_at_ms: now_ms(),
            connected_since_ms: self.connected_since_ms,
            downloads_paused_low_disk: self.tracker.paused(),
            active_downloads: self.tracker.active(),
            recent_rejections: self.tracker.rejections(),
            last_error: self.tracker.last_error.clone(),
        }
    }

    pub fn write_state(&self) {
        let Some(dir) = self.cfg.state_file.parent() else { return };
        let Some(name) = self.cfg.state_file.file_name().and_then(|n| n.to_str()) else { return };
        if let Ok(bytes) = serde_json::to_vec(&self.state())
            && let Err(e) = fsutil::atomic_write(dir, name, &bytes, 0o600)
        {
            tracing::warn!(error = %e, "cannot write monitor state");
        }
    }

    pub fn clear_state(&self) {
        let _ = std::fs::remove_file(&self.cfg.state_file);
    }
}

pub async fn serve<R, W>(cfg: HostConfig, input: R, mut output: W) -> Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut monitor = Monitor::new(cfg);
    monitor.write_state();
    tracing::info!("download monitor connected to Chrome");
    let mut tick = tokio::time::interval(Duration::from_secs(2));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut reading = Box::pin(read_next(input));
    let result = loop {
        tokio::select! {
            (input, msg) = &mut reading => {
                reading = Box::pin(read_next(input));
                match msg {
                    Ok(Some(v)) => {
                        let mut failed = None;
                        for cmd in monitor.handle(&v) {
                            if let Err(e) = write_message(&mut output, &cmd).await {
                                failed = Some(e);
                                break;
                            }
                        }
                        if let Some(e) = failed {
                            break Err(anyhow::Error::from(e).context("writing to Chrome"));
                        }
                        monitor.write_state();
                    }
                    Ok(None) => break Ok(()),
                    Err(e) => break Err(anyhow::Error::from(e).context("reading from Chrome")),
                }
            }
            _ = tick.tick() => {
                for cmd in monitor.disk_tick() {
                    if let Err(e) = write_message(&mut output, &cmd).await {
                        tracing::warn!(error = %e, "cannot send command to the extension");
                    }
                }
                monitor.write_state();
            }
        }
    };
    monitor.clear_state();
    tracing::info!("download monitor disconnected");
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(d: &std::path::Path, min_free: u64) -> HostConfig {
        std::fs::create_dir_all(d.join("incoming")).unwrap();
        std::fs::create_dir_all(d.join("events/completed")).unwrap();
        HostConfig { download_dir: d.to_path_buf(), min_free_disk_bytes: min_free, state_file: d.join("monitor.json") }
    }

    fn frame(v: &Value) -> Vec<u8> {
        let b = serde_json::to_vec(v).unwrap();
        let mut out = (b.len() as u32).to_ne_bytes().to_vec();
        out.extend(b);
        out
    }

    #[test]
    fn invocation_detection() {
        assert!(is_native_host_invocation("chrome-extension://cmaeanmbmdbpdlcfjpbllpaffikmhncc/"));
        assert!(!is_native_host_invocation("run"));
    }

    #[tokio::test]
    async fn framing_round_trip_and_limits() {
        let (mut a, mut b) = tokio::io::duplex(1 << 16);
        write_message(&mut a, &json!({"cmd": "cancel", "id": 3})).await.unwrap();
        drop(a);
        assert_eq!(read_message(&mut b).await.unwrap().unwrap()["id"], 3);
        assert!(read_message(&mut b).await.unwrap().is_none());

        let mut huge = ((MAX_MESSAGE + 1) as u32).to_ne_bytes().to_vec();
        huge.extend([0u8; 8]);
        assert!(read_message(&mut &huge[..]).await.is_err());
    }

    #[tokio::test]
    async fn serves_a_download_lifecycle() {
        let d = tempfile::tempdir().unwrap();
        let c = cfg(d.path(), 0);
        let file = d.path().join("incoming/report.zip");
        std::fs::write(&file, vec![7u8; 300]).unwrap();
        let path = file.to_str().unwrap();
        let mut input = Vec::new();
        input.extend(frame(
            &json!({"event": "created", "download": {"id": 4, "start_ms": 1000, "filename": "", "total": 300}}),
        ));
        input.extend(frame(&json!({"event": "progress", "download": {"id": 4, "start_ms": 1000, "filename": path, "received": 300, "total": 300}})));
        input.extend(frame(&json!({"event": "complete", "download": {"id": 4, "start_ms": 1000, "filename": path, "received": 300, "total": 300, "file_size": 300}})));
        input.extend(frame(&json!({"event": "complete", "download": {"id": 1, "start_ms": 5, "filename": "/nope/old.zip", "file_size": 10}, "replay": true})));
        input.extend(frame(&json!({"bogus": true})));
        let mut output = Vec::new();
        serve(c.clone(), &input[..], &mut output).await.unwrap();
        assert!(output.is_empty(), "no commands expected");
        let rec: chromine_shared::CompletedDownload =
            serde_json::from_slice(&std::fs::read(d.path().join("events/completed/4-1000.json")).unwrap()).unwrap();
        assert_eq!(rec.size, 300);
        assert_eq!(rec.source, chromine_shared::RecordSource::Extension);
        assert_eq!(std::fs::read_dir(d.path().join("events/completed")).unwrap().count(), 1);
        assert!(!c.state_file.exists(), "state file is removed when Chrome disconnects");
    }

    #[tokio::test]
    async fn low_disk_sends_cancel_commands() {
        let d = tempfile::tempdir().unwrap();
        let c = cfg(d.path(), u64::MAX / 2);
        let input =
            frame(&json!({"event": "created", "download": {"id": 9, "start_ms": 77, "filename": "", "total": 10}}));
        let mut output = Vec::new();
        serve(c, &input[..], &mut output).await.unwrap();
        let mut r = &output[..];
        let cmd = read_message(&mut r).await.unwrap().unwrap();
        assert_eq!(cmd, json!({"cmd": "cancel", "id": 9}));
    }

    #[tokio::test]
    async fn message_split_across_ticks_is_not_lost() {
        let d = tempfile::tempdir().unwrap();
        let (mut chrome, host_io) = tokio::io::duplex(1 << 16);
        let (host_in, host_out) = tokio::io::split(host_io);
        let host = tokio::spawn(serve(cfg(d.path(), u64::MAX / 2), host_in, host_out));
        let msg =
            frame(&json!({"event": "created", "download": {"id": 5, "start_ms": 8, "filename": "", "total": 10}}));
        chrome.write_all(&msg[..4]).await.unwrap();
        tokio::time::sleep(Duration::from_millis(2500)).await;
        chrome.write_all(&msg[4..]).await.unwrap();
        assert_eq!(read_message(&mut chrome).await.unwrap().unwrap(), json!({"cmd": "cancel", "id": 5}));
        drop(chrome);
        host.await.unwrap().unwrap();
    }

    #[test]
    fn snapshots_drop_downloads_chrome_no_longer_has() {
        let d = tempfile::tempdir().unwrap();
        let mut m = Monitor::new(cfg(d.path(), 0));
        let names = |m: &Monitor| {
            let mut v = m.state().active_downloads.into_iter().map(|a| a.filename).collect::<Vec<_>>();
            v.sort();
            v
        };
        m.handle(&json!({"event": "created", "download": {"id": 1, "start_ms": 10, "filename": "/x/kept.zip", "total": 100}}));
        m.handle(&json!({"event": "created", "download": {"id": 2, "start_ms": 20, "filename": "/x/discarded.zip", "total": 100}}));
        m.handle(&json!({"event": "created", "download": {"id": 3, "start_ms": 30, "filename": "/x/failed.zip", "total": 100}}));
        m.handle(&json!({"event": "interrupted", "download": {"id": 3, "start_ms": 30}}));
        m.handle(&json!({"event": "progress", "download": {"id": 3, "start_ms": 30, "filename": "/x/failed.zip", "received": 5, "total": 100}}));
        assert_eq!(names(&m), ["discarded.zip", "failed.zip", "kept.zip"]);

        m.handle(&json!({"event": "snapshot", "downloads": [
            {"id": 1, "start_ms": 10, "filename": "/x/kept.zip", "received": 40, "total": 100},
            {"id": 4, "start_ms": 40, "filename": "/x/missed.zip", "received": 1, "total": 100}
        ]}));
        assert_eq!(names(&m), ["kept.zip", "missed.zip"]);
        assert_eq!(m.state().active_downloads.iter().find(|a| a.filename == "kept.zip").unwrap().received_bytes, 40);

        m.handle(&json!({"event": "snapshot", "downloads": []}));
        assert!(names(&m).is_empty(), "an empty list from Chrome clears everything");
    }

    #[test]
    fn state_reports_active_downloads() {
        let d = tempfile::tempdir().unwrap();
        let mut m = Monitor::new(cfg(d.path(), 0));
        m.handle(&json!({"event": "created", "download": {"id": 2, "start_ms": 9, "filename": "/x/movie.mp4", "total": 100}}));
        m.write_state();
        let s: MonitorState = serde_json::from_slice(&std::fs::read(d.path().join("monitor.json")).unwrap()).unwrap();
        assert_eq!(s.active_downloads.len(), 1);
        assert_eq!(s.active_downloads[0].filename, "movie.mp4");
    }
}

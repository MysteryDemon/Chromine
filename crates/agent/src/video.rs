use std::collections::BTreeMap;
use std::io::Read;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result};
use chromine_shared::{RecordSource, fsutil, is_valid_guid, now_ms};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, BufReader};
use tokio::process::Command;
use tokio::sync::{mpsc, watch};

use crate::config::AgentConfig;
use crate::native::{read_next, write_message};

pub const DEFAULT_VIDEO_DIR: &str = "/tmp/chromine-video";
pub const HOST_BIN: &str = "chromine-video-host";
const MAX_URLS: usize = 8;
pub const QUALITIES: [u32; 4] = [360, 480, 720, 1080];
const MAX_URL_LEN: usize = 4096;
const STATUS_TTL: Duration = Duration::from_secs(3600);
const PARTIAL_SUFFIXES: &[&str] = &[".part", ".ytdl", ".temp", ".tmp", ".crdownload"];

pub fn is_host_invocation(argv0: &str) -> bool {
    Path::new(argv0).file_name().and_then(|n| n.to_str()) == Some(HOST_BIN)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoRequest {
    pub id: String,
    pub urls: Vec<String>,
    #[serde(default)]
    pub quality: Option<u32>,
    pub requested_at_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VideoState {
    Queued,
    Working,
    Done,
    Unsupported,
    Protected,
    Failed,
}

impl VideoState {
    pub fn is_final(self) -> bool {
        matches!(self, VideoState::Done | VideoState::Unsupported | VideoState::Protected | VideoState::Failed)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VideoStatus {
    pub id: String,
    pub state: VideoState,
    #[serde(default)]
    pub progress: Option<f64>,
    #[serde(default)]
    pub filename: Option<String>,
    pub updated_at_ms: i64,
}

fn requests_dir(dir: &Path) -> PathBuf {
    dir.join("requests")
}

fn status_dir(dir: &Path) -> PathBuf {
    dir.join("status")
}

pub fn prepare(dir: &Path) -> Result<()> {
    for d in [requests_dir(dir), status_dir(dir)] {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&d)
            .with_context(|| format!("cannot create {}", d.display()))?;
    }
    Ok(())
}

pub fn valid_url(u: &str) -> bool {
    u.len() <= MAX_URL_LEN
        && (u.starts_with("https://") || u.starts_with("http://"))
        && u.len() > "https://".len()
        && !u.chars().any(|c| c.is_whitespace() || c.is_control())
}

fn new_id() -> String {
    let mut b = [0u8; 6];
    let _ = std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut b));
    let rand: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!("{:012x}{rand}", now_ms())
}

fn read_json<T: DeserializeOwned>(path: &Path) -> Option<T> {
    let (f, meta) = fsutil::open_regular_nofollow(path).ok()?;
    if meta.len() > 64 * 1024 {
        return None;
    }
    let mut buf = Vec::new();
    f.take(64 * 1024).read_to_end(&mut buf).ok()?;
    serde_json::from_slice(&buf).ok()
}

fn write_json(dir: &Path, name: &str, v: &impl Serialize) -> std::io::Result<()> {
    fsutil::atomic_write(dir, name, &serde_json::to_vec(v).map_err(std::io::Error::other)?, 0o600)
}

pub fn set_status(dir: &Path, status: &VideoStatus) {
    if let Err(e) = write_json(&status_dir(dir), &format!("{}.json", status.id), status) {
        tracing::warn!(error = %e, "cannot write video download status");
    }
}

pub fn get_status(dir: &Path, id: &str) -> Option<VideoStatus> {
    read_json(&status_dir(dir).join(format!("{id}.json")))
}

fn status_message(reference: &str, st: &VideoStatus) -> Value {
    json!({
        "type": "status",
        "ref": reference,
        "state": st.state,
        "progress": st.progress,
        "filename": st.filename,
    })
}

fn accept(dir: &Path, v: &Value, watching: &mut BTreeMap<String, (String, Option<VideoStatus>)>) -> Option<Value> {
    if v.get("type").and_then(Value::as_str) != Some("download") {
        return None;
    }
    let reference = v
        .get("ref")
        .and_then(Value::as_str)
        .filter(|r| !r.is_empty() && r.len() <= 100 && r.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))?
        .to_string();
    let mut urls: Vec<String> = Vec::new();
    for u in v.get("urls").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str) {
        if urls.len() < MAX_URLS && valid_url(u) && !urls.iter().any(|x| x == u) {
            urls.push(u.to_string());
        }
    }
    if urls.is_empty() {
        return Some(json!({ "type": "status", "ref": reference, "state": VideoState::Unsupported }));
    }
    let quality =
        v.get("quality").and_then(Value::as_u64).and_then(|q| QUALITIES.into_iter().find(|&x| u64::from(x) == q));
    let req = VideoRequest { id: new_id(), urls, quality, requested_at_ms: now_ms() };
    let queued = VideoStatus {
        id: req.id.clone(),
        state: VideoState::Queued,
        progress: None,
        filename: None,
        updated_at_ms: now_ms(),
    };
    let written = write_json(&status_dir(dir), &format!("{}.json", req.id), &queued)
        .and_then(|_| write_json(&requests_dir(dir), &format!("{}.json", req.id), &req));
    if let Err(e) = written {
        tracing::warn!(error = %e, "cannot queue the video download");
        return Some(json!({ "type": "status", "ref": reference, "state": VideoState::Failed }));
    }
    tracing::info!(id = %req.id, quality = ?req.quality, "video download requested");
    watching.insert(req.id, (reference, None));
    None
}

pub async fn host_serve<R, W>(dir: PathBuf, input: R, mut output: W) -> Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    prepare(&dir)?;
    let mut watching: BTreeMap<String, (String, Option<VideoStatus>)> = BTreeMap::new();
    let mut tick = tokio::time::interval(Duration::from_millis(500));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut reading = Box::pin(read_next(input));
    loop {
        tokio::select! {
            (input, msg) = &mut reading => match msg {
                Ok(Some(v)) => {
                    reading = Box::pin(read_next(input));
                    if let Some(reply) = accept(&dir, &v, &mut watching) {
                        write_message(&mut output, &reply).await.context("writing to Chrome")?;
                    }
                }
                Ok(None) => return Ok(()),
                Err(e) => return Err(anyhow::Error::from(e).context("reading from Chrome")),
            },
            _ = tick.tick() => {
                let mut finished = Vec::new();
                for (id, (reference, last)) in watching.iter_mut() {
                    let Some(st) = get_status(&dir, id) else { continue };
                    if last.as_ref() == Some(&st) {
                        continue;
                    }
                    write_message(&mut output, &status_message(reference, &st)).await.context("writing to Chrome")?;
                    if st.state.is_final() {
                        finished.push(id.clone());
                    }
                    *last = Some(st);
                }
                for id in finished {
                    watching.remove(&id);
                }
            }
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    Done(PathBuf),
    Unsupported,
    Protected,
    Failed,
    Stopped,
}

pub fn parse_progress(line: &str) -> Option<f64> {
    let rest = line.trim_start().strip_prefix("[download]")?.trim_start();
    let pct = rest.split('%').next()?.trim();
    if rest.len() == pct.len() {
        return None;
    }
    pct.parse::<f64>().ok().filter(|p| (0.0..=100.0).contains(p))
}

fn started_download(line: &str) -> bool {
    parse_progress(line).is_some() || line.trim_start().starts_with("[download] Destination:")
}

pub fn redact_urls(line: &str) -> String {
    line.split(' ')
        .map(|w| match (w.starts_with("http://") || w.starts_with("https://"), w.find('?')) {
            (true, Some(q)) => format!("{}?…", &w[..q]),
            _ => w.to_string(),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn is_partial(name: &str) -> bool {
    name.starts_with('.') || PARTIAL_SUFFIXES.iter().any(|s| name.ends_with(s))
}

pub fn pick_output(staging: &Path) -> Option<PathBuf> {
    std::fs::read_dir(staging)
        .ok()?
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_str()?.to_string();
            let meta = std::fs::symlink_metadata(e.path()).ok()?;
            (meta.is_file() && meta.len() > 0 && !is_partial(&name)).then(|| (meta.len(), e.path()))
        })
        .max_by_key(|(len, _)| *len)
        .map(|(_, p)| p)
}

pub fn unique_target(dir: &Path, name: &str) -> PathBuf {
    let first = dir.join(name);
    if std::fs::symlink_metadata(&first).is_err() {
        return first;
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    (2..10_000)
        .map(|n| dir.join(format!("{stem} ({n}){ext}")))
        .find(|p| std::fs::symlink_metadata(p).is_err())
        .unwrap_or(first)
}

fn cookie_source(profile: &Path) -> Option<String> {
    let default = profile.join("Default");
    let has_db = default.join("Cookies").is_file() || default.join("Network/Cookies").is_file();
    has_db.then(|| format!("chrome+basictext:{}", default.display()))
}

#[derive(Default)]
struct Seen {
    started: bool,
    reported: bool,
    cookie_problem: bool,
    drm: bool,
    last: String,
}

struct Run<'a> {
    cfg: &'a AgentConfig,
    id: &'a str,
    staging: &'a Path,
    quality: Option<u32>,
}

impl Run<'_> {
    fn status(&self, state: VideoState, progress: Option<f64>, filename: Option<String>) {
        set_status(
            &self.cfg.video_dir,
            &VideoStatus { id: self.id.to_string(), state, progress, filename, updated_at_ms: now_ms() },
        );
    }

    async fn ytdlp(&self, url: &str, cookies: Option<&str>, shutdown: &mut watch::Receiver<bool>) -> (Outcome, bool) {
        let mut cmd = Command::new(&self.cfg.ytdlp_bin);
        cmd.args([
            "--ignore-config",
            "--no-playlist",
            "--playlist-items",
            "1",
            "--newline",
            "--progress",
            "--color",
            "no_color",
            "--no-mtime",
            "--windows-filenames",
            "-f",
            "bv*+ba/b",
            "--merge-output-format",
            "mp4/mkv",
        ]);
        if let Some(q) = self.quality {
            cmd.arg("-S").arg(format!("res:{q}"));
        }
        cmd.arg("-o").arg(self.staging.join("%(title).150B [%(id)s].%(ext)s"));
        if let Some(c) = cookies {
            cmd.arg("--cookies-from-browser").arg(c);
        }
        cmd.arg("--")
            .arg(url)
            .env_clear()
            .env("PATH", "/usr/local/bin:/usr/bin:/bin")
            .env("HOME", &self.cfg.home_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        for var in ["LANG", "TZ", "TMPDIR", "XDG_RUNTIME_DIR"] {
            if let Ok(v) = std::env::var(var) {
                cmd.env(var, v);
            }
        }
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                tracing::error!(error = %e, bin = %self.cfg.ytdlp_bin.display(), "cannot run yt-dlp");
                return (Outcome::Failed, false);
            }
        };
        let pgid = child.id().map(|p| p as i32);
        let (tx, mut lines) = mpsc::unbounded_channel::<String>();
        for stream in [
            child.stdout.take().map(|s| Box::new(s) as Box<dyn AsyncRead + Unpin + Send>),
            child.stderr.take().map(|s| Box::new(s) as Box<dyn AsyncRead + Unpin + Send>),
        ]
        .into_iter()
        .flatten()
        {
            let tx = tx.clone();
            tokio::spawn(async move {
                let mut r = BufReader::new(stream).lines();
                while let Ok(Some(l)) = r.next_line().await {
                    if tx.send(l).is_err() {
                        break;
                    }
                }
            });
        }
        drop(tx);

        let mut seen = Seen::default();
        let mut last_write = Instant::now().checked_sub(Duration::from_secs(10)).unwrap_or_else(Instant::now);
        let mut handle = |line: &str, seen: &mut Seen| {
            if started_download(line) {
                seen.started = true;
            }
            if !line.trim().is_empty() {
                seen.last = redact_urls(line).chars().take(300).collect();
            }
            if let Some(p) = parse_progress(line)
                && last_write.elapsed() >= Duration::from_millis(700)
            {
                self.status(VideoState::Working, Some(p), None);
                last_write = Instant::now();
            }
            if line.starts_with("ERROR:") {
                seen.reported = true;
                let lower = line.to_ascii_lowercase();
                if lower.contains("cookie") {
                    seen.cookie_problem = true;
                }
                if lower.contains("drm") {
                    seen.drm = true;
                }
                tracing::info!(id = self.id, "yt-dlp: {}", seen.last);
            }
        };
        let status = loop {
            tokio::select! {
                Some(line) = lines.recv() => handle(&line, &mut seen),
                s = child.wait() => break s,
                _ = shutdown.changed() => {
                    if let Some(pid) = pgid {
                        unsafe {
                            libc::kill(-pid, libc::SIGKILL);
                        }
                    }
                    let _ = child.wait().await;
                    return (Outcome::Stopped, false);
                }
            }
        };
        let drain_until = Instant::now() + Duration::from_secs(2);
        while let Ok(Some(line)) =
            tokio::time::timeout(drain_until.saturating_duration_since(Instant::now()), lines.recv()).await
        {
            handle(&line, &mut seen);
        }
        if let Some(pid) = pgid {
            unsafe {
                libc::kill(-pid, libc::SIGKILL);
            }
        }
        let outcome = match status {
            Ok(s) if s.success() => match pick_output(self.staging) {
                Some(f) => Outcome::Done(f),
                None if seen.started => Outcome::Failed,
                None => Outcome::Unsupported,
            },
            _ if seen.drm => Outcome::Protected,
            _ if seen.started => Outcome::Failed,
            _ if seen.reported => Outcome::Unsupported,
            other => {
                let exit = other.map(|s| s.to_string()).unwrap_or_else(|e| e.to_string());
                tracing::warn!(id = self.id, %exit, output = %seen.last, "yt-dlp stopped without reporting an error");
                Outcome::Failed
            }
        };
        (outcome, seen.cookie_problem)
    }

    async fn download(&self, url: &str, shutdown: &mut watch::Receiver<bool>) -> Outcome {
        let cookies = cookie_source(&self.cfg.profile_dir);
        let (outcome, cookie_problem) = self.ytdlp(url, cookies.as_deref(), shutdown).await;
        if outcome == Outcome::Unsupported && cookie_problem && cookies.is_some() {
            return self.ytdlp(url, None, shutdown).await.0;
        }
        outcome
    }

    fn publish(&self, file: &Path, page: &str) -> Result<String> {
        let name = file.file_name().and_then(|n| n.to_str()).context("the file name is not valid UTF-8")?;
        let incoming = self.cfg.incoming_dir();
        let target = unique_target(&incoming, name);
        std::fs::rename(file, &target).with_context(|| format!("cannot move the video into {}", incoming.display()))?;
        let final_name = target.file_name().and_then(|n| n.to_str()).unwrap_or(name).to_string();
        crate::downloads::write_completion_record(
            &incoming,
            &self.cfg.completed_dir(),
            self.id,
            &final_name,
            &target,
            (RecordSource::YtDlp, Some(page)),
            now_ms(),
        )?;
        Ok(final_name)
    }
}

fn next_request(dir: &Path) -> Option<VideoRequest> {
    let rd = requests_dir(dir);
    let mut names: Vec<String> = std::fs::read_dir(&rd)
        .ok()?
        .flatten()
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .filter(|n| n.ends_with(".json") && !n.starts_with('.'))
        .collect();
    names.sort();
    for name in names {
        let path = rd.join(&name);
        let req: Option<VideoRequest> = read_json(&path);
        let _ = std::fs::remove_file(&path);
        if let Some(req) = req
            && is_valid_guid(&req.id)
            && name == format!("{}.json", req.id)
        {
            return Some(req);
        }
    }
    None
}

fn expire_statuses(dir: &Path) {
    let Ok(rd) = std::fs::read_dir(status_dir(dir)) else { return };
    for e in rd.flatten() {
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| SystemTime::now().duration_since(t).ok())
            .is_some_and(|age| age > STATUS_TTL);
        if old {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

async fn process(cfg: &AgentConfig, staging_root: &Path, req: VideoRequest, shutdown: &mut watch::Receiver<bool>) {
    let staging = staging_root.join(&req.id);
    let run = Run { cfg, id: &req.id, staging: &staging, quality: req.quality };
    run.status(VideoState::Working, Some(0.0), None);
    let free = fsutil::disk_space(&cfg.download_dir).map(|d| d.available_bytes).unwrap_or(u64::MAX);
    if free < cfg.min_free_disk_bytes {
        tracing::warn!(id = %req.id, "not enough free disk space for a video download");
        run.status(VideoState::Failed, None, None);
        return;
    }
    for url in &req.urls {
        let _ = std::fs::remove_dir_all(&staging);
        if let Err(e) = std::fs::DirBuilder::new().recursive(true).mode(0o770).create(&staging) {
            tracing::error!(error = %e, "cannot create the video staging folder");
            run.status(VideoState::Failed, None, None);
            return;
        }
        let outcome = run.download(url, shutdown).await;
        match outcome {
            Outcome::Unsupported => continue,
            Outcome::Done(file) => {
                match run.publish(&file, url) {
                    Ok(name) => {
                        tracing::info!(id = %req.id, file = %name, "video downloaded");
                        run.status(VideoState::Done, Some(100.0), Some(name));
                    }
                    Err(e) => {
                        tracing::error!(id = %req.id, error = %e, "cannot hand the video over for upload");
                        run.status(VideoState::Failed, None, None);
                    }
                }
                let _ = std::fs::remove_dir_all(&staging);
                return;
            }
            Outcome::Protected => {
                tracing::info!(id = %req.id, "the video is DRM-protected");
                run.status(VideoState::Protected, None, None);
                let _ = std::fs::remove_dir_all(&staging);
                return;
            }
            Outcome::Failed | Outcome::Stopped => {
                tracing::warn!(id = %req.id, "video download failed");
                run.status(VideoState::Failed, None, None);
                let _ = std::fs::remove_dir_all(&staging);
                return;
            }
        }
    }
    let _ = std::fs::remove_dir_all(&staging);
    tracing::info!(id = %req.id, "no downloadable video found");
    run.status(VideoState::Unsupported, None, None);
}

pub async fn run_queue(cfg: AgentConfig, mut shutdown: watch::Receiver<bool>) {
    let staging_root = cfg.download_dir.join("video");
    if let Err(e) = prepare(&cfg.video_dir) {
        tracing::error!(error = %e, "video downloads are unavailable");
        return;
    }
    if let Ok(rd) = std::fs::read_dir(&staging_root) {
        for e in rd.flatten() {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
    loop {
        if *shutdown.borrow() {
            return;
        }
        match next_request(&cfg.video_dir) {
            Some(req) => process(&cfg, &staging_root, req, &mut shutdown).await,
            None => {
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_millis(700)) => {}
                    _ = shutdown.changed() => return,
                }
            }
        }
        expire_statuses(&cfg.video_dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    #[test]
    fn progress_lines_are_parsed() {
        assert_eq!(parse_progress("[download]  42.5% of   12.34MiB at    1.20MiB/s ETA 00:10"), Some(42.5));
        assert_eq!(parse_progress("[download] 100% of 3.00MiB in 00:00:01"), Some(100.0));
        assert_eq!(parse_progress("[download]   7.0% of ~  50.00MiB at  2.00MiB/s ETA 00:20 (frag 3/40)"), Some(7.0));
        assert_eq!(parse_progress("[download] Destination: /x/a.mp4"), None);
        assert_eq!(parse_progress("[youtube] abc: Downloading webpage"), None);
        assert!(started_download("[download] Destination: /x/a.mp4"));
    }

    #[test]
    fn urls_are_validated_and_redacted() {
        assert!(valid_url("https://example.com/watch?v=1"));
        assert!(valid_url("http://example.com/a.mp4"));
        assert!(!valid_url("https://"));
        assert!(!valid_url("file:///etc/passwd"));
        assert!(!valid_url("blob:https://example.com/abc"));
        assert!(!valid_url("javascript:alert(1)"));
        assert!(!valid_url("https://example.com/a b"));
        assert!(!valid_url(&format!("https://example.com/{}", "a".repeat(5000))));
        assert_eq!(
            redact_urls("ERROR: Unable to download https://cdn.example/v.m3u8?token=secret: HTTP 403"),
            "ERROR: Unable to download https://cdn.example/v.m3u8?… HTTP 403"
        );
    }

    #[test]
    fn output_and_target_files() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("a.f137.mp4.part"), vec![0; 50]).unwrap();
        std::fs::write(d.path().join("Clip [x1].mp4"), vec![0; 20]).unwrap();
        std::fs::write(d.path().join("Clip [x1].en.vtt"), vec![0; 5]).unwrap();
        assert_eq!(pick_output(d.path()).unwrap().file_name().unwrap(), "Clip [x1].mp4");
        let inc = d.path().join("incoming");
        std::fs::create_dir(&inc).unwrap();
        assert_eq!(unique_target(&inc, "v.mp4"), inc.join("v.mp4"));
        std::fs::write(inc.join("v.mp4"), b"x").unwrap();
        assert_eq!(unique_target(&inc, "v.mp4"), inc.join("v (2).mp4"));
        std::fs::write(inc.join("v (2).mp4"), b"x").unwrap();
        assert_eq!(unique_target(&inc, "v.mp4"), inc.join("v (3).mp4"));
    }

    #[test]
    fn requests_are_queued_from_the_extension() {
        let d = tempfile::tempdir().unwrap();
        prepare(d.path()).unwrap();
        let mut watching = BTreeMap::new();
        let reply = accept(
            d.path(),
            &json!({"type": "download", "ref": "r-1", "urls": ["blob:x", "javascript:1", "file:///etc/passwd"]}),
            &mut watching,
        );
        assert_eq!(reply.unwrap()["state"], "unsupported");
        assert!(
            accept(
                d.path(),
                &json!({"type": "download", "ref": "bad ref!", "urls": ["https://a.example/"]}),
                &mut watching
            )
            .is_none()
        );
        assert!(watching.is_empty());
        let urls = ["https://a.example/watch", "https://a.example/watch", "https://cdn.example/v.mp4"];
        assert!(accept(d.path(), &json!({"type": "download", "ref": "r-2", "urls": urls}), &mut watching).is_none());
        let (id, (reference, _)) = watching.iter().next().unwrap();
        assert_eq!(reference, "r-2");
        assert!(is_valid_guid(id));
        assert_eq!(get_status(d.path(), id).unwrap().state, VideoState::Queued);
        let req = next_request(d.path()).unwrap();
        assert_eq!(&req.id, id);
        assert_eq!(req.urls, vec!["https://a.example/watch", "https://cdn.example/v.mp4"]);
        assert_eq!(req.quality, None);
        assert!(next_request(d.path()).is_none(), "a request is taken once");

        for (quality, want) in [(json!(720), Some(720)), (json!(721), None), (json!("720"), None), (json!(null), None)]
        {
            let msg = json!({"type": "download", "ref": "r-3", "urls": ["https://a.example/"], "quality": quality});
            assert!(accept(d.path(), &msg, &mut watching).is_none());
            assert_eq!(next_request(d.path()).unwrap().quality, want, "{quality}");
        }

        let tmp = requests_dir(d.path()).join(".tmp-1-2-0123456789ab0123456789ab.json");
        std::fs::write(&tmp, b"{").unwrap();
        assert!(next_request(d.path()).is_none());
        assert!(tmp.exists(), "a file still being written by the host is left alone");
    }

    #[tokio::test]
    async fn host_survives_a_message_split_across_ticks() {
        let d = tempfile::tempdir().unwrap();
        let (mut chrome, host_io) = tokio::io::duplex(1 << 16);
        let (host_in, host_out) = tokio::io::split(host_io);
        let host = tokio::spawn(host_serve(d.path().to_path_buf(), host_in, host_out));
        let body =
            serde_json::to_vec(&json!({"type": "download", "ref": "r-9", "urls": ["https://a.example/v"]})).unwrap();
        chrome.write_all(&(body.len() as u32).to_ne_bytes()).await.unwrap();
        tokio::time::sleep(Duration::from_millis(1200)).await;
        chrome.write_all(&body).await.unwrap();
        let reply = crate::native::read_message(&mut chrome).await.unwrap().unwrap();
        assert_eq!(reply["ref"], "r-9");
        assert_eq!(reply["state"], "queued");
        drop(chrome);
        host.await.unwrap().unwrap();
    }

    fn test_cfg(dir: &Path, ytdlp_bin: PathBuf) -> AgentConfig {
        AgentConfig {
            download_dir: dir.join("downloads"),
            profile_dir: dir.join("profile"),
            chrome_bin: "/bin/false".into(),
            home_dir: dir.to_path_buf(),
            display_number: 1,
            screen_width: 1280,
            screen_height: 800,
            display_scale: 1.0,
            manage_display: false,
            headless: true,
            vnc_socket: dir.join("vnc.sock"),
            audio: false,
            audio_socket: dir.join("audio.sock"),
            pulseaudio_bin: "pulseaudio".into(),
            parec_bin: "parec".into(),
            video_dir: dir.join("video"),
            ytdlp_bin,
            min_free_disk_bytes: 0,
            safe_browsing: false,
            theme: crate::config::Theme::Dark,
            links_dir: "/code-links".into(),
            monitor_state_file: dir.join("monitor.json"),
            start_urls: vec![],
        }
    }

    #[tokio::test]
    async fn the_chosen_quality_reaches_yt_dlp() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let staging = d.path().join("staging");
        let args = d.path().join("args");
        let bin = d.path().join("yt-dlp");
        let script = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\nprintf x > '{}/v.mp4'\n",
            args.display(),
            staging.display()
        );
        std::fs::write(&bin, script).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let cfg = test_cfg(d.path(), bin);
        prepare(&cfg.video_dir).unwrap();
        for quality in [None, Some(360), Some(1080)] {
            let _ = std::fs::remove_dir_all(&staging);
            std::fs::create_dir_all(&staging).unwrap();
            let run = Run { cfg: &cfg, id: "0123456789ab0123456789ab", staging: &staging, quality };
            let (_stop, mut shutdown) = watch::channel(false);
            assert_eq!(run.download("https://a.example/v", &mut shutdown).await, Outcome::Done(staging.join("v.mp4")));
            let got = std::fs::read_to_string(&args).unwrap();
            let got: Vec<&str> = got.lines().collect();
            assert!(got.windows(2).any(|w| w == ["-f", "bv*+ba/b"]), "{got:?}");
            assert_eq!(got[got.len() - 2..], ["--", "https://a.example/v"]);
            match quality {
                Some(q) => assert!(got.windows(2).any(|w| w == ["-S", format!("res:{q}").as_str()]), "{got:?}"),
                None => assert!(!got.contains(&"-S"), "best quality has no resolution limit: {got:?}"),
            }
        }
    }

    #[tokio::test]
    async fn a_broken_yt_dlp_is_a_failure_not_an_unsupported_site() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let fake = |name: &str, body: &str| {
            let p = d.path().join(name);
            std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
            p
        };
        let cases = [
            (
                fake("unsupported", "echo 'ERROR: Unsupported URL: https://a.example/' >&2; exit 1"),
                Outcome::Unsupported,
            ),
            (
                fake(
                    "broken",
                    "echo 'yt-dlp: error while loading shared libraries: libz.so.1: failed to map segment' >&2; exit 127",
                ),
                Outcome::Failed,
            ),
            (d.path().join("missing"), Outcome::Failed),
            (
                fake("drm", "echo 'ERROR: [generic] Unable to download: This video is DRM protected' >&2; exit 1"),
                Outcome::Protected,
            ),
        ];
        for (bin, want) in cases {
            let cfg = test_cfg(d.path(), bin.clone());
            prepare(&cfg.video_dir).unwrap();
            let staging = d.path().join("staging");
            std::fs::create_dir_all(&staging).unwrap();
            let run = Run { cfg: &cfg, id: "0123456789ab0123456789ab", staging: &staging, quality: None };
            let (_stop, mut shutdown) = watch::channel(false);
            assert_eq!(run.download("https://a.example/", &mut shutdown).await, want, "{}", bin.display());
        }
    }

    #[test]
    fn protected_is_a_final_state_the_extension_understands() {
        assert!(VideoState::Protected.is_final());
        assert_eq!(serde_json::to_value(VideoState::Protected).unwrap(), "protected");
    }

    #[test]
    fn host_binary_name() {
        assert!(is_host_invocation("/usr/local/bin/chromine-video-host"));
        assert!(is_host_invocation("chromine-video-host"));
        assert!(!is_host_invocation("/usr/local/bin/chromine-agent"));
    }
}

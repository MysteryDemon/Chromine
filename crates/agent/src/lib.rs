pub mod audio;
pub mod chrome;
pub mod config;
pub mod display;
pub mod downloads;
pub mod history;
pub mod links;
pub mod native;
pub mod prefs;
pub mod video;

use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use chromine_shared::{AgentStatus, MonitorState, SCHEMA_VERSION, fsutil, now_ms};
use tokio::sync::watch;

use crate::chrome::{ChromeProcess, SandboxNote};
use crate::config::AgentConfig;

const MONITOR_GRACE_MS: i64 = 45_000;
const MONITOR_STALE_MS: i64 = 10_000;

pub const SHARED_DIR_MODE: u32 = 0o2770;

pub struct Shared {
    pub monitor_state_file: std::path::PathBuf,
    pub chrome_running: AtomicBool,
    pub chrome_started_at_ms: AtomicI64,
    pub chrome_starts: AtomicU32,
    pub chrome_version: std::sync::Mutex<Option<String>>,
    pub sandbox: std::sync::Mutex<(Option<bool>, Option<String>)>,
    pub sandbox_note: SandboxNote,
    pub last_error: std::sync::Mutex<Option<String>>,
    pub last_link: std::sync::Mutex<Option<chromine_shared::OpenedLink>>,
    pub started_at_ms: i64,
}

impl Shared {
    pub fn new(cfg: &AgentConfig) -> Arc<Shared> {
        Arc::new(Shared {
            monitor_state_file: cfg.monitor_state_file.clone(),
            chrome_running: AtomicBool::new(false),
            chrome_started_at_ms: AtomicI64::new(0),
            chrome_starts: AtomicU32::new(0),
            chrome_version: std::sync::Mutex::new(None),
            sandbox: std::sync::Mutex::new((None, None)),
            sandbox_note: Default::default(),
            last_error: std::sync::Mutex::new(None),
            last_link: std::sync::Mutex::new(None),
            started_at_ms: now_ms(),
        })
    }

    fn set_error(&self, e: impl ToString) {
        *self.last_error.lock().unwrap() = Some(e.to_string());
    }
}

pub fn prepare_directories(cfg: &AgentConfig) -> Result<()> {
    for dir in [cfg.incoming_dir(), cfg.events_dir(), cfg.completed_dir()] {
        if !dir.exists() {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(SHARED_DIR_MODE)
                .create(&dir)
                .with_context(|| format!("cannot create {}", dir.display()))?;
            let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(SHARED_DIR_MODE));
        }
        let probe = dir.join(format!(".write-test-{}", std::process::id()));
        std::fs::write(&probe, b"").with_context(|| {
            format!(
                "{} is not writable by uid {}; the download volume must be owned by the \
                 browser user and the shared group (see README, 'Storage')",
                dir.display(),
                unsafe { libc::getuid() }
            )
        })?;
        let _ = std::fs::remove_file(&probe);
    }
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&cfg.home_dir)?;
    if !cfg.profile_dir.exists() {
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&cfg.profile_dir)?;
    }
    let staging = cfg.download_dir.join("video");
    if !staging.exists() {
        std::fs::DirBuilder::new().recursive(true).mode(SHARED_DIR_MODE).create(&staging)?;
    }
    video::prepare(&cfg.video_dir)?;
    if cfg.manage_display
        && let Some(dir) = cfg.vnc_socket.parent()
        && !dir.exists()
    {
        std::fs::DirBuilder::new().recursive(true).mode(SHARED_DIR_MODE).create(dir)?;
    }
    Ok(())
}

pub async fn sandbox_self_test() -> (bool, String) {
    let out = tokio::process::Command::new("unshare")
        .args(["--user", "--pid", "--net", "--fork", "--", "true"])
        .stdin(std::process::Stdio::null())
        .output()
        .await;
    match out {
        Ok(o) if o.status.success() => (true, "user namespaces available; Chrome sandbox can be enabled".into()),
        Ok(o) => {
            let err = String::from_utf8_lossy(&o.stderr).trim().to_string();
            (
                false,
                format!(
                    "cannot create user namespaces ({err}). Chrome will refuse to start without its sandbox. \
                     Check that the browser service uses docker/browser/seccomp-chrome.json, that the host \
                     allows unprivileged user namespaces (kernel.unprivileged_userns_clone=1, \
                     user.max_user_namespaces>0) and, on Ubuntu 23.10+, the AppArmor userns restriction. \
                     See README 'Chrome sandbox'."
                ),
            )
        }
        Err(e) => (false, format!("could not run the sandbox self-test: {e}")),
    }
}

pub fn read_monitor_state(cfg: &AgentConfig) -> Option<MonitorState> {
    let bytes = std::fs::read(&cfg.monitor_state_file).ok()?;
    let state: MonitorState = serde_json::from_slice(&bytes).ok()?;
    (now_ms() - state.updated_at_ms < MONITOR_STALE_MS).then_some(state)
}

pub fn build_status(cfg: &AgentConfig, shared: &Shared) -> AgentStatus {
    let disk = fsutil::disk_space(&cfg.download_dir).unwrap_or_default();
    let (sandbox_ok, sandbox_detail) = shared.sandbox.lock().unwrap().clone();
    let chrome_running = shared.chrome_running.load(Ordering::SeqCst);
    let monitor = read_monitor_state(cfg);
    let monitor_detail = match (&monitor, chrome_running) {
        (None, true) if now_ms() - shared.chrome_started_at_ms.load(Ordering::SeqCst) > MONITOR_GRACE_MS => Some(
            "The \"Chromine download monitor\" extension is not connected, so new downloads are not detected. \
             Make sure it is turned on in chrome://extensions."
                .to_string(),
        ),
        _ => None,
    };
    let monitor = monitor.unwrap_or_default();
    let last_error = shared.last_error.lock().unwrap().clone().or(monitor.last_error.clone());
    AgentStatus {
        schema: SCHEMA_VERSION,
        updated_at_ms: now_ms(),
        agent_started_at_ms: shared.started_at_ms,
        chrome_running,
        chrome_version: shared.chrome_version.lock().unwrap().clone(),
        chrome_starts: shared.chrome_starts.load(Ordering::SeqCst),
        sandbox_ok,
        sandbox_detail,
        downloads_paused_low_disk: monitor.downloads_paused_low_disk,
        disk,
        min_free_disk_bytes: cfg.min_free_disk_bytes,
        active_downloads: monitor.active_downloads,
        recent_rejections: monitor.recent_rejections,
        last_error,
        monitor_connected: monitor.updated_at_ms > 0,
        monitor_detail,
        opened_link: shared.last_link.lock().unwrap().clone(),
    }
}

pub fn write_status(cfg: &AgentConfig, shared: &Shared) {
    let status = build_status(cfg, shared);
    match serde_json::to_vec_pretty(&status) {
        Ok(bytes) => {
            if let Err(e) = fsutil::atomic_write(&cfg.events_dir(), chromine_shared::AGENT_STATUS_FILE, &bytes, 0o640) {
                tracing::warn!(error = %e, "cannot write agent status");
            }
        }
        Err(e) => tracing::warn!(error = %e, "cannot encode agent status"),
    }
}

pub fn read_display_scale(path: &Path) -> Option<f64> {
    let text = std::fs::read_to_string(path).ok()?;
    let scale: f64 = text.trim().parse().ok()?;
    (1.0..=3.0).contains(&scale).then_some(scale)
}

pub async fn start_chrome(cfg: &AgentConfig, shared: &Shared, restore: bool) -> Result<ChromeProcess> {
    chrome::clear_stale_profile_lock(&cfg.profile_dir);
    prefs::apply(&cfg.profile_dir, &cfg.incoming_dir(), cfg.safe_browsing, cfg.theme)
        .context("cannot apply Chrome settings")?;
    if shared.chrome_version.lock().unwrap().is_none() {
        let v = chrome::version(&cfg.chrome_bin).await;
        *shared.chrome_version.lock().unwrap() = v;
    }
    let _ = std::fs::remove_file(&shared.monitor_state_file);
    let mut process = chrome::spawn(cfg, shared.sandbox_note.clone(), restore)?;
    if let Ok(Ok(status)) = tokio::time::timeout(Duration::from_secs(3), process.child.wait()).await {
        let note = shared.sandbox_note.lock().await.clone();
        bail!("Chrome exited at startup ({status}){}", note.map(|n| format!(": {n}")).unwrap_or_default());
    }
    tracing::info!(
        pid = process.pid,
        version = shared.chrome_version.lock().unwrap().as_deref().unwrap_or("?"),
        "Chrome started"
    );
    shared.chrome_running.store(true, Ordering::SeqCst);
    shared.chrome_started_at_ms.store(now_ms(), Ordering::SeqCst);
    shared.chrome_starts.fetch_add(1, Ordering::SeqCst);
    Ok(process)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    Shutdown,
    Crashed,
    Rescaled,
}

async fn close(process: &mut ChromeProcess) {
    process.request_exit();
    if tokio::time::timeout(Duration::from_secs(20), process.child.wait()).await.is_err() {
        tracing::warn!("Chrome did not exit in time; killing its process group");
        process.signal_group(libc::SIGKILL);
        let _ = process.child.wait().await;
    }
}

pub async fn drive(
    shared: Arc<Shared>,
    mut process: ChromeProcess,
    mut shutdown: watch::Receiver<bool>,
    mut scale: watch::Receiver<f64>,
) -> Exit {
    let exit = tokio::select! {
        status = process.child.wait() => {
            tracing::warn!(?status, "Chrome exited");
            Exit::Crashed
        }
        _ = shutdown.changed() => {
            tracing::info!("closing Chrome");
            close(&mut process).await;
            Exit::Shutdown
        }
        Ok(()) = scale.changed() => {
            tracing::info!(scale = *scale.borrow(), "display scale changed; restarting Chrome");
            close(&mut process).await;
            Exit::Rescaled
        }
    };
    process.signal_group(libc::SIGTERM);
    shared.chrome_running.store(false, Ordering::SeqCst);
    let _ = std::fs::remove_file(&shared.monitor_state_file);
    exit
}

fn reconcile_history(cfg: &AgentConfig, shared: &Shared) {
    match history::reconcile(&cfg.profile_dir, &cfg.incoming_dir(), &cfg.completed_dir()) {
        Ok(r) if r.recovered > 0 => {
            tracing::warn!(recovered = r.recovered, "recovered completed downloads from Chrome history")
        }
        Ok(_) => {}
        Err(e) => {
            tracing::warn!(error = %e, "Chrome history reconciliation failed");
            shared.set_error(format!("history reconciliation failed: {e}"));
        }
    }
}

pub async fn chrome_loop(
    mut cfg: AgentConfig,
    shared: Arc<Shared>,
    shutdown: watch::Receiver<bool>,
    mut scale: watch::Receiver<f64>,
) {
    let mut backoff = Duration::from_secs(1);
    let mut restore = false;
    loop {
        if *shutdown.borrow() {
            return;
        }
        reconcile_history(&cfg, &shared);
        cfg.display_scale = *scale.borrow_and_update();
        let started = Instant::now();
        match start_chrome(&cfg, &shared, std::mem::take(&mut restore)).await {
            Ok(process) => {
                if let Some(note) = shared.sandbox_note.lock().await.take() {
                    tracing::warn!(%note, "Chrome reported a sandbox problem");
                }
                match drive(shared.clone(), process, shutdown.clone(), scale.clone()).await {
                    Exit::Shutdown => return,
                    Exit::Rescaled => {
                        restore = true;
                        backoff = Duration::from_secs(1);
                        continue;
                    }
                    Exit::Crashed => {}
                }
            }
            Err(e) => {
                tracing::error!(error = %e, "Chrome start failed");
                shared.set_error(&e);
                if e.to_string().contains("sandbox") {
                    *shared.sandbox.lock().unwrap() = (Some(false), Some(e.to_string()));
                }
            }
        }
        if started.elapsed() > Duration::from_secs(60) {
            backoff = Duration::from_secs(1);
        }
        tracing::info!(delay = ?backoff, "restarting Chrome");
        let mut sd = shutdown.clone();
        tokio::select! {
            _ = tokio::time::sleep(backoff) => {}
            _ = sd.changed() => return,
        }
        backoff = (backoff * 2).min(Duration::from_secs(30));
    }
}

fn watch_display_scale(cfg: &AgentConfig, shutdown: watch::Receiver<bool>) -> watch::Receiver<f64> {
    let (tx, rx) = watch::channel(cfg.display_scale);
    if cfg.headless {
        return rx;
    }
    let file = cfg.vnc_socket.with_file_name(chromine_shared::DISPLAY_SCALE_FILE);
    let mut sd = shutdown;
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(2));
        loop {
            tokio::select! {
                _ = tick.tick() => {
                    if let Some(s) = read_display_scale(&file) {
                        tx.send_if_modified(|v| std::mem::replace(v, s) != s);
                    }
                }
                _ = sd.changed() => break,
            }
        }
    });
    rx
}

pub async fn run(mut cfg: AgentConfig) -> Result<()> {
    unsafe {
        libc::umask(0o007);
    }
    prepare_directories(&cfg)?;
    if let Some(s) = read_display_scale(&cfg.vnc_socket.with_file_name(chromine_shared::DISPLAY_SCALE_FILE)) {
        cfg.display_scale = s;
    }
    let shared = Shared::new(&cfg);

    let (ok, detail) = sandbox_self_test().await;
    if ok {
        tracing::info!("{detail}");
    } else {
        tracing::error!("{detail}");
    }
    *shared.sandbox.lock().unwrap() = (Some(ok), Some(detail));

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    {
        let tx = shutdown_tx.clone();
        tokio::spawn(async move {
            let mut term =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("SIGTERM handler");
            let mut int =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt()).expect("SIGINT handler");
            tokio::select! { _ = term.recv() => {}, _ = int.recv() => {} }
            tracing::info!("shutdown requested");
            let _ = tx.send(true);
        });
    }

    let mut display = if cfg.manage_display { Some(display::start(&cfg).await?) } else { None };
    let audio_task = if cfg.audio {
        match audio::start_pulse(&cfg).await {
            Ok(pulse) => Some(tokio::spawn(audio::run(cfg.clone(), pulse, shutdown_rx.clone()))),
            Err(e) => {
                tracing::error!(error = %e, "sound is unavailable");
                None
            }
        }
    } else {
        None
    };

    {
        let cfg = cfg.clone();
        let shared = shared.clone();
        let mut sd = shutdown_rx.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(3));
            loop {
                tokio::select! {
                    _ = tick.tick() => {
                        let (c, s) = (cfg.clone(), shared.clone());
                        let _ = tokio::task::spawn_blocking(move || write_status(&c, &s)).await;
                    }
                    _ = sd.changed() => break,
                }
            }
        });
    }

    let scale = watch_display_scale(&cfg, shutdown_rx.clone());
    let chrome_task = tokio::spawn(chrome_loop(cfg.clone(), shared.clone(), shutdown_rx.clone(), scale));
    let video_task = tokio::spawn(video::run_queue(cfg.clone(), shutdown_rx.clone()));
    if !cfg.headless {
        tokio::spawn(links::run(cfg.clone(), shared.clone(), shutdown_rx.clone()));
    }

    let mut failure: Option<String> = None;
    if let Some(stack) = display.as_mut() {
        let mut sd = shutdown_rx.clone();
        loop {
            tokio::select! {
                s = stack.xvnc.wait() => { failure = Some(format!("Xvnc exited ({s:?})")); break; }
                s = stack.openbox.wait() => {
                    tracing::warn!(status = ?s, "openbox exited; restarting");
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    stack.openbox = display::spawn_openbox(&cfg).await?;
                }
                _ = sd.changed() => break,
            }
        }
    } else {
        let mut sd = shutdown_rx.clone();
        let _ = sd.changed().await;
    }
    if let Some(f) = &failure {
        tracing::error!("{f}; stopping so the container restarts");
        shared.set_error(f);
    }
    let _ = shutdown_tx.send(true);
    let _ = tokio::time::timeout(Duration::from_secs(25), chrome_task).await;
    let _ = tokio::time::timeout(Duration::from_secs(5), video_task).await;
    if let Some(t) = audio_task {
        let _ = tokio::time::timeout(Duration::from_secs(5), t).await;
    }
    if let Some(mut stack) = display {
        display::stop_child(&mut stack.openbox, Duration::from_secs(3)).await;
        display::stop_child(&mut stack.xvnc, Duration::from_secs(3)).await;
    }
    write_status(&cfg, &shared);
    match failure {
        Some(f) => bail!(f),
        None => Ok(()),
    }
}

pub fn healthcheck(cfg: &AgentConfig) -> Result<()> {
    let path = cfg.events_dir().join(chromine_shared::AGENT_STATUS_FILE);
    let bytes = std::fs::read(&path).with_context(|| format!("no status file at {}", path.display()))?;
    let status: AgentStatus = serde_json::from_slice(&bytes)?;
    if now_ms() - status.updated_at_ms > 30_000 {
        bail!("agent heartbeat is stale");
    }
    if !status.chrome_running {
        bail!("Chrome is not running: {}", status.last_error.unwrap_or_default());
    }
    if cfg.manage_display && !cfg.vnc_socket.exists() {
        bail!("the VNC socket {} is missing", cfg.vnc_socket.display());
    }
    Ok(())
}

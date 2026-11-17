pub mod auth;
pub mod cleanup;
pub mod code;
pub mod config;
pub mod db;
pub mod filecloud;
pub mod ingest;
pub mod redact;
pub mod settings;
pub mod tls;
pub mod ui;
pub mod upload;
pub mod users;
pub mod web;
pub mod webrtc;

use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use chromine_shared::now_ms;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crate::cleanup::CleanupWorker;
use crate::config::Config;
use crate::db::Db;
use crate::filecloud::FileCloud;
use crate::ingest::Ingest;
use crate::upload::Uploader;

pub struct App {
    pub cfg: Arc<Config>,
    pub db: Db,
    pub ingest: Ingest,
    pub uploader: Uploader,
    pub cleanup: CleanupWorker,
    pub web: Arc<web::AppState>,
}

pub fn prepare_directories(cfg: &Config) -> Result<()> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&cfg.state_dir)
        .with_context(|| format!("cannot create STATE_DIR {}", cfg.state_dir.display()))?;
    for dir in [cfg.incoming_dir(), cfg.completed_dir()] {
        if !dir.exists() {
            std::fs::DirBuilder::new().recursive(true).mode(0o2770).create(&dir).with_context(|| {
                format!("cannot create {} (the download volume must be writable by the shared group)", dir.display())
            })?;
            let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o2770));
        }
    }
    if !cfg.jobs_dir().exists() {
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(cfg.jobs_dir())?;
    }
    for dir in [cfg.state_dir.clone(), cfg.jobs_dir(), cfg.incoming_dir(), cfg.completed_dir()] {
        let probe = dir.join(format!(".write-test-{}", std::process::id()));
        std::fs::write(&probe, b"").with_context(|| {
            format!("{} is not writable by this user; see README 'Storage and permissions'", dir.display())
        })?;
        let _ = std::fs::remove_file(&probe);
    }
    Ok(())
}

pub fn lock_state_dir(cfg: &Config) -> Result<std::fs::File> {
    let f =
        std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(cfg.state_dir.join("worker.lock"))?;
    match rustix::fs::flock(&f, rustix::fs::FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => Ok(f),
        Err(_) => bail!("another worker is already using {}", cfg.state_dir.display()),
    }
}

impl App {
    pub async fn build(cfg: Config, delay_scale: f64) -> Result<App> {
        let cfg = Arc::new(cfg);
        let db = Db::open(&cfg.db_path()).await?;
        let up = db.recover_after_restart(now_ms()).await?;
        if up > 0 {
            tracing::info!(uploads = up, "re-queued uploads interrupted by the last shutdown");
        }
        db.purge_sessions(now_ms()).await?;
        let (password_hash, password_changed) = auth::stored_password_hash(&cfg.state_dir, cfg.auth_password.expose())?;
        if password_changed {
            let n = db.delete_sessions_of("").await?;
            if n > 0 {
                tracing::warn!(sessions = n, "the dashboard password changed; every session was signed out");
            }
        }

        let upload_wake = Arc::new(Notify::new());
        let cleanup_wake = Arc::new(Notify::new());
        let settings = Arc::new(settings::Registry::open(cfg.clone()));

        let fc = FileCloud::new(cfg.filecloud_base_url.clone(), cfg.upload_request_timeout)?;
        let uploader = Uploader {
            db: db.clone(),
            fc,
            max_attempts: cfg.max_retry_attempts,
            settings: settings.clone(),
            wake: upload_wake.clone(),
            cleanup_wake: cleanup_wake.clone(),
            live: Default::default(),
            service_config: Default::default(),
            delay_scale,
        };
        let ingest = Ingest {
            db: db.clone(),
            owner: String::new(),
            incoming: cfg.incoming_dir(),
            completed: cfg.completed_dir(),
            jobs_dir: cfg.jobs_dir(),
            upload_wake: upload_wake.clone(),
            settings: settings.of("").subscribe(),
        };
        let cleanup = CleanupWorker {
            db: db.clone(),
            jobs_dir: cfg.jobs_dir(),
            max_attempts: cfg.max_retry_attempts,
            wake: cleanup_wake.clone(),
            delay_scale,
        };
        let ui = Arc::new(ui::UiAssets::load(&cfg.ui_dir)?.allow_frames(web::code_frame_source(&cfg)));
        let novnc = match ui::UiAssets::load_dir(&cfg.novnc_dir, &["vnc.html"]) {
            Ok(files) => Some(Arc::new(files)),
            Err(e) => {
                tracing::warn!(error = %e, "noVNC not found; the Browser tab cannot show the remote browser");
                None
            }
        };
        let webrtc = match webrtc::Hub::start(webrtc::Settings {
            vnc_socket: cfg.vnc_socket.clone(),
            audio_socket: cfg.audio_socket.clone(),
            ffmpeg: cfg.ffmpeg_bin.clone(),
            port: cfg.webrtc_port,
            live: settings.of("").subscribe(),
        })
        .await
        {
            Ok(hub) => Some(hub),
            Err(e) => {
                tracing::error!(error = format!("{e:#}"), "WebRTC streaming is unavailable");
                None
            }
        };
        let web = Arc::new(web::AppState {
            ui,
            novnc,
            webrtc,
            settings: settings.clone(),
            cfg: cfg.clone(),
            db: db.clone(),
            spaces: users::Spaces::new(cfg.clone(), ingest.clone(), settings.clone()),
            uploader: uploader.clone(),
            password_hash: config::Secret::new(password_hash),
            upload_wake,
            cleanup_wake,
            limiter: auth::LoginLimiter::new(10, Duration::from_secs(15 * 60)),
            login_failure_delay: Duration::from_millis((750.0 * delay_scale.min(1.0)) as u64),
            tls: match &cfg.https_host {
                Some(host) => Some(tls::Tls::new(host, &cfg.state_dir, &cfg.acme_directory)?),
                None => None,
            },
        });
        Ok(App { cfg, db, ingest, uploader, cleanup, web })
    }

    pub fn spawn_workers(&self, shutdown: &CancellationToken) -> Vec<tokio::task::JoinHandle<()>> {
        let mut handles = Vec::new();
        let ingest = self.ingest.clone();
        let sd = shutdown.clone();
        handles.push(tokio::spawn(async move {
            if let Err(e) = ingest.run(sd).await {
                tracing::error!(error = %e, "download ingestion stopped");
            }
        }));
        handles.push(tokio::spawn(self.uploader.clone().run(shutdown.clone())));
        handles.push(tokio::spawn(self.cleanup.clone().run(shutdown.clone())));
        let web = self.web.clone();
        let sd = shutdown.clone();
        handles.push(tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(30));
            loop {
                tokio::select! {
                    _ = tick.tick() => web.spaces.sync_logged(&web.db).await,
                    _ = sd.cancelled() => {
                        web.spaces.shutdown();
                        return;
                    }
                }
            }
        }));
        let db = self.db.clone();
        let sd = shutdown.clone();
        handles.push(tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(3600)) => { let _ = db.purge_sessions(now_ms()).await; }
                    _ = sd.cancelled() => return,
                }
            }
        }));
        handles
    }
}

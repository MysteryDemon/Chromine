use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use chromine_shared::{fsutil, now_ms};
use serde::Serialize;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crate::db::{Db, Job, RemoteSession};
use crate::filecloud::{ApiError, ErrorKind, FileCloud, ServiceConfig, chunk_range};
use crate::ingest::clean_display_name;
use crate::redact::sanitize;

const IN_ATTEMPT_RETRIES: u32 = 4;
const BUSY_BUDGET: Duration = Duration::from_secs(30 * 60);
const EXPIRY_MARGIN_MS: i64 = 10 * 60 * 1000;
const CONFIG_TTL: Duration = Duration::from_secs(600);

#[derive(Debug, Clone, Serialize, Default)]
pub struct LiveProgress {
    pub job_id: String,
    pub bytes: u64,
    pub total: u64,
}

pub type LiveSlot = Arc<std::sync::Mutex<Option<(String, u64, u64, Arc<AtomicU64>)>>>;

#[derive(Clone)]
pub struct Uploader {
    pub db: Db,
    pub fc: FileCloud,
    pub max_attempts: u32,
    pub settings: tokio::sync::watch::Receiver<crate::settings::Live>,
    pub wake: Arc<Notify>,
    pub cleanup_wake: Arc<Notify>,
    pub live: LiveSlot,
    pub service_config: Arc<std::sync::Mutex<Option<(Instant, ServiceConfig)>>>,
    pub delay_scale: f64,
}

#[derive(Debug)]
pub enum Failure {
    Transient(String, Option<Duration>),
    RateLimited(String, Duration),
    SessionGone(String),
    Permanent(String),
    FileGone(String),
    Shutdown,
}

pub fn backoff(n: u32, base: Duration, cap: Duration) -> Duration {
    let exp = base.saturating_mul(1u32.checked_shl(n.saturating_sub(1).min(20)).unwrap_or(u32::MAX));
    let d = exp.min(cap);
    let factor: f64 = rand::random_range(0.5..=1.0);
    d.mul_f64(factor)
}

pub fn human_bytes(b: u64) -> String {
    const U: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = b as f64;
    let mut i = 0;
    while v >= 1024.0 && i < 4 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 { format!("{b} B") } else { format!("{v:.1} {}", U[i]) }
}

impl Uploader {
    pub fn live_progress(&self) -> Option<LiveProgress> {
        let g = self.live.lock().unwrap();
        g.as_ref().map(|(id, base, total, inflight)| LiveProgress {
            job_id: id.clone(),
            bytes: (*base + inflight.load(Ordering::Relaxed)).min(*total),
            total: *total,
        })
    }

    pub fn cached_service_config(&self) -> Option<ServiceConfig> {
        self.service_config.lock().unwrap().as_ref().map(|(_, c)| c.clone())
    }

    fn scaled(&self, d: Duration) -> Duration {
        d.mul_f64(self.delay_scale)
    }

    async fn sleep(&self, d: Duration, shutdown: &CancellationToken) -> Result<(), Failure> {
        tokio::select! {
            _ = tokio::time::sleep(self.scaled(d)) => Ok(()),
            _ = shutdown.cancelled() => Err(Failure::Shutdown),
        }
    }

    async fn service_config(&self) -> Result<ServiceConfig, ApiError> {
        if let Some((at, c)) = self.service_config.lock().unwrap().as_ref()
            && at.elapsed() < CONFIG_TTL
        {
            return Ok(c.clone());
        }
        let c = self.fc.service_config().await?;
        *self.service_config.lock().unwrap() = Some((Instant::now(), c.clone()));
        Ok(c)
    }

    async fn call<T, F, Fut>(&self, what: &str, shutdown: &CancellationToken, mut f: F) -> Result<T, Failure>
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = Result<T, ApiError>>,
    {
        let mut failures = 0u32;
        let mut busy_waited = Duration::ZERO;
        loop {
            let res = tokio::select! {
                r = f() => r,
                _ = shutdown.cancelled() => return Err(Failure::Shutdown),
            };
            let e = match res {
                Ok(v) => return Ok(v),
                Err(e) => e,
            };
            match e.kind {
                ErrorKind::Busy => {
                    let base = e.retry_after.unwrap_or(Duration::from_secs(5));
                    let wait = base + base.mul_f64(rand::random_range(0.0..1.0));
                    busy_waited += wait;
                    if busy_waited > BUSY_BUDGET {
                        return Err(Failure::Transient(
                            format!("{what}: FileCloud24 stayed busy (another upload from this address is running)"),
                            Some(Duration::from_secs(60)),
                        ));
                    }
                    tracing::info!(%what, wait = ?wait, "FileCloud24 busy; waiting for a free upload slot");
                    self.sleep(wait, shutdown).await?;
                }
                ErrorKind::RateLimited => {
                    let wait = e.retry_after.unwrap_or(Duration::from_secs(15 * 60));
                    return Err(Failure::RateLimited(
                        format!(
                            "Hourly upload allowance reached ({}); retrying in {} min",
                            e.detail,
                            wait.as_secs().div_ceil(60)
                        ),
                        wait,
                    ));
                }
                ErrorKind::Server | ErrorKind::Network => {
                    failures += 1;
                    if failures > IN_ATTEMPT_RETRIES {
                        return Err(Failure::Transient(format!("{what}: {e}"), e.retry_after));
                    }
                    let mut wait = backoff(failures, Duration::from_secs(2), Duration::from_secs(60));
                    if let Some(ra) = e.retry_after {
                        wait = wait.max(ra);
                    }
                    tracing::warn!(%what, error = %e, attempt = failures, wait = ?wait, "transient FileCloud24 error; retrying");
                    self.sleep(wait, shutdown).await?;
                }
                ErrorKind::SessionGone => return Err(Failure::SessionGone(format!("{what}: {e}"))),
                ErrorKind::TooLarge => {
                    return Err(Failure::Permanent(format!(
                        "{what}: file is larger than FileCloud24 accepts ({})",
                        e.detail
                    )));
                }
                ErrorKind::LocalIo => return Err(Failure::FileGone(format!("{what}: {e}"))),
                ErrorKind::Rejected | ErrorKind::InvalidResponse => {
                    return Err(Failure::Permanent(format!("{what}: {e}")));
                }
            }
        }
    }

    async fn upload(&self, job: &Job, shutdown: &CancellationToken) -> Result<String, Failure> {
        let path = PathBuf::from(&job.staged_path);
        let size = job.size_bytes as u64;

        let check = {
            let path = path.clone();
            tokio::task::spawn_blocking(move || fsutil::open_regular_nofollow(&path).map(|(_, m)| m)).await
        };
        let meta = match check {
            Ok(Ok(m)) => m,
            Ok(Err(e)) => return Err(Failure::FileGone(format!("The claimed file is not available: {e}"))),
            Err(e) => return Err(Failure::Transient(format!("internal error: {e}"), None)),
        };
        let (dev, ino) = fsutil::identity(&meta);
        if dev as i64 != job.file_dev || ino as i64 != job.file_ino || meta.len() != size {
            return Err(Failure::FileGone("The claimed file changed after the download completed.".into()));
        }

        let cfg = self.call("reading service limits", shutdown, || self.service_config()).await?;
        if !cfg.ready {
            return Err(Failure::Transient(
                "FileCloud24 reports that it is not ready".into(),
                Some(Duration::from_secs(120)),
            ));
        }
        if cfg.max_upload_size > 0 && size > cfg.max_upload_size {
            return Err(Failure::Permanent(format!(
                "File is {} but FileCloud24 currently accepts at most {} per file. Split the file or keep it locally.",
                human_bytes(size),
                human_bytes(cfg.max_upload_size)
            )));
        }
        if cfg.hourly_upload_allowance > 0 && size > cfg.hourly_upload_allowance {
            return Err(Failure::Permanent(format!(
                "File is {} but the hourly upload allowance is {}; it can never be uploaded in one piece.",
                human_bytes(size),
                human_bytes(cfg.hourly_upload_allowance)
            )));
        }

        let mut session = job.remote_session();
        let mut chunks_done = job.remote_chunks_done.max(0) as u64;
        let mut finalize_attempted = job.finalize_attempted != 0;
        if let Some(s) = &session {
            let now = now_ms();
            let remaining_parts = chunks_done < s.chunk_count;
            if now >= s.expires_at_ms || (remaining_parts && s.expires_at_ms - now < EXPIRY_MARGIN_MS) {
                tracing::info!(job = %job.id, "upload session expired or about to expire; starting a new one");
                let _ = self.fc.abort(s).await;
                self.db.clear_remote_session(&job.id, now_ms()).await.map_err(db_err)?;
                session = None;
                chunks_done = 0;
                finalize_attempted = false;
            } else {
                tracing::info!(job = %job.id, chunks_done, chunk_count = s.chunk_count, "resuming upload session");
            }
        }
        let session = match session {
            Some(s) => s,
            None => {
                let name = clean_display_name(&job.original_filename);
                let ctype = mime_guess::from_path(&name).first_or_octet_stream().essence_str().to_string();
                let s = self.call("starting upload", shutdown, || self.fc.start(&name, size, &ctype)).await?;
                self.db.save_remote_session(&job.id, &s, now_ms()).await.map_err(db_err)?;
                tracing::info!(job = %job.id, chunk_size = s.chunk_size, chunk_count = s.chunk_count, "upload session created");
                chunks_done = 0;
                finalize_attempted = false;
                s
            }
        };

        let inflight = Arc::new(AtomicU64::new(0));
        while chunks_done < session.chunk_count {
            let idx = chunks_done;
            let (start, len) = chunk_range(size, session.chunk_size, idx);
            *self.live.lock().unwrap() = Some((job.id.clone(), start, size, inflight.clone()));
            let what = format!("part {} of {}", idx + 1, session.chunk_count);
            self.call(&what, shutdown, || {
                inflight.store(0, Ordering::Relaxed);
                self.fc.put_chunk(&session, &path, size, idx, inflight.clone())
            })
            .await?;
            chunks_done += 1;
            self.db.set_chunks_done(&job.id, chunks_done, start + len, now_ms()).await.map_err(db_err)?;
        }
        *self.live.lock().unwrap() = Some((job.id.clone(), size, size, Arc::new(AtomicU64::new(0))));

        self.db.set_finalizing(&job.id, now_ms()).await.map_err(db_err)?;
        match self.call("finalizing", shutdown, || self.fc.complete(&session)).await {
            Ok(link) => Ok(link),
            Err(Failure::SessionGone(m)) => {
                let note = if finalize_attempted {
                    "A previous finalize request was interrupted"
                } else {
                    "If a finalize request was interrupted"
                };
                Err(Failure::SessionGone(format!(
                    "{m}. {note}, FileCloud24 may already hold a copy (its link was never received); \
                     the file will be uploaded again."
                )))
            }
            Err(e) => Err(e),
        }
    }

    pub async fn process(&self, job: Job, shutdown: &CancellationToken) {
        let result = self.upload(&job, shutdown).await;
        *self.live.lock().unwrap() = None;
        let now = now_ms();
        let id = job.id.as_str();
        let max = self.max_attempts as i64;
        let outcome: anyhow::Result<()> = async {
            match result {
                Ok(link) => {
                    let delete = !self.settings.borrow().keep_local;
                    self.db.mark_uploaded(id, &link, delete, now).await?;
                    tracing::info!(job = %id, %link, "upload finalized; link saved");
                    self.cleanup_wake.notify_one();
                }
                Err(Failure::Shutdown) => {
                    self.db.release_upload(id, now).await?;
                }
                Err(Failure::RateLimited(msg, wait)) => {
                    tracing::warn!(job = %id, %msg, "rate limited");
                    self.db
                        .upload_retry_later(id, false, now + self.scaled(wait).as_millis() as i64, &sanitize(&msg), now)
                        .await?;
                }
                Err(Failure::Transient(msg, retry_after)) => {
                    let attempts = job.upload_attempts + 1;
                    if attempts >= max {
                        let msg = format!("{msg} (gave up after {attempts} attempts; use Retry to try again)");
                        tracing::error!(job = %id, %msg, "upload failed");
                        self.db.upload_failed(id, &sanitize(&msg), true, now).await?;
                    } else {
                        let mut wait = backoff(attempts as u32, Duration::from_secs(30), Duration::from_secs(30 * 60));
                        if let Some(ra) = retry_after {
                            wait = wait.max(ra);
                        }
                        tracing::warn!(job = %id, %msg, attempts, wait = ?wait, "upload attempt failed; will retry");
                        self.db
                            .upload_retry_later(
                                id,
                                true,
                                now + self.scaled(wait).as_millis() as i64,
                                &sanitize(&msg),
                                now,
                            )
                            .await?;
                    }
                }
                Err(Failure::SessionGone(msg)) => {
                    self.db.clear_remote_session(id, now).await?;
                    let attempts = job.upload_attempts + 1;
                    if attempts >= max {
                        self.db.upload_failed(id, &sanitize(&msg), true, now).await?;
                    } else {
                        tracing::warn!(job = %id, %msg, "upload session lost; a new session will be created");
                        self.db
                            .upload_retry_later(
                                id,
                                true,
                                now + self.scaled(Duration::from_secs(5)).as_millis() as i64,
                                &sanitize(&msg),
                                now,
                            )
                            .await?;
                    }
                }
                Err(Failure::Permanent(msg)) => {
                    self.abandon_session(id).await;
                    tracing::error!(job = %id, %msg, "upload failed permanently; file kept");
                    self.db.upload_failed(id, &sanitize(&msg), true, now).await?;
                }
                Err(Failure::FileGone(msg)) => {
                    self.abandon_session(id).await;
                    tracing::error!(job = %id, %msg, "claimed file unavailable");
                    self.db.mark_file_missing(id, &sanitize(&msg), now).await?;
                }
            }
            Ok(())
        }
        .await;
        if let Err(e) = outcome {
            tracing::error!(job = %id, error = %e, "could not record upload outcome");
        }
    }

    async fn abandon_session(&self, id: &str) {
        if let Ok(Some(job)) = self.db.get(id).await
            && let Some(s) = job.remote_session()
            && let Err(e) = self.fc.abort(&s).await
        {
            tracing::warn!(job = %id, error = %e, "could not abort remote upload session");
        }
        let _ = self.db.clear_remote_session(id, now_ms()).await;
    }

    pub async fn abort_session(&self, s: &RemoteSession) {
        if let Err(e) = self.fc.abort(s).await {
            tracing::warn!(error = %e, "could not abort remote upload session");
        }
    }

    pub async fn run(self, shutdown: CancellationToken) {
        loop {
            if shutdown.is_cancelled() {
                return;
            }
            let next = match self.db.next_upload(now_ms()).await {
                Ok(n) => n,
                Err(e) => {
                    tracing::error!(error = %e, "upload queue query failed");
                    None
                }
            };
            if let Some(job) = next {
                match self.db.claim_upload(&job.id, now_ms()).await {
                    Ok(true) => {
                        let job = self.db.get(&job.id).await.ok().flatten().unwrap_or(job);
                        self.process(job, &shutdown).await;
                    }
                    Ok(false) => {}
                    Err(e) => tracing::error!(error = %e, "cannot claim upload"),
                }
                continue;
            }
            let wait = match self.db.next_upload_due().await {
                Ok(Some(t)) => Duration::from_millis((t - now_ms()).clamp(50, 60_000) as u64),
                _ => Duration::from_secs(60),
            };
            tokio::select! {
                _ = self.wake.notified() => {}
                _ = tokio::time::sleep(wait) => {}
                _ = shutdown.cancelled() => return,
            }
        }
    }
}

fn db_err(e: anyhow::Error) -> Failure {
    Failure::Transient(format!("database error: {e}"), None)
}

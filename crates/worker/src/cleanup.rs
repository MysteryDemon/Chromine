use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use chromine_shared::now_ms;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crate::db::{Db, Job, state};
use crate::ingest::STAGED_NAME;
use crate::redact::sanitize;
use crate::upload::backoff;

#[derive(Clone)]
pub struct CleanupWorker {
    pub db: Db,
    pub jobs_dir: PathBuf,
    pub max_attempts: u32,
    pub wake: Arc<Notify>,
    pub delay_scale: f64,
}

pub fn job_file_path(jobs_dir: &Path, job: &Job) -> Option<PathBuf> {
    let expected = jobs_dir.join(&job.id).join(STAGED_NAME);
    (Path::new(&job.staged_path) == expected && uuid::Uuid::parse_str(&job.id).is_ok()).then_some(expected)
}

fn delete_job_file(jobs_dir: &Path, job: &Job) -> std::io::Result<()> {
    let path = job_file_path(jobs_dir, job)
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "unexpected staged path"))?;
    match std::fs::remove_file(&path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    match std::fs::remove_dir(path.parent().unwrap()) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

impl CleanupWorker {
    pub async fn process(&self, job: Job) {
        let now = now_ms();
        if !(job.upload_state == state::UP_DONE && job.cloud_link.is_some()) {
            tracing::error!(job = %job.id, "cleanup requested before success conditions were met; skipping");
            let _ = self.db.cleanup_failed(&job.id, "internal: success conditions not met", now).await;
            return;
        }
        let jobs_dir = self.jobs_dir.clone();
        let j = job.clone();
        let res = tokio::task::spawn_blocking(move || delete_job_file(&jobs_dir, &j)).await;
        let r: anyhow::Result<()> = async {
            match res {
                Ok(Ok(())) => {
                    self.db.mark_cleaned(&job.id, now).await?;
                    tracing::info!(job = %job.id, "local file deleted after successful processing");
                }
                Ok(Err(e)) => self.retry(&job, &format!("could not delete the local file: {e}")).await?,
                Err(e) => self.retry(&job, &format!("cleanup task failed: {e}")).await?,
            }
            Ok(())
        }
        .await;
        if let Err(e) = r {
            tracing::error!(job = %job.id, error = %e, "could not record cleanup outcome");
        }
    }

    async fn retry(&self, job: &Job, msg: &str) -> anyhow::Result<()> {
        let now = now_ms();
        let msg = sanitize(msg);
        let attempts = job.cleanup_attempts + 1;
        if attempts >= self.max_attempts as i64 {
            self.db.cleanup_failed(&job.id, &format!("{msg} (use Retry after fixing permissions)"), now).await
        } else {
            let wait = backoff(attempts as u32, Duration::from_secs(30), Duration::from_secs(3600));
            tracing::warn!(job = %job.id, error = %msg, "cleanup failed; will retry (no re-upload)");
            self.db
                .cleanup_retry_later(&job.id, now + wait.mul_f64(self.delay_scale).as_millis() as i64, &msg, now)
                .await
                .map(|_| ())
        }
    }

    pub async fn run(self, shutdown: CancellationToken) {
        loop {
            if shutdown.is_cancelled() {
                return;
            }
            match self.db.next_cleanup(now_ms()).await {
                Ok(Some(job)) => {
                    self.process(job).await;
                    continue;
                }
                Ok(None) => {}
                Err(e) => tracing::error!(error = %e, "cleanup queue query failed"),
            }
            let wait = match self.db.next_cleanup_due().await {
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

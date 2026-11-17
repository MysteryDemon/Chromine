use std::path::Path;
use std::str::FromStr;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::Serialize;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::{FromRow, SqlitePool};

pub mod state {
    pub const FILE_CLAIMING: &str = "claiming";
    pub const FILE_STAGED: &str = "staged";

    pub const UP_PENDING: &str = "pending";
    pub const UP_RETRY: &str = "retry_wait";
    pub const UP_DONE: &str = "uploaded";
    pub const UP_FAILED: &str = "failed";
    pub const UP_CANCELLED: &str = "cancelled";
    pub const UP_SKIPPED: &str = "skipped";

    pub const CL_WAITING: &str = "waiting";
    pub const CL_PENDING: &str = "pending";
    pub const CL_RETRY: &str = "retry_wait";
    pub const CL_RETAINED: &str = "retained";
    pub const CL_FAILED: &str = "failed";
}
use state::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct JobCounts {
    pub total: i64,
    pub uploaded: i64,
    pub active: i64,
    pub attention: i64,
    pub clearable: i64,
}

#[derive(Debug, Clone, FromRow, Serialize)]
pub struct Job {
    pub id: String,
    #[serde(skip)]
    pub owner: String,
    pub download_guid: String,
    pub source: String,
    pub source_url: Option<String>,
    pub original_filename: String,
    pub chrome_filename: String,
    pub size_bytes: i64,
    #[serde(skip)]
    pub file_dev: i64,
    #[serde(skip)]
    pub file_ino: i64,
    #[serde(skip)]
    pub incoming_path: String,
    #[serde(skip)]
    pub staged_path: String,
    pub download_completed_at: i64,
    pub created_at: i64,
    pub updated_at: i64,
    pub file_state: String,
    pub upload_state: String,
    pub upload_attempts: i64,
    pub upload_next_at: Option<i64>,
    pub upload_error: Option<String>,
    #[serde(skip)]
    pub remote_upload_id: Option<String>,
    #[serde(skip)]
    pub remote_upload_token: Option<String>,
    #[serde(skip)]
    pub remote_file_id: Option<String>,
    #[serde(skip)]
    pub remote_chunk_size: Option<i64>,
    #[serde(skip)]
    pub remote_chunk_count: Option<i64>,
    #[serde(skip)]
    pub remote_expires_at: Option<i64>,
    pub remote_chunks_done: i64,
    pub uploaded_bytes: i64,
    #[serde(skip)]
    pub finalize_attempted: i64,
    pub cloud_link: Option<String>,
    pub uploaded_at: Option<i64>,
    pub cleanup_state: String,
    pub cleanup_attempts: i64,
    pub cleanup_next_at: Option<i64>,
    pub cleanup_error: Option<String>,
    pub cleaned_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteSession {
    pub upload_id: String,
    pub token: String,
    pub file_id: Option<String>,
    pub chunk_size: u64,
    pub chunk_count: u64,
    pub expires_at_ms: i64,
}

impl Job {
    pub fn remote_session(&self) -> Option<RemoteSession> {
        Some(RemoteSession {
            upload_id: self.remote_upload_id.clone()?,
            token: self.remote_upload_token.clone()?,
            file_id: self.remote_file_id.clone(),
            chunk_size: u64::try_from(self.remote_chunk_size?).ok()?,
            chunk_count: u64::try_from(self.remote_chunk_count?).ok()?,
            expires_at_ms: self.remote_expires_at?,
        })
    }
}

pub struct NewJob {
    pub id: String,
    pub owner: String,
    pub download_guid: String,
    pub source: String,
    pub source_url: Option<String>,
    pub original_filename: String,
    pub chrome_filename: String,
    pub size_bytes: i64,
    pub file_dev: i64,
    pub file_ino: i64,
    pub incoming_path: String,
    pub staged_path: String,
    pub download_completed_at: i64,
}

#[derive(Debug, PartialEq, Eq)]
pub enum RetryOutcome {
    Upload,
    Cleanup,
    NothingToRetry,
    NotFound,
}

#[derive(Clone)]
pub struct Db {
    pub pool: SqlitePool,
}

#[derive(Debug, Clone, FromRow)]
pub struct SessionRow {
    pub username: String,
    pub owner: String,
    pub csrf_token: String,
    pub expires_at: i64,
    pub last_seen_at: i64,
}

pub struct NewSession<'a> {
    pub id_hash: &'a str,
    pub public_id: &'a str,
    pub username: &'a str,
    pub owner: &'a str,
    pub csrf: &'a str,
    pub ip: &'a str,
    pub user_agent: Option<&'a str>,
}

#[derive(Debug, Clone, FromRow)]
pub struct UserRow {
    pub id: String,
    pub username: String,
    pub password_hash: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, FromRow, Serialize)]
pub struct UserInfo {
    pub id: String,
    pub username: String,
    pub created_at: i64,
    pub sessions: i64,
    pub last_seen_at: Option<i64>,
}

#[derive(Debug, Clone, FromRow)]
pub struct SessionInfo {
    pub id_hash: String,
    pub public_id: String,
    pub ip: Option<String>,
    pub user_agent: Option<String>,
    pub created_at: i64,
    pub last_seen_at: i64,
}

impl Db {
    pub async fn open(path: &Path) -> Result<Db> {
        let opts = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))?
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Full)
            .foreign_keys(true)
            .busy_timeout(Duration::from_secs(15));
        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(opts)
            .await
            .with_context(|| format!("cannot open database {}", path.display()))?;
        sqlx::migrate!("./migrations").run(&pool).await.context("database migration failed")?;
        Ok(Db { pool })
    }

    pub async fn insert_claiming(&self, j: &NewJob, now: i64) -> Result<bool> {
        let r = sqlx::query(
            "INSERT INTO jobs (id, owner, download_guid, source, source_url, original_filename, chrome_filename, size_bytes,
                file_dev, file_ino, incoming_path, staged_path, download_completed_at, created_at, updated_at,
                file_state, upload_state, cleanup_state)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'claiming', 'waiting', 'waiting')
             ON CONFLICT(download_guid) DO NOTHING",
        )
        .bind(&j.id)
        .bind(&j.owner)
        .bind(&j.download_guid)
        .bind(&j.source)
        .bind(&j.source_url)
        .bind(&j.original_filename)
        .bind(&j.chrome_filename)
        .bind(j.size_bytes)
        .bind(j.file_dev)
        .bind(j.file_ino)
        .bind(&j.incoming_path)
        .bind(&j.staged_path)
        .bind(j.download_completed_at)
        .bind(now)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected() == 1)
    }

    pub async fn get(&self, id: &str) -> Result<Option<Job>> {
        Ok(sqlx::query_as::<_, Job>("SELECT * FROM jobs WHERE id = ?").bind(id).fetch_optional(&self.pool).await?)
    }

    pub async fn get_by_guid(&self, guid: &str) -> Result<Option<Job>> {
        Ok(sqlx::query_as::<_, Job>("SELECT * FROM jobs WHERE download_guid = ?")
            .bind(guid)
            .fetch_optional(&self.pool)
            .await?)
    }

    pub async fn list(&self, limit: i64) -> Result<Vec<Job>> {
        Ok(sqlx::query_as::<_, Job>("SELECT * FROM jobs ORDER BY created_at DESC LIMIT ?")
            .bind(limit)
            .fetch_all(&self.pool)
            .await?)
    }

    pub async fn list_page(&self, owner: &str, limit: i64, offset: i64) -> Result<Vec<Job>> {
        Ok(sqlx::query_as::<_, Job>(
            "SELECT * FROM jobs WHERE owner = ? ORDER BY created_at DESC, id DESC LIMIT ? OFFSET ?",
        )
        .bind(owner)
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn counts(&self, owner: &str) -> Result<JobCounts> {
        let (total, uploaded, active, attention, clearable) = sqlx::query_as::<_, (i64, i64, i64, i64, i64)>(
            "SELECT COUNT(*), \
             COALESCE(SUM(upload_state = 'uploaded'), 0), \
             COALESCE(SUM(upload_state IN ('pending', 'uploading', 'finalizing', 'retry_wait')), 0), \
             COALESCE(SUM(upload_state = 'failed' OR cleanup_state = 'failed' OR file_state = 'missing'), 0), \
             COALESCE(SUM(upload_state = 'uploaded' AND file_state IN ('deleted', 'missing')), 0) \
             FROM jobs WHERE owner = ?",
        )
        .bind(owner)
        .fetch_one(&self.pool)
        .await?;
        Ok(JobCounts { total, uploaded, active, attention, clearable })
    }

    pub async fn clear_finished(&self, owner: &str) -> Result<u64> {
        Ok(sqlx::query(
            "DELETE FROM jobs WHERE owner = ? AND upload_state = 'uploaded' AND file_state IN ('deleted', 'missing')",
        )
        .bind(owner)
        .execute(&self.pool)
        .await?
        .rows_affected())
    }

    pub async fn delete_jobs_of(&self, owner: &str) -> Result<Vec<Job>> {
        Ok(sqlx::query_as::<_, Job>("DELETE FROM jobs WHERE owner = ? RETURNING *")
            .bind(owner)
            .fetch_all(&self.pool)
            .await?)
    }

    pub async fn claiming_jobs(&self, owner: &str) -> Result<Vec<Job>> {
        Ok(sqlx::query_as::<_, Job>("SELECT * FROM jobs WHERE owner = ? AND file_state = 'claiming'")
            .bind(owner)
            .fetch_all(&self.pool)
            .await?)
    }

    pub async fn mark_staged(&self, id: &str, upload: bool, now: i64) -> Result<()> {
        self.set_plan(id, upload, true, now).await?;
        Ok(())
    }

    pub async fn start_skipped_upload(&self, id: &str, now: i64) -> Result<bool> {
        self.set_plan(id, true, false, now).await
    }

    async fn set_plan(&self, id: &str, upload: bool, staging: bool, now: i64) -> Result<bool> {
        let (upload, cleanup) = if upload { (UP_PENDING, CL_WAITING) } else { (UP_SKIPPED, CL_RETAINED) };
        let r = sqlx::query(
            "UPDATE jobs SET file_state = 'staged', upload_state = ?, upload_error = NULL, upload_attempts = 0,
                upload_next_at = NULL, cleanup_state = ?, updated_at = ?
             WHERE id = ? AND CASE WHEN ? THEN file_state = 'claiming'
                 ELSE file_state = 'staged' AND upload_state = 'skipped' END",
        )
        .bind(upload)
        .bind(cleanup)
        .bind(now)
        .bind(id)
        .bind(staging)
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected() == 1)
    }

    pub async fn mark_file_missing(&self, id: &str, error: &str, now: i64) -> Result<()> {
        sqlx::query(
            "UPDATE jobs SET file_state = 'missing',
                upload_state = CASE WHEN upload_state IN ('uploaded') THEN upload_state ELSE 'failed' END,
                upload_error = CASE WHEN upload_state IN ('uploaded') THEN upload_error ELSE ? END,
                updated_at = ?
             WHERE id = ?",
        )
        .bind(error)
        .bind(now)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn recover_after_restart(&self, now: i64) -> Result<u64> {
        Ok(sqlx::query(
            "UPDATE jobs SET upload_state = 'pending', updated_at = ? WHERE upload_state IN ('uploading', 'finalizing')",
        )
        .bind(now)
        .execute(&self.pool)
        .await?
        .rows_affected())
    }

    pub async fn next_upload(&self, now: i64) -> Result<Option<Job>> {
        Ok(sqlx::query_as::<_, Job>(
            "SELECT * FROM jobs WHERE file_state = 'staged'
               AND (upload_state = 'pending' OR (upload_state = 'retry_wait' AND upload_next_at <= ?))
             ORDER BY created_at LIMIT 1",
        )
        .bind(now)
        .fetch_optional(&self.pool)
        .await?)
    }

    pub async fn next_upload_due(&self) -> Result<Option<i64>> {
        Ok(sqlx::query_scalar::<_, Option<i64>>(
            "SELECT MIN(upload_next_at) FROM jobs WHERE file_state = 'staged' AND upload_state = 'retry_wait'",
        )
        .fetch_one(&self.pool)
        .await?)
    }

    pub async fn claim_upload(&self, id: &str, now: i64) -> Result<bool> {
        let r = sqlx::query(
            "UPDATE jobs SET upload_state = 'uploading', updated_at = ?
             WHERE id = ? AND file_state = 'staged' AND upload_state IN ('pending', 'retry_wait')",
        )
        .bind(now)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected() == 1)
    }

    pub async fn save_remote_session(&self, id: &str, s: &RemoteSession, now: i64) -> Result<()> {
        sqlx::query(
            "UPDATE jobs SET remote_upload_id = ?, remote_upload_token = ?, remote_file_id = ?, remote_chunk_size = ?,
                remote_chunk_count = ?, remote_expires_at = ?, remote_chunks_done = 0, uploaded_bytes = 0,
                finalize_attempted = 0, updated_at = ?
             WHERE id = ?",
        )
        .bind(&s.upload_id)
        .bind(&s.token)
        .bind(&s.file_id)
        .bind(s.chunk_size as i64)
        .bind(s.chunk_count as i64)
        .bind(s.expires_at_ms)
        .bind(now)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn clear_remote_session(&self, id: &str, now: i64) -> Result<()> {
        sqlx::query(
            "UPDATE jobs SET remote_upload_id = NULL, remote_upload_token = NULL, remote_file_id = NULL,
                remote_chunk_size = NULL, remote_chunk_count = NULL, remote_expires_at = NULL,
                remote_chunks_done = 0, uploaded_bytes = 0, finalize_attempted = 0, updated_at = ?
             WHERE id = ?",
        )
        .bind(now)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn set_chunks_done(&self, id: &str, chunks_done: u64, uploaded_bytes: u64, now: i64) -> Result<()> {
        sqlx::query("UPDATE jobs SET remote_chunks_done = ?, uploaded_bytes = ?, updated_at = ? WHERE id = ?")
            .bind(chunks_done as i64)
            .bind(uploaded_bytes as i64)
            .bind(now)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn set_finalizing(&self, id: &str, now: i64) -> Result<()> {
        sqlx::query("UPDATE jobs SET upload_state = 'finalizing', finalize_attempted = 1, updated_at = ? WHERE id = ?")
            .bind(now)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn mark_uploaded(&self, id: &str, link: &str, delete_after_success: bool, now: i64) -> Result<()> {
        sqlx::query(
            "UPDATE jobs SET upload_state = 'uploaded', cloud_link = ?, uploaded_at = ?, upload_error = NULL,
                upload_next_at = NULL, remote_upload_token = NULL, uploaded_bytes = size_bytes,
                cleanup_state = ?, updated_at = ?
             WHERE id = ?",
        )
        .bind(link)
        .bind(now)
        .bind(if delete_after_success { CL_PENDING } else { CL_RETAINED })
        .bind(now)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn upload_retry_later(
        &self,
        id: &str,
        count_attempt: bool,
        next_at: i64,
        error: &str,
        now: i64,
    ) -> Result<i64> {
        let attempts: i64 = sqlx::query_scalar(
            "UPDATE jobs SET upload_state = 'retry_wait', upload_attempts = upload_attempts + ?, upload_next_at = ?,
                upload_error = ?, updated_at = ?
             WHERE id = ? RETURNING upload_attempts",
        )
        .bind(if count_attempt { 1 } else { 0 })
        .bind(next_at)
        .bind(error)
        .bind(now)
        .bind(id)
        .fetch_one(&self.pool)
        .await?;
        Ok(attempts)
    }

    pub async fn upload_failed(&self, id: &str, error: &str, count_attempt: bool, now: i64) -> Result<()> {
        sqlx::query(
            "UPDATE jobs SET upload_state = 'failed', upload_attempts = upload_attempts + ?, upload_error = ?,
                upload_next_at = NULL, updated_at = ?
             WHERE id = ?",
        )
        .bind(if count_attempt { 1 } else { 0 })
        .bind(error)
        .bind(now)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn release_upload(&self, id: &str, now: i64) -> Result<()> {
        sqlx::query(
            "UPDATE jobs SET upload_state = 'pending', updated_at = ? WHERE id = ? AND upload_state IN ('uploading', 'finalizing')",
        )
        .bind(now)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn next_cleanup(&self, now: i64) -> Result<Option<Job>> {
        Ok(sqlx::query_as::<_, Job>(
            "SELECT * FROM jobs WHERE cleanup_state = 'pending' OR (cleanup_state = 'retry_wait' AND cleanup_next_at <= ?)
             ORDER BY updated_at LIMIT 1",
        )
        .bind(now)
        .fetch_optional(&self.pool)
        .await?)
    }

    pub async fn next_cleanup_due(&self) -> Result<Option<i64>> {
        Ok(sqlx::query_scalar::<_, Option<i64>>(
            "SELECT MIN(cleanup_next_at) FROM jobs WHERE cleanup_state = 'retry_wait'",
        )
        .fetch_one(&self.pool)
        .await?)
    }

    pub async fn mark_cleaned(&self, id: &str, now: i64) -> Result<()> {
        sqlx::query(
            "UPDATE jobs SET cleanup_state = 'deleted', file_state = 'deleted', cleaned_at = ?, cleanup_error = NULL,
                cleanup_next_at = NULL, updated_at = ?
             WHERE id = ? AND cleanup_state IN ('pending', 'retry_wait')",
        )
        .bind(now)
        .bind(now)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn cleanup_retry_later(&self, id: &str, next_at: i64, error: &str, now: i64) -> Result<i64> {
        Ok(sqlx::query_scalar(
            "UPDATE jobs SET cleanup_state = 'retry_wait', cleanup_attempts = cleanup_attempts + 1, cleanup_next_at = ?,
                cleanup_error = ?, updated_at = ?
             WHERE id = ? RETURNING cleanup_attempts",
        )
        .bind(next_at)
        .bind(error)
        .bind(now)
        .bind(id)
        .fetch_one(&self.pool)
        .await?)
    }

    pub async fn cleanup_failed(&self, id: &str, error: &str, now: i64) -> Result<()> {
        sqlx::query("UPDATE jobs SET cleanup_state = 'failed', cleanup_error = ?, cleanup_next_at = NULL, updated_at = ? WHERE id = ?")
            .bind(error)
            .bind(now)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn retry(&self, id: &str, now: i64) -> Result<RetryOutcome> {
        let Some(job) = self.get(id).await? else { return Ok(RetryOutcome::NotFound) };
        if job.file_state == FILE_STAGED && matches!(job.upload_state.as_str(), UP_FAILED | UP_CANCELLED | UP_RETRY) {
            let r = sqlx::query(
                "UPDATE jobs SET upload_state = 'pending', upload_attempts = 0, upload_next_at = NULL, upload_error = NULL, updated_at = ?
                 WHERE id = ? AND file_state = 'staged' AND upload_state IN ('failed', 'cancelled', 'retry_wait')",
            )
            .bind(now)
            .bind(id)
            .execute(&self.pool)
            .await?;
            if r.rows_affected() == 1 {
                return Ok(RetryOutcome::Upload);
            }
        }
        if matches!(job.cleanup_state.as_str(), CL_FAILED | CL_RETRY) {
            let r = sqlx::query(
                "UPDATE jobs SET cleanup_state = 'pending', cleanup_attempts = 0, cleanup_next_at = NULL, cleanup_error = NULL, updated_at = ?
                 WHERE id = ? AND cleanup_state IN ('failed', 'retry_wait')",
            )
            .bind(now)
            .bind(id)
            .execute(&self.pool)
            .await?;
            if r.rows_affected() == 1 {
                return Ok(RetryOutcome::Cleanup);
            }
        }
        Ok(RetryOutcome::NothingToRetry)
    }

    pub async fn cancel_upload(&self, id: &str, now: i64) -> Result<Option<Option<RemoteSession>>> {
        let mut tx = self.pool.begin().await?;
        let job = sqlx::query_as::<_, Job>("SELECT * FROM jobs WHERE id = ?").bind(id).fetch_optional(&mut *tx).await?;
        let Some(job) = job else { return Ok(None) };
        if !matches!(job.upload_state.as_str(), UP_PENDING | UP_RETRY | UP_FAILED) {
            return Ok(None);
        }
        sqlx::query(
            "UPDATE jobs SET upload_state = 'cancelled', upload_next_at = NULL, upload_error = 'Cancelled by user; the file is kept.',
                remote_upload_id = NULL, remote_upload_token = NULL, remote_file_id = NULL, remote_chunk_size = NULL,
                remote_chunk_count = NULL, remote_expires_at = NULL, remote_chunks_done = 0, uploaded_bytes = 0,
                finalize_attempted = 0, updated_at = ?
             WHERE id = ?",
        )
        .bind(now)
        .bind(id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(Some(job.remote_session()))
    }

    pub async fn create_session(&self, s: &NewSession<'_>, now: i64, expires_at: i64) -> Result<()> {
        sqlx::query(
            "INSERT INTO sessions (id_hash, public_id, username, owner, csrf_token, ip, user_agent, created_at, expires_at, last_seen_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(s.id_hash)
        .bind(s.public_id)
        .bind(s.username)
        .bind(s.owner)
        .bind(s.csrf)
        .bind(s.ip)
        .bind(s.user_agent)
        .bind(now)
        .bind(expires_at)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn list_sessions(&self, owner: &str, now: i64) -> Result<Vec<SessionInfo>> {
        Ok(sqlx::query_as::<_, SessionInfo>(
            "SELECT id_hash, public_id, ip, user_agent, created_at, last_seen_at FROM sessions
             WHERE owner = ? AND expires_at > ? AND public_id IS NOT NULL ORDER BY last_seen_at DESC",
        )
        .bind(owner)
        .bind(now)
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn delete_session_by_public_id(&self, owner: &str, public_id: &str) -> Result<bool> {
        let r = sqlx::query("DELETE FROM sessions WHERE owner = ? AND public_id = ?")
            .bind(owner)
            .bind(public_id)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected() == 1)
    }

    pub async fn delete_other_sessions(&self, owner: &str, keep_id_hash: &str) -> Result<u64> {
        Ok(sqlx::query("DELETE FROM sessions WHERE owner = ? AND id_hash != ?")
            .bind(owner)
            .bind(keep_id_hash)
            .execute(&self.pool)
            .await?
            .rows_affected())
    }

    pub async fn delete_sessions_of(&self, owner: &str) -> Result<u64> {
        Ok(sqlx::query("DELETE FROM sessions WHERE owner = ?").bind(owner).execute(&self.pool).await?.rows_affected())
    }

    pub async fn get_session(&self, id_hash: &str, now: i64) -> Result<Option<SessionRow>> {
        Ok(sqlx::query_as::<_, SessionRow>(
            "SELECT s.username, s.owner, s.csrf_token, s.expires_at, s.last_seen_at FROM sessions s
             WHERE s.id_hash = ? AND s.expires_at > ?
               AND (s.owner = '' OR EXISTS (SELECT 1 FROM users u WHERE u.id = s.owner AND u.username = s.username))",
        )
        .bind(id_hash)
        .bind(now)
        .fetch_optional(&self.pool)
        .await?)
    }

    pub async fn create_user(&self, u: &UserRow) -> Result<bool> {
        let r = sqlx::query(
            "INSERT INTO users (id, username, password_hash, created_at) VALUES (?, ?, ?, ?)
             ON CONFLICT(username) DO NOTHING",
        )
        .bind(&u.id)
        .bind(&u.username)
        .bind(&u.password_hash)
        .bind(u.created_at)
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected() == 1)
    }

    pub async fn user_by_name(&self, username: &str) -> Result<Option<UserRow>> {
        Ok(sqlx::query_as::<_, UserRow>("SELECT * FROM users WHERE username = ?")
            .bind(username)
            .fetch_optional(&self.pool)
            .await?)
    }

    pub async fn list_users(&self, now: i64) -> Result<Vec<UserInfo>> {
        Ok(sqlx::query_as::<_, UserInfo>(
            "SELECT u.id, u.username, u.created_at,
                (SELECT COUNT(*) FROM sessions s WHERE s.owner = u.id AND s.expires_at > ?) AS sessions,
                (SELECT MAX(s.last_seen_at) FROM sessions s WHERE s.owner = u.id) AS last_seen_at
             FROM users u ORDER BY u.username",
        )
        .bind(now)
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn user_ids(&self, now: i64) -> Result<Vec<(String, String, bool)>> {
        Ok(sqlx::query_as::<_, (String, String, bool)>(
            "SELECT u.id, u.username, EXISTS (SELECT 1 FROM sessions s WHERE s.owner = u.id AND s.expires_at > ?)
             FROM users u ORDER BY u.id",
        )
        .bind(now)
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn set_user_password(&self, id: &str, password_hash: &str) -> Result<bool> {
        let r = sqlx::query("UPDATE users SET password_hash = ? WHERE id = ?")
            .bind(password_hash)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected() == 1)
    }

    pub async fn delete_user(&self, id: &str) -> Result<bool> {
        let mut tx = self.pool.begin().await?;
        let r = sqlx::query("DELETE FROM users WHERE id = ?").bind(id).execute(&mut *tx).await?;
        sqlx::query("DELETE FROM sessions WHERE owner = ?").bind(id).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(r.rows_affected() == 1)
    }

    pub async fn touch_session(&self, id_hash: &str, now: i64) -> Result<()> {
        sqlx::query("UPDATE sessions SET last_seen_at = ? WHERE id_hash = ?")
            .bind(now)
            .bind(id_hash)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn delete_session(&self, id_hash: &str) -> Result<()> {
        sqlx::query("DELETE FROM sessions WHERE id_hash = ?").bind(id_hash).execute(&self.pool).await?;
        Ok(())
    }

    pub async fn purge_sessions(&self, now: i64) -> Result<u64> {
        Ok(sqlx::query("DELETE FROM sessions WHERE expires_at <= ?")
            .bind(now)
            .execute(&self.pool)
            .await?
            .rows_affected())
    }

    pub async fn ping(&self) -> Result<()> {
        sqlx::query_scalar::<_, i64>("SELECT 1").fetch_one(&self.pool).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use sha2::{Digest, Sha384};

    use super::*;

    #[tokio::test]
    async fn upgrading_drops_notification_columns_and_keeps_files_that_waited_on_them() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("chromine.db");
        let opts =
            SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display())).unwrap().create_if_missing(true);
        let pool = SqlitePool::connect_with(opts).await.unwrap();
        sqlx::migrate!("./migrations").run_to(3, &pool).await.unwrap();
        for (id, upload, upload_error, notify, cleanup) in [
            ("a", "uploaded", None, "failed", "waiting"),
            ("b", "skipped", None, "pending", "waiting"),
            ("c", "skipped", Some("too big"), "disabled", "retained"),
            ("d", "pending", None, "waiting", "waiting"),
            ("e", "uploaded", None, "sent", "deleted"),
            ("f", "uploaded", None, "disabled", "pending"),
        ] {
            sqlx::query(
                "INSERT INTO jobs (id, download_guid, source, original_filename, chrome_filename, size_bytes, file_dev,
                    file_ino, incoming_path, staged_path, download_completed_at, created_at, updated_at, file_state,
                    upload_state, upload_error, notify_state, cleanup_state)
                 VALUES (?, ?, 'cdp', 'f', 'f', 1, 0, 0, '', '', 0, 0, 0, 'staged', ?, ?, ?, ?)",
            )
            .bind(id)
            .bind(id)
            .bind(upload)
            .bind(upload_error)
            .bind(notify)
            .bind(cleanup)
            .execute(&pool)
            .await
            .unwrap();
        }
        pool.close().await;

        let db = Db::open(&path).await.unwrap();
        for (id, cleanup) in [
            ("a", "retained"),
            ("b", "retained"),
            ("c", "retained"),
            ("d", "waiting"),
            ("e", "deleted"),
            ("f", "pending"),
        ] {
            let job = db.get(id).await.unwrap().unwrap();
            assert_eq!(job.cleanup_state, cleanup, "{id}");
            assert!(job.upload_error.is_none(), "{id}");
        }
        let columns: Vec<String> =
            sqlx::query_scalar("SELECT name FROM pragma_table_info('jobs')").fetch_all(&db.pool).await.unwrap();
        assert!(columns.iter().all(|c| !c.starts_with("notif") && !c.starts_with("telegram")), "{columns:?}");
    }

    #[test]
    fn applied_migrations_are_unchanged() {
        let applied = [(
            "0001_init.sql",
            "b2f62a892f723685f19def2a4f80a87dd426b1a4f159baa6469ebbe6b56cf5398f61fd93097dbdb09e0355cf835b318c",
        )];
        for (name, expected) in applied {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations").join(name);
            let actual = hex::encode(Sha384::digest(std::fs::read(&path).unwrap()));
            assert_eq!(actual, expected, "{name} was modified after release; add a new migration instead");
        }
    }
}

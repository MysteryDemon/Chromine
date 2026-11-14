use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use bytes::Bytes;
use futures_util::StreamExt;
use reqwest::header::{CONTENT_LENGTH, CONTENT_TYPE, HeaderMap, RETRY_AFTER};
use reqwest::{Client, StatusCode};
use serde::Deserialize;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use url::Url;

use crate::db::RemoteSession;

pub const STREAM_BUFFER: usize = 64 * 1024;
pub const MAX_ACCEPTED_CHUNK_SIZE: u64 = 1024 * 1024 * 1024;

#[derive(Debug, Clone, Deserialize)]
pub struct ServiceConfig {
    #[serde(default)]
    pub max_upload_size: u64,
    #[serde(default)]
    pub hourly_upload_allowance: u64,
    #[serde(default)]
    pub max_folder_size: u64,
    #[serde(default = "yes")]
    pub ready: bool,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ErrorKind {
    Busy,
    RateLimited,
    SessionGone,
    TooLarge,
    Rejected,
    Server,
    Network,
    InvalidResponse,
    LocalIo,
}

#[derive(Debug, Clone)]
pub struct ApiError {
    pub kind: ErrorKind,
    pub status: Option<u16>,
    pub detail: String,
    pub retry_after: Option<Duration>,
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.status {
            Some(s) => write!(f, "FileCloud24 HTTP {s}: {}", self.detail),
            None => write!(f, "FileCloud24: {}", self.detail),
        }
    }
}

impl std::error::Error for ApiError {}

impl ApiError {
    fn new(kind: ErrorKind, status: Option<u16>, detail: impl Into<String>) -> Self {
        ApiError { kind, status, detail: detail.into(), retry_after: None }
    }
}

#[derive(Debug, Deserialize)]
struct StartResponse {
    upload_id: String,
    upload_token: String,
    chunk_size: u64,
    chunk_count: u64,
    expires_at: Option<String>,
    #[serde(default)]
    file_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CompleteResponse {
    #[serde(default)]
    download_url: Option<String>,
    #[serde(default)]
    file_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ErrorBody {
    #[serde(default)]
    detail: Option<serde_json::Value>,
    #[serde(default)]
    busy: Option<bool>,
}

#[derive(Clone)]
pub struct FileCloud {
    http: Client,
    base: Url,
}

pub fn parse_retry_after(headers: &HeaderMap) -> Option<Duration> {
    let v = headers.get(RETRY_AFTER)?.to_str().ok()?.trim().to_string();
    let d = if let Ok(secs) = v.parse::<u64>() {
        Duration::from_secs(secs)
    } else {
        let when = chrono::DateTime::parse_from_rfc2822(&v).ok()?;
        let delta = when.timestamp_millis() - chrono::Utc::now().timestamp_millis();
        Duration::from_millis(delta.max(0) as u64)
    };
    Some(d.min(Duration::from_secs(6 * 3600)))
}

fn valid_id(s: &str) -> bool {
    !s.is_empty() && s.len() <= 128 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn valid_token(s: &str) -> bool {
    !s.is_empty() && s.len() <= 1024 && s.bytes().all(|b| (0x21..=0x7e).contains(&b))
}

pub fn expected_chunks(size: u64, chunk_size: u64) -> u64 {
    if size == 0 { 0 } else { size.div_ceil(chunk_size) }
}

pub fn chunk_range(size: u64, chunk_size: u64, index: u64) -> (u64, u64) {
    let start = index * chunk_size;
    let len = chunk_size.min(size.saturating_sub(start));
    (start, len)
}

impl FileCloud {
    pub fn new(base: Url, request_timeout: Duration) -> anyhow::Result<FileCloud> {
        let http = Client::builder()
            .user_agent(concat!("chromine/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(20))
            .timeout(request_timeout)
            .pool_idle_timeout(Duration::from_secs(60))
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(FileCloud { http, base })
    }

    pub fn base(&self) -> &Url {
        &self.base
    }

    fn url(&self, path: &str) -> Url {
        let mut u = self.base.clone();
        let p = format!("{}{}", self.base.path().trim_end_matches('/'), path);
        u.set_path(&p);
        u
    }

    fn net_error(e: reqwest::Error) -> ApiError {
        let e = e.without_url();
        let what = if e.is_timeout() {
            "request timed out"
        } else if e.is_connect() {
            "connection failed"
        } else {
            "request failed"
        };
        ApiError::new(ErrorKind::Network, None, format!("{what}: {e}"))
    }

    async fn error_from(resp: reqwest::Response, session_scoped: bool) -> ApiError {
        let status = resp.status();
        let retry_after = parse_retry_after(resp.headers());
        let body = resp.bytes().await.unwrap_or_default();
        let parsed: Option<ErrorBody> = serde_json::from_slice(&body[..body.len().min(64 * 1024)]).ok();
        let detail = parsed
            .as_ref()
            .and_then(|b| b.detail.as_ref())
            .map(|d| match d {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            })
            .unwrap_or_else(|| status.canonical_reason().unwrap_or("error").to_string());
        let detail: String = detail.chars().filter(|c| !c.is_control()).take(300).collect();
        let busy = parsed.as_ref().and_then(|b| b.busy).unwrap_or(false);
        let kind = match status {
            StatusCode::SERVICE_UNAVAILABLE if busy => ErrorKind::Busy,
            StatusCode::TOO_MANY_REQUESTS => ErrorKind::RateLimited,
            StatusCode::PAYLOAD_TOO_LARGE => ErrorKind::TooLarge,
            StatusCode::NOT_FOUND | StatusCode::GONE | StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
                if session_scoped =>
            {
                ErrorKind::SessionGone
            }
            s if s.is_server_error() => ErrorKind::Server,
            StatusCode::REQUEST_TIMEOUT => ErrorKind::Server,
            _ => ErrorKind::Rejected,
        };
        ApiError { kind, status: Some(status.as_u16()), detail, retry_after }
    }

    pub async fn service_config(&self) -> Result<ServiceConfig, ApiError> {
        let resp = self.http.get(self.url("/api/config")).send().await.map_err(Self::net_error)?;
        if !resp.status().is_success() {
            return Err(Self::error_from(resp, false).await);
        }
        resp.json::<ServiceConfig>().await.map_err(|e| {
            ApiError::new(
                ErrorKind::InvalidResponse,
                None,
                format!("invalid /api/config response: {}", e.without_url()),
            )
        })
    }

    pub async fn start(&self, filename: &str, size: u64, content_type: &str) -> Result<RemoteSession, ApiError> {
        let body = serde_json::json!({ "filename": filename, "size_bytes": size, "content_type": content_type });
        let resp = self.http.post(self.url("/api/v2/uploads")).json(&body).send().await.map_err(Self::net_error)?;
        if !resp.status().is_success() {
            return Err(Self::error_from(resp, false).await);
        }
        let s: StartResponse = resp.json().await.map_err(|e| {
            ApiError::new(ErrorKind::InvalidResponse, None, format!("invalid start response: {}", e.without_url()))
        })?;
        let session = RemoteSession {
            upload_id: s.upload_id,
            token: s.upload_token,
            file_id: s.file_id.filter(|f| valid_id(f)),
            chunk_size: s.chunk_size,
            chunk_count: s.chunk_count,
            expires_at_ms: s
                .expires_at
                .as_deref()
                .and_then(|e| chrono::DateTime::parse_from_rfc3339(e).ok())
                .map(|t| t.timestamp_millis())
                .unwrap_or_else(|| chrono::Utc::now().timestamp_millis() + 3_600_000),
        };
        if let Err(e) = validate_session(&session, size) {
            if valid_id(&session.upload_id) && valid_token(&session.token) {
                let _ = self.abort(&session).await;
            }
            return Err(e);
        }
        Ok(session)
    }

    pub async fn put_chunk(
        &self,
        s: &RemoteSession,
        path: &Path,
        size: u64,
        index: u64,
        progress: Arc<AtomicU64>,
    ) -> Result<(), ApiError> {
        let (start, len) = chunk_range(size, s.chunk_size, index);
        let body = chunk_body(path, start, len, progress)
            .await
            .map_err(|e| ApiError::new(ErrorKind::LocalIo, None, format!("cannot read file: {e}")))?;
        let resp = self
            .http
            .put(self.url(&format!("/api/v2/uploads/{}/chunks/{index}", s.upload_id)))
            .header("X-Upload-Token", &s.token)
            .header(CONTENT_TYPE, "application/octet-stream")
            .header(CONTENT_LENGTH, len)
            .body(body)
            .send()
            .await
            .map_err(Self::net_error)?;
        if resp.status().is_success() {
            let _ = resp.bytes().await;
            return Ok(());
        }
        Err(Self::error_from(resp, true).await)
    }

    pub async fn complete(&self, s: &RemoteSession) -> Result<String, ApiError> {
        let resp = self
            .http
            .post(self.url(&format!("/api/v2/uploads/{}/complete", s.upload_id)))
            .header("X-Upload-Token", &s.token)
            .header(CONTENT_LENGTH, 0)
            .send()
            .await
            .map_err(Self::net_error)?;
        if !resp.status().is_success() {
            return Err(Self::error_from(resp, true).await);
        }
        let c: CompleteResponse = resp.json().await.map_err(|e| {
            ApiError::new(ErrorKind::InvalidResponse, None, format!("invalid complete response: {}", e.without_url()))
        })?;
        self.extract_link(c.download_url.as_deref(), c.file_id.as_deref().or(s.file_id.as_deref()))
    }

    pub fn extract_link(&self, download_url: Option<&str>, file_id: Option<&str>) -> Result<String, ApiError> {
        if let Some(u) = download_url {
            let parsed = Url::parse(u)
                .map_err(|_| ApiError::new(ErrorKind::InvalidResponse, None, "download_url is not a URL"))?;
            if !matches!(parsed.scheme(), "https" | "http") || parsed.host_str().is_none() {
                return Err(ApiError::new(ErrorKind::InvalidResponse, None, "download_url is not an http(s) URL"));
            }
            return Ok(parsed.to_string());
        }
        match file_id {
            Some(id) if valid_id(id) => {
                tracing::warn!("complete response had no download_url; building it from file_id");
                Ok(self.url(&format!("/download/{id}")).to_string())
            }
            _ => Err(ApiError::new(ErrorKind::InvalidResponse, None, "complete response contained no download_url")),
        }
    }

    pub async fn abort(&self, s: &RemoteSession) -> Result<(), ApiError> {
        let resp = self
            .http
            .delete(self.url(&format!("/api/v2/uploads/{}", s.upload_id)))
            .header("X-Upload-Token", &s.token)
            .send()
            .await
            .map_err(Self::net_error)?;
        if resp.status().is_success() || resp.status() == StatusCode::NOT_FOUND {
            return Ok(());
        }
        Err(Self::error_from(resp, true).await)
    }
}

pub fn validate_session(s: &RemoteSession, size: u64) -> Result<(), ApiError> {
    let bad = |m: String| Err(ApiError::new(ErrorKind::InvalidResponse, None, m));
    if !valid_id(&s.upload_id) {
        return bad("server returned an invalid upload_id".into());
    }
    if !valid_token(&s.token) {
        return bad("server returned an invalid upload_token".into());
    }
    if s.chunk_size == 0 || s.chunk_size > MAX_ACCEPTED_CHUNK_SIZE {
        return bad(format!("server returned an unusable chunk_size ({})", s.chunk_size));
    }
    let expected = expected_chunks(size, s.chunk_size);
    if s.chunk_count != expected {
        return bad(format!(
            "server chunk layout does not match the file: chunk_count {} for {} bytes in {}-byte parts (expected {})",
            s.chunk_count, size, s.chunk_size, expected
        ));
    }
    Ok(())
}

pub async fn chunk_body(
    path: &Path,
    offset: u64,
    len: u64,
    progress: Arc<AtomicU64>,
) -> std::io::Result<reqwest::Body> {
    let (std_file, _meta) = chromine_shared::fsutil::open_regular_nofollow(path)?;
    let mut file = tokio::fs::File::from_std(std_file);
    file.seek(std::io::SeekFrom::Start(offset)).await?;
    let reader = file.take(len);
    let stream =
        tokio_util::io::ReaderStream::with_capacity(reader, STREAM_BUFFER).map(move |r: std::io::Result<Bytes>| {
            if let Ok(b) = &r {
                progress.fetch_add(b.len() as u64, Ordering::Relaxed);
            }
            r
        });
    Ok(reqwest::Body::wrap_stream(stream))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_math() {
        assert_eq!(expected_chunks(0, 8), 0);
        assert_eq!(expected_chunks(1, 8), 1);
        assert_eq!(expected_chunks(8, 8), 1);
        assert_eq!(expected_chunks(9, 8), 2);
        assert_eq!(chunk_range(20, 8, 0), (0, 8));
        assert_eq!(chunk_range(20, 8, 1), (8, 8));
        assert_eq!(chunk_range(20, 8, 2), (16, 4));
    }

    #[test]
    fn session_validation() {
        let ok = RemoteSession {
            upload_id: "s28RaSrgIAfzwi6sSuSdakTCdhfRa5Rl".into(),
            token: "vLX0dvYsfADseBX28fzR2k4o1Pk-qUoCSFJsJBqpANA".into(),
            file_id: None,
            chunk_size: 8 * 1024 * 1024,
            chunk_count: 2,
            expires_at_ms: 0,
        };
        assert!(validate_session(&ok, 10 * 1024 * 1024).is_ok());
        assert!(validate_session(&RemoteSession { chunk_count: 3, ..ok.clone() }, 10 * 1024 * 1024).is_err());
        assert!(validate_session(&RemoteSession { chunk_size: 0, ..ok.clone() }, 10).is_err());
        assert!(validate_session(&RemoteSession { upload_id: "../x".into(), ..ok.clone() }, 10 * 1024 * 1024).is_err());
        assert!(validate_session(&RemoteSession { token: "a b".into(), ..ok.clone() }, 10 * 1024 * 1024).is_err());
        assert!(validate_session(&RemoteSession { chunk_count: 0, ..ok.clone() }, 0).is_ok());
    }

    #[test]
    fn retry_after_forms() {
        let mut h = HeaderMap::new();
        h.insert(RETRY_AFTER, "12".parse().unwrap());
        assert_eq!(parse_retry_after(&h), Some(Duration::from_secs(12)));
        h.insert(RETRY_AFTER, "999999".parse().unwrap());
        assert_eq!(parse_retry_after(&h), Some(Duration::from_secs(6 * 3600)));
        let future = (chrono::Utc::now() + chrono::Duration::seconds(60)).to_rfc2822();
        h.insert(RETRY_AFTER, future.parse().unwrap());
        let d = parse_retry_after(&h).unwrap();
        assert!(d > Duration::from_secs(50) && d <= Duration::from_secs(61));
        h.insert(RETRY_AFTER, "garbage".parse().unwrap());
        assert_eq!(parse_retry_after(&h), None);
    }

    #[test]
    fn link_extraction() {
        let fc = FileCloud::new(Url::parse("https://filecloud24.cc").unwrap(), Duration::from_secs(5)).unwrap();
        assert_eq!(
            fc.extract_link(Some("https://filecloud24.cc/download/abc"), None).unwrap(),
            "https://filecloud24.cc/download/abc"
        );
        assert_eq!(fc.extract_link(None, Some("abc_DEF-1")).unwrap(), "https://filecloud24.cc/download/abc_DEF-1");
        assert!(fc.extract_link(Some("javascript:alert(1)"), None).is_err());
        assert!(fc.extract_link(None, Some("../x")).is_err());
        assert!(fc.extract_link(None, None).is_err());
    }
}

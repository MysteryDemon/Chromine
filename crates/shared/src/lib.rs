pub mod fsutil;

use serde::{Deserialize, Serialize};

pub const SCHEMA_VERSION: u32 = 1;

pub const INCOMING_SUBDIR: &str = "incoming";
pub const EVENTS_SUBDIR: &str = "events";
pub const COMPLETED_SUBDIR: &str = "completed";
pub const JOBS_SUBDIR: &str = "jobs";
pub const AGENT_STATUS_FILE: &str = "agent-status.json";
pub const DISPLAY_SCALE_FILE: &str = "display-scale";
pub const USERS_FILE: &str = "users.json";
pub const USERS_SUBDIR: &str = "users";
pub const VNC_SOCKET_NAME: &str = "vnc.sock";
pub const AUDIO_SOCKET_NAME: &str = "audio.sock";

pub const MAX_RECORD_BYTES: u64 = 64 * 1024;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsersFile {
    pub schema: u32,
    pub known: Vec<String>,
    pub active: Vec<String>,
}

pub fn is_valid_user_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

pub fn user_dir(root: &std::path::Path, id: &str) -> std::path::PathBuf {
    root.join(USERS_SUBDIR).join(id)
}

pub fn read_users_file(path: &std::path::Path) -> Option<UsersFile> {
    let (file, meta) = fsutil::open_regular_nofollow(path).ok()?;
    if meta.len() > MAX_RECORD_BYTES {
        return None;
    }
    let users: UsersFile = serde_json::from_reader(std::io::BufReader::new(file)).ok()?;
    (users.schema == SCHEMA_VERSION
        && users.known.iter().chain(&users.active).all(|id| is_valid_user_id(id))
        && users.active.iter().all(|id| users.known.contains(id)))
    .then_some(users)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordSource {
    Extension,
    Cdp,
    History,
    #[serde(rename = "ytdlp")]
    YtDlp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompletedDownload {
    pub schema: u32,
    pub guid: String,
    pub suggested_filename: String,
    pub file_path: String,
    pub size: u64,
    pub dev: u64,
    pub ino: u64,
    pub completed_at_ms: i64,
    pub source: RecordSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveDownload {
    pub guid: String,
    pub filename: String,
    pub received_bytes: u64,
    pub total_bytes: u64,
    pub started_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RejectedDownload {
    pub filename: String,
    pub reason: String,
    pub at_ms: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiskSpace {
    pub available_bytes: u64,
    pub total_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentStatus {
    pub schema: u32,
    pub updated_at_ms: i64,
    pub agent_started_at_ms: i64,
    pub chrome_running: bool,
    pub chrome_version: Option<String>,
    pub chrome_starts: u32,
    pub sandbox_ok: Option<bool>,
    pub sandbox_detail: Option<String>,
    pub downloads_paused_low_disk: bool,
    pub disk: DiskSpace,
    pub min_free_disk_bytes: u64,
    pub active_downloads: Vec<ActiveDownload>,
    pub recent_rejections: Vec<RejectedDownload>,
    pub last_error: Option<String>,
    #[serde(default)]
    pub monitor_connected: bool,
    #[serde(default)]
    pub monitor_detail: Option<String>,
    #[serde(default)]
    pub opened_link: Option<OpenedLink>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenedLink {
    pub url: String,
    pub at_ms: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MonitorState {
    pub updated_at_ms: i64,
    pub connected_since_ms: i64,
    pub downloads_paused_low_disk: bool,
    pub active_downloads: Vec<ActiveDownload>,
    pub recent_rejections: Vec<RejectedDownload>,
    pub last_error: Option<String>,
}

pub fn is_valid_guid(guid: &str) -> bool {
    !guid.is_empty() && guid.len() <= 64 && guid.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

pub fn is_partial_download_name(name: &str) -> bool {
    name.ends_with(".crdownload")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guid_validation() {
        assert!(is_valid_guid("3f99765c-64d5-47fd-abe2-a6863d0c7ce9"));
        assert!(is_valid_guid("12-1793743839882"));
        assert!(!is_valid_guid(""));
        assert!(!is_valid_guid("../../etc/passwd"));
        assert!(!is_valid_guid("abc/def"));
        assert!(!is_valid_guid(&"a".repeat(65)));
    }

    #[test]
    fn partial_names() {
        assert!(is_partial_download_name("Unconfirmed 12345.crdownload"));
        assert!(is_partial_download_name("video.mp4.crdownload"));
        assert!(!is_partial_download_name("video.mp4"));
    }

    #[test]
    fn record_round_trip() {
        let r = CompletedDownload {
            schema: SCHEMA_VERSION,
            guid: "3f99765c-64d5-47fd-abe2-a6863d0c7ce9".into(),
            suggested_filename: "report.zip".into(),
            file_path: "/downloads/incoming/report.zip".into(),
            size: 10,
            dev: 1,
            ino: 2,
            completed_at_ms: 3,
            source: RecordSource::Cdp,
            source_url: Some("https://example.com/report.zip".into()),
        };
        let s = serde_json::to_string(&r).unwrap();
        assert!(s.contains("\"source\":\"cdp\""));
        let back: CompletedDownload = serde_json::from_str(&s).unwrap();
        assert_eq!(back, r);
        let old: CompletedDownload =
            serde_json::from_str(&s.replace(",\"source_url\":\"https://example.com/report.zip\"", "")).unwrap();
        assert_eq!(old.source_url, None);
    }
}

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use url::Url;

#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(s: impl Into<String>) -> Self {
        Secret(s.into())
    }
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<redacted>")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StreamMode {
    Vnc,
    Webrtc,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WebrtcCodec {
    Auto,
    Av1,
    H265,
    H264,
    Vp9,
    Vp8,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub auth_user: String,
    pub auth_password: Secret,
    pub public_url: Option<Url>,
    pub session_ttl: Duration,
    pub download_dir: PathBuf,
    pub state_dir: PathBuf,
    pub delete_after_success: bool,
    pub min_free_disk_bytes: u64,
    pub filecloud_base_url: Url,
    pub upload_request_timeout: Duration,
    pub max_retry_attempts: u32,
    pub listen: SocketAddr,
    pub novnc_dir: PathBuf,
    pub vnc_socket: PathBuf,
    pub audio_socket: PathBuf,
    pub ui_dir: PathBuf,
    pub display_scale: f64,
    pub stream_mode: StreamMode,
    pub webrtc_port: u16,
    pub webrtc_host: Option<String>,
    pub webrtc_codec: WebrtcCodec,
    pub ffmpeg_bin: PathBuf,
    pub code_socket: Option<PathBuf>,
    pub code_links_dir: PathBuf,
    pub code_listen: SocketAddr,
    pub code_port: u16,
    pub code_public_url: Option<Url>,
    pub https_host: Option<String>,
    pub acme_directory: String,
    pub acme_listen: SocketAddr,
    pub dashboard_port: u16,
}

pub fn is_public_ip(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(ip) => !(ip.is_private() || ip.is_loopback() || ip.is_link_local() || ip.is_unspecified()),
        std::net::IpAddr::V6(ip) => !(ip.is_loopback() || ip.is_unspecified() || (ip.segments()[0] & 0xfe00) == 0xfc00),
    }
}

pub fn check_https_host(h: &str) -> std::result::Result<(), String> {
    match h.parse::<std::net::IpAddr>() {
        Ok(ip) if !is_public_ip(ip) => Err(format!(
            "HTTPS_HOST {h} is not a public IP address; use the address the internet reaches this server on, or HTTPS_HOST=auto"
        )),
        Ok(_) => Ok(()),
        Err(_) if h.contains('.') && h.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-') => Ok(()),
        Err(_) => Err(format!("HTTPS_HOST {h:?} must be this server's public IP address or domain name")),
    }
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

pub fn secret(name: &str) -> Result<Option<Secret>> {
    if let Some(path) = env(&format!("{name}_FILE")) {
        let v = std::fs::read_to_string(&path).with_context(|| format!("cannot read {name}_FILE ({path})"))?;
        let v = v.trim().to_string();
        if v.is_empty() {
            bail!("{name}_FILE ({path}) is empty");
        }
        return Ok(Some(Secret(v)));
    }
    Ok(env(name).map(Secret))
}

fn parse_bool(name: &str, default: bool) -> Result<bool> {
    match env(name).as_deref().map(str::to_ascii_lowercase).as_deref() {
        None => Ok(default),
        Some("1" | "true" | "yes" | "on") => Ok(true),
        Some("0" | "false" | "no" | "off") => Ok(false),
        Some(v) => bail!("{name} must be true or false, got {v:?}"),
    }
}

fn parse_num<T: std::str::FromStr>(name: &str, default: T) -> Result<T> {
    match env(name) {
        Some(v) => v.parse().map_err(|_| anyhow::anyhow!("{name} must be a number, got {v:?}")),
        None => Ok(default),
    }
}

pub fn check_password(p: &str) -> std::result::Result<(), String> {
    let bcrypt = ["$2a$", "$2b$", "$2y$"].iter().any(|x| p.starts_with(x)) && p.len() == 60;
    if (p.starts_with("$argon2") && p.contains("$v=")) || bcrypt {
        return Err("APP_AUTH_PASSWORD must be the password itself, not a hash".into());
    }
    if p.chars().count() < 12 {
        return Err("APP_AUTH_PASSWORD must be at least 12 characters".into());
    }
    Ok(())
}

pub fn parse_base_url(raw: &str, allow_http: bool) -> Result<Url> {
    let mut u = Url::parse(raw).with_context(|| format!("FILECLOUD_BASE_URL {raw:?} is not a URL"))?;
    match u.scheme() {
        "https" => {}
        "http" if allow_http => {}
        _ => bail!("FILECLOUD_BASE_URL must use https"),
    }
    if u.host_str().is_none() || u.query().is_some() || u.fragment().is_some() || !u.username().is_empty() {
        bail!("FILECLOUD_BASE_URL must be a plain origin such as https://filecloud24.cc");
    }
    let path = u.path().trim_end_matches('/').to_string();
    u.set_path(&path);
    Ok(u)
}

impl Config {
    pub fn from_env() -> Result<Config> {
        let mut problems: Vec<String> = Vec::new();
        let mut need = |name: &str, v: Option<String>| -> String {
            v.unwrap_or_else(|| {
                problems.push(format!("{name} is required"));
                String::new()
            })
        };

        let auth_user = need("APP_AUTH_USER", env("APP_AUTH_USER"));
        let auth_password = match secret("APP_AUTH_PASSWORD")? {
            Some(p) => {
                if let Err(e) = check_password(p.expose()) {
                    problems.push(e);
                }
                p
            }
            None => {
                problems.push("APP_AUTH_PASSWORD (or APP_AUTH_PASSWORD_FILE) is required".into());
                Secret(String::new())
            }
        };

        let mut web_url = |name: &str| match env(name) {
            Some(v) => match Url::parse(&v) {
                Ok(u) if matches!(u.scheme(), "http" | "https") && u.host_str().is_some() => Some(u),
                _ => {
                    problems.push(format!("{name} {v:?} must be an http(s) URL"));
                    None
                }
            },
            None => None,
        };
        let public_url = web_url("APP_PUBLIC_URL");
        let code_public_url = web_url("CODE_PUBLIC_URL");

        let allow_http = parse_bool("FILECLOUD_ALLOW_HTTP", false)?;
        let base = env("FILECLOUD_BASE_URL").unwrap_or_else(|| "https://filecloud24.cc".into());
        let filecloud_base_url = match parse_base_url(&base, allow_http) {
            Ok(u) => u,
            Err(e) => {
                problems.push(e.to_string());
                Url::parse("https://filecloud24.cc").unwrap()
            }
        };

        let download_dir = PathBuf::from(env("DOWNLOAD_DIR").unwrap_or_else(|| "/downloads".into()));
        let state_dir = PathBuf::from(env("STATE_DIR").unwrap_or_else(|| "/state".into()));
        if !download_dir.is_absolute() || !state_dir.is_absolute() {
            problems.push("DOWNLOAD_DIR and STATE_DIR must be absolute paths".into());
        }

        let timeout: u64 = parse_num("UPLOAD_REQUEST_TIMEOUT_SECS", 300)?;
        if !(10..=3600).contains(&timeout) {
            problems.push("UPLOAD_REQUEST_TIMEOUT_SECS must be between 10 and 3600".into());
        }
        let max_retry_attempts: u32 = parse_num("MAX_RETRY_ATTEMPTS", 8)?;
        if !(1..=100).contains(&max_retry_attempts) {
            problems.push("MAX_RETRY_ATTEMPTS must be between 1 and 100".into());
        }
        let display_scale: f64 = parse_num("DISPLAY_SCALE", 1.0)?;
        if !(1.0..=3.0).contains(&display_scale) {
            problems.push("DISPLAY_SCALE must be between 1 and 3".into());
        }
        let stream_mode = match env("STREAM_MODE").map(|v| v.to_ascii_lowercase()).as_deref() {
            None | Some("vnc") => StreamMode::Vnc,
            Some("webrtc") => StreamMode::Webrtc,
            Some(v) => {
                problems.push(format!("STREAM_MODE must be vnc or webrtc, got {v:?}"));
                StreamMode::Vnc
            }
        };
        let webrtc_codec = match env("WEBRTC_CODEC").map(|v| v.to_ascii_lowercase()).as_deref() {
            None | Some("auto") => WebrtcCodec::Auto,
            Some("av1") => WebrtcCodec::Av1,
            Some("h265") => WebrtcCodec::H265,
            Some("h264") => WebrtcCodec::H264,
            Some("vp9") => WebrtcCodec::Vp9,
            Some("vp8") => WebrtcCodec::Vp8,
            Some(v) => {
                problems.push(format!("WEBRTC_CODEC must be auto, av1, h265, h264, vp9 or vp8, got {v:?}"));
                WebrtcCodec::Auto
            }
        };
        let webrtc_port: u16 = parse_num("WEBRTC_PORT", 6758)?;
        if webrtc_port == 0 {
            problems.push("WEBRTC_PORT must be a UDP port number".into());
        }
        let webrtc_host = env("WEBRTC_HOST");
        if let Some(h) = &webrtc_host
            && (h.contains('/') || h.contains(' '))
        {
            problems.push("WEBRTC_HOST must be a host name or IP address".into());
        }
        let session_hours: u64 = parse_num("APP_SESSION_TTL_HOURS", 12)?;
        let listen: SocketAddr = env("WORKER_LISTEN")
            .unwrap_or_else(|| "0.0.0.0:8000".into())
            .parse()
            .context("WORKER_LISTEN must be host:port")?;
        let code_listen: SocketAddr = env("CODE_LISTEN")
            .unwrap_or_else(|| "0.0.0.0:8001".into())
            .parse()
            .context("CODE_LISTEN must be host:port")?;
        let code_port: u16 = parse_num("CODE_PORT", 6759)?;
        let https_host = env("HTTPS_HOST")
            .map(|h| h.trim_matches(['[', ']']).to_ascii_lowercase())
            .filter(|h| !matches!(h.as_str(), "off" | "false" | "no" | "0"));
        if let Some(h) = &https_host
            && h != crate::tls::AUTO
            && let Err(e) = check_https_host(h)
        {
            problems.push(e);
        }
        let acme_listen: SocketAddr = env("ACME_LISTEN")
            .unwrap_or_else(|| "0.0.0.0:8080".into())
            .parse()
            .context("ACME_LISTEN must be host:port")?;

        let cfg = Config {
            auth_user,
            auth_password,
            public_url,
            session_ttl: Duration::from_secs(session_hours.clamp(1, 24 * 30) * 3600),
            download_dir,
            state_dir,
            delete_after_success: parse_bool("DELETE_AFTER_SUCCESS", true)?,
            min_free_disk_bytes: parse_num("MIN_FREE_DISK_BYTES", 2 * 1024 * 1024 * 1024)?,
            filecloud_base_url,
            upload_request_timeout: Duration::from_secs(timeout),
            max_retry_attempts,
            listen,
            ui_dir: PathBuf::from(env("UI_DIR").unwrap_or_else(|| "/usr/share/chromine/ui".into())),
            novnc_dir: PathBuf::from(env("NOVNC_DIR").unwrap_or_else(|| "/usr/share/novnc".into())),
            vnc_socket: PathBuf::from(env("VNC_SOCKET").unwrap_or_else(|| "/vnc/vnc.sock".into())),
            audio_socket: PathBuf::from(env("AUDIO_SOCKET").unwrap_or_else(|| "/vnc/audio.sock".into())),
            display_scale,
            stream_mode,
            webrtc_port,
            webrtc_host,
            webrtc_codec,
            ffmpeg_bin: PathBuf::from(env("FFMPEG_BIN").unwrap_or_else(|| "/usr/bin/ffmpeg".into())),
            code_socket: env("CODE_SOCKET").map(PathBuf::from),
            code_links_dir: PathBuf::from(env("CODE_LINKS_DIR").unwrap_or_else(|| "/code-links".into())),
            code_listen,
            code_port,
            code_public_url,
            https_host,
            acme_directory: env("ACME_DIRECTORY").unwrap_or_else(|| crate::tls::LETS_ENCRYPT.into()),
            acme_listen,
            dashboard_port: parse_num("DASHBOARD_PORT", 6757)?,
        };
        if !problems.is_empty() {
            bail!("invalid configuration:\n  - {}", problems.join("\n  - "));
        }
        Ok(cfg)
    }

    pub fn incoming_dir(&self) -> PathBuf {
        self.download_dir.join(chromine_shared::INCOMING_SUBDIR)
    }
    pub fn completed_dir(&self) -> PathBuf {
        self.download_dir.join(chromine_shared::EVENTS_SUBDIR).join(chromine_shared::COMPLETED_SUBDIR)
    }
    pub fn agent_status_file(&self) -> PathBuf {
        self.download_dir.join(chromine_shared::EVENTS_SUBDIR).join(chromine_shared::AGENT_STATUS_FILE)
    }
    pub fn jobs_dir(&self) -> PathBuf {
        self.download_dir.join(chromine_shared::JOBS_SUBDIR)
    }
    pub fn db_path(&self) -> PathBuf {
        self.state_dir.join("chromine.db")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn https_host_must_be_public() {
        for ok in ["203.0.113.5", "2001:db8::1", "chromine.example.com"] {
            assert!(check_https_host(ok).is_ok(), "{ok}");
        }
        for bad in [
            "127.0.0.1",
            "10.0.0.5",
            "192.168.1.2",
            "172.16.0.1",
            "::1",
            "fd00::1",
            "0.0.0.0",
            "localhost",
            "a b.com",
            "https://x.com",
        ] {
            assert!(check_https_host(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn secrets_are_not_printed() {
        let dbg = format!("{:?}", Secret::new("$argon2id$v=19$m=19456,t=2,p=1$c2FsdA$aGFzaA"));
        assert!(!dbg.contains("argon2"));
        assert_eq!(dbg, "<redacted>");
    }

    #[test]
    fn password_rules() {
        assert!(check_password("correct horse battery").is_ok());
        assert!(check_password("short").is_err());
        assert!(check_password("").is_err());
        let h = crate::auth::hash_password("correct horse battery").unwrap();
        assert!(check_password(&h).unwrap_err().contains("not a hash"));
        assert!(check_password("$2b$12$KIXQJ3pZ1g1i7Qn3QYy6UeZ0iV3t5w0W8s4m7K9JcF0Z2Vb8hH6Wa").is_err());
        assert!(check_password("$2ecret-but-fine!").is_ok());
    }

    #[test]
    fn base_url_rules() {
        assert_eq!(parse_base_url("https://filecloud24.cc/", false).unwrap().as_str(), "https://filecloud24.cc/");
        assert!(parse_base_url("http://filecloud24.cc", false).is_err());
        assert!(parse_base_url("http://127.0.0.1:9", true).is_ok());
        assert!(parse_base_url("https://x.cc/?a=b", false).is_err());
        assert!(parse_base_url("ftp://x.cc", false).is_err());
        assert!(parse_base_url("https://user:pw@x.cc", false).is_err());
    }
}

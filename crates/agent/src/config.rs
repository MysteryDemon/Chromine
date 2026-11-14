use std::path::PathBuf;

use anyhow::{Context, Result, bail};

pub const DEFAULT_MONITOR_STATE_FILE: &str = "/tmp/chromine-download-monitor.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Theme {
    Dark,
    Light,
}

impl Theme {
    pub fn name(self) -> &'static str {
        match self {
            Theme::Dark => "dark",
            Theme::Light => "light",
        }
    }

    pub fn gtk(self) -> &'static str {
        match self {
            Theme::Dark => "Adwaita:dark",
            Theme::Light => "Adwaita",
        }
    }
}

#[derive(Clone)]
pub struct AgentConfig {
    pub download_dir: PathBuf,
    pub profile_dir: PathBuf,
    pub chrome_bin: PathBuf,
    pub home_dir: PathBuf,
    pub display_number: u32,
    pub screen_width: u32,
    pub screen_height: u32,
    pub display_scale: f64,
    pub manage_display: bool,
    pub headless: bool,
    pub vnc_socket: PathBuf,
    pub audio: bool,
    pub audio_socket: PathBuf,
    pub pulseaudio_bin: PathBuf,
    pub parec_bin: PathBuf,
    pub video_dir: PathBuf,
    pub ytdlp_bin: PathBuf,
    pub min_free_disk_bytes: u64,
    pub safe_browsing: bool,
    pub theme: Theme,
    pub links_dir: PathBuf,
    pub monitor_state_file: PathBuf,
    pub start_urls: Vec<String>,
}

impl std::fmt::Debug for AgentConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentConfig")
            .field("download_dir", &self.download_dir)
            .field("profile_dir", &self.profile_dir)
            .field("chrome_bin", &self.chrome_bin)
            .field("display_number", &self.display_number)
            .field("screen", &format!("{}x{}", self.screen_width, self.screen_height))
            .field("display_scale", &self.display_scale)
            .field("manage_display", &self.manage_display)
            .field("vnc_socket", &self.vnc_socket)
            .field("audio", &self.audio)
            .field("audio_socket", &self.audio_socket)
            .field("video_dir", &self.video_dir)
            .field("ytdlp_bin", &self.ytdlp_bin)
            .field("min_free_disk_bytes", &self.min_free_disk_bytes)
            .field("safe_browsing", &self.safe_browsing)
            .field("theme", &self.theme)
            .field("links_dir", &self.links_dir)
            .field("monitor_state_file", &self.monitor_state_file)
            .finish()
    }
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

fn parse_u64(name: &str, default: u64) -> Result<u64> {
    match env(name) {
        Some(v) => v.parse().with_context(|| format!("{name} must be a non-negative integer, got {v:?}")),
        None => Ok(default),
    }
}

fn parse_f64(name: &str, default: f64) -> Result<f64> {
    match env(name) {
        Some(v) => v.parse().with_context(|| format!("{name} must be a number, got {v:?}")),
        None => Ok(default),
    }
}

fn parse_bool(name: &str, default: bool) -> Result<bool> {
    match env(name).as_deref() {
        None => Ok(default),
        Some("1" | "true" | "yes" | "on") => Ok(true),
        Some("0" | "false" | "no" | "off") => Ok(false),
        Some(v) => bail!("{name} must be true or false, got {v:?}"),
    }
}

impl AgentConfig {
    pub fn from_env() -> Result<Self> {
        let manage_display = parse_bool("AGENT_MANAGE_DISPLAY", true)?;
        let cfg = AgentConfig {
            download_dir: PathBuf::from(env("DOWNLOAD_DIR").unwrap_or_else(|| "/downloads".into())),
            profile_dir: PathBuf::from(env("CHROME_PROFILE_DIR").unwrap_or_else(|| "/profile".into())),
            chrome_bin: PathBuf::from(env("CHROME_BIN").unwrap_or_else(|| "/usr/bin/google-chrome-stable".into())),
            home_dir: PathBuf::from(env("HOME").unwrap_or_else(|| "/tmp/chrome-home".into())),
            display_number: parse_u64("DISPLAY_NUMBER", 1)? as u32,
            screen_width: parse_u64("SCREEN_WIDTH", 1600)? as u32,
            screen_height: parse_u64("SCREEN_HEIGHT", 900)? as u32,
            display_scale: parse_f64("DISPLAY_SCALE", 1.0)?,
            manage_display,
            headless: parse_bool("CHROME_HEADLESS", false)?,
            vnc_socket: PathBuf::from(env("VNC_SOCKET").unwrap_or_else(|| "/vnc/vnc.sock".into())),
            audio: parse_bool("AUDIO_ENABLED", true)?,
            audio_socket: PathBuf::from(env("AUDIO_SOCKET").unwrap_or_else(|| "/vnc/audio.sock".into())),
            pulseaudio_bin: PathBuf::from(env("PULSEAUDIO_BIN").unwrap_or_else(|| "pulseaudio".into())),
            parec_bin: PathBuf::from(env("PAREC_BIN").unwrap_or_else(|| "parec".into())),
            video_dir: PathBuf::from(env("VIDEO_DIR").unwrap_or_else(|| crate::video::DEFAULT_VIDEO_DIR.into())),
            ytdlp_bin: PathBuf::from(env("YTDLP_BIN").unwrap_or_else(|| "yt-dlp".into())),
            min_free_disk_bytes: parse_u64("MIN_FREE_DISK_BYTES", 2 * 1024 * 1024 * 1024)?,
            safe_browsing: parse_bool("CHROME_SAFE_BROWSING", false)?,
            theme: match env("DEFAULT_THEME").map(|v| v.to_ascii_lowercase()).as_deref() {
                None | Some("dark") => Theme::Dark,
                Some("light") => Theme::Light,
                Some(v) => bail!("DEFAULT_THEME must be dark or light, got {v:?}"),
            },
            links_dir: PathBuf::from(env("CODE_LINKS_DIR").unwrap_or_else(|| "/code-links".into())),
            monitor_state_file: PathBuf::from(
                env("MONITOR_STATE_FILE").unwrap_or_else(|| DEFAULT_MONITOR_STATE_FILE.into()),
            ),
            start_urls: Vec::new(),
        };
        if !cfg.download_dir.is_absolute()
            || !cfg.profile_dir.is_absolute()
            || !cfg.vnc_socket.is_absolute()
            || !cfg.audio_socket.is_absolute()
        {
            bail!("DOWNLOAD_DIR, CHROME_PROFILE_DIR, VNC_SOCKET and AUDIO_SOCKET must be absolute paths");
        }
        if !(320..=7680).contains(&cfg.screen_width) || !(240..=4320).contains(&cfg.screen_height) {
            bail!("SCREEN_WIDTH/SCREEN_HEIGHT are out of range");
        }
        if !(1.0..=3.0).contains(&cfg.display_scale) {
            bail!("DISPLAY_SCALE must be between 1 and 3");
        }
        Ok(cfg)
    }

    pub fn incoming_dir(&self) -> PathBuf {
        self.download_dir.join(chromine_shared::INCOMING_SUBDIR)
    }

    pub fn events_dir(&self) -> PathBuf {
        self.download_dir.join(chromine_shared::EVENTS_SUBDIR)
    }

    pub fn completed_dir(&self) -> PathBuf {
        self.events_dir().join(chromine_shared::COMPLETED_SUBDIR)
    }

    pub fn display(&self) -> String {
        format!(":{}", self.display_number)
    }
}

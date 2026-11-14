use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

use crate::config::AgentConfig;

pub const FORBIDDEN_FLAGS: &[&str] = &[
    "--no-sandbox",
    "--disable-setuid-sandbox",
    "--disable-web-security",
    "--ignore-certificate-errors",
    "--remote-debugging-port",
    "--remote-debugging-pipe",
    "--remote-debugging-address",
    "--remote-allow-origins",
    "--enable-automation",
    "--allow-running-insecure-content",
    "--disable-site-isolation-trials",
    "--single-process",
    "--no-zygote",
];

pub fn chrome_args(cfg: &AgentConfig, restore: bool) -> Vec<String> {
    let mut args = vec![
        format!("--user-data-dir={}", cfg.profile_dir.display()),
        "--no-first-run".to_string(),
        "--no-default-browser-check".to_string(),
        "--password-store=basic".to_string(),
        "--hide-crash-restore-bubble".to_string(),
    ];
    if cfg.headless {
        args.push("--headless=new".to_string());
    } else {
        args.push("--start-maximized".to_string());
        args.push("--hide-scrollbars".to_string());
        args.push(format!("--force-device-scale-factor={}", cfg.display_scale));
    }
    if restore {
        args.push("--restore-last-session".to_string());
    } else {
        args.extend(cfg.start_urls.iter().cloned());
    }
    args
}

pub fn clear_stale_profile_lock(profile: &Path) {
    for name in ["SingletonLock", "SingletonSocket", "SingletonCookie"] {
        let p = profile.join(name);
        if std::fs::symlink_metadata(&p).is_ok()
            && let Err(e) = std::fs::remove_file(&p)
        {
            tracing::warn!(file = %p.display(), error = %e, "could not remove stale Chrome lock");
        }
    }
}

pub struct ChromeProcess {
    pub child: Child,
    pub pid: i32,
}

impl ChromeProcess {
    pub fn request_exit(&self) {
        unsafe {
            libc::kill(self.pid, libc::SIGTERM);
        }
    }

    pub fn signal_group(&self, sig: i32) {
        unsafe {
            libc::kill(-self.pid, sig);
        }
    }
}

pub type SandboxNote = Arc<Mutex<Option<String>>>;

pub async fn version(chrome_bin: &Path) -> Option<String> {
    let out = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        Command::new(chrome_bin).arg("--version").stdin(Stdio::null()).output(),
    )
    .await
    .ok()?
    .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let v = text.split_whitespace().find(|w| w.chars().next().is_some_and(|c| c.is_ascii_digit()))?;
    Some(format!("Chrome/{v}"))
}

pub fn spawn(cfg: &AgentConfig, sandbox_note: SandboxNote, restore: bool) -> Result<ChromeProcess> {
    let args = chrome_args(cfg, restore);
    for arg in &args {
        if FORBIDDEN_FLAGS.iter().any(|f| arg.starts_with(f)) {
            bail!("refusing to launch Chrome with {arg}");
        }
    }
    for url in &cfg.start_urls {
        if url.starts_with('-') {
            bail!("start URL must not look like a switch: {url}");
        }
    }

    let mut cmd = command(cfg);
    cmd.args(&args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(false);
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = cmd.spawn().with_context(|| format!("failed to start {}", cfg.chrome_bin.display()))?;
    let pid = child.id().context("Chrome exited immediately")? as i32;

    for stream in [
        child.stdout.take().map(|s| Box::new(s) as Box<dyn tokio::io::AsyncRead + Unpin + Send>),
        child.stderr.take().map(|s| Box::new(s) as Box<dyn tokio::io::AsyncRead + Unpin + Send>),
    ]
    .into_iter()
    .flatten()
    {
        let note = sandbox_note.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stream).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if line.contains("No usable sandbox") || line.contains("sandbox") && line.contains("FATAL") {
                    tracing::error!(target: "chrome", "{line}");
                    *note.lock().await = Some(line.chars().take(400).collect());
                } else if line.contains("chromine_agent::") {
                    tracing::info!(target: "monitor", "{}", line.trim());
                } else if line.contains(":FATAL:") {
                    tracing::error!(target: "chrome", "{line}");
                } else {
                    tracing::debug!(target: "chrome", "{line}");
                }
            }
        });
    }
    Ok(ChromeProcess { child, pid })
}

fn command(cfg: &AgentConfig) -> Command {
    let mut cmd = Command::new(&cfg.chrome_bin);
    cmd.env_clear()
        .env("PATH", "/usr/local/bin:/usr/bin:/bin")
        .env("HOME", &cfg.home_dir)
        .env("XDG_CONFIG_HOME", cfg.home_dir.join(".config"))
        .env("XDG_CACHE_HOME", cfg.home_dir.join(".cache"))
        .env("DISPLAY", cfg.display())
        .env("GTK_THEME", cfg.theme.gtk())
        .env("DOWNLOAD_DIR", &cfg.download_dir)
        .env("MIN_FREE_DISK_BYTES", cfg.min_free_disk_bytes.to_string())
        .env("MONITOR_STATE_FILE", &cfg.monitor_state_file)
        .env("VIDEO_DIR", &cfg.video_dir)
        .env("PULSE_SERVER", crate::audio::pulse_server(cfg));
    for var in ["LANG", "LANGUAGE", "LC_ALL", "TZ", "XDG_RUNTIME_DIR", "LOG_LEVEL"] {
        if let Ok(v) = std::env::var(var) {
            cmd.env(var, v);
        }
    }
    cmd
}

pub async fn open_url(cfg: &AgentConfig, url: &str) -> Result<()> {
    if !crate::links::is_web_link(url) {
        bail!("not a web link");
    }
    let mut cmd = command(cfg);
    cmd.arg(format!("--user-data-dir={}", cfg.profile_dir.display()))
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let status = tokio::time::timeout(std::time::Duration::from_secs(15), cmd.status())
        .await
        .context("Chrome did not take the link within 15 s")??;
    if !status.success() {
        bail!("Chrome refused the link ({status})");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> AgentConfig {
        AgentConfig {
            download_dir: "/downloads".into(),
            profile_dir: "/profile".into(),
            chrome_bin: "/usr/bin/google-chrome-stable".into(),
            home_dir: "/tmp/h".into(),
            display_number: 1,
            screen_width: 1600,
            screen_height: 900,
            display_scale: 2.0,
            manage_display: true,
            headless: false,
            vnc_socket: "/vnc/vnc.sock".into(),
            audio: false,
            audio_socket: "/vnc/audio.sock".into(),
            pulseaudio_bin: "pulseaudio".into(),
            parec_bin: "parec".into(),
            video_dir: "/tmp/v".into(),
            ytdlp_bin: "yt-dlp".into(),
            min_free_disk_bytes: 0,
            safe_browsing: false,
            theme: crate::config::Theme::Dark,
            links_dir: "/code-links".into(),
            monitor_state_file: "/tmp/m.json".into(),
            start_urls: vec![],
        }
    }

    #[test]
    fn chrome_is_not_remote_controlled_and_keeps_its_security() {
        for headless in [false, true] {
            let mut c = cfg();
            c.headless = headless;
            let args = chrome_args(&c, false);
            for a in &args {
                assert!(!FORBIDDEN_FLAGS.iter().any(|f| a.starts_with(f)), "forbidden flag {a}");
                assert!(!a.contains("remote-debugging") && !a.contains("automation"), "{a}");
            }
            assert!(args.iter().any(|a| a == "--user-data-dir=/profile"));
            assert!(!args.iter().any(|a| a == "--force-dark-mode"));
        }
    }

    #[test]
    fn headed_chrome_fills_the_screen_without_scrollbars() {
        let args = chrome_args(&cfg(), false);
        assert!(args.contains(&"--start-maximized".to_string()));
        assert!(args.contains(&"--hide-scrollbars".to_string()));
        assert!(args.contains(&"--force-device-scale-factor=2".to_string()));
        assert!(
            !args.iter().any(|a| a.starts_with("--window-size") || a.starts_with("--window-position")),
            "a fixed window size stops Chrome from maximizing: {args:?}"
        );
        assert!(!args.contains(&"--restore-last-session".to_string()));
    }

    #[test]
    fn a_rescale_restart_reopens_the_previous_tabs() {
        let mut c = cfg();
        c.start_urls = vec!["https://example.com/".into()];
        c.display_scale = 1.5;
        let args = chrome_args(&c, true);
        assert!(args.contains(&"--restore-last-session".to_string()));
        assert!(args.contains(&"--force-device-scale-factor=1.5".to_string()));
        assert!(!args.contains(&"https://example.com/".to_string()));
    }

    #[test]
    fn stale_locks_are_removed() {
        let d = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink("otherhost-123", d.path().join("SingletonLock")).unwrap();
        std::fs::write(d.path().join("SingletonCookie"), b"").unwrap();
        std::fs::write(d.path().join("Local State"), b"{}").unwrap();
        clear_stale_profile_lock(d.path());
        assert!(std::fs::symlink_metadata(d.path().join("SingletonLock")).is_err());
        assert!(!d.path().join("SingletonCookie").exists());
        assert!(d.path().join("Local State").exists());
    }
}

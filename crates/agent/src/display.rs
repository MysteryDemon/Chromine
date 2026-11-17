use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};

use crate::config::AgentConfig;

pub struct DisplayStack {
    pub xvnc: Child,
    pub openbox: Child,
}

fn base_command(cfg: &AgentConfig, program: &str) -> Command {
    let mut c = Command::new(program);
    c.env_clear()
        .env("PATH", "/usr/local/bin:/usr/bin:/bin")
        .env("HOME", &cfg.home_dir)
        .env("DISPLAY", cfg.display())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    for var in ["LANG", "TZ", "XDG_RUNTIME_DIR"] {
        if let Ok(v) = std::env::var(var) {
            c.env(var, v);
        }
    }
    c
}

pub fn log_output(name: &'static str, child: &mut Child) {
    for stream in [
        child.stdout.take().map(|s| Box::new(s) as Box<dyn tokio::io::AsyncRead + Unpin + Send>),
        child.stderr.take().map(|s| Box::new(s) as Box<dyn tokio::io::AsyncRead + Unpin + Send>),
    ]
    .into_iter()
    .flatten()
    {
        tokio::spawn(async move {
            let mut lines = BufReader::new(stream).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::info!(target: "display", process = name, "{line}");
            }
        });
    }
}

pub fn xvnc_args(cfg: &AgentConfig) -> Vec<String> {
    vec![
        cfg.display(),
        "-geometry".into(),
        format!(
            "{}x{}",
            (cfg.screen_width as f64 * cfg.display_scale).round(),
            (cfg.screen_height as f64 * cfg.display_scale).round()
        ),
        "-depth".into(),
        "24".into(),
        "-rfbport".into(),
        "-1".into(),
        "-rfbunixpath".into(),
        cfg.vnc_socket.to_string_lossy().into_owned(),
        "-rfbunixmode".into(),
        "0660".into(),
        "-SecurityTypes".into(),
        "None".into(),
        "-AlwaysShared".into(),
        "-UseBlacklist=0".into(),
        "-desktop".into(),
        "Chromine".into(),
        "-nolisten".into(),
        "tcp".into(),
    ]
}

async fn wait_for_socket(path: &std::path::Path, child: &mut Child, timeout: Duration) -> Result<()> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if path.exists() {
            return Ok(());
        }
        if let Some(status) = child.try_wait()? {
            bail!("Xvnc exited during startup ({status})");
        }
        if tokio::time::Instant::now() > deadline {
            bail!("Xvnc did not create {} in time", path.display());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

pub async fn spawn_openbox(cfg: &AgentConfig) -> Result<Child> {
    let mut c = base_command(cfg, "openbox");
    c.arg("--sm-disable");
    let mut child = c.spawn().context("failed to start openbox")?;
    log_output("openbox", &mut child);
    Ok(child)
}

pub async fn start(cfg: &AgentConfig) -> Result<DisplayStack> {
    if std::fs::symlink_metadata(&cfg.vnc_socket).is_ok() {
        std::fs::remove_file(&cfg.vnc_socket)
            .with_context(|| format!("cannot remove the old VNC socket {}", cfg.vnc_socket.display()))?;
    }
    let mut xvnc_cmd = base_command(cfg, "Xvnc");
    xvnc_cmd.args(xvnc_args(cfg));
    let mut xvnc = xvnc_cmd.spawn().context("failed to start Xvnc")?;
    log_output("Xvnc", &mut xvnc);
    let socket = PathBuf::from(format!("/tmp/.X11-unix/X{}", cfg.display_number));
    wait_for_socket(&socket, &mut xvnc, Duration::from_secs(20)).await?;
    wait_for_socket(&cfg.vnc_socket, &mut xvnc, Duration::from_secs(20)).await?;
    tracing::info!(display = %cfg.display(), socket = %cfg.vnc_socket.display(), "virtual display ready");

    let openbox = spawn_openbox(cfg).await?;
    Ok(DisplayStack { xvnc, openbox })
}

pub async fn stop_child(child: &mut Child, grace: Duration) {
    if let Some(pid) = child.id() {
        unsafe {
            libc::kill(pid as i32, libc::SIGTERM);
        }
        if tokio::time::timeout(grace, child.wait()).await.is_err() {
            let _ = child.kill().await;
        }
    }
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
            scale_file: "/vnc/display-scale".into(),
            users_dir: "/users".into(),
            user: None,
        }
    }

    #[test]
    fn vnc_listens_only_on_the_unix_socket() {
        let s = xvnc_args(&cfg()).join(" ");
        assert!(s.contains("-rfbport -1"));
        assert!(s.contains("-rfbunixpath /vnc/vnc.sock"));
        assert!(s.contains("-geometry 3200x1800"));
        assert!(s.contains("-rfbunixmode 0660"));
        assert!(s.contains("-nolisten tcp"));
        assert!(s.contains("-UseBlacklist=0"), "every viewer arrives from the same socket path");
    }
}

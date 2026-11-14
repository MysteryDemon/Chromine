use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::process::{Child, Command};
use tokio::sync::{Semaphore, watch};

use crate::config::AgentConfig;

pub const SINK: &str = "chromine";
pub const RATE: u32 = 48_000;
pub const CHANNELS: u32 = 2;
const FRAME: usize = 4;
const MAX_LISTENERS: usize = 4;

pub fn pulse_dir(cfg: &AgentConfig) -> PathBuf {
    cfg.home_dir.join("pulse")
}

pub fn pulse_server(cfg: &AgentConfig) -> String {
    format!("unix:{}", pulse_dir(cfg).join("native").display())
}

fn command(cfg: &AgentConfig, program: &Path) -> Command {
    let mut c = Command::new(program);
    c.env_clear()
        .env("PATH", "/usr/local/bin:/usr/bin:/bin")
        .env("HOME", &cfg.home_dir)
        .env("PULSE_SERVER", pulse_server(cfg))
        .stdin(Stdio::null())
        .kill_on_drop(true);
    for var in ["LANG", "TZ"] {
        if let Ok(v) = std::env::var(var) {
            c.env(var, v);
        }
    }
    c
}

pub fn pulse_args(cfg: &AgentConfig) -> Vec<String> {
    vec![
        "--daemonize=no".into(),
        "--use-pid-file=no".into(),
        "--system=no".into(),
        "-n".into(),
        "--exit-idle-time=-1".into(),
        "--disallow-exit".into(),
        "--disallow-module-loading".into(),
        "--log-target=stderr".into(),
        "--log-level=error".into(),
        format!(
            "--load=module-native-protocol-unix socket={} auth-anonymous=1",
            pulse_dir(cfg).join("native").display()
        ),
        format!(
            "--load=module-null-sink sink_name={SINK} rate={RATE} channels={CHANNELS} sink_properties=device.description=Chromine"
        ),
    ]
}

pub fn parec_args() -> Vec<String> {
    vec![
        "--raw".into(),
        "--format=s16le".into(),
        format!("--rate={RATE}"),
        format!("--channels={CHANNELS}"),
        format!("--device={SINK}.monitor"),
        "--latency-msec=40".into(),
    ]
}

pub async fn start_pulse(cfg: &AgentConfig) -> Result<Child> {
    let dir = pulse_dir(cfg);
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)
        .with_context(|| format!("cannot create {}", dir.display()))?;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    let socket = dir.join("native");
    if std::fs::symlink_metadata(&socket).is_ok() {
        std::fs::remove_file(&socket).with_context(|| format!("cannot remove {}", socket.display()))?;
    }
    let mut c = command(cfg, &cfg.pulseaudio_bin);
    c.args(pulse_args(cfg)).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = c.spawn().context("failed to start PulseAudio")?;
    crate::display::log_output("pulseaudio", &mut child);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !socket.exists() {
        if let Some(status) = child.try_wait()? {
            bail!("PulseAudio exited during startup ({status})");
        }
        if tokio::time::Instant::now() > deadline {
            bail!("PulseAudio did not create {} in time", socket.display());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    tracing::info!(socket = %cfg.audio_socket.display(), "sound ready");
    Ok(child)
}

pub async fn run(cfg: AgentConfig, mut pulse: Child, mut shutdown: watch::Receiver<bool>) {
    let server = tokio::spawn(serve(cfg.clone(), shutdown.clone()));
    loop {
        tokio::select! {
            s = pulse.wait() => {
                tracing::warn!(status = ?s, "PulseAudio exited; restarting");
                tokio::time::sleep(Duration::from_secs(2)).await;
                match start_pulse(&cfg).await {
                    Ok(c) => pulse = c,
                    Err(e) => {
                        tracing::error!(error = %e, "sound is unavailable");
                        server.abort();
                        return;
                    }
                }
            }
            _ = shutdown.changed() => break,
        }
    }
    crate::display::stop_child(&mut pulse, Duration::from_secs(2)).await;
    let _ = server.await;
}

pub async fn serve(cfg: AgentConfig, mut shutdown: watch::Receiver<bool>) {
    let path = cfg.audio_socket.clone();
    if std::fs::symlink_metadata(&path).is_ok() {
        let _ = std::fs::remove_file(&path);
    }
    let listener = match UnixListener::bind(&path) {
        Ok(l) => l,
        Err(e) => {
            tracing::error!(error = %e, socket = %path.display(), "cannot open the sound socket");
            return;
        }
    };
    if let Err(e) = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o660)) {
        tracing::warn!(error = %e, "cannot restrict the sound socket");
    }
    let slots = Arc::new(Semaphore::new(MAX_LISTENERS));
    loop {
        tokio::select! {
            r = listener.accept() => match r {
                Ok((conn, _)) => {
                    let Ok(permit) = slots.clone().try_acquire_owned() else { continue };
                    let cfg = cfg.clone();
                    let sd = shutdown.clone();
                    tokio::spawn(async move {
                        stream(&cfg, conn, sd).await;
                        drop(permit);
                    });
                }
                Err(e) => {
                    tracing::warn!(error = %e, "sound socket accept failed");
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            },
            _ = shutdown.changed() => break,
        }
    }
    let _ = std::fs::remove_file(&path);
}

async fn stream(cfg: &AgentConfig, conn: UnixStream, mut shutdown: watch::Receiver<bool>) {
    let mut c = command(cfg, &cfg.parec_bin);
    c.args(parec_args()).stdout(Stdio::piped()).stderr(Stdio::null());
    let mut child = match c.spawn() {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, "cannot start sound capture");
            return;
        }
    };
    let Some(mut pcm) = child.stdout.take() else { return };
    let (mut rx, mut tx) = conn.into_split();
    let mut buf = vec![0u8; 16 * 1024];
    let mut held = 0usize;
    let mut probe = [0u8; 1];
    loop {
        tokio::select! {
            r = pcm.read(&mut buf[held..]) => {
                let n = match r {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                let filled = held + n;
                let whole = filled - filled % FRAME;
                if buf[..whole].iter().any(|&b| b != 0) && tx.write_all(&buf[..whole]).await.is_err() {
                    break;
                }
                buf.copy_within(whole..filled, 0);
                held = filled - whole;
            }
            r = rx.read(&mut probe) => {
                if matches!(r, Ok(0) | Err(_)) {
                    break;
                }
            }
            _ = shutdown.changed() => break,
        }
    }
    let _ = child.kill().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sound_server_has_no_network_listener() {
        let cfg = AgentConfig {
            download_dir: "/downloads".into(),
            profile_dir: "/profile".into(),
            chrome_bin: "/bin/false".into(),
            home_dir: "/tmp/chrome-home".into(),
            display_number: 1,
            screen_width: 1600,
            screen_height: 900,
            display_scale: 1.0,
            manage_display: true,
            headless: false,
            vnc_socket: "/vnc/vnc.sock".into(),
            audio: true,
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
        };
        let args = pulse_args(&cfg).join(" ");
        assert!(args.contains("-n "), "{args}");
        assert!(args.contains("--disallow-module-loading"));
        assert!(args.contains("module-native-protocol-unix socket=/tmp/chrome-home/pulse/native"));
        assert!(!args.contains("tcp") && !args.contains("zeroconf") && !args.contains("rtp"), "{args}");
        assert_eq!(pulse_server(&cfg), "unix:/tmp/chrome-home/pulse/native");
        assert!(parec_args().contains(&"--device=chromine.monitor".to_string()));
    }

    #[tokio::test]
    async fn streams_frames_and_skips_silence() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let fake = d.path().join("parec");
        let script = "#!/bin/sh\nhead -c 4096 /dev/zero\nsleep 0.2\nprintf 'abc'\nsleep 0.2\nprintf 'defgh'\nsleep 0.2\nhead -c 4096 /dev/zero\nsleep 0.2\nprintf 'ijkl'\nexec sleep 30\n";
        std::fs::write(&fake, script).unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let cfg = AgentConfig {
            download_dir: d.path().join("downloads"),
            profile_dir: d.path().join("profile"),
            chrome_bin: "/bin/false".into(),
            home_dir: d.path().to_path_buf(),
            display_number: 1,
            screen_width: 1280,
            screen_height: 800,
            display_scale: 1.0,
            manage_display: false,
            headless: true,
            vnc_socket: d.path().join("vnc.sock"),
            audio: true,
            audio_socket: d.path().join("audio.sock"),
            pulseaudio_bin: "pulseaudio".into(),
            parec_bin: fake,
            video_dir: d.path().join("video"),
            ytdlp_bin: "yt-dlp".into(),
            min_free_disk_bytes: 0,
            safe_browsing: false,
            theme: crate::config::Theme::Dark,
            links_dir: "/code-links".into(),
            monitor_state_file: d.path().join("monitor.json"),
            start_urls: vec![],
        };
        let (stop, shutdown) = watch::channel(false);
        let server = tokio::spawn(serve(cfg.clone(), shutdown));
        while !cfg.audio_socket.exists() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let mode = std::fs::metadata(&cfg.audio_socket).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o660);
        let mut conn = UnixStream::connect(&cfg.audio_socket).await.unwrap();
        let mut got = Vec::new();
        while got.len() < 12 {
            let mut b = [0u8; 8192];
            let n = tokio::time::timeout(Duration::from_secs(5), conn.read(&mut b)).await.unwrap().unwrap();
            assert!(n > 0);
            got.extend_from_slice(&b[..n]);
        }
        assert_eq!(got, b"abcdefghijkl", "whole frames arrive in order and silence is skipped");
        drop(conn);
        stop.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(5), server).await.unwrap().unwrap();
        assert!(!cfg.audio_socket.exists());
    }
}

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::{Duration, Instant};

use chromine_shared::{USERS_SUBDIR, UsersFile, is_valid_user_id, read_users_file};
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::Sandbox;
use crate::config::AgentConfig;

struct Instance {
    stop: watch::Sender<bool>,
    task: JoinHandle<()>,
    display: u32,
}

pub fn free_display(after: u32, taken: &HashSet<u32>, x_dir: &Path) -> u32 {
    (after + 1..)
        .find(|n| {
            !taken.contains(n)
                && !x_dir.join(format!(".X{n}-lock")).exists()
                && !x_dir.join(".X11-unix").join(format!("X{n}")).exists()
        })
        .expect("a free display number")
}

async fn keep_running(cfg: AgentConfig, sandbox: Sandbox, stop: watch::Receiver<bool>) {
    let id = cfg.user.clone().unwrap_or_default();
    let mut backoff = Duration::from_secs(2);
    loop {
        if *stop.borrow() {
            return;
        }
        let _ = std::fs::remove_dir_all(cfg.user_run_dir(&id));
        let started = Instant::now();
        match crate::run_instance(cfg.clone(), sandbox.clone(), stop.clone()).await {
            Ok(()) => {}
            Err(e) => tracing::error!(user = %id, error = format!("{e:#}"), "a user's browser stopped"),
        }
        if *stop.borrow() {
            return;
        }
        if started.elapsed() > Duration::from_secs(60) {
            backoff = Duration::from_secs(2);
        }
        let mut s = stop.clone();
        tokio::select! {
            _ = tokio::time::sleep(backoff) => {}
            _ = s.changed() => return,
        }
        backoff = (backoff * 2).min(Duration::from_secs(30));
    }
}

fn collect_garbage(base: &AgentConfig, users: &UsersFile, running: &HashMap<String, Instance>) {
    let roots = [base.users_dir.clone(), base.vnc_root().join(USERS_SUBDIR), base.download_dir.join(USERS_SUBDIR)];
    for root in roots {
        let Ok(rd) = std::fs::read_dir(&root) else { continue };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if !is_valid_user_id(&name) || users.known.contains(&name) || running.contains_key(&name) {
                continue;
            }
            match std::fs::remove_dir_all(e.path()) {
                Ok(()) => tracing::info!(user = %name, dir = %root.display(), "removed a deleted user's files"),
                Err(err) => tracing::debug!(user = %name, error = %err, "cannot remove a deleted user's files yet"),
            }
        }
    }
}

pub async fn supervise(base: AgentConfig, sandbox: Sandbox, mut shutdown: watch::Receiver<bool>) {
    let file = base.users_file();
    let mut running: HashMap<String, Instance> = HashMap::new();
    let mut tick = tokio::time::interval(Duration::from_secs(2));
    loop {
        tokio::select! {
            _ = tick.tick() => {}
            _ = shutdown.changed() => break,
        }
        running.retain(|_, i| !i.task.is_finished());
        let Some(users) = read_users_file(&file) else { continue };
        for (id, inst) in &running {
            if !users.active.contains(id) && !*inst.stop.borrow() {
                tracing::info!(user = %id, "user signed out; closing their browser");
                let _ = inst.stop.send(true);
            }
        }
        for id in &users.active {
            if running.contains_key(id) {
                continue;
            }
            let taken: HashSet<u32> = running.values().map(|i| i.display).collect();
            let display = free_display(base.display_number, &taken, Path::new("/tmp"));
            let cfg = base.for_user(id, display);
            tracing::info!(user = %id, display = %cfg.display(), "user signed in; starting their browser");
            let (stop, stop_rx) = watch::channel(false);
            let task = tokio::spawn(keep_running(cfg, sandbox.clone(), stop_rx));
            running.insert(id.clone(), Instance { stop, task, display });
        }
        collect_garbage(&base, &users, &running);
    }
    for i in running.values() {
        let _ = i.stop.send(true);
    }
    for (_, i) in running {
        let _ = tokio::time::timeout(Duration::from_secs(25), i.task).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_numbers_skip_taken_and_stale_ones() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join(".X2-lock"), "").unwrap();
        std::fs::create_dir(d.path().join(".X11-unix")).unwrap();
        std::fs::write(d.path().join(".X11-unix").join("X4"), "").unwrap();
        let taken: HashSet<u32> = [3].into();
        assert_eq!(free_display(1, &taken, d.path()), 5);
        assert_eq!(free_display(1, &HashSet::new(), d.path()), 3);
    }

    #[test]
    fn only_files_of_deleted_users_are_removed() {
        let d = tempfile::tempdir().unwrap();
        let keep = "0123456789abcdef0123456789abcdef";
        let gone = "fedcba9876543210fedcba9876543210";
        let base = AgentConfig {
            download_dir: d.path().join("downloads"),
            profile_dir: d.path().join("profile"),
            chrome_bin: "/bin/false".into(),
            home_dir: d.path().join("home"),
            display_number: 1,
            screen_width: 1600,
            screen_height: 900,
            display_scale: 1.0,
            manage_display: true,
            headless: false,
            vnc_socket: d.path().join("vnc").join("vnc.sock"),
            audio: false,
            audio_socket: d.path().join("vnc").join("audio.sock"),
            pulseaudio_bin: "pulseaudio".into(),
            parec_bin: "parec".into(),
            video_dir: d.path().join("video"),
            ytdlp_bin: "yt-dlp".into(),
            min_free_disk_bytes: 0,
            safe_browsing: false,
            theme: crate::config::Theme::Dark,
            links_dir: d.path().join("links"),
            monitor_state_file: d.path().join("m.json"),
            start_urls: vec![],
            scale_file: d.path().join("vnc").join("display-scale"),
            users_dir: d.path().join("users"),
            user: None,
        };
        let roots = [d.path().join("users"), d.path().join("vnc/users"), d.path().join("downloads/users")];
        for root in &roots {
            for id in [keep, gone, "not-a-user"] {
                std::fs::create_dir_all(root.join(id).join("inner")).unwrap();
            }
        }
        let users = UsersFile { schema: 1, known: vec![keep.into()], active: vec![], ..Default::default() };
        collect_garbage(&base, &users, &HashMap::new());
        for root in &roots {
            assert!(root.join(keep).exists());
            assert!(root.join("not-a-user").exists());
            assert!(!root.join(gone).exists());
        }
    }
}

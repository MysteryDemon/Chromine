use std::io::Read;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use chromine_shared::{OpenedLink, fsutil, now_ms};
use tokio::sync::watch;

use crate::Shared;
use crate::config::AgentConfig;

const MAX_LINK: usize = 8192;

pub fn is_web_link(url: &str) -> bool {
    let rest = ["https://", "http://"]
        .iter()
        .find_map(|p| url.get(..p.len()).filter(|head| head.eq_ignore_ascii_case(p)).map(|_| &url[p.len()..]));
    rest.is_some_and(|r| !r.is_empty() && !r.starts_with('/'))
        && url.len() <= MAX_LINK
        && !url.chars().any(|c| c.is_whitespace() || c.is_control())
}

pub fn take_links(dir: &Path) -> Vec<String> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut names: Vec<String> = rd
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n.ends_with(".url") && !n.starts_with('.'))
        .collect();
    names.sort();
    let mut links = Vec::new();
    for name in names {
        let path = dir.join(&name);
        let text = fsutil::open_regular_nofollow(&path).ok().and_then(|(file, meta)| {
            let mut s = String::new();
            (meta.len() as usize <= MAX_LINK && file.take(MAX_LINK as u64).read_to_string(&mut s).is_ok()).then_some(s)
        });
        let _ = std::fs::remove_file(&path);
        match text.map(|t| t.trim().to_string()) {
            Some(url) if is_web_link(&url) => links.push(url),
            _ => tracing::warn!(file = %name, "ignored a link from VS Code that is not an http(s) address"),
        }
    }
    links
}

pub async fn run(cfg: AgentConfig, shared: Arc<Shared>, mut shutdown: watch::Receiver<bool>) {
    let mut tick = tokio::time::interval(Duration::from_millis(700));
    loop {
        tokio::select! {
            _ = tick.tick() => {}
            _ = shutdown.changed() => return,
        }
        if !shared.chrome_running.load(Ordering::SeqCst) {
            continue;
        }
        let dir = cfg.links_dir.clone();
        let links = tokio::task::spawn_blocking(move || take_links(&dir)).await.unwrap_or_default();
        for url in links {
            match crate::chrome::open_url(&cfg, &url).await {
                Ok(()) => {
                    tracing::info!("opened a link from VS Code in the remote browser");
                    *shared.last_link.lock().unwrap() =
                        Some(OpenedLink { url: url.chars().take(300).collect(), at_ms: now_ms() });
                }
                Err(e) => tracing::warn!(error = %e, "could not open a link from VS Code"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_web_links_reach_chrome() {
        for ok in ["https://auth.openai.com/oauth/authorize?a=1&b=2", "http://localhost:1455/x", "HTTPS://Example.com"]
        {
            assert!(is_web_link(ok), "{ok}");
        }
        for bad in [
            "--remote-debugging-port=9222",
            "https://",
            "https:///etc/passwd",
            "file:///etc/passwd",
            "javascript:alert(1)",
            "chrome://settings",
            "https://a.com/ --remote-debugging-port=1",
            "https://a.com/\n--x",
            "",
        ] {
            assert!(!is_web_link(bad), "{bad:?}");
        }
        assert!(!is_web_link(&format!("https://a.com/{}", "a".repeat(MAX_LINK))));
    }

    #[test]
    fn links_are_read_once_in_order_and_junk_is_dropped() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("2-b.url"), "https://example.com/b\n").unwrap();
        std::fs::write(d.path().join("1-a.url"), "https://example.com/a").unwrap();
        std::fs::write(d.path().join("3.url"), "--remote-debugging-port=9222").unwrap();
        std::fs::write(d.path().join(".link.tmp"), "https://example.com/unfinished").unwrap();
        std::fs::write(d.path().join("secret.txt"), "x").unwrap();
        std::os::unix::fs::symlink(d.path().join("secret.txt"), d.path().join("4.url")).unwrap();
        assert_eq!(take_links(d.path()), vec!["https://example.com/a", "https://example.com/b"]);
        let mut left: Vec<String> =
            std::fs::read_dir(d.path()).unwrap().flatten().map(|e| e.file_name().into_string().unwrap()).collect();
        left.sort();
        assert_eq!(left, vec![".link.tmp", "secret.txt"]);
        assert!(take_links(d.path()).is_empty());
        assert!(take_links(&d.path().join("missing")).is_empty());
    }
}

use std::collections::HashMap;
use std::net::IpAddr;
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use argon2::password_hash::phc::PasswordHash;
use argon2::password_hash::{PasswordHasher, PasswordVerifier};
use base64::Engine;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

pub const SESSION_COOKIE: &str = "chromine_session";
pub const PASSWORD_FILE: &str = "password.hash";

pub fn hash_password(password: &str) -> Result<String> {
    let hash =
        argon2::Argon2::default().hash_password(password.as_bytes()).map_err(|e| anyhow!("hashing failed: {e}"))?;
    Ok(hash.to_string())
}

pub fn verify_password(password: &str, hash: &str) -> bool {
    PasswordHash::new(hash)
        .map(|h| argon2::Argon2::default().verify_password(password.as_bytes(), &h).is_ok())
        .unwrap_or(false)
}

pub fn stored_password_hash(state_dir: &Path, password: &str) -> Result<(String, bool)> {
    if let Ok(stored) = std::fs::read_to_string(state_dir.join(PASSWORD_FILE)) {
        let stored = stored.trim();
        if verify_password(password, stored) {
            return Ok((stored.to_string(), false));
        }
    }
    let hash = hash_password(password)?;
    chromine_shared::fsutil::atomic_write(state_dir, PASSWORD_FILE, hash.as_bytes(), 0o600)?;
    Ok((hash, true))
}

pub fn ct_eq(a: &str, b: &str) -> bool {
    a.len() == b.len() && bool::from(a.as_bytes().ct_eq(b.as_bytes()))
}

pub fn random_token() -> String {
    let mut b = [0u8; 32];
    for x in b.iter_mut() {
        *x = rand::random();
    }
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
}

pub fn session_hash(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

pub struct LoginLimiter {
    window: Duration,
    max_failures: u32,
    state: Mutex<HashMap<IpAddr, (u32, Instant)>>,
}

impl LoginLimiter {
    pub fn new(max_failures: u32, window: Duration) -> Self {
        LoginLimiter { window, max_failures, state: Mutex::new(HashMap::new()) }
    }

    pub fn allowed(&self, ip: IpAddr) -> bool {
        let mut s = self.state.lock().unwrap();
        match s.get(&ip) {
            Some((n, since)) if since.elapsed() < self.window => *n < self.max_failures,
            Some(_) => {
                s.remove(&ip);
                true
            }
            None => true,
        }
    }

    pub fn failed(&self, ip: IpAddr) {
        let mut s = self.state.lock().unwrap();
        if s.len() > 10_000 {
            s.retain(|_, (_, since)| since.elapsed() < self.window);
        }
        let e = s.entry(ip).or_insert((0, Instant::now()));
        if e.1.elapsed() >= self.window {
            *e = (0, Instant::now());
        }
        e.0 += 1;
    }

    pub fn succeeded(&self, ip: IpAddr) {
        self.state.lock().unwrap().remove(&ip);
    }
}

pub fn origin_allowed(origin: Option<&str>, forwarded_host: Option<&str>, public_url: Option<&url::Url>) -> bool {
    let Some(origin) = origin else { return false };
    let Ok(o) = url::Url::parse(origin) else { return false };
    if let Some(p) = public_url {
        return o.origin() == p.origin();
    }
    let Some(host) = forwarded_host else { return false };
    let origin_host = match (o.host_str(), o.port()) {
        (Some(h), Some(p)) => format!("{h}:{p}"),
        (Some(h), None) => h.to_string(),
        _ => return false,
    };
    let strip_default = |h: &str| -> String {
        h.strip_suffix(":80").or_else(|| h.strip_suffix(":443")).unwrap_or(h).to_ascii_lowercase()
    };
    strip_default(&origin_host) == strip_default(host)
}

pub fn safe_next(next: Option<&str>) -> String {
    match next {
        Some(n) if n.starts_with('/') && !n.starts_with("//") && !n.starts_with("/\\") && !n.contains(['\r', '\n']) => {
            n.to_string()
        }
        _ => "/".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argon2_round_trip() {
        let h = hash_password("s3cret pass").unwrap();
        assert!(h.starts_with("$argon2id$"));
        assert!(verify_password("s3cret pass", &h));
        assert!(!verify_password("wrong", &h));
        assert!(!verify_password("s3cret pass", "s3cret pass"));
    }

    #[test]
    fn a_new_password_is_detected_and_remembered() {
        let d = tempfile::tempdir().unwrap();
        let (first, changed) = stored_password_hash(d.path(), "first password 1").unwrap();
        assert!(changed, "no stored hash yet");
        let (again, changed) = stored_password_hash(d.path(), "first password 1").unwrap();
        assert!(!changed && again == first, "same password keeps the stored hash");
        let (second, changed) = stored_password_hash(d.path(), "second password 2").unwrap();
        assert!(changed && second != first);
        assert!(verify_password("second password 2", &second));
        let mode = std::os::unix::fs::PermissionsExt::mode(
            &std::fs::metadata(d.path().join(PASSWORD_FILE)).unwrap().permissions(),
        );
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn limiter_blocks_after_failures() {
        let l = LoginLimiter::new(3, Duration::from_secs(60));
        let ip: IpAddr = "10.0.0.1".parse().unwrap();
        for _ in 0..3 {
            assert!(l.allowed(ip));
            l.failed(ip);
        }
        assert!(!l.allowed(ip));
        let other: IpAddr = "10.0.0.2".parse().unwrap();
        assert!(l.allowed(other));
        l.succeeded(ip);
        assert!(l.allowed(ip));
    }

    #[test]
    fn origin_rules() {
        assert!(origin_allowed(Some("http://127.0.0.1:6757"), Some("127.0.0.1:6757"), None));
        assert!(!origin_allowed(Some("http://evil.example"), Some("127.0.0.1:6757"), None));
        assert!(!origin_allowed(None, Some("127.0.0.1:6757"), None));
        assert!(!origin_allowed(Some("null"), Some("127.0.0.1:6757"), None));
        assert!(origin_allowed(Some("https://chrome.example.com"), Some("chrome.example.com:443"), None));
        let p = url::Url::parse("https://chrome.example.com").unwrap();
        assert!(origin_allowed(Some("https://chrome.example.com"), Some("anything"), Some(&p)));
        assert!(!origin_allowed(Some("http://chrome.example.com"), Some("chrome.example.com"), Some(&p)));
    }

    #[test]
    fn next_redirects_stay_local() {
        assert_eq!(safe_next(Some("/browser/vnc.html")), "/browser/vnc.html");
        assert_eq!(safe_next(Some("//evil.example")), "/");
        assert_eq!(safe_next(Some("https://evil.example")), "/");
        assert_eq!(safe_next(Some("/\\evil")), "/");
        assert_eq!(safe_next(None), "/");
    }

    #[test]
    fn tokens_are_unique_and_hashed() {
        let a = random_token();
        let b = random_token();
        assert_ne!(a, b);
        assert_eq!(session_hash(&a).len(), 64);
        assert!(ct_eq(&a, &a.clone()));
        assert!(!ct_eq(&a, &b));
    }
}

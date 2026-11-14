use std::sync::OnceLock;
use std::sync::RwLock;

static KNOWN_SECRETS: OnceLock<RwLock<Vec<String>>> = OnceLock::new();

pub fn register_secret(s: &str) {
    if s.len() < 6 {
        return;
    }
    let lock = KNOWN_SECRETS.get_or_init(|| RwLock::new(Vec::new()));
    let mut v = lock.write().unwrap();
    if !v.iter().any(|x| x == s) {
        v.push(s.to_string());
    }
}

fn redact_headers(s: &str) -> String {
    let mut out = s.to_string();
    for key in ["x-upload-token", "authorization", "cookie", "upload_token"] {
        loop {
            let lower = out.to_ascii_lowercase();
            let Some(pos) = lower.find(key) else { break };
            let after = pos + key.len();
            let rest = &out[after..];
            let skip = rest
                .chars()
                .take_while(|c| matches!(c, ':' | '=' | ' ' | '"' | '\''))
                .map(char::len_utf8)
                .sum::<usize>();
            if skip == 0 {
                out.replace_range(pos..after, &"*".repeat(key.len()));
                continue;
            }
            let mut value_start = after + skip;
            let tail = out[value_start..].to_ascii_lowercase();
            for scheme in ["basic ", "bearer ", "token "] {
                if tail.starts_with(scheme) {
                    value_start += scheme.len();
                    break;
                }
            }
            let value_len = out[value_start..]
                .chars()
                .take_while(|c| !c.is_whitespace() && *c != '"' && *c != '\'' && *c != ',' && *c != '}')
                .map(char::len_utf8)
                .sum::<usize>();
            out.replace_range(pos..value_start + value_len, &format!("{}<redacted>", "*".repeat(key.len())));
        }
    }
    out
}

pub fn sanitize(msg: &str) -> String {
    let mut s = msg.to_string();
    if let Some(lock) = KNOWN_SECRETS.get() {
        for secret in lock.read().unwrap().iter() {
            if s.contains(secret.as_str()) {
                s = s.replace(secret.as_str(), "<redacted>");
            }
        }
    }
    s = redact_headers(&s);
    let s: String = s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    let s = s.trim();
    if s.chars().count() > 600 {
        let mut t: String = s.chars().take(600).collect();
        t.push('…');
        t
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_headers_and_registered_secrets() {
        register_secret("0123456789abcdef0123456789abcdef");
        let s = sanitize("X-Upload-Token: abcSECRETdef failed; secret=0123456789abcdef0123456789abcdef");
        assert!(!s.contains("abcSECRETdef"), "{s}");
        assert!(!s.contains("0123456789abcdef0123456789abcdef"), "{s}");
        let s = sanitize("{\"upload_token\": \"tok-123456\", \"x\": 1}");
        assert!(!s.contains("tok-123456"), "{s}");
        let s = sanitize("Authorization: Basic Y2hyb21pbmU6c2VjcmV0");
        assert!(!s.contains("Y2hyb21pbmU6c2VjcmV0"), "{s}");
    }

    #[test]
    fn strips_control_chars_and_bounds_length() {
        let s = sanitize("line1\nline2\u{7}");
        assert_eq!(s, "line1 line2");
        assert!(sanitize(&"x".repeat(5000)).chars().count() <= 601);
        assert_eq!(sanitize("HTTP 503 at 12:30"), "HTTP 503 at 12:30");
    }
}

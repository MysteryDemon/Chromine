use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use base64::Engine;
use sha2::{Digest, Sha256};

pub struct UiAssets {
    root: PathBuf,
    html_csp: HashMap<PathBuf, String>,
}

pub fn inline_scripts(html: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let lower = html.to_ascii_lowercase();
    let mut pos = 0;
    while let Some(start) = lower[pos..].find("<script") {
        let tag_start = pos + start;
        let Some(tag_end_rel) = lower[tag_start..].find('>') else { break };
        let tag_end = tag_start + tag_end_rel;
        let attrs = &lower[tag_start + 7..tag_end];
        let Some(close_rel) = lower[tag_end + 1..].find("</script") else { break };
        let body_end = tag_end + 1 + close_rel;
        if !attrs.contains("src=") {
            out.push(&html[tag_end + 1..body_end]);
        }
        pos = body_end + 8;
    }
    out
}

pub fn csp_for_html(html: &str) -> String {
    let mut hashes: Vec<String> = inline_scripts(html)
        .into_iter()
        .map(|s| format!("'sha256-{}'", base64::engine::general_purpose::STANDARD.encode(Sha256::digest(s.as_bytes()))))
        .collect();
    hashes.sort();
    hashes.dedup();
    format!(
        "default-src 'self'; script-src 'self' {}; style-src 'self' 'unsafe-inline'; img-src 'self' data:; \
         font-src 'self' data:; connect-src 'self'; frame-src 'self'; frame-ancestors 'self'; base-uri 'none'; \
         form-action 'self'; object-src 'none'",
        hashes.join(" ")
    )
}

pub fn is_public(path: &str) -> bool {
    path == "/login"
        || path == "/manifest.webmanifest"
        || path.starts_with("/_next/static/")
        || path.starts_with("/icons/")
}

impl UiAssets {
    pub fn load(root: &Path) -> Result<UiAssets> {
        let canonical = root.canonicalize().with_context(|| {
            format!("UI directory {} not found (build it with `npm run build` in ui/, or set UI_DIR)", root.display())
        })?;
        if !canonical.join("index.html").is_file() || !canonical.join("login.html").is_file() {
            bail!("UI directory {} does not contain index.html and login.html", canonical.display());
        }
        Self::load_dir(&canonical, &[])
    }

    pub fn load_dir(root: &Path, required: &[&str]) -> Result<UiAssets> {
        let root = root.canonicalize().with_context(|| format!("{} not found", root.display()))?;
        for f in required {
            if !root.join(f).is_file() {
                bail!("{} does not contain {f}", root.display());
            }
        }
        let mut html_csp = HashMap::new();
        let mut stack = vec![root.clone()];
        while let Some(dir) = stack.pop() {
            for e in std::fs::read_dir(&dir)? {
                let e = e?;
                let ft = e.file_type()?;
                if ft.is_dir() {
                    stack.push(e.path());
                } else if ft.is_file() && e.path().extension().and_then(|x| x.to_str()) == Some("html") {
                    let html = std::fs::read_to_string(e.path())?;
                    html_csp.insert(e.path(), csp_for_html(&html));
                }
            }
        }
        Ok(UiAssets { root, html_csp })
    }

    pub fn allow_frames(mut self, source: Option<String>) -> UiAssets {
        if let Some(source) = source {
            for csp in self.html_csp.values_mut() {
                *csp = csp.replace("frame-src 'self';", &format!("frame-src 'self' {source};"));
            }
        }
        self
    }

    pub fn resolve(&self, url_path: &str) -> Option<PathBuf> {
        let rel = url_path.trim_start_matches('/');
        let rel = if rel.is_empty() { "index.html" } else { rel };
        let p = Path::new(rel);
        if !p.components().all(|c| matches!(c, Component::Normal(_))) {
            return None;
        }
        let candidate = self.root.join(p);
        let file = if candidate.is_file() {
            candidate
        } else if p.extension().is_none() && self.root.join(format!("{rel}.html")).is_file() {
            self.root.join(format!("{rel}.html"))
        } else {
            return None;
        };
        let real = file.canonicalize().ok()?;
        real.starts_with(&self.root).then_some(real)
    }

    pub async fn respond(&self, url_path: &str) -> Response {
        let Some(file) = self.resolve(url_path) else {
            return self.not_found().await;
        };
        self.send(&file, StatusCode::OK).await
    }

    async fn not_found(&self) -> Response {
        let f = self.root.join("404.html");
        if f.is_file() {
            return self.send(&f, StatusCode::NOT_FOUND).await;
        }
        (StatusCode::NOT_FOUND, "not found").into_response()
    }

    async fn send(&self, file: &Path, status: StatusCode) -> Response {
        let Ok(body) = tokio::fs::read(file).await else {
            return (StatusCode::NOT_FOUND, "not found").into_response();
        };
        let mime = mime_guess::from_path(file).first_or_octet_stream();
        let ctype = if file.extension().is_some_and(|x| x == "webmanifest") {
            "application/manifest+json; charset=utf-8".to_string()
        } else if mime.type_() == "text" || mime.essence_str() == "application/javascript" {
            format!("{}; charset=utf-8", mime.essence_str())
        } else {
            mime.essence_str().to_string()
        };
        let mut r = (status, body).into_response();
        let h = r.headers_mut();
        if let Ok(v) = HeaderValue::from_str(&ctype) {
            h.insert(header::CONTENT_TYPE, v);
        }
        if file.strip_prefix(&self.root).is_ok_and(|p| p.starts_with("_next/static")) {
            h.insert(header::CACHE_CONTROL, HeaderValue::from_static("public, max-age=31536000, immutable"));
        } else {
            h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        }
        if let Some(csp) = self.html_csp.get(file)
            && let Ok(v) = HeaderValue::from_str(csp)
        {
            h.insert(header::CONTENT_SECURITY_POLICY, v);
        }
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_only_inline_scripts() {
        let html = r#"<html><script src="/_next/a.js" async></script><SCRIPT>self.a=1</SCRIPT><script id="_R_">b()</script>
            <script type="application/json">{"x":1}</script></html>"#;
        assert_eq!(inline_scripts(html), vec!["self.a=1", "b()", r#"{"x":1}"#]);
        let csp = csp_for_html("<script>alert(1)</script>");
        assert!(csp.contains("'sha256-bhHHL3z2vDgxUt0W3dWQOrprscmda2Y5pLsLg4GF+pI='"), "{csp}");
        assert!(!csp.contains("script-src 'self' 'unsafe-inline'"));
    }

    #[test]
    fn resolves_inside_root_only() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("index.html"), "<p>i</p>").unwrap();
        std::fs::write(d.path().join("login.html"), "<p>l</p>").unwrap();
        std::fs::create_dir_all(d.path().join("_next/static/c")).unwrap();
        std::fs::write(d.path().join("_next/static/c/a.js"), "x").unwrap();
        std::fs::write(d.path().parent().unwrap().join("outside.txt"), "secret").ok();
        std::os::unix::fs::symlink("/etc/passwd", d.path().join("passwd")).unwrap();
        let ui = UiAssets::load(d.path()).unwrap();
        assert!(ui.resolve("/").unwrap().ends_with("index.html"));
        assert!(ui.resolve("/login").unwrap().ends_with("login.html"));
        assert!(ui.resolve("/_next/static/c/a.js").is_some());
        assert!(ui.resolve("/../outside.txt").is_none());
        assert!(ui.resolve("/_next/../../etc/passwd").is_none());
        assert!(ui.resolve("/passwd").is_none(), "symlinks out of the export are refused");
        assert!(ui.resolve("/missing").is_none());
        assert!(is_public("/login") && is_public("/_next/static/c/a.js"));
        assert!(is_public("/manifest.webmanifest") && is_public("/icons/icon-192.png"));
        assert!(!is_public("/") && !is_public("/index.txt") && !is_public("/_next/other"));
        assert!(!is_public("/manifest.webmanifest.bak") && !is_public("/iconsx/a.png"));
    }
}

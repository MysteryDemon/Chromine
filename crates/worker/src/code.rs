use std::path::Path;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum_extra::extract::cookie::CookieJar;
use chromine_shared::fsutil;
use hyper_util::rt::TokioIo;

use crate::auth::{self, SESSION_COOKIE};
use crate::web::{Shared, authenticate};

const HOP_BY_HOP: [&str; 8] = [
    "connection",
    "keep-alive",
    "proxy-connection",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "upgrade",
];

const OPEN_JS: &str = include_str!("code-open.js");
const OPEN_TAG: &str = "<script src=\"/_chromine/open.js\"></script>";
const MAX_LINK: usize = 8192;
const MAX_PAGE: usize = 8 << 20;

pub fn router(st: Shared) -> Router {
    Router::new()
        .route("/_chromine/open.js", get(open_js))
        .route("/_chromine/open", post(open_link))
        .fallback(proxy)
        .with_state(st)
}

async fn open_js(State(st): State<Shared>, jar: CookieJar) -> Response {
    if authenticate(&st, &jar).await.is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    (
        [
            (header::CONTENT_TYPE, HeaderValue::from_static("text/javascript; charset=utf-8")),
            (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
        ],
        OPEN_JS,
    )
        .into_response()
}

fn same_origin(st: &Shared, headers: &HeaderMap) -> bool {
    let origin = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok());
    let host = headers.get(header::HOST).and_then(|v| v.to_str().ok());
    auth::origin_allowed(origin, host, st.cfg.code_public_url.as_ref())
}

pub fn web_link(raw: &str) -> Option<String> {
    let url = url::Url::parse(raw.trim()).ok()?;
    (matches!(url.scheme(), "http" | "https") && url.host_str().is_some_and(|h| !h.is_empty()) && raw.len() <= MAX_LINK)
        .then(|| url.into())
}

async fn open_link(State(st): State<Shared>, jar: CookieJar, headers: HeaderMap, body: Bytes) -> Response {
    let Some(me) = authenticate(&st, &jar).await else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    if !same_origin(&st, &headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(url) = std::str::from_utf8(&body).ok().and_then(web_link) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos();
    let name = format!("{nanos}-w{:08x}.url", rand::random::<u32>());
    match fsutil::atomic_write(&st.cfg.code_links_dir_of(me.owner()), &name, format!("{url}\n").as_bytes(), 0o660) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => {
            tracing::warn!(error = %e, "cannot hand a VS Code link to the remote browser");
            StatusCode::SERVICE_UNAVAILABLE.into_response()
        }
    }
}

pub fn inject_opener(html: &[u8]) -> Vec<u8> {
    let at = html.windows(6).position(|w| w.eq_ignore_ascii_case(b"<head>")).map(|p| p + 6).unwrap_or(0);
    [&html[..at], OPEN_TAG.as_bytes(), &html[at..]].concat()
}

fn page(status: StatusCode, title: &str, text: &str) -> Response {
    page_with(status, title, text, "")
}

fn page_with(status: StatusCode, title: &str, text: &str, head: &str) -> Response {
    let html = format!(
        "<!doctype html><meta charset=utf-8><meta name=viewport content=\"width=device-width\">{head}<title>{title}</title>\
         <body style=\"margin:0;display:grid;place-items:center;height:100vh;background:#000;color:#aaa;font:15px system-ui\">\
         <p style=\"max-width:28rem;padding:0 16px;text-align:center\">{text}</p>"
    );
    let mut r = (status, Html(html)).into_response();
    r.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r
}

fn is_upgrade(headers: &HeaderMap) -> bool {
    headers.get(header::UPGRADE).is_some()
        && headers
            .get(header::CONNECTION)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.split(',').any(|t| t.trim().eq_ignore_ascii_case("upgrade")))
}

pub fn without_session_cookie(headers: &HeaderMap) -> Option<HeaderValue> {
    let kept: Vec<&str> = headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .map(str::trim)
        .filter(|c| !c.is_empty() && c.split('=').next().map(str::trim) != Some(SESSION_COOKIE))
        .collect();
    (!kept.is_empty()).then(|| HeaderValue::from_str(&kept.join("; ")).ok()).flatten()
}

fn upstream_headers(from: &HeaderMap, upgrade: bool) -> HeaderMap {
    let mut h = HeaderMap::new();
    for (name, value) in from {
        if HOP_BY_HOP.contains(&name.as_str()) || name == header::COOKIE {
            continue;
        }
        h.append(name, value.clone());
    }
    if let Some(c) = without_session_cookie(from) {
        h.insert(header::COOKIE, c);
    }
    if upgrade {
        h.insert(header::CONNECTION, HeaderValue::from_static("upgrade"));
        if let Some(u) = from.get(header::UPGRADE) {
            h.insert(header::UPGRADE, u.clone());
        }
    }
    h
}

async fn proxy(State(st): State<Shared>, jar: CookieJar, mut req: Request) -> Response {
    if st.cfg.code_socket.is_none() {
        return page(StatusCode::NOT_FOUND, "VS Code", "VS Code is not set up on this server.");
    }
    let Some(me) = authenticate(&st, &jar).await else {
        return page(
            StatusCode::UNAUTHORIZED,
            "Sign in",
            "Sign in to the Chromine dashboard first, then reload this page.",
        );
    };
    let Some(socket) = st.cfg.code_socket_of(me.owner()) else {
        return page(StatusCode::NOT_FOUND, "VS Code", "VS Code is not set up on this server.");
    };
    let upgrade = is_upgrade(req.headers());
    if upgrade && !same_origin(&st, req.headers()) {
        return page(StatusCode::FORBIDDEN, "Refused", "Cross-site connection refused.");
    }
    match forward(&socket, &mut req, upgrade).await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, user = %me.owner(), "VS Code is not answering");
            let text = if me.is_admin() {
                "VS Code is not running yet. If this lasts, start it with <code>docker compose up -d code</code>."
            } else {
                "Your VS Code is starting. This page reloads by itself."
            };
            page_with(StatusCode::BAD_GATEWAY, "VS Code is starting", text, "<meta http-equiv=refresh content=3>")
        }
    }
}

async fn forward(socket: &Path, req: &mut Request, upgrade: bool) -> anyhow::Result<Response> {
    let stream = tokio::net::UnixStream::connect(socket).await?;
    let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream)).await?;
    tokio::spawn(async move {
        let _ = conn.with_upgrades().await;
    });
    let client_upgrade = upgrade.then(|| hyper::upgrade::on(&mut *req));
    let path = req.uri().path_and_query().map(|p| p.as_str()).unwrap_or("/").to_string();
    let workbench = req.method() == Method::GET && req.uri().path() == "/";
    let mut builder = hyper::Request::builder().method(req.method().clone()).uri(path);
    let mut headers = upstream_headers(req.headers(), upgrade);
    if workbench {
        headers.remove(header::ACCEPT_ENCODING);
    }
    *builder.headers_mut().expect("fresh request builder") = headers;
    let body = std::mem::replace(req.body_mut(), Body::empty());
    let mut resp = sender.send_request(builder.body(body)?).await?;

    if resp.status() == StatusCode::SWITCHING_PROTOCOLS
        && let Some(client_upgrade) = client_upgrade
    {
        let upstream_upgrade = hyper::upgrade::on(&mut resp);
        tokio::spawn(async move {
            if let (Ok(client), Ok(upstream)) = tokio::join!(client_upgrade, upstream_upgrade) {
                let _ = tokio::io::copy_bidirectional(&mut TokioIo::new(client), &mut TokioIo::new(upstream)).await;
            }
        });
        let (parts, _) = resp.into_parts();
        return Ok(Response::from_parts(parts, Body::empty()));
    }

    let (mut parts, body) = resp.into_parts();
    for name in HOP_BY_HOP.iter().chain(&["transfer-encoding"]) {
        parts.headers.remove(*name);
    }
    let html = parts
        .headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("text/html"));
    if workbench && parts.status == StatusCode::OK && html && !parts.headers.contains_key(header::CONTENT_ENCODING) {
        let page = axum::body::to_bytes(Body::new(body), MAX_PAGE).await?;
        parts.headers.remove(header::CONTENT_LENGTH);
        return Ok(Response::from_parts(parts, Body::from(inject_opener(&page))));
    }
    Ok(Response::from_parts(parts, Body::new(body)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_dashboard_session_never_reaches_vs_code() {
        let mut h = HeaderMap::new();
        h.append(header::COOKIE, HeaderValue::from_static("a=1; chromine_session=secret; vscode-tkn=x"));
        h.append(header::COOKIE, HeaderValue::from_static(" chromine_session = again"));
        assert_eq!(without_session_cookie(&h).unwrap(), "a=1; vscode-tkn=x");
        let mut only = HeaderMap::new();
        only.insert(header::COOKIE, HeaderValue::from_static("chromine_session=secret"));
        assert!(without_session_cookie(&only).is_none());
        h.insert(header::CONNECTION, HeaderValue::from_static("keep-alive, Upgrade"));
        h.insert(header::UPGRADE, HeaderValue::from_static("websocket"));
        assert!(is_upgrade(&h));
        let up = upstream_headers(&h, true);
        assert_eq!(up.get(header::UPGRADE).unwrap(), "websocket");
        assert_eq!(up.get(header::CONNECTION).unwrap(), "upgrade");
        assert!(!format!("{:?}", up.get_all(header::COOKIE).iter().collect::<Vec<_>>()).contains("secret"));
        let plain = upstream_headers(&h, false);
        assert!(plain.get(header::UPGRADE).is_none() && plain.get(header::CONNECTION).is_none());
    }

    #[test]
    fn the_link_catcher_loads_first_in_the_page() {
        let out = String::from_utf8(inject_opener(b"<!DOCTYPE html><HTML><HEAD><meta charset=utf-8></HEAD>")).unwrap();
        assert_eq!(out, format!("<!DOCTYPE html><HTML><HEAD>{OPEN_TAG}<meta charset=utf-8></HEAD>"));
        assert!(String::from_utf8(inject_opener(b"<p>x</p>")).unwrap().starts_with(OPEN_TAG));
        assert_eq!(web_link(" https://user:pw@example.com/a ").as_deref(), Some("https://user:pw@example.com/a"));
        for bad in ["javascript:alert(1)", "file:///etc/passwd", "--remote-debugging-port=1", "https://", ""] {
            assert!(web_link(bad).is_none(), "{bad}");
        }
    }
}

use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::ws::rejection::WebSocketUpgradeRejection;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, Path, Query, Request, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Extension, Json};
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use chromine_shared::{AgentStatus, DiskSpace, fsutil, now_ms};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Notify;

use crate::auth::{self, LoginLimiter, SESSION_COOKIE};
use crate::config::{Config, Secret};
use crate::db::{Db, Job, NewSession, RetryOutcome, SessionRow, UserRow, state};
use crate::ingest::Orphan;
use crate::settings::Live;
use crate::upload::Uploader;

pub struct AppState {
    pub ui: Arc<crate::ui::UiAssets>,
    pub novnc: Option<Arc<crate::ui::UiAssets>>,
    pub webrtc: Option<crate::webrtc::Hub>,
    pub settings: Arc<crate::settings::Registry>,
    pub cfg: Arc<Config>,
    pub db: Db,
    pub spaces: crate::users::Spaces,
    pub uploader: Uploader,
    pub password_hash: Secret,
    pub upload_wake: Arc<Notify>,
    pub cleanup_wake: Arc<Notify>,
    pub limiter: LoginLimiter,
    pub login_failure_delay: Duration,
    pub tls: Option<Arc<crate::tls::Tls>>,
}

pub(crate) type Shared = Arc<AppState>;

#[derive(Clone)]
pub(crate) struct Authed {
    id_hash: String,
    session: SessionRow,
}

impl Authed {
    pub(crate) fn owner(&self) -> &str {
        &self.session.owner
    }

    pub(crate) fn is_admin(&self) -> bool {
        self.session.owner.is_empty()
    }
}

fn client_ip(peer: SocketAddr) -> IpAddr {
    peer.ip()
}

fn request_host(headers: &HeaderMap) -> Option<String> {
    headers.get(header::HOST).and_then(|v| v.to_str().ok()).map(|s| s.trim().to_string())
}

pub(crate) async fn authenticate(st: &AppState, jar: &CookieJar) -> Option<Authed> {
    let token = jar.get(SESSION_COOKIE)?.value().to_string();
    if token.len() < 32 || token.len() > 128 {
        return None;
    }
    let id_hash = auth::session_hash(&token);
    let now = now_ms();
    let session = st.db.get_session(&id_hash, now).await.ok()??;
    if session.owner.is_empty() && !auth::ct_eq(&session.username, &st.cfg.auth_user) {
        return None;
    }
    if now - session.last_seen_at > 60_000 {
        let _ = st.db.touch_session(&id_hash, now).await;
    }
    Some(Authed { id_hash, session })
}

fn wants_html(headers: &HeaderMap) -> bool {
    headers.get(header::ACCEPT).and_then(|v| v.to_str().ok()).is_some_and(|a| a.contains("text/html"))
}

fn login_redirect(next: &str) -> Response {
    let q: String = url::form_urlencoded::Serializer::new(String::new()).append_pair("next", next).finish();
    Redirect::to(&format!("/login?{q}")).into_response()
}

fn api_error(status: StatusCode, msg: &str) -> Response {
    (status, Json(json!({ "error": msg }))).into_response()
}

fn admin_only() -> Response {
    api_error(StatusCode::FORBIDDEN, "only the admin can do this")
}

async fn require_auth(State(st): State<Shared>, jar: CookieJar, mut req: Request, next: Next) -> Response {
    let Some(a) = authenticate(&st, &jar).await else {
        let path = req.uri().path_and_query().map(|p| p.as_str()).unwrap_or("/").to_string();
        if path.starts_with("/api/") || !wants_html(req.headers()) {
            return api_error(StatusCode::UNAUTHORIZED, "authentication required");
        }
        return login_redirect(&path);
    };
    if !matches!(*req.method(), Method::GET | Method::HEAD) {
        let headers = req.headers();
        let token = headers.get("x-csrf-token").and_then(|v| v.to_str().ok()).unwrap_or("");
        if !auth::ct_eq(token, &a.session.csrf_token) {
            return api_error(StatusCode::FORBIDDEN, "missing or invalid CSRF token");
        }
        let origin = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok());
        if !auth::origin_allowed(origin, request_host(headers).as_deref(), st.cfg.public_url.as_ref()) {
            return api_error(StatusCode::FORBIDDEN, "cross-origin request refused");
        }
    }
    req.extensions_mut().insert(a);
    next.run(req).await
}

pub fn code_frame_source(cfg: &Config) -> Option<String> {
    cfg.code_socket.as_ref()?;
    Some(match &cfg.code_public_url {
        Some(u) => u.origin().ascii_serialization(),
        None => format!("*:{}", cfg.code_port),
    })
}

pub fn content_security_policy(cfg: &Config) -> String {
    let code = code_frame_source(cfg).map(|s| format!(" {s}")).unwrap_or_default();
    format!(
        "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; connect-src 'self'; \
         frame-src 'self'{code}; frame-ancestors 'self'; base-uri 'none'; form-action 'self'; object-src 'none'"
    )
}

async fn security_headers(State(csp): State<HeaderValue>, req: Request, next: Next) -> Response {
    let mut resp = next.run(req).await;
    let h = resp.headers_mut();
    if !h.contains_key(header::CONTENT_SECURITY_POLICY) {
        h.insert(header::CONTENT_SECURITY_POLICY, csp);
    }
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    h.insert(header::REFERRER_POLICY, HeaderValue::from_static("same-origin"));
    h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("SAMEORIGIN"));
    h.insert(
        HeaderName::from_static("permissions-policy"),
        HeaderValue::from_static("camera=(), microphone=(), geolocation=(), payment=()"),
    );
    if !h.contains_key(header::CACHE_CONTROL) {
        h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
    resp
}

pub fn router(st: Shared) -> Router {
    let csp = HeaderValue::from_str(&content_security_policy(&st.cfg)).expect("CSP is plain ASCII");
    let protected = Router::new()
        .route("/", get(index))
        .route("/api/session", get(api_session))
        .route("/api/sessions", get(api_sessions))
        .route("/api/sessions/logout-others", post(api_logout_others))
        .route("/api/sessions/{id}/logout", post(api_logout_session))
        .route("/api/state", get(api_state))
        .route("/api/jobs/clear", post(api_clear_history))
        .route("/api/jobs/{id}/retry", post(api_retry))
        .route("/api/jobs/{id}/cancel", post(api_cancel))
        .route("/api/orphans/import", post(api_import))
        .route("/api/settings", get(api_settings).post(api_save_settings))
        .route("/api/settings/reset", post(api_reset_settings))
        .route("/api/users", get(api_users).post(api_create_user))
        .route("/api/users/{id}/password", post(api_user_password))
        .route("/api/users/{id}/logout", post(api_user_logout))
        .route("/api/users/{id}/delete", post(api_delete_user))
        .route("/logout", post(logout))
        .route("/browser", get(novnc_redirect))
        .route("/browser/", get(novnc_redirect))
        .route("/browser/websockify", get(vnc_websocket))
        .route("/browser/audio", get(audio_websocket))
        .route("/browser/webrtc", get(webrtc_websocket))
        .route("/browser/webrtc.html", get(webrtc_asset))
        .route("/browser/webrtc.js", get(webrtc_asset))
        .route("/browser/webrtc.css", get(webrtc_asset))
        .route("/browser/{*path}", get(novnc_file))
        .route_layer(middleware::from_fn_with_state(st.clone(), require_auth));

    Router::new()
        .route("/healthz", get(healthz))
        .route("/login", get(login_page).post(login_submit))
        .route("/_next/static/{*path}", get(ui_public_asset))
        .route("/manifest.webmanifest", get(ui_public_asset))
        .route("/icons/{*path}", get(ui_public_asset))
        .merge(protected)
        .fallback(ui_protected_file)
        .layer(middleware::from_fn_with_state(csp, security_headers))
        .with_state(st)
}

async fn healthz(State(st): State<Shared>) -> Response {
    match st.db.ping().await {
        Ok(()) => Json(json!({ "status": "ok" })).into_response(),
        Err(_) => api_error(StatusCode::SERVICE_UNAVAILABLE, "database unavailable"),
    }
}

async fn index(State(st): State<Shared>) -> Response {
    st.ui.respond("/").await
}

async fn ui_public_asset(State(st): State<Shared>, uri: axum::http::Uri) -> Response {
    st.ui.respond(uri.path()).await
}

async fn ui_protected_file(
    State(st): State<Shared>,
    jar: CookieJar,
    method: Method,
    uri: axum::http::Uri,
    headers: HeaderMap,
) -> Response {
    if !matches!(method, Method::GET | Method::HEAD) {
        return api_error(StatusCode::NOT_FOUND, "not found");
    }
    if authenticate(&st, &jar).await.is_none() {
        let path = uri.path_and_query().map(|p| p.as_str()).unwrap_or("/").to_string();
        if path.starts_with("/api/") || !wants_html(&headers) {
            return api_error(StatusCode::UNAUTHORIZED, "authentication required");
        }
        return login_redirect(&path);
    }
    st.ui.respond(uri.path()).await
}

async fn login_page(State(st): State<Shared>, jar: CookieJar) -> Response {
    if authenticate(&st, &jar).await.is_some() {
        return Redirect::to("/").into_response();
    }
    st.ui.respond("/login").await
}

#[derive(Deserialize)]
struct LoginForm {
    username: String,
    password: String,
    #[serde(default)]
    next: Option<String>,
}

async fn login_submit(
    State(st): State<Shared>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    https: Option<axum::Extension<crate::tls::Https>>,
    headers: HeaderMap,
    jar: CookieJar,
    body: axum::body::Bytes,
) -> Response {
    let json_mode = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("application/json"));
    let form: Option<LoginForm> = if json_mode {
        serde_json::from_slice(&body).ok()
    } else {
        let mut f = LoginForm { username: String::new(), password: String::new(), next: None };
        for (k, v) in url::form_urlencoded::parse(&body) {
            match &*k {
                "username" => f.username = v.into_owned(),
                "password" => f.password = v.into_owned(),
                "next" => f.next = Some(v.into_owned()),
                _ => {}
            }
        }
        Some(f)
    };
    let next = auth::safe_next(form.as_ref().and_then(|f| f.next.as_deref()));
    let fail = |status: StatusCode, code: &str, message: &str| -> Response {
        if json_mode {
            return (status, Json(json!({ "status": "error", "message": message }))).into_response();
        }
        let q: String = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("error", code)
            .append_pair("next", &next)
            .finish();
        Redirect::to(&format!("/login?{q}")).into_response()
    };

    let origin = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok());
    if !auth::origin_allowed(origin, request_host(&headers).as_deref(), st.cfg.public_url.as_ref()) {
        if json_mode {
            return (
                StatusCode::FORBIDDEN,
                Json(json!({ "status": "error", "message": "Cross-origin login refused." })),
            )
                .into_response();
        }
        return (StatusCode::FORBIDDEN, "cross-origin login refused").into_response();
    }
    let Some(form) = form else {
        return fail(StatusCode::BAD_REQUEST, "invalid", "Enter your username and password.");
    };
    let ip = client_ip(peer);
    if !st.limiter.allowed(ip) {
        tracing::warn!(%ip, "login throttled");
        return fail(StatusCode::TOO_MANY_REQUESTS, "throttled", "Too many failed attempts. Try again in 15 minutes.");
    }
    let is_admin = auth::ct_eq(form.username.trim(), &st.cfg.auth_user);
    let user: Option<UserRow> = if is_admin {
        None
    } else {
        st.db.user_by_name(&form.username.trim().to_ascii_lowercase()).await.unwrap_or_else(|e| {
            tracing::error!(error = %e, "looking up a user failed");
            None
        })
    };
    let hash = match &user {
        Some(u) => u.password_hash.clone(),
        None => st.password_hash.expose().to_string(),
    };
    let password = form.password.clone();
    let pw_ok = tokio::task::spawn_blocking(move || auth::verify_password(&password, &hash)).await.unwrap_or(false);
    if !(pw_ok && (is_admin || user.is_some())) {
        st.limiter.failed(ip);
        tracing::warn!(%ip, "failed login");
        tokio::time::sleep(st.login_failure_delay).await;
        return fail(StatusCode::UNAUTHORIZED, "invalid", "Invalid username or password.");
    }
    st.limiter.succeeded(ip);
    let token = auth::random_token();
    let csrf = auth::random_token();
    let now = now_ms();
    let ttl = st.cfg.session_ttl;
    let id_hash = auth::session_hash(&token);
    let public_id = auth::random_token();
    let ip_text = ip.to_string();
    let user_agent: Option<String> = headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .map(|ua| ua.chars().filter(|c| !c.is_control()).take(400).collect());
    let (username, owner) = match &user {
        Some(u) => (u.username.as_str(), u.id.as_str()),
        None => (st.cfg.auth_user.as_str(), ""),
    };
    let session = NewSession {
        id_hash: &id_hash,
        public_id: &public_id,
        username,
        owner,
        csrf: &csrf,
        ip: &ip_text,
        user_agent: user_agent.as_deref(),
    };
    if let Err(e) = st.db.create_session(&session, now, now + ttl.as_millis() as i64).await {
        tracing::error!(error = %e, "cannot create session");
        return fail(StatusCode::INTERNAL_SERVER_ERROR, "invalid", "Cannot create a session; try again.");
    }
    if owner.is_empty() {
        tracing::info!(%ip, "login");
    } else {
        tracing::info!(%ip, user = %owner, "user login");
        st.spaces.sync_logged(&st.db).await;
    }
    let cookie = Cookie::build((SESSION_COOKIE, token))
        .http_only(true)
        .secure(https.is_some_and(|h| h.0.0))
        .same_site(SameSite::Lax)
        .path("/")
        .max_age(time_duration(ttl))
        .build();
    if json_mode {
        return (jar.add(cookie), Json(json!({ "status": "success" }))).into_response();
    }
    (jar.add(cookie), Redirect::to(&next)).into_response()
}

fn time_duration(d: Duration) -> time::Duration {
    time::Duration::seconds(d.as_secs() as i64)
}

async fn logout(State(st): State<Shared>, jar: CookieJar, req: Request) -> Response {
    if let Some(a) = req.extensions().get::<Authed>() {
        let _ = st.db.delete_session(&a.id_hash).await;
        if !a.is_admin() {
            st.spaces.sync_logged(&st.db).await;
        }
    }
    let cookie = Cookie::build((SESSION_COOKIE, "")).path("/").max_age(time_duration(Duration::ZERO)).build();
    (jar.remove(cookie), Json(json!({ "ok": true }))).into_response()
}

async fn api_session(State(st): State<Shared>, req: Request) -> Response {
    let a = req.extensions().get::<Authed>().cloned();
    match a {
        Some(a) => Json(json!({
            "user": a.session.username,
            "admin": a.is_admin(),
            "csrf": a.session.csrf_token,
            "display": display_view(&st.settings.of(a.owner()).current()),
            "code": st.cfg.code_socket.as_ref().map(|_| json!({
                "url": st.cfg.code_public_url.as_ref().map(|u| u.as_str()),
                "port": st.cfg.code_port,
            })),
        }))
        .into_response(),
        None => api_error(StatusCode::UNAUTHORIZED, "authentication required"),
    }
}

#[derive(Serialize)]
struct SessionView {
    id: String,
    ip: Option<String>,
    user_agent: Option<String>,
    created_at: i64,
    last_seen_at: i64,
    current: bool,
}

async fn api_sessions(State(st): State<Shared>, req: Request) -> Response {
    let Some(me) = req.extensions().get::<Authed>().cloned() else {
        return api_error(StatusCode::UNAUTHORIZED, "authentication required");
    };
    match st.db.list_sessions(me.owner(), now_ms()).await {
        Ok(list) => {
            let sessions: Vec<SessionView> = list
                .into_iter()
                .map(|s| SessionView {
                    current: s.id_hash == me.id_hash,
                    id: s.public_id,
                    ip: s.ip,
                    user_agent: s.user_agent,
                    created_at: s.created_at,
                    last_seen_at: s.last_seen_at,
                })
                .collect();
            Json(json!({ "sessions": sessions })).into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "listing sessions failed");
            api_error(StatusCode::INTERNAL_SERVER_ERROR, "database error")
        }
    }
}

async fn api_logout_session(
    State(st): State<Shared>,
    Extension(me): Extension<Authed>,
    Path(id): Path<String>,
) -> Response {
    if id.is_empty() || id.len() > 64 || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
        return api_error(StatusCode::BAD_REQUEST, "invalid session id");
    }
    match st.db.delete_session_by_public_id(me.owner(), &id).await {
        Ok(true) => {
            tracing::info!("signed out a session from the dashboard");
            if !me.is_admin() {
                st.spaces.sync_logged(&st.db).await;
            }
            Json(json!({ "logged_out": true })).into_response()
        }
        Ok(false) => api_error(StatusCode::NOT_FOUND, "that session has already ended"),
        Err(e) => {
            tracing::error!(error = %e, "signing out a session failed");
            api_error(StatusCode::INTERNAL_SERVER_ERROR, "database error")
        }
    }
}

async fn api_logout_others(State(st): State<Shared>, req: Request) -> Response {
    let Some(me) = req.extensions().get::<Authed>().cloned() else {
        return api_error(StatusCode::UNAUTHORIZED, "authentication required");
    };
    match st.db.delete_other_sessions(me.owner(), &me.id_hash).await {
        Ok(n) => {
            tracing::info!(sessions = n, "signed out every other session");
            Json(json!({ "logged_out": n })).into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "signing out other sessions failed");
            api_error(StatusCode::INTERNAL_SERVER_ERROR, "database error")
        }
    }
}

fn display_view(l: &Live) -> serde_json::Value {
    json!({
        "stream_mode": l.stream_mode,
        "display_scale": l.display_scale,
        "vnc_quality": l.vnc_quality,
        "vnc_compression": l.vnc_compression,
    })
}

fn settings_view(st: &AppState, owner: &str, values: &Live) -> Response {
    Json(json!({
        "values": values,
        "defaults": st.settings.of(owner).defaults(),
        "webrtc_ready": st.webrtc.is_some(),
    }))
    .into_response()
}

async fn api_settings(State(st): State<Shared>, Extension(me): Extension<Authed>) -> Response {
    settings_view(&st, me.owner(), &st.settings.of(me.owner()).current())
}

async fn api_save_settings(
    State(st): State<Shared>,
    Extension(me): Extension<Authed>,
    Json(patch): Json<serde_json::Map<String, serde_json::Value>>,
) -> Response {
    if !me.is_admin() && patch.contains_key("webrtc_host") {
        return admin_only();
    }
    let names: Vec<String> = patch.keys().cloned().collect();
    match st.settings.of(me.owner()).update(patch).await {
        Ok(values) => {
            tracing::info!(changed = ?names, user = %me.owner(), "settings saved");
            settings_view(&st, me.owner(), &values)
        }
        Err(e) => api_error(StatusCode::BAD_REQUEST, &format!("{e:#}")),
    }
}

async fn api_reset_settings(State(st): State<Shared>, Extension(me): Extension<Authed>) -> Response {
    match st.settings.of(me.owner()).reset().await {
        Ok(values) => {
            tracing::info!(user = %me.owner(), "settings reset to defaults");
            settings_view(&st, me.owner(), &values)
        }
        Err(e) => api_error(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e:#}")),
    }
}

#[derive(Serialize)]
struct JobView {
    #[serde(flatten)]
    job: Job,
    progress_bytes: u64,
    can_retry: bool,
    can_cancel: bool,
}

fn job_view(job: Job, live: Option<&crate::upload::LiveProgress>) -> JobView {
    let progress_bytes = match live {
        Some(l) if l.job_id == job.id => l.bytes,
        _ => job.uploaded_bytes.max(0) as u64,
    };
    let staged = job.file_state == state::FILE_STAGED;
    let can_retry = (staged
        && matches!(
            job.upload_state.as_str(),
            state::UP_FAILED | state::UP_CANCELLED | state::UP_RETRY | state::UP_SKIPPED
        ))
        || matches!(job.cleanup_state.as_str(), state::CL_FAILED | state::CL_RETRY);
    let can_cancel =
        staged && matches!(job.upload_state.as_str(), state::UP_PENDING | state::UP_RETRY | state::UP_FAILED);
    JobView { job, progress_bytes, can_retry, can_cancel }
}

#[derive(Serialize)]
struct DiskView {
    downloads: DiskSpace,
    state: DiskSpace,
    min_free_bytes: u64,
    low: bool,
}

#[derive(Serialize)]
struct BrowserView {
    status: Option<AgentStatus>,
    stale: bool,
}

const PAGE_SIZE: i64 = 20;

#[derive(Deserialize)]
struct StateQuery {
    page: Option<i64>,
}

async fn api_state(
    State(st): State<Shared>,
    Extension(me): Extension<Authed>,
    Query(q): Query<StateQuery>,
) -> Response {
    let owner = me.owner().to_string();
    let counts = match st.db.counts(&owner).await {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(error = %e, "counting jobs failed");
            return api_error(StatusCode::INTERNAL_SERVER_ERROR, "database error");
        }
    };
    let pages = ((counts.total + PAGE_SIZE - 1) / PAGE_SIZE).max(1);
    let page = q.page.unwrap_or(1).clamp(1, pages);
    let jobs = match st.db.list_page(&owner, PAGE_SIZE, (page - 1) * PAGE_SIZE).await {
        Ok(j) => j,
        Err(e) => {
            tracing::error!(error = %e, "listing jobs failed");
            return api_error(StatusCode::INTERNAL_SERVER_ERROR, "database error");
        }
    };
    let progress = st.uploader.live_progress();
    let jobs: Vec<JobView> = jobs.into_iter().map(|j| job_view(j, progress.as_ref())).collect();
    let live = st.settings.of(&owner).current();

    let cfg = st.cfg.clone();
    let ingest = st.spaces.ingest(&owner);
    let (disk, browser, orphans) = tokio::task::spawn_blocking(move || {
        let downloads = fsutil::disk_space(&cfg.download_dir).unwrap_or_default();
        let state = fsutil::disk_space(&cfg.state_dir).unwrap_or_default();
        let disk = DiskView {
            low: downloads.available_bytes < cfg.min_free_disk_bytes,
            downloads,
            state,
            min_free_bytes: cfg.min_free_disk_bytes,
        };
        let status: Option<AgentStatus> =
            std::fs::read(cfg.agent_status_file(&owner)).ok().and_then(|b| serde_json::from_slice(&b).ok());
        let stale = status.as_ref().is_none_or(|s| now_ms() - s.updated_at_ms > 20_000);
        let orphans: Vec<Orphan> = ingest.map(|i| i.orphans()).unwrap_or_default();
        (disk, BrowserView { status, stale }, orphans)
    })
    .await
    .unwrap_or_else(|_| {
        (
            DiskView { downloads: DiskSpace::default(), state: DiskSpace::default(), min_free_bytes: 0, low: false },
            BrowserView { status: None, stale: true },
            Vec::new(),
        )
    });

    Json(json!({
        "now": now_ms(),
        "jobs": jobs,
        "page": page,
        "pages": pages,
        "per_page": PAGE_SIZE,
        "counts": counts,
        "disk": disk,
        "browser": browser,
        "orphans": orphans,
        "display": display_view(&live),
        "https": st.tls.as_ref().filter(|_| me.is_admin()).map(|t| t.status()),
        "settings": {
            "delete_after_success": !live.keep_local,
            "filecloud_base_url": st.cfg.filecloud_base_url.as_str(),
            "max_upload_size": st.uploader.cached_service_config().map(|c| c.max_upload_size),
            "max_retry_attempts": st.cfg.max_retry_attempts,
        }
    }))
    .into_response()
}

fn valid_job_id(id: &str) -> bool {
    uuid::Uuid::parse_str(id).is_ok()
}

async fn owned_job(st: &AppState, me: &Authed, id: &str) -> Result<Job, (StatusCode, &'static str)> {
    if !valid_job_id(id) {
        return Err((StatusCode::BAD_REQUEST, "invalid job id"));
    }
    match st.db.get(id).await {
        Ok(Some(job)) if job.owner == me.owner() => Ok(job),
        Ok(_) => Err((StatusCode::NOT_FOUND, "job not found")),
        Err(e) => {
            tracing::error!(error = %e, "loading a job failed");
            Err((StatusCode::INTERNAL_SERVER_ERROR, "database error"))
        }
    }
}

async fn api_retry(State(st): State<Shared>, Extension(me): Extension<Authed>, Path(id): Path<String>) -> Response {
    let job = match owned_job(&st, &me, &id).await {
        Ok(j) => j,
        Err((status, msg)) => return api_error(status, msg),
    };
    if job.file_state == state::FILE_STAGED && job.upload_state == state::UP_SKIPPED {
        if !st.settings.of(me.owner()).current().filecloud {
            return api_error(
                StatusCode::CONFLICT,
                "FileCloud24 is off in Settings, so there is nowhere to send this file.",
            );
        }
        return match st.db.start_skipped_upload(&id, now_ms()).await {
            Ok(true) => {
                st.upload_wake.notify_one();
                Json(json!({ "retried": "upload" })).into_response()
            }
            Ok(false) => api_error(StatusCode::CONFLICT, "nothing to retry for this job in its current state"),
            Err(e) => {
                tracing::error!(error = %e, "retry failed");
                api_error(StatusCode::INTERNAL_SERVER_ERROR, "database error")
            }
        };
    }
    match st.db.retry(&id, now_ms()).await {
        Ok(RetryOutcome::Upload) => {
            st.upload_wake.notify_one();
            Json(json!({ "retried": "upload" })).into_response()
        }
        Ok(RetryOutcome::Cleanup) => {
            st.cleanup_wake.notify_one();
            Json(json!({ "retried": "cleanup" })).into_response()
        }
        Ok(RetryOutcome::NothingToRetry) => {
            api_error(StatusCode::CONFLICT, "nothing to retry for this job in its current state")
        }
        Ok(RetryOutcome::NotFound) => api_error(StatusCode::NOT_FOUND, "job not found"),
        Err(e) => {
            tracing::error!(error = %e, "retry failed");
            api_error(StatusCode::INTERNAL_SERVER_ERROR, "database error")
        }
    }
}

async fn api_clear_history(State(st): State<Shared>, Extension(me): Extension<Authed>) -> Response {
    match st.db.clear_finished(me.owner()).await {
        Ok(n) => {
            tracing::info!(cleared = n, "cleared finished uploads from the history");
            Json(json!({ "cleared": n })).into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "clearing the history failed");
            api_error(StatusCode::INTERNAL_SERVER_ERROR, "database error")
        }
    }
}

async fn api_cancel(State(st): State<Shared>, Extension(me): Extension<Authed>, Path(id): Path<String>) -> Response {
    if let Err((status, msg)) = owned_job(&st, &me, &id).await {
        return api_error(status, msg);
    }
    match st.db.cancel_upload(&id, now_ms()).await {
        Ok(Some(session)) => {
            if let Some(s) = session {
                st.uploader.abort_session(&s).await;
            }
            Json(json!({ "cancelled": true })).into_response()
        }
        Ok(None) => api_error(StatusCode::CONFLICT, "this upload cannot be cancelled in its current state"),
        Err(e) => {
            tracing::error!(error = %e, "cancel failed");
            api_error(StatusCode::INTERNAL_SERVER_ERROR, "database error")
        }
    }
}

#[derive(Deserialize)]
struct ImportBody {
    name: String,
}

async fn api_import(
    State(st): State<Shared>,
    Extension(me): Extension<Authed>,
    Json(body): Json<ImportBody>,
) -> Response {
    let Some(ingest) = st.spaces.ingest(me.owner()) else {
        return api_error(StatusCode::SERVICE_UNAVAILABLE, "downloads for this user are not ready yet");
    };
    match ingest.import_orphan(&body.name).await {
        Ok(id) => Json(json!({ "job_id": id })).into_response(),
        Err(e) => api_error(StatusCode::BAD_REQUEST, &crate::redact::sanitize(&e.to_string())),
    }
}

async fn novnc_redirect() -> Response {
    Redirect::to("/browser/vnc.html").into_response()
}

async fn novnc_file(State(st): State<Shared>, Path(path): Path<String>) -> Response {
    match &st.novnc {
        Some(files) => files.respond(&path).await,
        None => api_error(StatusCode::SERVICE_UNAVAILABLE, "noVNC is not installed (NOVNC_DIR)"),
    }
}

async fn vnc_websocket(
    State(st): State<Shared>,
    Extension(me): Extension<Authed>,
    headers: HeaderMap,
    ws: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Response {
    let socket = st.cfg.vnc_socket_of(me.owner());
    socket_websocket(&st, &headers, ws, socket, "remote display")
}

async fn audio_websocket(
    State(st): State<Shared>,
    Extension(me): Extension<Authed>,
    headers: HeaderMap,
    ws: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Response {
    let socket = st.cfg.audio_socket_of(me.owner());
    socket_websocket(&st, &headers, ws, socket, "sound")
}

async fn webrtc_websocket(
    State(st): State<Shared>,
    Extension(me): Extension<Authed>,
    headers: HeaderMap,
    ws: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Response {
    let origin = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok());
    if !auth::origin_allowed(origin, request_host(&headers).as_deref(), st.cfg.public_url.as_ref()) {
        return api_error(StatusCode::FORBIDDEN, "cross-origin request refused");
    }
    let Some(hub) = st.webrtc.clone() else {
        return api_error(StatusCode::SERVICE_UNAVAILABLE, "WebRTC streaming is unavailable; see the worker log");
    };
    let public = st.cfg.public_url.as_ref().and_then(|u| u.host_str().map(str::to_string));
    let configured = st.settings.of("").current().webrtc_host().map(str::to_string).or(public);
    let Some(host) = crate::webrtc::candidate_host(configured.as_deref(), request_host(&headers).as_deref()) else {
        return api_error(StatusCode::BAD_REQUEST, "cannot tell which address to stream to; set WEBRTC_HOST");
    };
    let ws = match ws {
        Ok(ws) => ws,
        Err(e) => return e.into_response(),
    };
    let screen = crate::webrtc::Screen {
        key: me.owner().to_string(),
        vnc_socket: st.cfg.vnc_socket_of(me.owner()),
        audio_socket: st.cfg.audio_socket_of(me.owner()),
        live: st.settings.of(me.owner()).subscribe(),
    };
    ws.on_upgrade(move |ws| crate::webrtc::signal(ws, hub, host, screen))
}

const WEBRTC_HTML: &str = include_str!("../assets/webrtc.html");
const WEBRTC_JS: &str = include_str!("../assets/webrtc.js");
const WEBRTC_CSS: &str = include_str!("../assets/webrtc.css");

async fn webrtc_asset(uri: axum::http::Uri) -> Response {
    let (body, kind) = match uri.path() {
        "/browser/webrtc.html" => (WEBRTC_HTML, "text/html; charset=utf-8"),
        "/browser/webrtc.js" => (WEBRTC_JS, "text/javascript; charset=utf-8"),
        _ => (WEBRTC_CSS, "text/css; charset=utf-8"),
    };
    let mut r = body.into_response();
    let h = r.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(kind));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    if kind.starts_with("text/html")
        && let Ok(v) = HeaderValue::from_str(&crate::ui::csp_for_html(body))
    {
        h.insert(header::CONTENT_SECURITY_POLICY, v);
    }
    r
}

fn socket_websocket(
    st: &AppState,
    headers: &HeaderMap,
    ws: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
    socket: PathBuf,
    what: &'static str,
) -> Response {
    let origin = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok());
    if !auth::origin_allowed(origin, request_host(headers).as_deref(), st.cfg.public_url.as_ref()) {
        return api_error(StatusCode::FORBIDDEN, "cross-origin request refused");
    }
    let ws = match ws {
        Ok(ws) => ws,
        Err(e) => return e.into_response(),
    };
    ws.protocols(["binary"]).on_upgrade(move |ws| vnc_bridge(ws, socket, what))
}

async fn vnc_bridge(mut ws: WebSocket, socket: PathBuf, what: &'static str) {
    let stream = match tokio::net::UnixStream::connect(&socket).await {
        Ok(s) => s,
        Err(e) => {
            if what == "sound" {
                tracing::debug!(error = %e, socket = %socket.display(), "{what} unavailable");
            } else {
                tracing::warn!(error = %e, socket = %socket.display(), "{what} unavailable");
            }
            let _ = ws.send(Message::Close(Some(CloseFrame { code: 1011, reason: "unavailable".into() }))).await;
            return;
        }
    };
    tracing::debug!("{what} connected");
    let (mut vnc_rx, mut vnc_tx) = stream.into_split();
    let (mut ws_tx, mut ws_rx) = ws.split();
    let to_vnc = async {
        while let Some(Ok(msg)) = ws_rx.next().await {
            match msg {
                Message::Binary(data) => {
                    if vnc_tx.write_all(&data).await.is_err() {
                        break;
                    }
                }
                Message::Close(_) => break,
                _ => {}
            }
        }
        let _ = vnc_tx.shutdown().await;
    };
    let to_ws = async {
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            match vnc_rx.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if ws_tx.send(Message::Binary(bytes::Bytes::copy_from_slice(&buf[..n]))).await.is_err() {
                        break;
                    }
                }
            }
        }
        let _ = ws_tx.send(Message::Close(None)).await;
    };
    tokio::select! {
        _ = to_vnc => {}
        _ = to_ws => {}
    }
    tracing::debug!("{what} disconnected");
}

async fn api_users(State(st): State<Shared>, Extension(me): Extension<Authed>) -> Response {
    if !me.is_admin() {
        return admin_only();
    }
    match st.db.list_users(now_ms()).await {
        Ok(users) => Json(json!({ "users": users, "min_password": crate::users::MIN_PASSWORD })).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "listing users failed");
            api_error(StatusCode::INTERNAL_SERVER_ERROR, "database error")
        }
    }
}

#[derive(Deserialize)]
struct NewUserBody {
    username: String,
    password: String,
}

async fn api_create_user(
    State(st): State<Shared>,
    Extension(me): Extension<Authed>,
    Json(body): Json<NewUserBody>,
) -> Response {
    if !me.is_admin() {
        return admin_only();
    }
    let username = match crate::users::check_username(&body.username, &st.cfg.auth_user) {
        Ok(u) => u,
        Err(e) => return api_error(StatusCode::BAD_REQUEST, &e),
    };
    if let Err(e) = crate::users::check_user_password(&body.password) {
        return api_error(StatusCode::BAD_REQUEST, &e);
    }
    let password = body.password;
    let hash = match tokio::task::spawn_blocking(move || auth::hash_password(&password)).await {
        Ok(Ok(h)) => h,
        _ => return api_error(StatusCode::INTERNAL_SERVER_ERROR, "cannot hash the password"),
    };
    let user = UserRow {
        id: crate::users::new_user_id(),
        username: username.clone(),
        password_hash: hash,
        created_at: now_ms(),
    };
    match st.db.create_user(&user).await {
        Ok(true) => {
            tracing::info!(user = %user.id, "user created");
            st.spaces.sync_logged(&st.db).await;
            Json(json!({ "id": user.id, "username": username })).into_response()
        }
        Ok(false) => api_error(StatusCode::CONFLICT, "a user with that name already exists"),
        Err(e) => {
            tracing::error!(error = %e, "creating a user failed");
            api_error(StatusCode::INTERNAL_SERVER_ERROR, "database error")
        }
    }
}

#[derive(Deserialize)]
struct PasswordBody {
    password: String,
}

async fn api_user_password(
    State(st): State<Shared>,
    Extension(me): Extension<Authed>,
    Path(id): Path<String>,
    Json(body): Json<PasswordBody>,
) -> Response {
    if !me.is_admin() {
        return admin_only();
    }
    if !chromine_shared::is_valid_user_id(&id) {
        return api_error(StatusCode::BAD_REQUEST, "invalid user id");
    }
    if let Err(e) = crate::users::check_user_password(&body.password) {
        return api_error(StatusCode::BAD_REQUEST, &e);
    }
    let password = body.password;
    let hash = match tokio::task::spawn_blocking(move || auth::hash_password(&password)).await {
        Ok(Ok(h)) => h,
        _ => return api_error(StatusCode::INTERNAL_SERVER_ERROR, "cannot hash the password"),
    };
    match st.db.set_user_password(&id, &hash).await {
        Ok(true) => {
            let _ = st.db.delete_sessions_of(&id).await;
            tracing::info!(user = %id, "user password changed; their sessions were signed out");
            st.spaces.sync_logged(&st.db).await;
            Json(json!({ "ok": true })).into_response()
        }
        Ok(false) => api_error(StatusCode::NOT_FOUND, "user not found"),
        Err(e) => {
            tracing::error!(error = %e, "changing a user password failed");
            api_error(StatusCode::INTERNAL_SERVER_ERROR, "database error")
        }
    }
}

async fn api_user_logout(
    State(st): State<Shared>,
    Extension(me): Extension<Authed>,
    Path(id): Path<String>,
) -> Response {
    if !me.is_admin() {
        return admin_only();
    }
    if !chromine_shared::is_valid_user_id(&id) {
        return api_error(StatusCode::BAD_REQUEST, "invalid user id");
    }
    match st.db.delete_sessions_of(&id).await {
        Ok(n) => {
            tracing::info!(user = %id, sessions = n, "signed out a user");
            st.spaces.sync_logged(&st.db).await;
            Json(json!({ "logged_out": n })).into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "signing out a user failed");
            api_error(StatusCode::INTERNAL_SERVER_ERROR, "database error")
        }
    }
}

async fn api_delete_user(
    State(st): State<Shared>,
    Extension(me): Extension<Authed>,
    Path(id): Path<String>,
) -> Response {
    if !me.is_admin() {
        return admin_only();
    }
    if !chromine_shared::is_valid_user_id(&id) {
        return api_error(StatusCode::BAD_REQUEST, "invalid user id");
    }
    match st.db.delete_user(&id).await {
        Ok(true) => {}
        Ok(false) => return api_error(StatusCode::NOT_FOUND, "user not found"),
        Err(e) => {
            tracing::error!(error = %e, "deleting a user failed");
            return api_error(StatusCode::INTERNAL_SERVER_ERROR, "database error");
        }
    }
    st.spaces.sync_logged(&st.db).await;
    st.settings.forget(&id);
    let jobs = st.db.delete_jobs_of(&id).await.unwrap_or_else(|e| {
        tracing::error!(error = %e, "removing a deleted user's transfers failed");
        Vec::new()
    });
    let jobs_dir = st.cfg.jobs_dir();
    let removed = jobs.len();
    let _ = tokio::task::spawn_blocking(move || {
        for job in jobs {
            if uuid::Uuid::parse_str(&job.id).is_ok() {
                let _ = std::fs::remove_dir_all(jobs_dir.join(&job.id));
            }
        }
    })
    .await;
    tracing::info!(user = %id, transfers = removed, "user deleted");
    Json(json!({ "deleted": true })).into_response()
}

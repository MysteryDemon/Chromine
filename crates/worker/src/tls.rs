use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use axum::Router;
use axum::extract::{ConnectInfo, Path as UrlPath, State};
use axum::http::{HeaderMap, StatusCode, Uri, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use chromine_shared::{fsutil, now_ms};
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto;
use hyper_util::service::TowerToHyperService;
use instant_acme::{
    Account, AccountCredentials, AuthorizationStatus, ChallengeType, Identifier, NewAccount, NewOrder, OrderStatus,
    RetryPolicy,
};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;
use serde::Serialize;
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;
use tokio_util::sync::CancellationToken;
use x509_parser::extensions::GeneralName;

pub const LETS_ENCRYPT: &str = "https://acme-v02.api.letsencrypt.org/directory";
pub const AUTO: &str = "auto";
pub const PUBLIC_IP_LOOKUPS: [&str; 3] =
    ["https://api.ipify.org", "https://ipv4.icanhazip.com", "https://checkip.amazonaws.com"];
const CHECK_EVERY: Duration = Duration::from_secs(6 * 3600);

#[derive(Debug, Clone, Copy)]
pub struct Https(pub bool);

#[derive(Debug, Clone, Default, Serialize)]
pub struct TlsStatus {
    pub expires_at_ms: Option<i64>,
    pub error: Option<String>,
}

pub struct Cert {
    pub key: Arc<CertifiedKey>,
    pub hosts: Vec<String>,
    pub not_before_ms: i64,
    pub not_after_ms: i64,
}

impl Cert {
    pub fn renew_at_ms(&self) -> i64 {
        self.not_before_ms + (self.not_after_ms - self.not_before_ms) * 2 / 3
    }
}

pub struct Tls {
    host: Option<String>,
    lookups: Vec<String>,
    dir: PathBuf,
    directory_url: String,
    provider: Arc<CryptoProvider>,
    challenges: Mutex<HashMap<String, String>>,
    current: RwLock<Option<Arc<Cert>>>,
    error: Mutex<Option<String>>,
}

impl std::fmt::Debug for Tls {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tls").field("host", &self.host).finish()
    }
}

impl ResolvesServerCert for Tls {
    fn resolve(&self, _hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        self.current.read().unwrap().as_ref().map(|c| c.key.clone())
    }
}

pub fn parse_cert(chain_pem: &[u8], key_pem: &[u8], provider: &CryptoProvider) -> Result<Cert> {
    let chain: Vec<CertificateDer<'static>> =
        CertificateDer::pem_slice_iter(chain_pem).collect::<Result<_, _>>().context("certificate is not valid PEM")?;
    let leaf = chain.first().context("certificate file is empty")?;
    let (_, parsed) = x509_parser::parse_x509_certificate(leaf).map_err(|e| anyhow!("unreadable certificate: {e}"))?;
    let validity = parsed.validity();
    let not_before_ms = validity.not_before.timestamp() * 1000;
    let not_after_ms = validity.not_after.timestamp() * 1000;
    let hosts = match parsed.subject_alternative_name() {
        Ok(Some(san)) => san.value.general_names.iter().filter_map(san_host).collect(),
        _ => Vec::new(),
    };
    let key = PrivateKeyDer::from_pem_slice(key_pem).context("private key is not valid PEM")?;
    let key = CertifiedKey::from_der(chain, key, provider).context("certificate and key do not match")?;
    Ok(Cert { key: Arc::new(key), hosts, not_before_ms, not_after_ms })
}

fn san_host(name: &GeneralName) -> Option<String> {
    match name {
        GeneralName::IPAddress(b) => match b.len() {
            4 => Some(IpAddr::from(<[u8; 4]>::try_from(*b).ok()?).to_string()),
            16 => Some(IpAddr::from(<[u8; 16]>::try_from(*b).ok()?).to_string()),
            _ => None,
        },
        GeneralName::DNSName(d) => Some(d.to_ascii_lowercase()),
        _ => None,
    }
}

fn canonical(host: &str) -> String {
    host.parse::<IpAddr>().map(|ip| ip.to_string()).unwrap_or_else(|_| host.to_ascii_lowercase())
}

pub async fn public_ip(lookups: &[String]) -> Result<String> {
    let http = reqwest::Client::builder().timeout(Duration::from_secs(10)).build()?;
    let mut problems = Vec::new();
    for url in lookups {
        let answer = async {
            let body = http.get(url).send().await?.error_for_status()?.text().await?;
            let ip: IpAddr = body.trim().parse().map_err(|_| anyhow!("the answer is not an IP address"))?;
            if !crate::config::is_public_ip(ip) {
                bail!("answered {ip}, which is not a public address");
            }
            Ok::<_, anyhow::Error>(ip.to_string())
        }
        .await;
        match answer {
            Ok(ip) => return Ok(ip),
            Err(e) => problems.push(format!("{url}: {e:#}")),
        }
    }
    bail!("Cannot find this server's public IP address ({}). Set HTTPS_HOST to it in .env.", problems.join("; "))
}

fn backoff(failures: u32) -> Duration {
    Duration::from_secs((600u64 << failures.saturating_sub(1).min(4)).min(7200))
}

impl Tls {
    pub fn new(host: &str, state_dir: &Path, directory_url: &str) -> Result<Arc<Tls>> {
        Tls::open(host, state_dir, directory_url, PUBLIC_IP_LOOKUPS.map(String::from).to_vec())
    }

    fn open(host: &str, state_dir: &Path, directory_url: &str, lookups: Vec<String>) -> Result<Arc<Tls>> {
        let dir = state_dir.join("tls");
        std::fs::create_dir_all(&dir).with_context(|| format!("cannot create {}", dir.display()))?;
        let tls = Arc::new(Tls {
            host: (host != AUTO).then(|| canonical(host)),
            lookups,
            dir,
            directory_url: directory_url.to_string(),
            provider: Arc::new(rustls::crypto::aws_lc_rs::default_provider()),
            challenges: Mutex::new(HashMap::new()),
            current: RwLock::new(None),
            error: Mutex::new(None),
        });
        if let (Ok(chain), Ok(key)) = (std::fs::read(tls.dir.join("cert.pem")), std::fs::read(tls.dir.join("key.pem")))
        {
            match parse_cert(&chain, &key, &tls.provider) {
                Ok(cert) => *tls.current.write().unwrap() = Some(Arc::new(cert)),
                Err(e) => tracing::warn!(error = %e, "ignoring the saved HTTPS certificate"),
            }
        }
        Ok(tls)
    }

    pub fn ready(&self) -> bool {
        self.current.read().unwrap().as_ref().is_some_and(|c| c.not_after_ms > now_ms())
    }

    pub fn status(&self) -> TlsStatus {
        TlsStatus {
            expires_at_ms: self.current.read().unwrap().as_ref().map(|c| c.not_after_ms),
            error: self.error.lock().unwrap().clone(),
        }
    }

    pub fn install(&self, cert: Cert) {
        *self.current.write().unwrap() = Some(Arc::new(cert));
    }

    pub fn answer(&self, token: &str) -> Option<String> {
        self.challenges.lock().unwrap().get(token).cloned()
    }

    pub fn acceptor(self: &Arc<Self>) -> Result<TlsAcceptor> {
        let mut config = rustls::ServerConfig::builder_with_provider(self.provider.clone())
            .with_safe_default_protocol_versions()?
            .with_no_client_auth()
            .with_cert_resolver(self.clone());
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        Ok(TlsAcceptor::from(Arc::new(config)))
    }

    async fn account(&self) -> Result<Account> {
        let path = self.dir.join("account.json");
        if let Ok(bytes) = std::fs::read(&path) {
            let creds: AccountCredentials =
                serde_json::from_slice(&bytes).context("saved ACME account is unreadable")?;
            return Ok(Account::builder()?.from_credentials(creds).await?);
        }
        let new = NewAccount { contact: &[], terms_of_service_agreed: true, only_return_existing: false };
        let (account, creds) = Account::builder()?.create(&new, self.directory_url.clone(), None).await?;
        fsutil::atomic_write(&self.dir, "account.json", &serde_json::to_vec(&creds)?, 0o600)?;
        Ok(account)
    }

    async fn issue(&self, host: &str) -> Result<()> {
        let identifier = match host.parse::<IpAddr>() {
            Ok(ip) => Identifier::Ip(ip),
            Err(_) => Identifier::Dns(host.to_string()),
        };
        let ids = [identifier];
        let mut new = NewOrder::new(&ids);
        if matches!(ids[0], Identifier::Ip(_)) {
            new = new.profile("shortlived");
        }
        let account = self.account().await?;
        let mut order = account.new_order(&new).await?;
        let mut tokens = Vec::new();
        let mut authorizations = order.authorizations();
        while let Some(authz) = authorizations.next().await {
            let mut authz = authz?;
            match authz.status {
                AuthorizationStatus::Pending => {}
                AuthorizationStatus::Valid => continue,
                other => bail!("Let's Encrypt authorization is {other:?}"),
            }
            let mut challenge =
                authz.challenge(ChallengeType::Http01).context("Let's Encrypt offered no port 80 check")?;
            let answer = challenge.key_authorization().as_str().to_string();
            self.challenges.lock().unwrap().insert(challenge.token.clone(), answer);
            tokens.push(challenge.token.clone());
            challenge.set_ready().await?;
        }
        let retry =
            RetryPolicy::new().initial_delay(Duration::from_secs(2)).backoff(1.5).timeout(Duration::from_secs(180));
        let status = order.poll_ready(&retry).await;
        {
            let mut c = self.challenges.lock().unwrap();
            for t in &tokens {
                c.remove(t);
            }
        }
        let status = status?;
        if status != OrderStatus::Ready {
            let detail = order.state().error.as_ref().map(|p| format!(": {p:?}")).unwrap_or_default();
            bail!(
                "Let's Encrypt could not check {host} on port 80 (order {status:?}){detail}. Make sure TCP port 80 is open to the internet."
            );
        }
        let key_pem = order.finalize().await?;
        let chain_pem = order.poll_certificate(&retry).await?;
        let cert = parse_cert(chain_pem.as_bytes(), key_pem.as_bytes(), &self.provider)?;
        if !cert.hosts.iter().any(|h| h == host) {
            bail!("Let's Encrypt returned a certificate for {:?} instead of {host}", cert.hosts);
        }
        fsutil::atomic_write(&self.dir, "key.pem", key_pem.as_bytes(), 0o600)?;
        fsutil::atomic_write(&self.dir, "cert.pem", chain_pem.as_bytes(), 0o600)?;
        tracing::info!(host, expires_at_ms = cert.not_after_ms, "HTTPS certificate issued");
        self.install(cert);
        Ok(())
    }

    async fn target(&self) -> Result<String> {
        if let Some(host) = &self.host {
            return Ok(host.clone());
        }
        let found = public_ip(&self.lookups).await;
        let saved = self.current.read().unwrap().as_ref().and_then(|c| c.hosts.first().cloned());
        match (found, saved) {
            (Ok(ip), _) => Ok(ip),
            (Err(e), Some(saved)) => {
                tracing::warn!(error = %format!("{e:#}"), host = %saved, "keeping the address of the saved HTTPS certificate");
                Ok(saved)
            }
            (Err(e), None) => Err(e),
        }
    }

    async fn step(&self) -> Result<Duration> {
        let host = self.target().await?;
        let fresh = self
            .current
            .read()
            .unwrap()
            .as_ref()
            .is_some_and(|c| c.hosts.contains(&host) && c.renew_at_ms() > now_ms());
        if !fresh {
            self.issue(&host).await.with_context(|| format!("No certificate for {host}"))?;
        }
        let renew_at = self.current.read().unwrap().as_ref().map_or(0, |c| c.renew_at_ms());
        Ok(Duration::from_millis((renew_at - now_ms()).max(60_000) as u64).min(CHECK_EVERY))
    }

    pub async fn run(self: Arc<Self>, shutdown: CancellationToken) {
        let mut failures = 0u32;
        loop {
            let wait = match self.step().await {
                Ok(wait) => {
                    failures = 0;
                    *self.error.lock().unwrap() = None;
                    wait
                }
                Err(e) => {
                    failures += 1;
                    let wait = backoff(failures);
                    let msg = crate::redact::sanitize(&format!("{e:#}"));
                    tracing::error!(error = %msg, retry_in_secs = wait.as_secs(), "cannot get an HTTPS certificate");
                    *self.error.lock().unwrap() = Some(msg);
                    wait
                }
            };
            tokio::select! {
                _ = tokio::time::sleep(wait) => {}
                _ = shutdown.cancelled() => return,
            }
        }
    }
}

pub fn challenge_router(tls: Arc<Tls>, dashboard_port: u16) -> Router {
    Router::new()
        .route("/.well-known/acme-challenge/{token}", get(answer_challenge))
        .fallback(to_dashboard)
        .with_state((tls, dashboard_port))
}

async fn answer_challenge(State((tls, _)): State<(Arc<Tls>, u16)>, UrlPath(token): UrlPath<String>) -> Response {
    match tls.answer(&token) {
        Some(answer) => ([(header::CONTENT_TYPE, "text/plain")], answer).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn to_dashboard(State((_, port)): State<(Arc<Tls>, u16)>, headers: HeaderMap) -> Response {
    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .and_then(|h| h.parse::<axum::http::uri::Authority>().ok());
    match host {
        Some(a) => Redirect::temporary(&format!("https://{}:{port}/", a.host())).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn to_https(headers: HeaderMap, uri: Uri) -> Response {
    let Some(host) = headers.get(header::HOST).and_then(|v| v.to_str().ok()) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let path = uri.path_and_query().map(|p| p.as_str()).unwrap_or("/");
    Redirect::temporary(&format!("https://{host}{path}")).into_response()
}

async fn connection<I>(io: I, app: Router, peer: SocketAddr, https: bool)
where
    I: hyper::rt::Read + hyper::rt::Write + Unpin + Send + 'static,
{
    let app = app.layer(axum::Extension(ConnectInfo(peer))).layer(axum::Extension(Https(https)));
    let _ = auto::Builder::new(TokioExecutor::new())
        .serve_connection_with_upgrades(io, TowerToHyperService::new(app))
        .await;
}

pub async fn serve(listener: TcpListener, app: Router, tls: Option<Arc<Tls>>, shutdown: CancellationToken) {
    let acceptor = match tls.as_ref().map(|t| t.acceptor()).transpose() {
        Ok(a) => a,
        Err(e) => {
            tracing::error!(error = %e, "cannot set up HTTPS; serving plain HTTP");
            None
        }
    };
    let redirect = Router::new().fallback(to_https);
    loop {
        let (stream, peer) = tokio::select! {
            r = listener.accept() => match r {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!(error = %e, "accept failed");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
            },
            _ = shutdown.cancelled() => return,
        };
        let (app, redirect, tls, acceptor) = (app.clone(), redirect.clone(), tls.clone(), acceptor.clone());
        tokio::spawn(async move {
            let mut first = [0u8; 1];
            let hello =
                matches!(tokio::time::timeout(Duration::from_secs(10), stream.peek(&mut first)).await, Ok(Ok(1)))
                    && first[0] == 0x16;
            let ready = tls.as_ref().is_some_and(|t| t.ready());
            match (hello, ready, acceptor) {
                (true, true, Some(acceptor)) => {
                    if let Ok(Ok(s)) = tokio::time::timeout(Duration::from_secs(10), acceptor.accept(stream)).await {
                        connection(TokioIo::new(s), app, peer, true).await;
                    }
                }
                (false, true, Some(_)) => connection(TokioIo::new(stream), redirect, peer, false).await,
                (false, _, _) => connection(TokioIo::new(stream), app, peer, false).await,
                _ => {}
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider() -> CryptoProvider {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        rustls::crypto::aws_lc_rs::default_provider()
    }

    fn cert_pem(host: &str, not_before: i64, days: i64) -> (String, String) {
        let key = rcgen::KeyPair::generate().unwrap();
        let mut params = rcgen::CertificateParams::new(vec![host.into()]).unwrap();
        params.not_before = time::OffsetDateTime::from_unix_timestamp(not_before).unwrap();
        params.not_after = time::OffsetDateTime::from_unix_timestamp(not_before + days * 86400).unwrap();
        (params.self_signed(&key).unwrap().pem(), key.serialize_pem())
    }

    fn save(state: &Path, host: &str) {
        let (cert, key) = cert_pem(host, now_ms() / 1000 - 3600, 6);
        std::fs::create_dir_all(state.join("tls")).unwrap();
        std::fs::write(state.join("tls/cert.pem"), cert).unwrap();
        std::fs::write(state.join("tls/key.pem"), key).unwrap();
    }

    async fn lookup_server() -> String {
        let app = Router::new()
            .route("/junk", get(|| async { "<html>hello</html>" }))
            .route("/private", get(|| async { "10.0.0.7\n" }))
            .route("/good", get(|| async { "203.0.113.9\n" }))
            .route("/v6", get(|| async { "2001:DB8:0::1" }));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{addr}")
    }

    #[test]
    fn renewal_starts_after_two_thirds_of_the_lifetime() {
        let provider = provider();
        let (cert, key) = cert_pem("127.0.0.1", 1_700_000_000, 6);
        let c = parse_cert(cert.as_bytes(), key.as_bytes(), &provider).unwrap();
        assert_eq!(c.not_before_ms, 1_700_000_000_000);
        assert_eq!(c.renew_at_ms(), 1_700_000_000_000 + 4 * 86400 * 1000);
        assert_eq!(c.hosts, ["127.0.0.1"]);
        let (other, _) = cert_pem("2001:DB8:0::1", 1_700_000_000, 6);
        let other_key = rcgen::KeyPair::generate().unwrap();
        assert!(parse_cert(other.as_bytes(), other_key.serialize_pem().as_bytes(), &provider).is_err());
        let (v6, v6_key) = cert_pem("2001:DB8:0::1", 1_700_000_000, 6);
        assert_eq!(parse_cert(v6.as_bytes(), v6_key.as_bytes(), &provider).unwrap().hosts, ["2001:db8::1"]);
        assert_eq!(canonical("2001:DB8:0::1"), "2001:db8::1");
    }

    #[tokio::test]
    async fn the_public_ip_lookup_skips_bad_answers() {
        provider();
        let base = lookup_server().await;
        let urls = |paths: &[&str]| paths.iter().map(|p| format!("{base}{p}")).collect::<Vec<_>>();
        let ip = public_ip(&urls(&["/missing", "/junk", "/private", "/good"])).await.unwrap();
        assert_eq!(ip, "203.0.113.9");
        assert_eq!(public_ip(&urls(&["/v6"])).await.unwrap(), "2001:db8::1");
        let err = format!("{:#}", public_ip(&urls(&["/junk", "/private"])).await.unwrap_err());
        assert!(
            err.contains("not an IP address") && err.contains("10.0.0.7") && err.contains("Set HTTPS_HOST"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn auto_mode_follows_the_server_ip() {
        provider();
        let base = lookup_server().await;
        let d = tempfile::tempdir().unwrap();
        let no_acme = "http://127.0.0.1:9/directory";

        let tls = Tls::open(AUTO, d.path(), no_acme, vec![format!("{base}/junk")]).unwrap();
        let err = format!("{:#}", tls.step().await.unwrap_err());
        assert!(err.contains("Cannot find this server's public IP"), "{err}");

        save(d.path(), "203.0.113.9");
        let tls = Tls::open(AUTO, d.path(), no_acme, vec![format!("{base}/good")]).unwrap();
        let wait = tls.step().await.unwrap();
        assert!(wait > Duration::from_secs(60) && wait <= CHECK_EVERY, "{wait:?}");

        let offline = Tls::open(AUTO, d.path(), no_acme, vec![format!("{base}/junk")]).unwrap();
        assert!(offline.step().await.is_ok());

        save(d.path(), "198.51.100.4");
        let moved = Tls::open(AUTO, d.path(), no_acme, vec![format!("{base}/good")]).unwrap();
        assert!(moved.ready());
        assert!(moved.step().await.is_err());
        assert_eq!(moved.current.read().unwrap().as_ref().unwrap().hosts, ["198.51.100.4"]);

        let fixed = Tls::open("198.51.100.4", d.path(), no_acme, vec![format!("{base}/good")]).unwrap();
        assert!(fixed.step().await.is_ok());
        let renamed = Tls::open("203.0.113.9", d.path(), no_acme, vec![]).unwrap();
        assert!(renamed.step().await.is_err());
    }

    #[tokio::test]
    async fn port_80_answers_the_certificate_check_and_sends_people_to_the_dashboard() {
        use tower::ServiceExt;
        let d = tempfile::tempdir().unwrap();
        let tls = Tls::new(AUTO, d.path(), LETS_ENCRYPT).unwrap();
        tls.challenges.lock().unwrap().insert("tok".into(), "tok.thumb".into());
        let app = challenge_router(tls.clone(), 6757);
        let get = |p: &str, host: &str| {
            axum::http::Request::get(p).header(header::HOST, host).body(axum::body::Body::empty()).unwrap()
        };
        let r = app.clone().oneshot(get("/.well-known/acme-challenge/tok", "203.0.113.5")).await.unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        let body = axum::body::to_bytes(r.into_body(), 1024).await.unwrap();
        assert_eq!(&body[..], b"tok.thumb");
        let r = app.clone().oneshot(get("/.well-known/acme-challenge/other", "203.0.113.5")).await.unwrap();
        assert_eq!(r.status(), StatusCode::NOT_FOUND);
        let r = app.clone().oneshot(get("/anything", "203.0.113.5")).await.unwrap();
        assert_eq!(r.headers()[header::LOCATION], "https://203.0.113.5:6757/");
        let r = app.oneshot(get("/", "[2001:db8::1]:80")).await.unwrap();
        assert_eq!(r.headers()[header::LOCATION], "https://[2001:db8::1]:6757/");
        assert!(!tls.ready());
        assert!(d.path().join("tls").is_dir());
    }

    #[test]
    fn retries_back_off_up_to_two_hours() {
        assert_eq!(backoff(1), Duration::from_secs(600));
        assert_eq!(backoff(2), Duration::from_secs(1200));
        assert_eq!(backoff(4), Duration::from_secs(4800));
        assert_eq!(backoff(9), Duration::from_secs(7200));
    }
}

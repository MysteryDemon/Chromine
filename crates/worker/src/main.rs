use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use chromine_worker::config::Config;
use chromine_worker::{App, lock_state_dir, prepare_directories, redact};
use tokio_util::sync::CancellationToken;

fn init_logging() {
    use tracing_subscriber::{EnvFilter, fmt};
    let level = std::env::var("LOG_LEVEL").unwrap_or_else(|_| "info".into());
    let spec = if level.contains('=') { level } else { format!("{level},sqlx=warn,hyper=warn") };
    let filter = EnvFilter::try_new(&spec).unwrap_or_else(|_| EnvFilter::new("info"));
    if std::env::var("LOG_FORMAT").as_deref() == Ok("json") {
        fmt().with_env_filter(filter).json().init();
    } else {
        fmt().with_env_filter(filter).init();
    }
}

fn usage() -> ! {
    eprintln!(
        "usage: chromine-worker <command>\n\n\
         commands:\n  \
         serve             run the worker (default)\n  \
         check-config      validate configuration and exit\n  \
         healthcheck       used by the container health check\n  \
         filecloud-smoke --upload-public-test-file\n                    \
         upload a small random file to FileCloud24 and print its public link"
    );
    std::process::exit(2);
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let cmd = args.get(1).map(String::as_str).unwrap_or("serve");
    match cmd {
        "serve" => {
            init_logging();
            runtime()?.block_on(serve())
        }
        "check-config" => {
            Config::from_env()?;
            println!("configuration OK");
            Ok(())
        }
        "healthcheck" => healthcheck(),
        "filecloud-smoke" => {
            init_logging();
            if !args.iter().any(|a| a == "--upload-public-test-file") {
                bail!(
                    "this uploads a small random file to the public FileCloud24 service; pass --upload-public-test-file to confirm"
                );
            }
            runtime()?.block_on(filecloud_smoke())
        }
        "-h" | "--help" | "help" => usage(),
        _ => usage(),
    }
}

fn runtime() -> Result<tokio::runtime::Runtime> {
    Ok(tokio::runtime::Builder::new_multi_thread().enable_all().build()?)
}

fn healthcheck() -> Result<()> {
    let listen: SocketAddr = std::env::var("WORKER_LISTEN").unwrap_or_else(|_| "0.0.0.0:8000".into()).parse()?;
    let port = listen.port();
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse()?;
    use std::io::{Read, Write};
    let mut s = std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(3)).context("worker not listening")?;
    s.set_read_timeout(Some(Duration::from_secs(5)))?;
    s.write_all(b"GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")?;
    let mut buf = String::new();
    s.read_to_string(&mut buf)?;
    if buf.starts_with("HTTP/1.1 200") {
        println!("ok");
        Ok(())
    } else {
        eprintln!("unhealthy: {}", buf.lines().next().unwrap_or(""));
        std::process::exit(1);
    }
}

async fn serve() -> Result<()> {
    unsafe {
        libc::umask(0o077);
    }
    let cfg = Config::from_env()?;
    redact::register_secret(cfg.auth_password.expose());
    prepare_directories(&cfg)?;
    let _lock = lock_state_dir(&cfg)?;
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        delete_after_success = cfg.delete_after_success,
        filecloud = %cfg.filecloud_base_url,
        "starting worker"
    );

    let app = App::build(cfg.clone(), 1.0).await?;

    let shutdown = CancellationToken::new();
    let handles = app.spawn_workers(&shutdown);

    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let tls = app.web.tls.clone();
    let listener =
        tokio::net::TcpListener::bind(cfg.listen).await.with_context(|| format!("cannot listen on {}", cfg.listen))?;
    tracing::info!(listen = %cfg.listen, https = tls.is_some(), "dashboard listening");
    let router = chromine_worker::web::router(app.web.clone());
    if let Some(t) = &tls {
        let challenge = tokio::net::TcpListener::bind(cfg.acme_listen)
            .await
            .with_context(|| format!("cannot listen on {} for the HTTPS certificate check", cfg.acme_listen))?;
        tracing::info!(listen = %cfg.acme_listen, host = cfg.https_host.as_deref().unwrap_or(""), "HTTPS enabled; certificate check and redirect listener ready");
        tokio::spawn(chromine_worker::tls::serve(
            challenge,
            chromine_worker::tls::challenge_router(t.clone(), cfg.dashboard_port),
            None,
            shutdown.clone(),
        ));
        tokio::spawn(t.clone().run(shutdown.clone()));
    }
    if let Some(socket) = &cfg.code_socket {
        let code_listener = tokio::net::TcpListener::bind(cfg.code_listen)
            .await
            .with_context(|| format!("cannot listen on {} for VS Code", cfg.code_listen))?;
        tracing::info!(listen = %cfg.code_listen, socket = %socket.display(), "VS Code reachable through the dashboard login");
        let code = chromine_worker::code::router(app.web.clone());
        tokio::spawn(chromine_worker::tls::serve(code_listener, code, tls.clone(), shutdown.clone()));
    }
    {
        let sd = shutdown.clone();
        tokio::spawn(async move {
            let mut term =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("SIGTERM handler");
            tokio::select! {
                _ = term.recv() => {}
                _ = tokio::signal::ctrl_c() => {}
                _ = sd.cancelled() => {}
            }
            tracing::info!("shutting down");
            sd.cancel();
        });
    }
    chromine_worker::tls::serve(listener, router, tls, shutdown.clone()).await;
    shutdown.cancel();
    for h in handles {
        let _ = tokio::time::timeout(Duration::from_secs(20), h).await;
    }
    tracing::info!("worker stopped");
    Ok(())
}

async fn filecloud_smoke() -> Result<()> {
    let base = std::env::var("FILECLOUD_BASE_URL").unwrap_or_else(|_| "https://filecloud24.cc".into());
    let base = chromine_worker::config::parse_base_url(&base, false)?;
    let fc = chromine_worker::filecloud::FileCloud::new(base, Duration::from_secs(120))?;
    let cfg = fc.service_config().await?;
    println!(
        "service config: max_upload_size={} hourly_allowance={} ready={}",
        cfg.max_upload_size, cfg.hourly_upload_allowance, cfg.ready
    );
    let dir = tempfile_dir()?;
    let path = dir.join("chromine-smoke.bin");
    let data: Vec<u8> = (0..4096).map(|_| rand::random::<u8>()).collect();
    std::fs::write(&path, &data)?;
    let s = fc.start("chromine-smoke-test.bin", data.len() as u64, "application/octet-stream").await?;
    println!("session: chunk_size={} chunk_count={}", s.chunk_size, s.chunk_count);
    for i in 0..s.chunk_count {
        fc.put_chunk(&s, &path, data.len() as u64, i, Arc::new(Default::default())).await?;
    }
    let link = fc.complete(&s).await?;
    println!("uploaded: {link}");
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

fn tempfile_dir() -> Result<std::path::PathBuf> {
    let d = std::env::temp_dir().join(format!("chromine-smoke-{}", std::process::id()));
    std::fs::create_dir_all(&d)?;
    Ok(d)
}

use anyhow::Result;
use chromine_agent::config::AgentConfig;

fn init_logging() {
    use tracing_subscriber::{EnvFilter, fmt};
    let level = std::env::var("LOG_LEVEL").unwrap_or_else(|_| "info".into());
    let filter = EnvFilter::try_new(&level).unwrap_or_else(|_| EnvFilter::new("info"));
    if std::env::var("LOG_FORMAT").as_deref() == Ok("json") {
        fmt().with_env_filter(filter).json().with_target(true).init();
    } else {
        fmt().with_env_filter(filter).with_target(true).init();
    }
}

fn main() -> Result<()> {
    if std::env::args().next().is_some_and(|a| chromine_agent::video::is_host_invocation(&a)) {
        let level = std::env::var("LOG_LEVEL").unwrap_or_else(|_| "info".into());
        let filter = tracing_subscriber::EnvFilter::try_new(&level)
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
        tracing_subscriber::fmt().with_env_filter(filter).with_writer(std::io::stderr).with_ansi(false).init();
        unsafe {
            libc::umask(0o007);
        }
        let dir = std::env::var("VIDEO_DIR")
            .ok()
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| chromine_agent::video::DEFAULT_VIDEO_DIR.into());
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
        return rt.block_on(chromine_agent::video::host_serve(dir.into(), tokio::io::stdin(), tokio::io::stdout()));
    }
    if std::env::args().nth(1).is_some_and(|a| chromine_agent::native::is_native_host_invocation(&a)) {
        let level = std::env::var("LOG_LEVEL").unwrap_or_else(|_| "info".into());
        let filter = tracing_subscriber::EnvFilter::try_new(&level)
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
        tracing_subscriber::fmt().with_env_filter(filter).with_writer(std::io::stderr).with_ansi(false).init();
        unsafe {
            libc::umask(0o007);
        }
        let cfg = chromine_agent::native::HostConfig::from_env()?;
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
        return rt.block_on(chromine_agent::native::serve(cfg, tokio::io::stdin(), tokio::io::stdout()));
    }

    let cmd = std::env::args().nth(1).unwrap_or_else(|| "run".into());
    match cmd.as_str() {
        "run" => {
            init_logging();
            let cfg = AgentConfig::from_env()?;
            tracing::info!(config = ?cfg, version = env!("CARGO_PKG_VERSION"), "starting browser agent");
            let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
            rt.block_on(chromine_agent::run(cfg))
        }
        "healthcheck" => {
            let cfg = AgentConfig::from_env()?;
            match chromine_agent::healthcheck(&cfg) {
                Ok(()) => {
                    println!("ok");
                    Ok(())
                }
                Err(e) => {
                    eprintln!("unhealthy: {e:#}");
                    std::process::exit(1);
                }
            }
        }
        "sandbox-check" => {
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
            let (ok, detail) = rt.block_on(chromine_agent::sandbox_self_test());
            println!("{detail}");
            std::process::exit(if ok { 0 } else { 1 });
        }
        "version" => {
            println!("chromine-agent {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        other => {
            eprintln!("unknown command {other:?}; expected run | healthcheck | sandbox-check | version");
            std::process::exit(2);
        }
    }
}

use anyhow::Result;
use tokio::net::TcpListener;
use tracing::info;
use tracing_subscriber::EnvFilter;
use venue_hl::config::Config;

/// Resolves on SIGINT (Ctrl-C) or SIGTERM (container stop).
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! { _ = ctrl_c => {}, _ = term => {} }
    info!("shutdown requested");
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let cfg = Config::from_env()?;
    info!(
        account = %cfg.address,
        mode = ?cfg.mode(),
        testnet = cfg.testnet,
        listen = %cfg.listen_addr,
        "venue-hl starting"
    );
    let listener = TcpListener::bind(cfg.listen_addr).await?;
    venue_hl::serve(cfg, listener, shutdown_signal()).await
}

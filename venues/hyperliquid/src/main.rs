use anyhow::Result;
use tokio::net::TcpListener;
use tracing::info;
use tracing_subscriber::EnvFilter;
use venue_hl::config::Config;

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
    venue_hl::serve(cfg, listener, async {
        let _ = tokio::signal::ctrl_c().await;
        info!("shutdown requested");
    })
    .await
}

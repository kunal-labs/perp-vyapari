use anyhow::{Context, Result};
use gateway::{health_interval, parse_venues, probe};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;
use venue_proto::v1::Mode;

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
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let venues = parse_venues(&std::env::var("VENUES").context("VENUES is required")?)?;
    let every = health_interval(std::env::var("HEALTH_SECS").ok())?;
    info!(
        venues = venues.len(),
        "gateway starting (M0: health monitor only, Telegram arrives in M1)"
    );

    let shutdown = shutdown_signal();
    tokio::pin!(shutdown);
    let mut tick = tokio::time::interval(every);
    loop {
        tokio::select! {
            _ = tick.tick() => {
                for v in &venues {
                    match probe(v).await {
                        Ok(h) => info!(
                            venue = %v.name,
                            kind = %h.venue,
                            mode = ?Mode::try_from(h.mode).unwrap_or(Mode::Unspecified),
                            account = %h.account,
                            testnet = h.testnet,
                            "healthy"
                        ),
                        Err(e) => warn!(venue = %v.name, error = %format!("{e:#}"), "unhealthy"),
                    }
                }
            }
            _ = &mut shutdown => {
                info!("shutdown requested");
                return Ok(());
            }
        }
    }
}

use anyhow::{Context, Result};
use gateway::{health_interval, parse_venues, probe};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

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

    let mut tick = tokio::time::interval(every);
    loop {
        tokio::select! {
            _ = tick.tick() => {
                for v in &venues {
                    match probe(v).await {
                        Ok(h) => info!(venue = %v.name, kind = %h.venue, mode = h.mode, account = %h.account, testnet = h.testnet, "healthy"),
                        Err(e) => warn!(venue = %v.name, error = %format!("{e:#}"), "unhealthy"),
                    }
                }
            }
            _ = tokio::signal::ctrl_c() => {
                info!("shutdown requested");
                return Ok(());
            }
        }
    }
}

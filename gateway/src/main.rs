use anyhow::{Context, Result};
use gateway::{app::App, parse_venues, probe, telegram};
use teloxide::Bot;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

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
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| "info,teloxide=warn".into()),
        )
        .init();

    let venues = parse_venues(&std::env::var("VENUES").context("VENUES is required")?)?;
    let owner = telegram::parse_owner(std::env::var("TELEGRAM_OWNER_ID").ok())?;
    let token = telegram::read_token(
        &std::env::var("TELEGRAM_TOKEN_FILE").context("TELEGRAM_TOKEN_FILE is required")?,
    )?;
    info!(venues = venues.len(), "gateway starting");

    // One-off startup health log; ongoing liveness comes from each venue's event stream.
    for v in &venues {
        match probe(v).await {
            Ok(h) => {
                info!(venue = %v.name, kind = %h.venue, account = %h.account, testnet = h.testnet, "venue healthy")
            }
            Err(e) => warn!(venue = %v.name, error = %format!("{e:#}"), "venue not reachable yet"),
        }
    }

    let app = App::new(&venues)?;
    telegram::run(Bot::new(token), owner, app, shutdown_signal()).await
}

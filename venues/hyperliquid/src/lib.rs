//! Hyperliquid venue service library (used by the binary and integration tests).

pub mod alerts;
pub mod config;
pub mod hl;
pub mod monitor;
pub mod service;
pub mod snapshot;
#[cfg(feature = "testing")]
pub mod testing;

use std::{sync::Arc, time::SystemTime};

use anyhow::Result;
use tokio::net::TcpListener;
use tokio_stream::wrappers::TcpListenerStream;
use venue_proto::v1::venue_service_server::VenueServiceServer;

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or_default()
}

/// Serve the venue gRPC API (and run the account monitor) until `shutdown` resolves.
pub async fn serve(
    cfg: config::Config,
    listener: TcpListener,
    shutdown: impl std::future::Future<Output = ()> + Send,
) -> Result<()> {
    let client = hl::InfoClient::new(&cfg.api_url)?;
    let shared = monitor::Shared::new();
    let monitor = tokio::spawn(monitor::run(cfg.clone(), client.clone(), shared.clone()));
    let svc = service::HyperliquidVenue::new(cfg, client, Arc::clone(&shared));
    let result = tonic::transport::Server::builder()
        .add_service(VenueServiceServer::new(svc))
        .serve_with_incoming_shutdown(TcpListenerStream::new(listener), shutdown)
        .await;
    monitor.abort();
    result?;
    Ok(())
}

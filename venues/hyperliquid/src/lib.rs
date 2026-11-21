//! Hyperliquid venue service library (used by the binary and integration tests).

pub mod config;
pub mod service;

use anyhow::Result;
use tokio::net::TcpListener;
use tokio_stream::wrappers::TcpListenerStream;
use venue_proto::v1::venue_service_server::VenueServiceServer;

/// Serve the venue gRPC API on an already-bound listener until `shutdown` resolves.
pub async fn serve(
    cfg: config::Config,
    listener: TcpListener,
    shutdown: impl std::future::Future<Output = ()> + Send,
) -> Result<()> {
    tonic::transport::Server::builder()
        .add_service(VenueServiceServer::new(service::HyperliquidVenue::new(cfg)))
        .serve_with_incoming_shutdown(TcpListenerStream::new(listener), shutdown)
        .await?;
    Ok(())
}

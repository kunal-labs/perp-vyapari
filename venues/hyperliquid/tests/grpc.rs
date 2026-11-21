//! End-to-end: start the gRPC server on an ephemeral port and call it.

use std::time::Duration;

use tokio::net::TcpListener;
use tokio_stream::StreamExt;
use venue_hl::config::Config;
use venue_proto::v1::{
    event, venue_service_client::VenueServiceClient, HealthRequest, ListPositionsRequest, Mode,
    StreamEventsRequest,
};

const ADDR: &str = "0x00000000000000000000000000000000000000aa";

async fn start() -> (String, tokio::sync::oneshot::Sender<()>) {
    let cfg = Config::from_lookup(|k| match k {
        "HL_ADDRESS" => Some(ADDR.into()),
        "HEARTBEAT_SECS" => Some("1".into()),
        _ => None,
    })
    .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(venue_hl::serve(cfg, listener, async {
        let _ = rx.await;
    }));
    (url, tx)
}

#[tokio::test]
async fn health_reports_read_only_identity() {
    let (url, _stop) = start().await;
    let mut c = VenueServiceClient::connect(url).await.unwrap();
    let h = c.health(HealthRequest {}).await.unwrap().into_inner();
    assert_eq!(h.venue, "hyperliquid");
    assert_eq!(h.account, ADDR);
    assert_eq!(h.mode(), Mode::ReadOnly);
    assert!(!h.testnet);
    assert!(h.server_time_ms > 0);
}

#[tokio::test]
async fn unimplemented_rpcs_say_so() {
    let (url, _stop) = start().await;
    let mut c = VenueServiceClient::connect(url).await.unwrap();
    let err = c.list_positions(ListPositionsRequest {}).await.unwrap_err();
    assert_eq!(err.code(), tonic::Code::Unimplemented);
}

#[tokio::test]
async fn event_stream_emits_heartbeat() {
    let (url, _stop) = start().await;
    let mut c = VenueServiceClient::connect(url).await.unwrap();
    let mut s = c
        .stream_events(StreamEventsRequest {})
        .await
        .unwrap()
        .into_inner();
    let ev = tokio::time::timeout(Duration::from_secs(3), s.next())
        .await
        .expect("no heartbeat within 3s")
        .expect("stream ended")
        .unwrap();
    assert!(matches!(ev.kind, Some(event::Kind::Heartbeat(_))));
}

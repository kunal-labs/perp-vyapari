//! Gateway <-> venue-hl over real gRPC on an ephemeral port.

use gateway::{probe, VenueEndpoint};
use tokio::net::TcpListener;
use venue_hl::config::Config;
use venue_proto::v1::Mode;

#[tokio::test]
async fn probe_reaches_hyperliquid_venue() {
    let cfg = Config::from_lookup(|k| {
        (k == "HL_ADDRESS").then(|| "0x00000000000000000000000000000000000000bb".to_string())
    })
    .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(venue_hl::serve(cfg, listener, std::future::pending()));

    let h = probe(&VenueEndpoint {
        name: "hl".into(),
        url,
    })
    .await
    .unwrap();
    assert_eq!(h.venue, "hyperliquid");
    assert_eq!(h.mode(), Mode::ReadOnly);
}

#[tokio::test]
async fn probe_fails_cleanly_when_venue_down() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    assert!(probe(&VenueEndpoint {
        name: "hl".into(),
        url
    })
    .await
    .is_err());
}

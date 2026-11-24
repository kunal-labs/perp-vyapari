//! Command handling end to end: App -> gRPC -> venue-hl -> fake Hyperliquid API.

use gateway::{app::App, probe, VenueEndpoint};
use tokio::net::TcpListener;
use venue_hl::{config::Config, testing::FakeHl};
use venue_proto::v1::Mode;

async fn start_venue() -> VenueEndpoint {
    let api = FakeHl::with_defaults().start().await;
    let cfg = Config::from_lookup(|k| match k {
        "HL_ADDRESS" => Some("0x00000000000000000000000000000000000000bb".into()),
        "HL_API_URL" => Some(api.clone()),
        _ => None,
    })
    .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(venue_hl::serve(cfg, listener, std::future::pending()));
    VenueEndpoint {
        name: "hl".into(),
        url,
    }
}

#[tokio::test]
async fn probe_reaches_hyperliquid_venue() {
    let ep = start_venue().await;
    let h = probe(&ep).await.unwrap();
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

#[tokio::test]
async fn read_commands_render_venue_data() {
    let app = App::new(&[start_venue().await]).unwrap();
    assert!(app.handle("hello").await.is_none(), "plain text is ignored");

    let pos = app.handle("/pos").await.unwrap();
    assert!(pos.contains("BTC LONG"), "{pos}");
    assert!(pos.contains("🛡 stop 58,000.00"), "{pos}");
    assert!(
        pos.contains("ETH SHORT") && pos.contains("NO STOP"),
        "{pos}"
    );

    let acct = app.handle("/acct hl").await.unwrap();
    assert!(acct.contains("Equity (perps): <b>320.50</b>"), "{acct}");
    assert!(acct.contains("Collateral (unified USDC): 400.25"), "{acct}");

    let orders = app.handle("/orders btc").await.unwrap();
    assert!(
        orders.contains("BTC sell 0.1 stop trigger 58,000.00 (reduce-only)"),
        "{orders}"
    );

    let pnl = app.handle("/pnl").await.unwrap();
    assert!(pnl.contains("-79.50"), "{pnl}");

    let status = app.handle("/status").await.unwrap();
    assert!(status.contains("read-only"), "{status}");
}

#[tokio::test]
async fn price_alert_commands_round_trip() {
    let app = App::new(&[start_venue().await]).unwrap();
    let added = app.handle("/alert btc above 70,000").await.unwrap();
    assert!(added.contains("BTC ≥ 70,000.00"), "{added}");
    assert!(app.handle("/alerts").await.unwrap().contains("pa1"));
    assert!(app
        .handle("/unalert pa1")
        .await
        .unwrap()
        .contains("removed"));
    assert!(app
        .handle("/alert nope above 1")
        .await
        .unwrap()
        .contains("unknown Hyperliquid perp"));
    assert!(app
        .handle("/alert btc sideways 1")
        .await
        .unwrap()
        .contains("usage"));
}

#[tokio::test]
async fn unreachable_venue_reports_error_not_panic() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    let app = App::new(&[VenueEndpoint {
        name: "hl".into(),
        url,
    }])
    .unwrap();
    let r = app.handle("/pos").await.unwrap();
    assert!(r.starts_with("❌ <b>hl</b>"), "{r}");
}

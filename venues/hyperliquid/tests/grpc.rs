//! End-to-end: fake Hyperliquid API -> venue-hl gRPC server -> client.

use std::time::Duration;

use tokio::net::TcpListener;
use tokio_stream::StreamExt;
use venue_hl::{config::Config, testing::FakeHl};
use venue_proto::v1::{
    event, venue_service_client::VenueServiceClient, AddPriceAlertRequest, GetAccountRequest,
    HealthRequest, ListOrdersRequest, ListPositionsRequest, ListPriceAlertsRequest, Mode,
    PriceDirection, RemovePriceAlertRequest, SetProtectionRequest, StreamEventsRequest,
};

const ADDR: &str = "0x00000000000000000000000000000000000000aa";

type Client = VenueServiceClient<tonic::transport::Channel>;

async fn start(fake: &FakeHl) -> (Client, tokio::sync::oneshot::Sender<()>) {
    let api = fake.start().await;
    let cfg = Config::from_lookup(|k| match k {
        "HL_ADDRESS" => Some(ADDR.into()),
        "HEARTBEAT_SECS" => Some("1".into()),
        "POLL_SECS" => Some("1".into()),
        "HL_API_URL" => Some(api.clone()),
        _ => None,
    })
    .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(venue_hl::serve(cfg, listener, async {
        let _ = rx.await;
    }));
    (VenueServiceClient::connect(url).await.unwrap(), tx)
}

#[tokio::test]
async fn health_reports_read_only_identity() {
    let (mut c, _stop) = start(&FakeHl::with_defaults()).await;
    let h = c.health(HealthRequest {}).await.unwrap().into_inner();
    assert_eq!(h.venue, "hyperliquid");
    assert_eq!(h.account, ADDR);
    assert_eq!(h.mode(), Mode::ReadOnly);
}

#[tokio::test]
async fn read_views_map_fake_account() {
    let (mut c, _stop) = start(&FakeHl::with_defaults()).await;
    let a = c
        .get_account(GetAccountRequest {})
        .await
        .unwrap()
        .into_inner();
    assert_eq!(a.account_value, "420.5", "main + xyz dex");
    assert_eq!(a.total_collateral, "400.25");

    let ps = c
        .list_positions(ListPositionsRequest {})
        .await
        .unwrap()
        .into_inner()
        .positions;
    assert_eq!(ps.len(), 3, "BTC, ETH on main + xyz:NVDA");
    assert!(ps[0].has_exchange_stop);
    assert!(!ps[1].has_exchange_stop);

    let os = c
        .list_orders(ListOrdersRequest {
            symbol: String::new(),
        })
        .await
        .unwrap()
        .into_inner()
        .orders;
    assert_eq!(os.len(), 2, "orders from every dex");
}

#[tokio::test]
async fn writes_are_not_available_yet() {
    let (mut c, _stop) = start(&FakeHl::with_defaults()).await;
    let err = c
        .set_protection(SetProtectionRequest {
            symbol: "BTC".into(),
            ..Default::default()
        })
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::Unimplemented);
}

#[tokio::test]
async fn price_alert_crud_and_validation() {
    let (mut c, _stop) = start(&FakeHl::with_defaults()).await;
    let add = |sym: &str, dir: PriceDirection, px: &str| AddPriceAlertRequest {
        symbol: sym.into(),
        direction: dir.into(),
        price: px.into(),
    };
    let a = c
        .add_price_alert(add("btc", PriceDirection::Above, "70000"))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(a.symbol, "BTC", "symbol normalised to venue casing");
    assert_eq!(
        c.add_price_alert(add("NOPE", PriceDirection::Above, "1"))
            .await
            .unwrap_err()
            .code(),
        tonic::Code::NotFound
    );
    assert_eq!(
        c.add_price_alert(add("BTC", PriceDirection::Unspecified, "1"))
            .await
            .unwrap_err()
            .code(),
        tonic::Code::InvalidArgument
    );
    assert_eq!(
        c.add_price_alert(add("BTC", PriceDirection::Below, "-5"))
            .await
            .unwrap_err()
            .code(),
        tonic::Code::InvalidArgument
    );
    assert_eq!(
        c.list_price_alerts(ListPriceAlertsRequest {})
            .await
            .unwrap()
            .into_inner()
            .alerts
            .len(),
        1
    );
    let r = c
        .remove_price_alert(RemovePriceAlertRequest {
            alert_id: a.alert_id,
        })
        .await
        .unwrap()
        .into_inner();
    assert!(r.removed);
}

#[tokio::test]
async fn event_stream_pushes_no_stop_and_price_alerts() {
    let fake = FakeHl::with_defaults();
    let (mut c, _stop) = start(&fake).await;
    c.add_price_alert(AddPriceAlertRequest {
        symbol: "BTC".into(),
        direction: PriceDirection::Above.into(),
        price: "59000".into(),
    })
    .await
    .unwrap();
    let mut s = c
        .stream_events(StreamEventsRequest {})
        .await
        .unwrap()
        .into_inner();

    let mut codes = Vec::new();
    let mut saw_heartbeat = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline && !(saw_heartbeat && codes.len() >= 2) {
        let Ok(Some(Ok(ev))) = tokio::time::timeout(Duration::from_secs(3), s.next()).await else {
            break;
        };
        match ev.kind {
            Some(event::Kind::Alert(a)) => codes.push(format!("{}:{}", a.code, a.symbol)),
            Some(event::Kind::Heartbeat(_)) => saw_heartbeat = true,
            _ => {}
        }
    }
    assert!(saw_heartbeat);
    assert!(codes.contains(&"NO_STOP:ETH".to_string()), "got {codes:?}");
    assert!(codes.contains(&"PRICE:BTC".to_string()), "got {codes:?}");
    // One-shot: the price alert is consumed.
    assert!(c
        .list_price_alerts(ListPriceAlertsRequest {})
        .await
        .unwrap()
        .into_inner()
        .alerts
        .is_empty());
}

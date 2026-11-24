//! In-process fake of the Hyperliquid `/info` API for tests (feature `testing`).

use std::sync::Arc;

use axum::{extract::State, routing::post, Json, Router};
use serde_json::{json, Value};
use tokio::{net::TcpListener, sync::Mutex};

/// Canned responses keyed by `/info` request `type`. Mutable so tests can change state.
#[derive(Clone, Default)]
pub struct FakeHl {
    pub responses: Arc<Mutex<std::collections::HashMap<String, Value>>>,
}

pub const STATE: &str = r#"{"marginSummary":{"accountValue":"320.5","totalNtlPos":"6000.0","totalRawUsd":"-5679.5","totalMarginUsed":"300.0"},
  "crossMaintenanceMarginUsed":"75.0","withdrawable":"0.0",
  "assetPositions":[{"type":"oneWay","position":{"coin":"BTC","szi":"0.1","leverage":{"type":"cross","value":20},
    "entryPx":"59000.0","positionValue":"6000.0","unrealizedPnl":"20.5","liquidationPx":"56000.0","marginUsed":"300.0","maxLeverage":40}},
    {"type":"oneWay","position":{"coin":"ETH","szi":"-2.0","leverage":{"type":"isolated","value":5},
    "entryPx":"3000.0","positionValue":"6100.0","unrealizedPnl":"-100.0","liquidationPx":null,"marginUsed":"1200.0","maxLeverage":25}}],
  "time":1795222383028}"#;
pub const SPOT: &str =
    r#"{"balances":[{"coin":"USDC","token":0,"total":"400.25","hold":"400.25","entryNtl":"0.0"}]}"#;
pub const ORDERS: &str = r#"[{"coin":"BTC","side":"A","limitPx":"53000.0","sz":"0.1","oid":11,"timestamp":1795213643987,"isTrigger":true,"triggerPx":"58000.0","reduceOnly":true,"orderType":"Stop Market"}]"#;
pub const META: &str = r#"[{"universe":[{"szDecimals":5,"name":"BTC","maxLeverage":40},{"szDecimals":4,"name":"ETH","maxLeverage":25}]},
  [{"funding":"0.0000125","markPx":"59205.0"},{"funding":"-0.00003","markPx":"3050.0"}]]"#;

impl FakeHl {
    /// A unified account: BTC long with a stop, ETH short without one.
    pub fn with_defaults() -> Self {
        let mut m = std::collections::HashMap::new();
        m.insert(
            "clearinghouseState".into(),
            serde_json::from_str(STATE).unwrap(),
        );
        m.insert(
            "spotClearinghouseState".into(),
            serde_json::from_str(SPOT).unwrap(),
        );
        m.insert(
            "frontendOpenOrders".into(),
            serde_json::from_str(ORDERS).unwrap(),
        );
        m.insert(
            "metaAndAssetCtxs".into(),
            serde_json::from_str(META).unwrap(),
        );
        m.insert("userAbstraction".into(), json!("unifiedAccount"));
        m.insert("userFillsByTime".into(), json!([]));
        Self {
            responses: Arc::new(Mutex::new(m)),
        }
    }

    /// Serve on an ephemeral localhost port; returns the base URL.
    pub async fn start(&self) -> String {
        let app = Router::new()
            .route("/info", post(info))
            .with_state(self.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        url
    }
}

async fn info(State(fake): State<FakeHl>, Json(body): Json<Value>) -> Json<Value> {
    let kind = body["type"].as_str().unwrap_or_default().to_string();
    let m = fake.responses.lock().await;
    Json(m.get(&kind).cloned().unwrap_or(Value::Null))
}

//! Minimal Hyperliquid `/info` client (read-only, unauthenticated).
//!
//! Signing and the `/exchange` endpoint arrive with managed mode; read paths stay
//! plain HTTP so they work with nothing but the account address.

use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use serde::{de::DeserializeOwned, Deserialize};
use serde_json::{json, Value};

/// How long the perp-dex list and account mode are cached (they rarely change).
const SLOW_CACHE: Duration = Duration::from_secs(3600);

/// The perp dex a coin trades on: `"xyz:NVDA"` -> `"xyz"`, `"BTC"` -> `""` (main dex).
pub fn dex_of(coin: &str) -> &str {
    coin.split_once(':').map_or("", |(d, _)| d)
}

/// Add `"dex"` to a request body for non-main dexes.
fn with_dex(mut body: Value, dex: &str) -> Value {
    if !dex.is_empty() {
        body["dex"] = Value::String(dex.to_string());
    }
    body
}

#[derive(Default)]
struct SlowCache {
    dexes: Option<(Instant, Vec<String>)>,
    abstraction: Option<(Instant, String)>,
}

pub const MAINNET_URL: &str = "https://api.hyperliquid.xyz";
pub const TESTNET_URL: &str = "https://api.hyperliquid-testnet.xyz";

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarginSummary {
    pub account_value: String,
    pub total_margin_used: String,
    #[serde(default)]
    pub total_ntl_pos: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Leverage {
    #[serde(rename = "type")]
    pub kind: String,
    pub value: u32,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PositionData {
    pub coin: String,
    pub szi: String,
    pub leverage: Leverage,
    pub entry_px: Option<String>,
    #[serde(default)]
    pub position_value: String,
    pub unrealized_pnl: String,
    pub liquidation_px: Option<String>,
    pub margin_used: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AssetPosition {
    pub position: PositionData,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClearinghouseState {
    pub margin_summary: MarginSummary,
    #[serde(default)]
    pub cross_maintenance_margin_used: String,
    pub withdrawable: String,
    pub asset_positions: Vec<AssetPosition>,
    pub time: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SpotBalance {
    pub coin: String,
    pub total: String,
    #[serde(default)]
    pub hold: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SpotState {
    pub balances: Vec<SpotBalance>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrontendOrder {
    pub coin: String,
    /// "B" = bid/buy, "A" = ask/sell.
    pub side: String,
    pub limit_px: String,
    pub sz: String,
    pub oid: u64,
    pub timestamp: i64,
    #[serde(default)]
    pub is_trigger: bool,
    #[serde(default)]
    pub trigger_px: String,
    #[serde(default)]
    pub reduce_only: bool,
    #[serde(default)]
    pub order_type: String,
    #[serde(default)]
    pub is_position_tpsl: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UniverseAsset {
    pub name: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Meta {
    pub universe: Vec<UniverseAsset>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetCtx {
    pub funding: String,
    pub mark_px: String,
    #[serde(default)]
    pub mid_px: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserFill {
    pub coin: String,
    pub px: String,
    pub sz: String,
    pub side: String,
    pub time: i64,
    #[serde(default)]
    pub closed_pnl: String,
    #[serde(default)]
    pub fee: String,
    pub oid: u64,
    #[serde(default)]
    pub tid: u64,
}

/// Thin typed client over `POST {base}/info`.
#[derive(Clone)]
pub struct InfoClient {
    http: reqwest::Client,
    url: String,
    cache: Arc<Mutex<SlowCache>>,
}

impl InfoClient {
    pub fn new(base_url: &str) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .user_agent(concat!("perp-vyapari/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self {
            http,
            url: format!("{}/info", base_url.trim_end_matches('/')),
            cache: Arc::new(Mutex::new(SlowCache::default())),
        })
    }

    async fn post<T: DeserializeOwned>(&self, body: Value) -> Result<T> {
        let kind = body["type"].as_str().unwrap_or("?").to_string();
        let resp = self
            .http
            .post(&self.url)
            .json(&body)
            .send()
            .await
            .with_context(|| format!("hyperliquid info {kind}: request failed"))?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            anyhow::bail!(
                "hyperliquid info {kind}: HTTP {status}: {}",
                truncate(&text, 200)
            );
        }
        serde_json::from_str(&text).with_context(|| {
            format!(
                "hyperliquid info {kind}: unexpected response: {}",
                truncate(&text, 200)
            )
        })
    }

    pub async fn clearinghouse_state(&self, user: &str, dex: &str) -> Result<ClearinghouseState> {
        self.post(with_dex(
            json!({"type": "clearinghouseState", "user": user}),
            dex,
        ))
        .await
    }

    pub async fn spot_state(&self, user: &str) -> Result<SpotState> {
        self.post(json!({"type": "spotClearinghouseState", "user": user}))
            .await
    }

    pub async fn open_orders(&self, user: &str, dex: &str) -> Result<Vec<FrontendOrder>> {
        self.post(with_dex(
            json!({"type": "frontendOpenOrders", "user": user}),
            dex,
        ))
        .await
    }

    pub async fn meta_and_ctxs(&self, dex: &str) -> Result<(Meta, Vec<AssetCtx>)> {
        self.post(with_dex(json!({"type": "metaAndAssetCtxs"}), dex))
            .await
    }

    /// All perp dexes: `""` for the main dex, then HIP-3 dex names (e.g. `"xyz"`). Cached.
    pub async fn perp_dexes(&self) -> Result<Vec<String>> {
        if let Some((at, d)) = &self.cache.lock().unwrap().dexes {
            if at.elapsed() < SLOW_CACHE {
                return Ok(d.clone());
            }
        }
        let v: Vec<Value> = self.post(json!({"type": "perpDexs"})).await?;
        let mut dexes = vec![String::new()];
        for d in &v {
            if let Some(name) = d.get("name").and_then(Value::as_str) {
                if !name.is_empty() && !dexes.iter().any(|x| x == name) {
                    dexes.push(name.to_string());
                }
            }
        }
        self.cache.lock().unwrap().dexes = Some((Instant::now(), dexes.clone()));
        Ok(dexes)
    }

    /// Account abstraction mode, e.g. "unifiedAccount", "default", "disabled". Cached.
    pub async fn user_abstraction(&self, user: &str) -> Result<String> {
        if let Some((at, a)) = &self.cache.lock().unwrap().abstraction {
            if at.elapsed() < SLOW_CACHE {
                return Ok(a.clone());
            }
        }
        let v: Value = self
            .post(json!({"type": "userAbstraction", "user": user}))
            .await?;
        let a = v.as_str().unwrap_or("unknown").to_string();
        self.cache.lock().unwrap().abstraction = Some((Instant::now(), a.clone()));
        Ok(a)
    }

    pub async fn fills_since(&self, user: &str, start_ms: i64) -> Result<Vec<UserFill>> {
        self.post(json!({"type": "userFillsByTime", "user": user, "startTime": start_ms}))
            .await
    }
}

fn truncate(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dex_routing() {
        assert_eq!(dex_of("xyz:NVDA"), "xyz");
        assert_eq!(dex_of("BTC"), "");
        assert_eq!(with_dex(json!({"type": "x"}), ""), json!({"type": "x"}));
        assert_eq!(
            with_dex(json!({"type": "x"}), "xyz"),
            json!({"type": "x", "dex": "xyz"})
        );
    }
}

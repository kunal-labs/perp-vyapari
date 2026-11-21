//! gRPC implementation of `VenueService` for Hyperliquid.
//!
//! Read-only views and price alerts are live. Protection/close RPCs return
//! `UNIMPLEMENTED` until managed mode lands.

use std::{
    pin::Pin,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

use tokio_stream::{
    wrappers::{errors::BroadcastStreamRecvError, BroadcastStream, IntervalStream},
    Stream, StreamExt,
};
use tonic::{Request, Response, Status};
use venue_proto::v1::{
    event, venue_service_server::VenueService, Account, AddPriceAlertRequest, CancelRuleRequest,
    CancelRuleResponse, ClosePositionRequest, ClosePositionResponse, Event, GetAccountRequest,
    HealthRequest, HealthResponse, Heartbeat, ListOrdersRequest, ListOrdersResponse,
    ListPositionsRequest, ListPositionsResponse, ListPriceAlertsRequest, ListPriceAlertsResponse,
    ListRulesRequest, ListRulesResponse, PriceAlert, PriceDirection, ProtectionRule,
    RemovePriceAlertRequest, RemovePriceAlertResponse, SetProtectionRequest, Severity,
    StreamEventsRequest,
};

use crate::{config::Config, hl::InfoClient, monitor::Shared, now_ms, snapshot::Snapshot};

pub const VENUE: &str = "hyperliquid";
const MAX_PRICE_ALERTS: usize = 100;

pub struct HyperliquidVenue {
    cfg: Config,
    client: InfoClient,
    shared: Arc<Shared>,
    next_alert_id: AtomicU64,
}

impl HyperliquidVenue {
    pub fn new(cfg: Config, client: InfoClient, shared: Arc<Shared>) -> Self {
        Self {
            cfg,
            client,
            shared,
            next_alert_id: AtomicU64::new(1),
        }
    }

    async fn snapshot(&self) -> Result<Snapshot, Status> {
        Snapshot::fetch(&self.client, &self.cfg.address)
            .await
            .map_err(|e| Status::unavailable(format!("{e:#}")))
    }
}

fn not_yet(what: &str) -> Status {
    Status::unimplemented(format!(
        "{what} is not implemented yet (needs managed mode)"
    ))
}

type EventStream = Pin<Box<dyn Stream<Item = Result<Event, Status>> + Send>>;

#[tonic::async_trait]
impl VenueService for HyperliquidVenue {
    async fn health(&self, _: Request<HealthRequest>) -> Result<Response<HealthResponse>, Status> {
        Ok(Response::new(HealthResponse {
            venue: VENUE.into(),
            version: env!("CARGO_PKG_VERSION").into(),
            mode: self.cfg.mode().into(),
            account: self.cfg.address.clone(),
            testnet: self.cfg.testnet,
            server_time_ms: now_ms(),
        }))
    }

    async fn get_account(
        &self,
        _: Request<GetAccountRequest>,
    ) -> Result<Response<Account>, Status> {
        Ok(Response::new(self.snapshot().await?.account()))
    }

    async fn list_positions(
        &self,
        _: Request<ListPositionsRequest>,
    ) -> Result<Response<ListPositionsResponse>, Status> {
        let s = self.snapshot().await?;
        Ok(Response::new(ListPositionsResponse {
            positions: s.positions(),
            as_of_ms: s.state.time,
        }))
    }

    async fn list_orders(
        &self,
        req: Request<ListOrdersRequest>,
    ) -> Result<Response<ListOrdersResponse>, Status> {
        let s = self.snapshot().await?;
        Ok(Response::new(ListOrdersResponse {
            orders: s.orders(&req.into_inner().symbol),
            as_of_ms: s.state.time,
        }))
    }

    async fn set_protection(
        &self,
        _: Request<SetProtectionRequest>,
    ) -> Result<Response<ProtectionRule>, Status> {
        Err(not_yet("SetProtection"))
    }

    async fn list_rules(
        &self,
        _: Request<ListRulesRequest>,
    ) -> Result<Response<ListRulesResponse>, Status> {
        // No active rules exist until the manager lands; an empty list is truthful.
        Ok(Response::new(ListRulesResponse { rules: vec![] }))
    }

    async fn cancel_rule(
        &self,
        _: Request<CancelRuleRequest>,
    ) -> Result<Response<CancelRuleResponse>, Status> {
        Err(not_yet("CancelRule"))
    }

    async fn close_position(
        &self,
        _: Request<ClosePositionRequest>,
    ) -> Result<Response<ClosePositionResponse>, Status> {
        Err(not_yet("ClosePosition"))
    }

    async fn add_price_alert(
        &self,
        req: Request<AddPriceAlertRequest>,
    ) -> Result<Response<PriceAlert>, Status> {
        let r = req.into_inner();
        if r.direction() == PriceDirection::Unspecified {
            return Err(Status::invalid_argument("direction must be ABOVE or BELOW"));
        }
        match r.price.parse::<f64>() {
            Ok(p) if p > 0.0 && p.is_finite() => {}
            _ => return Err(Status::invalid_argument("price must be a positive number")),
        }
        let (meta, _) = self
            .client
            .meta_and_ctxs()
            .await
            .map_err(|e| Status::unavailable(format!("{e:#}")))?;
        let symbol = meta
            .universe
            .iter()
            .find(|a| a.name.eq_ignore_ascii_case(&r.symbol))
            .map(|a| a.name.clone())
            .ok_or_else(|| Status::not_found(format!("unknown Hyperliquid perp {:?}", r.symbol)))?;

        let mut alerts = self.shared.price_alerts.lock().await;
        if alerts.len() >= MAX_PRICE_ALERTS {
            return Err(Status::resource_exhausted("too many price alerts"));
        }
        let alert = PriceAlert {
            alert_id: format!("pa{}", self.next_alert_id.fetch_add(1, Ordering::Relaxed)),
            symbol,
            direction: r.direction,
            price: r.price,
            created_ms: now_ms(),
        };
        alerts.push(alert.clone());
        Ok(Response::new(alert))
    }

    async fn list_price_alerts(
        &self,
        _: Request<ListPriceAlertsRequest>,
    ) -> Result<Response<ListPriceAlertsResponse>, Status> {
        Ok(Response::new(ListPriceAlertsResponse {
            alerts: self.shared.price_alerts.lock().await.clone(),
        }))
    }

    async fn remove_price_alert(
        &self,
        req: Request<RemovePriceAlertRequest>,
    ) -> Result<Response<RemovePriceAlertResponse>, Status> {
        let id = req.into_inner().alert_id;
        let mut alerts = self.shared.price_alerts.lock().await;
        let before = alerts.len();
        alerts.retain(|a| a.alert_id != id);
        Ok(Response::new(RemovePriceAlertResponse {
            removed: alerts.len() != before,
        }))
    }

    type StreamEventsStream = EventStream;

    async fn stream_events(
        &self,
        _: Request<StreamEventsRequest>,
    ) -> Result<Response<Self::StreamEventsStream>, Status> {
        let shared = self.shared.clone();
        let beats = IntervalStream::new(tokio::time::interval(self.cfg.heartbeat)).map(move |_| {
            Ok(Event {
                time_ms: now_ms(),
                severity: Severity::Info.into(),
                kind: Some(event::Kind::Heartbeat(Heartbeat {
                    venue_stream_connected: shared.connected.load(Ordering::Relaxed),
                })),
            })
        });
        let pushes = BroadcastStream::new(self.shared.events.subscribe()).filter_map(|r| match r {
            Ok(ev) => Some(Ok(ev)),
            // A slow subscriber skipped events; keep streaming rather than dropping it.
            Err(BroadcastStreamRecvError::Lagged(_)) => None,
        });
        // Replay what is currently wrong first, so a (re)connecting gateway never misses
        // an ongoing condition that was raised before it subscribed.
        let replay: Vec<Result<Event, Status>> = self
            .shared
            .active_alerts
            .lock()
            .await
            .iter()
            .map(|r| {
                Ok(Event {
                    time_ms: now_ms(),
                    severity: r.severity.into(),
                    kind: Some(event::Kind::Alert(r.alert.clone())),
                })
            })
            .collect();
        Ok(Response::new(Box::pin(
            tokio_stream::iter(replay).chain(beats.merge(pushes)),
        )))
    }
}

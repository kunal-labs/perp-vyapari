//! gRPC implementation of `VenueService` for Hyperliquid.
//!
//! M0: `Health` and the heartbeat event stream are real; the remaining RPCs
//! return `UNIMPLEMENTED` until M1 (read-only) and M2/M3 (managed).

use std::{pin::Pin, time::SystemTime};

use tokio_stream::{wrappers::IntervalStream, Stream, StreamExt};
use tonic::{Request, Response, Status};
use venue_proto::v1::{
    event, venue_service_server::VenueService, Account, CancelRuleRequest, CancelRuleResponse,
    ClosePositionRequest, ClosePositionResponse, Event, GetAccountRequest, HealthRequest,
    HealthResponse, Heartbeat, ListOrdersRequest, ListOrdersResponse, ListPositionsRequest,
    ListPositionsResponse, ListRulesRequest, ListRulesResponse, ProtectionRule,
    SetProtectionRequest, Severity, StreamEventsRequest,
};

use crate::config::Config;

pub const VENUE: &str = "hyperliquid";

pub struct HyperliquidVenue {
    cfg: Config,
}

impl HyperliquidVenue {
    pub fn new(cfg: Config) -> Self {
        Self { cfg }
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or_default()
}

fn not_yet(milestone: &str) -> Status {
    Status::unimplemented(format!("not implemented yet (planned for {milestone})"))
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
        Err(not_yet("M1"))
    }

    async fn list_positions(
        &self,
        _: Request<ListPositionsRequest>,
    ) -> Result<Response<ListPositionsResponse>, Status> {
        Err(not_yet("M1"))
    }

    async fn list_orders(
        &self,
        _: Request<ListOrdersRequest>,
    ) -> Result<Response<ListOrdersResponse>, Status> {
        Err(not_yet("M1"))
    }

    async fn set_protection(
        &self,
        _: Request<SetProtectionRequest>,
    ) -> Result<Response<ProtectionRule>, Status> {
        Err(not_yet("M2"))
    }

    async fn list_rules(
        &self,
        _: Request<ListRulesRequest>,
    ) -> Result<Response<ListRulesResponse>, Status> {
        Err(not_yet("M3"))
    }

    async fn cancel_rule(
        &self,
        _: Request<CancelRuleRequest>,
    ) -> Result<Response<CancelRuleResponse>, Status> {
        Err(not_yet("M3"))
    }

    async fn close_position(
        &self,
        _: Request<ClosePositionRequest>,
    ) -> Result<Response<ClosePositionResponse>, Status> {
        Err(not_yet("M2"))
    }

    type StreamEventsStream = EventStream;

    async fn stream_events(
        &self,
        _: Request<StreamEventsRequest>,
    ) -> Result<Response<Self::StreamEventsStream>, Status> {
        let ticks = IntervalStream::new(tokio::time::interval(self.cfg.heartbeat));
        let stream = ticks.map(|_| {
            Ok(Event {
                time_ms: now_ms(),
                severity: Severity::Info.into(),
                // M1 wires this to the real Hyperliquid WebSocket state.
                kind: Some(event::Kind::Heartbeat(Heartbeat {
                    venue_stream_connected: false,
                })),
            })
        });
        Ok(Response::new(Box::pin(stream)))
    }
}

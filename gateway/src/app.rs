//! Routing commands to venue services and rendering replies. Transport-agnostic:
//! the Telegram layer calls `App::handle` and sends whatever text comes back.

use std::collections::BTreeMap;

use tonic::transport::{Channel, Endpoint};
use venue_proto::v1::{
    venue_service_client::VenueServiceClient, AddPriceAlertRequest, GetAccountRequest,
    HealthRequest, ListOrdersRequest, ListPositionsRequest, ListPriceAlertsRequest, Mode,
    PriceDirection, RemovePriceAlertRequest,
};

use crate::{
    commands::{self, Command, Dir},
    format, VenueEndpoint,
};

#[derive(Clone)]
pub struct App {
    venues: BTreeMap<String, VenueServiceClient<Channel>>,
}

impl App {
    /// Channels connect lazily, so the gateway starts even if a venue is down.
    pub fn new(endpoints: &[VenueEndpoint]) -> anyhow::Result<Self> {
        let mut venues = BTreeMap::new();
        for e in endpoints {
            let ch = Endpoint::from_shared(e.url.clone())?
                .connect_timeout(std::time::Duration::from_secs(5))
                .timeout(std::time::Duration::from_secs(15))
                .connect_lazy();
            venues.insert(e.name.clone(), VenueServiceClient::new(ch));
        }
        Ok(Self { venues })
    }

    pub fn venue_names(&self) -> Vec<String> {
        self.venues.keys().cloned().collect()
    }

    pub fn client(&self, name: &str) -> Option<VenueServiceClient<Channel>> {
        self.venues.get(name).cloned()
    }

    fn targets(&self, venue: &Option<String>) -> Vec<(String, VenueServiceClient<Channel>)> {
        match venue {
            Some(v) => self
                .venues
                .get(v)
                .map(|c| vec![(v.clone(), c.clone())])
                .unwrap_or_default(),
            None => self
                .venues
                .iter()
                .map(|(k, c)| (k.clone(), c.clone()))
                .collect(),
        }
    }

    /// For commands that act on a single venue: explicit, or the only one configured.
    fn single(
        &self,
        venue: &Option<String>,
    ) -> Result<(String, VenueServiceClient<Channel>), String> {
        match venue {
            Some(v) => self
                .venues
                .get(v)
                .map(|c| (v.clone(), c.clone()))
                .ok_or_else(|| format!("unknown venue {v}")),
            None if self.venues.len() == 1 => {
                let (k, c) = self.venues.iter().next().expect("one venue");
                Ok((k.clone(), c.clone()))
            }
            None => Err(format!(
                "which venue? add one of: {}",
                self.venue_names().join(", ")
            )),
        }
    }

    /// Handle one message text. `None` means "not a command, stay silent".
    pub async fn handle(&self, text: &str) -> Option<String> {
        let names = self.venue_names();
        let cmd = match commands::parse(text, &names) {
            Ok(Some(c)) => c,
            Ok(None) => return None,
            Err(usage) => return Some(format::esc(&usage)),
        };
        Some(self.run(cmd).await)
    }

    async fn run(&self, cmd: Command) -> String {
        match cmd {
            Command::Help => commands::help(&self.venue_names()),
            Command::Status => {
                self.each(&None, |n, mut c| async move {
                    let h = c.health(HealthRequest {}).await?.into_inner();
                    let mode = match h.mode() {
                        Mode::Managed => "managed",
                        Mode::ReadOnly => "read-only",
                        Mode::Unspecified => "unknown",
                    };
                    Ok(format!(
                        "✅ <b>{}</b> {} v{} · {mode}{} · <code>{}</code>",
                        format::esc(&n),
                        format::esc(&h.venue),
                        format::esc(&h.version),
                        if h.testnet { " · TESTNET" } else { "" },
                        format::esc(&h.account)
                    ))
                })
                .await
            }
            Command::Acct { venue } => {
                self.each(&venue, |n, mut c| async move {
                    Ok(format::account(
                        &n,
                        &c.get_account(GetAccountRequest {}).await?.into_inner(),
                    ))
                })
                .await
            }
            Command::Pos { venue } => {
                self.each(&venue, |n, mut c| async move {
                    Ok(format::positions(
                        &n,
                        &c.list_positions(ListPositionsRequest {})
                            .await?
                            .into_inner()
                            .positions,
                    ))
                })
                .await
            }
            Command::Orders { venue, symbol } => {
                let symbol = symbol.unwrap_or_default();
                self.each(&venue, move |n, mut c| {
                    let symbol = symbol.clone();
                    async move {
                        let r = c
                            .list_orders(ListOrdersRequest { symbol })
                            .await?
                            .into_inner();
                        Ok(format::orders(&n, &r.orders))
                    }
                })
                .await
            }
            Command::Pnl { venue } => {
                self.each(&venue, |n, mut c| async move {
                    let a = c.get_account(GetAccountRequest {}).await?.into_inner();
                    let p = c
                        .list_positions(ListPositionsRequest {})
                        .await?
                        .into_inner()
                        .positions;
                    Ok(format::pnl(&n, &a, &p))
                })
                .await
            }
            Command::Alerts { venue } => {
                self.each(&venue, |n, mut c| async move {
                    Ok(format::price_alerts(
                        &n,
                        &c.list_price_alerts(ListPriceAlertsRequest {})
                            .await?
                            .into_inner()
                            .alerts,
                    ))
                })
                .await
            }
            Command::Alert {
                venue,
                symbol,
                dir,
                price,
            } => {
                let (n, mut c) = match self.single(&venue) {
                    Ok(x) => x,
                    Err(e) => return format::esc(&e),
                };
                let direction = match dir {
                    Dir::Above => PriceDirection::Above,
                    Dir::Below => PriceDirection::Below,
                };
                match c
                    .add_price_alert(AddPriceAlertRequest {
                        symbol,
                        direction: direction.into(),
                        price,
                    })
                    .await
                {
                    Ok(a) => {
                        let a = a.into_inner();
                        let d = if a.direction() == PriceDirection::Above {
                            "≥"
                        } else {
                            "≤"
                        };
                        format!(
                            "🔔 <b>{}</b> alert <code>{}</code>: {} {d} {}",
                            format::esc(&n),
                            format::esc(&a.alert_id),
                            format::esc(&a.symbol),
                            format::money(&a.price, 2)
                        )
                    }
                    Err(s) => venue_error(&n, &s),
                }
            }
            Command::Unalert { venue, id } => {
                let (n, mut c) = match self.single(&venue) {
                    Ok(x) => x,
                    Err(e) => return format::esc(&e),
                };
                match c
                    .remove_price_alert(RemovePriceAlertRequest {
                        alert_id: id.clone(),
                    })
                    .await
                {
                    Ok(r) if r.get_ref().removed => {
                        format!("🔕 removed <code>{}</code>", format::esc(&id))
                    }
                    Ok(_) => format!(
                        "no alert <code>{}</code> on {}",
                        format::esc(&id),
                        format::esc(&n)
                    ),
                    Err(s) => venue_error(&n, &s),
                }
            }
        }
    }

    /// Run `f` against each target venue (in name order); join replies, isolating failures.
    async fn each<F, Fut>(&self, venue: &Option<String>, f: F) -> String
    where
        F: Fn(String, VenueServiceClient<Channel>) -> Fut,
        Fut: std::future::Future<Output = Result<String, tonic::Status>>,
    {
        let targets = self.targets(venue);
        if targets.is_empty() {
            return format!(
                "unknown venue; configured: {}",
                self.venue_names().join(", ")
            );
        }
        let futs = targets.into_iter().map(|(n, c)| {
            let fut = f(n.clone(), c);
            async move { fut.await.unwrap_or_else(|s| venue_error(&n, &s)) }
        });
        futures_join_all(futs).await.join("\n\n")
    }
}

fn venue_error(venue: &str, s: &tonic::Status) -> String {
    format!(
        "❌ <b>{}</b>: {}",
        format::esc(venue),
        format::esc(s.message())
    )
}

async fn futures_join_all<I, F>(futs: I) -> Vec<String>
where
    I: IntoIterator<Item = F>,
    F: std::future::Future<Output = String>,
{
    let mut set = Vec::new();
    for f in futs {
        set.push(f);
    }
    let mut out = Vec::with_capacity(set.len());
    // Venues are few; awaiting in order keeps reply order stable.
    for f in set {
        out.push(f.await);
    }
    out
}

//! Gateway library: Telegram <-> venue services.
//!
//! | Variable              | Required | Meaning                                                |
//! |-----------------------|----------|--------------------------------------------------------|
//! | `VENUES`              | yes      | Comma list `name=url`, e.g. `hl=http://venue-hl:50051`  |
//! | `TELEGRAM_TOKEN_FILE` | yes      | File containing the bot token                          |
//! | `TELEGRAM_OWNER_ID`   | yes      | Numeric Telegram user id allowed to use the bot        |

pub mod app;
pub mod commands;
pub mod format;
pub mod telegram;

use anyhow::{bail, Context, Result};
use venue_proto::v1::{venue_service_client::VenueServiceClient, HealthRequest, HealthResponse};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VenueEndpoint {
    pub name: String,
    pub url: String,
}

/// Parse `name=url[,name=url...]`. Names are short routing keys used in commands (`/pos hl`).
pub fn parse_venues(spec: &str) -> Result<Vec<VenueEndpoint>> {
    let mut out: Vec<VenueEndpoint> = Vec::new();
    for part in spec.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let (name, url) = part
            .split_once('=')
            .with_context(|| format!("venue entry {part:?} must be name=url"))?;
        let (name, url) = (name.trim(), url.trim());
        if name.is_empty()
            || !name
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        {
            bail!("venue name {name:?} must be lowercase alphanumeric");
        }
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            bail!("venue url {url:?} must start with http:// or https://");
        }
        if out.iter().any(|v| v.name == name) {
            bail!("duplicate venue name {name:?}");
        }
        out.push(VenueEndpoint {
            name: name.into(),
            url: url.into(),
        });
    }
    if out.is_empty() {
        bail!("VENUES must list at least one venue");
    }
    Ok(out)
}

/// One health probe against a venue service.
pub async fn probe(endpoint: &VenueEndpoint) -> Result<HealthResponse> {
    let mut client = VenueServiceClient::connect(endpoint.url.clone())
        .await
        .with_context(|| format!("connect {}", endpoint.url))?;
    Ok(client.health(HealthRequest {}).await?.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_list() {
        let v =
            parse_venues(" hl=http://venue-hl:50051 , lighter=http://venue-lighter:50052").unwrap();
        assert_eq!(v.len(), 2);
        assert_eq!(
            v[0],
            VenueEndpoint {
                name: "hl".into(),
                url: "http://venue-hl:50051".into()
            }
        );
        assert_eq!(v[1].name, "lighter");
    }

    #[test]
    fn rejects_bad_entries() {
        assert!(parse_venues("").is_err());
        assert!(parse_venues("hl").is_err());
        assert!(parse_venues("HL=http://x").is_err());
        assert!(parse_venues("hl=venue-hl:50051").is_err());
        assert!(parse_venues("hl=http://a,hl=http://b").is_err());
    }
}

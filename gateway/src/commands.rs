//! Telegram command parsing. Pure and venue-agnostic.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Dir {
    Above,
    Below,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Help,
    Status,
    Acct {
        venue: Option<String>,
    },
    Pos {
        venue: Option<String>,
    },
    Orders {
        venue: Option<String>,
        symbol: Option<String>,
    },
    Pnl {
        venue: Option<String>,
    },
    Alerts {
        venue: Option<String>,
    },
    Alert {
        venue: Option<String>,
        symbol: String,
        dir: Dir,
        price: String,
    },
    Unalert {
        venue: Option<String>,
        id: String,
    },
}

/// Parse a message. `venues` are the configured routing names (e.g. `["hl"]`).
/// Returns `Err(usage)` for recognised commands with bad arguments, `Ok(None)` for non-commands.
pub fn parse(text: &str, venues: &[String]) -> Result<Option<Command>, String> {
    let text = text.trim();
    if !text.starts_with('/') {
        return Ok(None);
    }
    let mut words = text.split_whitespace();
    // Strip "@BotName" suffix used in group chats.
    let head = words.next().unwrap_or_default();
    let cmd = head
        .split('@')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let mut args: Vec<&str> = words.collect();

    // Optional leading venue argument.
    let venue = match args.first() {
        Some(a) if venues.iter().any(|v| v.eq_ignore_ascii_case(a)) => {
            let v = a.to_ascii_lowercase();
            args.remove(0);
            Some(v)
        }
        _ => None,
    };

    let c = match cmd.as_str() {
        "/start" | "/help" => Command::Help,
        "/status" => Command::Status,
        "/acct" | "/account" => Command::Acct { venue },
        "/pos" | "/positions" => Command::Pos { venue },
        "/orders" => Command::Orders {
            venue,
            symbol: args.first().map(|s| s.to_ascii_uppercase()),
        },
        "/pnl" => Command::Pnl { venue },
        "/alerts" => Command::Alerts { venue },
        "/alert" => {
            const USAGE: &str = "usage: /alert [venue] <symbol> above|below <price>";
            let [symbol, dir, price] = args.as_slice() else {
                return Err(USAGE.into());
            };
            let dir = match dir.to_ascii_lowercase().as_str() {
                "above" | ">" | ">=" => Dir::Above,
                "below" | "<" | "<=" => Dir::Below,
                _ => return Err(USAGE.into()),
            };
            let price = price.replace(',', "");
            match price.parse::<f64>() {
                Ok(p) if p > 0.0 && p.is_finite() => {}
                _ => return Err(USAGE.into()),
            }
            Command::Alert {
                venue,
                symbol: symbol.to_ascii_uppercase(),
                dir,
                price,
            }
        }
        "/unalert" => {
            let [id] = args.as_slice() else {
                return Err("usage: /unalert [venue] <alert-id>".into());
            };
            Command::Unalert {
                venue,
                id: id.to_string(),
            }
        }
        _ => return Err(format!("unknown command {head}; try /help")),
    };
    Ok(Some(c))
}

pub fn help(venues: &[String]) -> String {
    format!(
        "<b>perp-vyapari</b> (read-only)\n\
         Venues: {}\n\n\
         /acct [venue] – balances and margin\n\
         /pos [venue] – positions, stops, liquidation\n\
         /orders [venue] [symbol] – open orders\n\
         /pnl [venue] – unrealised PnL\n\
         /alerts [venue] – price alerts\n\
         /alert [venue] &lt;symbol&gt; above|below &lt;price&gt;\n\
         /unalert [venue] &lt;id&gt;\n\
         /status – venue health",
        venues.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v() -> Vec<String> {
        vec!["hl".into(), "lighter".into()]
    }

    #[test]
    fn plain_text_is_ignored() {
        assert_eq!(parse("hello", &v()), Ok(None));
    }

    #[test]
    fn venue_argument_is_optional() {
        assert_eq!(parse("/pos", &v()), Ok(Some(Command::Pos { venue: None })));
        assert_eq!(
            parse("/pos HL", &v()),
            Ok(Some(Command::Pos {
                venue: Some("hl".into())
            }))
        );
        assert_eq!(
            parse("/pos@VyapariAgent hl", &v()),
            Ok(Some(Command::Pos {
                venue: Some("hl".into())
            }))
        );
    }

    #[test]
    fn orders_symbol() {
        assert_eq!(
            parse("/orders hl btc", &v()),
            Ok(Some(Command::Orders {
                venue: Some("hl".into()),
                symbol: Some("BTC".into())
            }))
        );
        assert_eq!(
            parse("/orders eth", &v()),
            Ok(Some(Command::Orders {
                venue: None,
                symbol: Some("ETH".into())
            }))
        );
    }

    #[test]
    fn alert_parsing() {
        assert_eq!(
            parse("/alert hl btc above 70,000", &v()),
            Ok(Some(Command::Alert {
                venue: Some("hl".into()),
                symbol: "BTC".into(),
                dir: Dir::Above,
                price: "70000".into()
            }))
        );
        assert!(matches!(
            parse("/alert eth < 2500.5", &v()),
            Ok(Some(Command::Alert {
                dir: Dir::Below,
                ..
            }))
        ));
        assert!(parse("/alert btc sideways 1", &v()).is_err());
        assert!(parse("/alert btc above -1", &v()).is_err());
        assert!(parse("/alert btc above", &v()).is_err());
    }

    #[test]
    fn unknown_command_is_an_error() {
        assert!(parse("/buy btc", &v()).is_err());
    }
}

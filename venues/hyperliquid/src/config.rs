//! Runtime configuration, read from environment variables.
//!
//! | Variable              | Required | Meaning                                              |
//! |-----------------------|----------|------------------------------------------------------|
//! | `HL_ADDRESS`          | yes      | Hyperliquid account (master) address, `0x` + 40 hex   |
//! | `HL_AGENT_KEY_FILE`   | no       | File holding a trade-only API (agent) wallet key      |
//! | `HL_TESTNET`          | no       | `true` to use Hyperliquid testnet (default `false`)  |
//! | `VENUE_LISTEN_ADDR`   | no       | gRPC listen address (default `0.0.0.0:50051`)         |
//! | `HEARTBEAT_SECS`      | no       | Event-stream heartbeat interval (default `15`)        |
//! | `HL_API_URL`          | no       | Override API base URL (default: mainnet/testnet)      |
//! | `POLL_SECS`           | no       | Account poll interval for alerts/fills (default `10`) |
//! | `LIQ_ALERT_PCT`       | no       | Alert when mark is within this % of liq (default `5`) |
//! | `FUNDING_ALERT_APR_PCT` | no     | Alert when paying funding above this APR % (default `100`) |
//! | `ALERT_REPEAT_MINS`   | no       | Repeat interval for ongoing alerts (default `30`)     |

use std::{fmt, net::SocketAddr, path::PathBuf, time::Duration};

use anyhow::{bail, Context, Result};
use venue_proto::v1::Mode;

use crate::{alerts::Thresholds, hl};

/// A secret that never prints its value.
#[derive(Clone)]
pub struct Secret(String);

impl Secret {
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub address: String,
    pub agent_key: Option<Secret>,
    pub testnet: bool,
    pub listen_addr: SocketAddr,
    pub heartbeat: Duration,
    pub api_url: String,
    pub poll: Duration,
    pub thresholds: Thresholds,
}

impl Config {
    pub fn mode(&self) -> Mode {
        if self.agent_key.is_some() {
            Mode::Managed
        } else {
            Mode::ReadOnly
        }
    }

    pub fn from_env() -> Result<Self> {
        Self::from_lookup(|k| std::env::var(k).ok())
    }

    /// Build from any key lookup (lets tests avoid touching process env).
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self> {
        let address = get("HL_ADDRESS").context("HL_ADDRESS is required")?;
        let address = validate_address(address.trim())?;

        let agent_key = match get("HL_AGENT_KEY_FILE").filter(|s| !s.is_empty()) {
            None => None,
            Some(path) => Some(read_key_file(&PathBuf::from(path))?),
        };

        let testnet = match get("HL_TESTNET").as_deref().map(str::trim) {
            None | Some("") | Some("false") | Some("0") => false,
            Some("true") | Some("1") => true,
            Some(other) => bail!("HL_TESTNET must be true/false, got {other:?}"),
        };

        let listen_addr = get("VENUE_LISTEN_ADDR")
            .unwrap_or_else(|| "0.0.0.0:50051".into())
            .parse()
            .context("VENUE_LISTEN_ADDR is not a socket address")?;

        let heartbeat_secs: u64 = get("HEARTBEAT_SECS")
            .unwrap_or_else(|| "15".into())
            .parse()
            .context("HEARTBEAT_SECS must be an integer")?;
        if heartbeat_secs == 0 {
            bail!("HEARTBEAT_SECS must be > 0");
        }

        let api_url = get("HL_API_URL")
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| {
                if testnet {
                    hl::TESTNET_URL
                } else {
                    hl::MAINNET_URL
                }
                .into()
            });

        let poll_secs: u64 = parse_or(&get, "POLL_SECS", 10)?;
        if poll_secs == 0 {
            bail!("POLL_SECS must be > 0");
        }
        let liq: f64 = parse_or(&get, "LIQ_ALERT_PCT", 5.0)?;
        let funding: f64 = parse_or(&get, "FUNDING_ALERT_APR_PCT", 100.0)?;
        let repeat_mins: i64 = parse_or(&get, "ALERT_REPEAT_MINS", 30)?;
        if !(liq > 0.0 && funding > 0.0 && repeat_mins > 0) {
            bail!("LIQ_ALERT_PCT, FUNDING_ALERT_APR_PCT and ALERT_REPEAT_MINS must be > 0");
        }

        Ok(Self {
            address,
            agent_key,
            testnet,
            listen_addr,
            heartbeat: Duration::from_secs(heartbeat_secs),
            api_url,
            poll: Duration::from_secs(poll_secs),
            thresholds: Thresholds {
                liq_distance_pct: liq,
                funding_apr_pct: funding,
                repeat_ms: repeat_mins * 60 * 1000,
            },
        })
    }
}

fn parse_or<T: std::str::FromStr>(
    get: &impl Fn(&str) -> Option<String>,
    key: &str,
    default: T,
) -> Result<T> {
    match get(key).filter(|s| !s.trim().is_empty()) {
        None => Ok(default),
        Some(v) => v
            .trim()
            .parse()
            .map_err(|_| anyhow::anyhow!("{key} has invalid value {v:?}")),
    }
}

fn validate_address(a: &str) -> Result<String> {
    let hex = a
        .strip_prefix("0x")
        .context("HL_ADDRESS must start with 0x")?;
    if hex.len() != 40 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!("HL_ADDRESS must be 0x followed by 40 hex characters");
    }
    Ok(format!("0x{}", hex.to_ascii_lowercase()))
}

fn read_key_file(path: &PathBuf) -> Result<Secret> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("cannot read HL_AGENT_KEY_FILE {}", path.display()))?;
    let key = raw.trim();
    let hex = key.strip_prefix("0x").unwrap_or(key);
    if hex.len() != 64 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        // Do not echo any part of the file.
        bail!("HL_AGENT_KEY_FILE does not contain a 32-byte hex private key");
    }
    Ok(Secret(format!("0x{hex}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::io::Write;

    const ADDR: &str = "0xAbCdEf0123456789abcdef0123456789ABCDEF01";

    fn cfg(pairs: &[(&str, &str)]) -> Result<Config> {
        let m: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        Config::from_lookup(|k| m.get(k).cloned())
    }

    #[test]
    fn read_only_defaults() {
        let c = cfg(&[("HL_ADDRESS", ADDR)]).unwrap();
        assert_eq!(c.address, ADDR.to_ascii_lowercase());
        assert_eq!(c.mode(), Mode::ReadOnly);
        assert!(!c.testnet);
        assert_eq!(c.listen_addr.port(), 50051);
        assert_eq!(c.heartbeat, Duration::from_secs(15));
        assert_eq!(c.api_url, hl::MAINNET_URL);
        assert_eq!(c.poll, Duration::from_secs(10));
        assert_eq!(c.thresholds.liq_distance_pct, 5.0);
    }

    #[test]
    fn testnet_url_and_threshold_overrides() {
        let c = cfg(&[
            ("HL_ADDRESS", ADDR),
            ("HL_TESTNET", "true"),
            ("LIQ_ALERT_PCT", "8.5"),
            ("ALERT_REPEAT_MINS", "5"),
        ])
        .unwrap();
        assert_eq!(c.api_url, hl::TESTNET_URL);
        assert_eq!(c.thresholds.liq_distance_pct, 8.5);
        assert_eq!(c.thresholds.repeat_ms, 300_000);
        assert!(cfg(&[("HL_ADDRESS", ADDR), ("LIQ_ALERT_PCT", "-1")]).is_err());
        assert!(cfg(&[("HL_ADDRESS", ADDR), ("POLL_SECS", "abc")]).is_err());
    }

    #[test]
    fn rejects_bad_address() {
        assert!(cfg(&[]).is_err());
        assert!(cfg(&[("HL_ADDRESS", "0x1234")]).is_err());
        assert!(cfg(&[("HL_ADDRESS", "AbCdEf0123456789abcdef0123456789ABCDEF01")]).is_err());
    }

    #[test]
    fn managed_with_key_file_and_redacted_debug() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        writeln!(f, "0x{}", "a1".repeat(32)).unwrap();
        let path = f.path().to_str().unwrap().to_string();
        let c = cfg(&[
            ("HL_ADDRESS", ADDR),
            ("HL_AGENT_KEY_FILE", &path),
            ("HL_TESTNET", "true"),
        ])
        .unwrap();
        assert_eq!(c.mode(), Mode::Managed);
        assert!(c.testnet);
        let dbg = format!("{c:?}");
        assert!(!dbg.contains("a1a1"), "key leaked into Debug output");
        assert!(dbg.contains("<redacted>"));
    }

    #[test]
    fn rejects_malformed_key_without_echo() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        writeln!(f, "not-a-key-SECRETVALUE").unwrap();
        let path = f.path().to_str().unwrap().to_string();
        let err = cfg(&[("HL_ADDRESS", ADDR), ("HL_AGENT_KEY_FILE", &path)]).unwrap_err();
        assert!(!format!("{err:#}").contains("SECRETVALUE"));
    }

    #[test]
    fn rejects_bad_testnet_and_heartbeat() {
        assert!(cfg(&[("HL_ADDRESS", ADDR), ("HL_TESTNET", "yes")]).is_err());
        assert!(cfg(&[("HL_ADDRESS", ADDR), ("HEARTBEAT_SECS", "0")]).is_err());
    }
}

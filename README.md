# perp-vyapari

Manage perpetual-futures positions from Telegram, with a dedicated service per venue that actively manages stop-loss and take-profit.

- **Hyperliquid**: `venues/hyperliquid` (Rust), in progress.
- **Lighter**: `venues/lighter` (Go), planned.

> **Status:** read-only. Telegram shows Hyperliquid balances, positions, orders and PnL, and pushes alerts and fills. Placing or moving stops is not built yet.

## How it fits together

```
Telegram  <->  gateway (Rust)  <-- gRPC: proto/venue.proto -->  venue-hl (Rust)  <->  Hyperliquid
                                                            \->  venue-lighter (Go, planned)
```

- **gateway**: the only component that talks to Telegram. It accepts commands only from the owner's Telegram account, asks for confirmation before any write, and routes each command to a venue. It never holds exchange keys.
- **venue services**: one per exchange, all implementing the same gRPC contract in `proto/venue.proto`. Each one owns its exchange connection, its local state and its SL/TP manager.
- **Safety model**: every managed position keeps a real stop order on the exchange. The service moves that stop and never keeps it only in memory, so a crashed service still leaves you protected.

## Telegram commands

The bot answers only its owner (by numeric Telegram user id) in a private chat, and ignores everyone else. `[venue]` is optional; leave it out to query all venues.

| Command | What it shows |
|---|---|
| `/acct [venue]` | Equity, unrealised PnL, margin, withdrawable; unified-account USDC collateral |
| `/pos [venue]` | Positions with entry, mark, leverage, exchange-side stop (or a NO STOP warning), liquidation distance |
| `/orders [venue] [symbol]` | Open orders including stops and take-profits |
| `/pnl [venue]` | Unrealised PnL per position |
| `/alert [venue] <symbol> above\|below <price>` | One-shot price alert |
| `/alerts [venue]`, `/unalert [venue] <id>` | List or remove price alerts |
| `/status` | Venue service health, mode and account |

**Pushed automatically:**
- Fills.
- Positions without an exchange-side stop.
- Liquidation getting close.
- Expensive funding against your position.
- Price alerts.
- Venue outages.

Ongoing conditions repeat every `ALERT_REPEAT_MINS` while they last. Price alerts live in memory and are lost on restart.

## Access modes

| Mode | What you provide | What you get |
|---|---|---|
| Read-only | Your Hyperliquid address | Positions, orders, PnL, alerts |
| Managed | Also a Hyperliquid **API (agent) wallet** key, which can trade but cannot withdraw | Everything above, plus SL/TP placement and active management |

Never give this project a key that can withdraw funds.

## Quick start

Requirements: Rust (stable, 1.93+). Podman or Docker for containers. You don't need to install protobuf; a bundled `protoc` is used.

```bash
make build
make test
cp deploy/.env.example deploy/.env    # set HL_ADDRESS, TELEGRAM_OWNER_ID, TELEGRAM_TOKEN_HOST_FILE
make run                              # read-only stack
MANAGED=1 make run                    # managed: also mounts the agent key file into venue-hl
```

## Make targets

| Target | What it does |
|---|---|
| `make build` | Build all Rust crates |
| `make test` | Tidiness check, `cargo fmt --check`, clippy with warnings as errors, all tests (bounded by `SUITE_TIMEOUT`, default 600 s) |
| `make run` | Start gateway and venue-hl with compose; `MANAGED=1` adds the agent-key overlay |
| `make image` | Build the `gateway` and `venue-hl` container images |
| `make deploy` | Build, push and deploy to a Cloud Run worker pool using `deploy/config.toml` |
| `make tidy-check` | Check that Makefile recipes are one-line script calls, scripts are well-formed, and this README lists every target |
| `make clean` | Remove build output |
| `make help` | List targets |

## Configuration

**venue-hl**

| Variable | Required | Default | Meaning |
|---|---|---|---|
| `HL_ADDRESS` | yes | — | Hyperliquid account address (`0x` + 40 hex) |
| `HL_AGENT_KEY_FILE` | no | — | File containing a trade-only agent key; when set, the service runs in managed mode |
| `HL_TESTNET` | no | `false` | `true` to use Hyperliquid testnet |
| `VENUE_LISTEN_ADDR` | no | `0.0.0.0:50051` | gRPC listen address |
| `HEARTBEAT_SECS` | no | `15` | Heartbeat interval on the event stream |
| `HL_API_URL` | no | mainnet or testnet | Override the Hyperliquid API base URL |
| `POLL_SECS` | no | `10` | How often the account is checked for alerts and fills |
| `LIQ_ALERT_PCT` | no | `5` | Alert when mark is within this % of liquidation |
| `FUNDING_ALERT_APR_PCT` | no | `100` | Alert when the position pays funding above this annualised % |
| `ALERT_REPEAT_MINS` | no | `30` | Repeat interval for ongoing alerts |

**gateway**

| Variable | Required | Default | Meaning |
|---|---|---|---|
| `VENUES` | yes | — | Comma list of `name=url`, e.g. `hl=http://venue-hl:50051` |
| `TELEGRAM_TOKEN_FILE` | yes | — | File containing the bot token |
| `TELEGRAM_OWNER_ID` | yes | — | Numeric Telegram user id allowed to use the bot |

**compose** (`deploy/.env`):
- Venue: `HL_ADDRESS`, `HL_TESTNET`, the alert thresholds, `HL_AGENT_KEY_HOST_FILE` (managed mode only).
- Gateway: `TELEGRAM_OWNER_ID`, `TELEGRAM_TOKEN_HOST_FILE`.
- Both: `RUST_LOG`.

The Telegram token is mounted into the gateway only. Exchange keys are mounted into the venue service only. Secret files can stay `chmod 600`: `make run` maps the container user to you (on Podman through `deploy/compose.podman.yaml`; on Docker by running as your user ID).

**Try commands without Telegram** against running venue services:

```bash
VENUES=hl=http://127.0.0.1:50051 cargo run -p gateway --example ask -- /pos
```

## Deploying to Cloud Run

Production runs as a **Cloud Run worker pool**. That suits always-on background programs: no HTTP port, no scale-to-zero, exactly one instance. Both containers run in the same instance and talk over localhost.

1. Edit `deploy/config.toml`: project, region, pool name, CPU and memory, account address, alert thresholds, owner Telegram ID, and the **names** of the Secret Manager secrets.
2. Create each secret once from a file that only you can read:
   ```bash
   gcloud secrets create perp-vyapari-telegram-token --project <project> --data-file=$HOME/.config/perp-vyapari/telegram_token
   ```
3. Stop any local copy (`make run`). Two bots polling the same Telegram token conflict.
4. `make deploy`

The script is safe to re-run. It creates the image repository and service account if they're missing, and grants that account access to exactly the secrets listed. Images are tagged with the git commit. The Telegram token is mounted into the gateway container only; the trading key, when added, goes into `venue-hl` only.

To rotate the bot token:
```bash
gcloud secrets versions add perp-vyapari-telegram-token --data-file=<file>
make deploy
```

## Layout

```
proto/venue.proto        gRPC contract shared by every venue
crates/venue-proto/      Rust bindings generated from the contract
gateway/                 Telegram gateway
venues/hyperliquid/      Hyperliquid venue service
deploy/                  Containerfiles, compose.yaml (+ managed and podman overlays), .env.example
scripts/                 Logic behind every make target (scripts/lib: deploy manifest renderer)
```

## Roadmap

1. Scaffold (done).
2. Hyperliquid read-only (done): Telegram `/pos`, `/orders`, `/pnl`, `/acct`; alerts for liquidation distance, price and funding.
3. Hyperliquid execution: set and modify exchange-side TP/SL with confirm buttons; `/close`. Tested on testnet first.
4. Active manager: trailing stop, breakeven move, take-profit ladder, saved state, reconcile on restart, watchdog.
5. Lighter venue in Go against the same contract.
6. Natural-language commands.

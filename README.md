# perp-vyapari

Manage perpetual-futures positions from Telegram, with a dedicated service per venue that actively manages stop-loss and take-profit.

- **Hyperliquid**: `venues/hyperliquid` (Rust), in progress.
- **Lighter**: `venues/lighter` (Go), planned.

> **Status:** scaffold. The venue service answers health checks and streams heartbeats; positions, Telegram commands and SL/TP management are not built yet.

## How it fits together

```
Telegram  <->  gateway (Rust)  <-- gRPC: proto/venue.proto -->  venue-hl (Rust)  <->  Hyperliquid
                                                            \->  venue-lighter (Go, planned)
```

- **gateway**: the only component that talks to Telegram. It accepts commands only from the owner's Telegram account, asks for confirmation before any write, and routes each command to a venue. It never holds exchange keys.
- **venue services**: one per exchange, all implementing the same gRPC contract in `proto/venue.proto`. Each one owns its exchange connection, its local state and its SL/TP manager.
- **Safety model**: every managed position keeps a real stop order on the exchange. The service moves that stop and never keeps it only in memory, so a crashed service still leaves you protected.

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
cp deploy/.env.example deploy/.env    # set HL_ADDRESS
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

**gateway**

| Variable | Required | Default | Meaning |
|---|---|---|---|
| `VENUES` | yes | — | Comma list of `name=url`, e.g. `hl=http://venue-hl:50051` |
| `HEALTH_SECS` | no | `30` | Health-check interval |

**compose** (`deploy/.env`): `HL_ADDRESS`, `HL_TESTNET`, `HL_AGENT_KEY_HOST_FILE` (managed only), `HEALTH_SECS`, `RUST_LOG`.

## Layout

```
proto/venue.proto        gRPC contract shared by every venue
crates/venue-proto/      Rust bindings generated from the contract
gateway/                 Telegram gateway
venues/hyperliquid/      Hyperliquid venue service
deploy/                  Containerfiles, compose.yaml, compose.managed.yaml, .env.example
scripts/                 Logic behind every make target
```

## Roadmap

1. Scaffold (done).
2. Hyperliquid read-only: Telegram `/pos`, `/orders`, `/pnl`, `/acct`; alerts for liquidation distance, price and funding.
3. Hyperliquid execution: set and modify exchange-side TP/SL with confirm buttons; `/close`. Tested on testnet first.
4. Active manager: trailing stop, breakeven move, take-profit ladder, saved state, reconcile on restart, watchdog.
5. Lighter venue in Go against the same contract.
6. Natural-language commands.

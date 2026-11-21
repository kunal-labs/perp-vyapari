# perp-vyapari — Agent Charter  (v1 · 2026-11-21)
> All agents: follow this on every task. If a request conflicts, stop and raise
> `⚠️ CHARTER CONFLICT: <ID>`. Changes only via @techo Charter Capture or /discuss → ADR.

## Mission
Manage the owner's Hyperliquid and Lighter perp positions from Telegram, with venue services that actively manage SL/TP safely.

## Tenets (ranked — earlier wins on conflict)
- T1 Capital safety beats features: an exchange-side stop always exists for managed positions.

## Norms
### Code & repo
- N1 Every execution change is tested on Hyperliquid/Lighter testnet before mainnet.
### Build & tooling
- N2 Extra authorized make targets: tidy-check, image.

## Decisions (binding until superseded)
- D1 Venue-specific services behind one language-neutral gRPC contract (proto/venue.proto): Rust for Hyperliquid, Go for Lighter → user directive, 2026-11-21.

## Guardrails
- G1 No withdrawal-capable keys, ever; venue keys stay inside venue services (gateway never sees them).
- G2 Every write from Telegram needs explicit confirmation; only the owner's numeric Telegram user ID is accepted.

## Pointers
ADRs: docs/adr/ · Roadmap: Milestones · Deliberation: Discussions

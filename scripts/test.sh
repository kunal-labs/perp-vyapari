#!/usr/bin/env bash
# Tidiness check, formatting, lints and the full test suite. Fails closed on hangs.
set -euo pipefail
cd "$(dirname "$0")/.."
SUITE_TIMEOUT="${SUITE_TIMEOUT:-600}"

./scripts/check-repo-tidiness.sh
cargo fmt --all -- --check
timeout -k 5 "${SUITE_TIMEOUT}s" cargo clippy --workspace --all-targets --locked -- -D warnings
timeout -k 5 "${SUITE_TIMEOUT}s" cargo test --workspace --locked

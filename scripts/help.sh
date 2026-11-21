#!/usr/bin/env bash
set -euo pipefail
cat <<'EOF'
make build       Build all Rust crates
make test        Tidiness check, fmt, clippy (-D warnings) and all tests (bounded by SUITE_TIMEOUT, default 600s)
make run         Start gateway + venue-hl with compose (needs deploy/.env)
make image       Build container images (gateway, venue-hl)
make tidy-check  Verify Makefile/scripts/README invariants
make clean       Remove build output
make help        Show this help
EOF

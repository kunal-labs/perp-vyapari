#!/usr/bin/env bash
# Run the stack with compose (podman or docker). Reads deploy/.env.
# MANAGED=1 adds the managed-mode overlay (mounts the agent key into venue-hl).
set -euo pipefail
cd "$(dirname "$0")/.."
if [[ ! -f deploy/.env ]]; then
  echo "deploy/.env missing: copy deploy/.env.example and fill it in" >&2
  exit 1
fi
files=(-f deploy/compose.yaml)
if [[ "${MANAGED:-0}" == "1" ]]; then
  files+=(-f deploy/compose.managed.yaml)
fi
if command -v podman >/dev/null 2>&1; then
  exec podman compose "${files[@]}" --env-file deploy/.env up --build "$@"
fi
exec docker compose "${files[@]}" --env-file deploy/.env up --build "$@"

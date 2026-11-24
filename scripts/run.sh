#!/usr/bin/env bash
# Run the stack with compose (podman or docker). Reads deploy/.env.
# MANAGED=1 adds the managed-mode overlay (mounts the agent key into venue-hl).
#
# Secret files (token, agent key) stay 0600 and owned by you. To let the containers read
# them, the container user is mapped to the invoking host user:
#   podman (rootless): host user -> image's nonroot uid 65532 (deploy/compose.podman.yaml)
#   docker:            containers run as the invoking uid:gid
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
  files+=(-f deploy/compose.podman.yaml)
  exec podman compose "${files[@]}" --env-file deploy/.env up --build "$@"
fi
CONTAINER_USER="$(id -u):$(id -g)"
export CONTAINER_USER
exec docker compose "${files[@]}" --env-file deploy/.env up --build "$@"

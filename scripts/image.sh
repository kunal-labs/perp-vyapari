#!/usr/bin/env bash
# Build one image per service with podman (or docker).
set -euo pipefail
cd "$(dirname "$0")/.."
ENGINE="$(command -v podman || command -v docker)"
for svc in gateway venue-hl; do
  "$ENGINE" build -f "deploy/Containerfile.${svc}" -t "perp-vyapari/${svc}:dev" .
done

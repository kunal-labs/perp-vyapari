#!/usr/bin/env bash
# Build, push and deploy the stack as a Cloud Run worker pool, driven by deploy/config.toml.
# Idempotent: safe to re-run. Requires gcloud (authenticated) and podman or docker.
#
# Secrets must already exist in Secret Manager (this script never reads secret values).
# Create one with:  gcloud secrets create <name> --project <project> --data-file=<0600 file>
set -euo pipefail
cd "$(dirname "$0")/.."

CONFIG="${CONFIG:-deploy/config.toml}"
render() { python3 scripts/lib/render_workerpool.py "$CONFIG" "$@"; }
get() { render --get "$1"; }

PROJECT="$(get gcp.project)"
REGION="$(get gcp.region)"
REPO="$(get gcp.artifact_repo)"
SA_NAME="$(get gcp.service_account)"
SA="${SA_NAME}@${PROJECT}.iam.gserviceaccount.com"
POOL="$(get worker_pool.name)"
TOKEN_SECRET="$(get secrets.telegram_token)"
KEY_SECRET="$(get secrets.hl_agent_key)"
REGISTRY="${REGION}-docker.pkg.dev"
TAG="$(git rev-parse --short=12 HEAD)"
if [[ -n "$(git status --porcelain)" ]]; then
  TAG="${TAG}-dirty"
fi
ENGINE="$(command -v podman || command -v docker)"
g() { gcloud --project "$PROJECT" --quiet "$@"; }

echo "==> project=$PROJECT region=$REGION pool=$POOL tag=$TAG"

echo "==> artifact registry repo"
g artifacts repositories describe "$REPO" --location "$REGION" >/dev/null 2>&1 \
  || g artifacts repositories create "$REPO" --location "$REGION" --repository-format docker \
       --description "perp-vyapari images"

echo "==> service account"
g iam service-accounts describe "$SA" >/dev/null 2>&1 \
  || g iam service-accounts create "$SA_NAME" --display-name "perp-vyapari worker pool"

echo "==> secrets (must exist; values never read here)"
for s in "$TOKEN_SECRET" "$KEY_SECRET"; do
  [[ -z "$s" ]] && continue
  if ! g secrets describe "$s" >/dev/null 2>&1; then
    echo "missing secret '$s': create it with: gcloud secrets create $s --project $PROJECT --data-file=<file>" >&2
    exit 1
  fi
  # A just-created service account can take a short while to become bindable.
  for attempt in 1 2 3 4 5 6; do
    if g secrets add-iam-policy-binding "$s" --member "serviceAccount:$SA" \
         --role roles/secretmanager.secretAccessor >/dev/null 2>&1; then
      break
    fi
    if [[ "$attempt" == 6 ]]; then
      echo "could not grant $SA access to secret $s" >&2
      exit 1
    fi
    sleep 10
  done
done

echo "==> build + push images"
gcloud auth print-access-token | "$ENGINE" login -u oauth2accesstoken --password-stdin "$REGISTRY" >/dev/null
for svc in gateway venue-hl; do
  ref="${REGISTRY}/${PROJECT}/${REPO}/${svc}:${TAG}"
  "$ENGINE" build -q -f "deploy/Containerfile.${svc}" -t "$ref" . >/dev/null
  "$ENGINE" push -q "$ref"
done

echo "==> deploy worker pool"
manifest="$(mktemp)"
trap 'rm -f "$manifest"' EXIT
render "$TAG" >"$manifest"
g beta run worker-pools replace "$manifest" --region "$REGION"

echo "==> done: gcloud beta run worker-pools describe $POOL --project $PROJECT --region $REGION"

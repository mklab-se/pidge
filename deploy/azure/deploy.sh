#!/usr/bin/env bash
# Deploy the pidge remote MCP server to Azure.
#
#   deploy/azure/deploy.sh [--skip-build] [--skip-entra]
#
# Idempotent. Phases:
#   1. Bicep: identity, registry, vault, logs, environment (no app yet)
#   2. Build the container image in ACR (cloud build, no local Docker needed)
#   3. Bicep again with the image: creates/updates the Container App
#   4. Register the server's OAuth callback on the pidge Entra app
#
# Requires: az CLI logged in to the subscription that owns the resource group.
set -euo pipefail

RESOURCE_GROUP="${PIDGE_RG:-pidge}"
ENTRA_APP_ID="${PIDGE_CLIENT_ID:-e49f90dc-c265-4392-b62f-b26704f9088f}"
ALLOWED_EMAILS="${PIDGE_MCP_ALLOWED_EMAILS:?set PIDGE_MCP_ALLOWED_EMAILS (comma-separated)}"
IMAGE_TAG="${IMAGE_TAG:-$(git rev-parse --short HEAD)}"
SKIP_BUILD=false
SKIP_ENTRA=false
for arg in "$@"; do
  case "$arg" in
    --skip-build) SKIP_BUILD=true ;;
    --skip-entra) SKIP_ENTRA=true ;;
    *) echo "unknown argument: $arg" >&2; exit 2 ;;
  esac
done

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
TEMPLATE="$REPO_ROOT/deploy/azure/main.bicep"

log() { printf '\033[1;34m==>\033[0m %s\n' "$*"; }

DEPLOYER_OID="$(az ad signed-in-user show --query id -o tsv 2>/dev/null || true)"

log "Phase 1: base infrastructure in resource group '$RESOURCE_GROUP'"
PHASE1="$(az deployment group create \
  --resource-group "$RESOURCE_GROUP" \
  --name pidge-mcp-infra \
  --template-file "$TEMPLATE" \
  --parameters allowedEmails="$ALLOWED_EMAILS" vaultAdminObjectId="$DEPLOYER_OID" \
  --query properties.outputs -o json)"
REGISTRY="$(jq -r .registryName.value <<<"$PHASE1")"
LOGIN_SERVER="$(jq -r .registryLoginServer.value <<<"$PHASE1")"
IMAGE="$LOGIN_SERVER/pidge-mcp:$IMAGE_TAG"

if [[ "$SKIP_BUILD" == true ]]; then
  log "Skipping image build; using $IMAGE"
else
  log "Phase 2: building $IMAGE in ACR"
  az acr build \
    --registry "$REGISTRY" \
    --image "pidge-mcp:$IMAGE_TAG" \
    --file "$REPO_ROOT/deploy/azure/Dockerfile" \
    --platform linux/amd64 \
    "$REPO_ROOT" >/dev/null
fi

log "Phase 3: container app"
PHASE3="$(az deployment group create \
  --resource-group "$RESOURCE_GROUP" \
  --name pidge-mcp-app \
  --template-file "$TEMPLATE" \
  --parameters allowedEmails="$ALLOWED_EMAILS" vaultAdminObjectId="$DEPLOYER_OID" image="$IMAGE" \
  --query properties.outputs -o json)"
PUBLIC_URL="$(jq -r .publicUrl.value <<<"$PHASE3")"
CALLBACK="$PUBLIC_URL/callback"

CURRENT="$(az ad app show --id "$ENTRA_APP_ID" --query 'publicClient.redirectUris' -o json)"
if jq -e --arg u "$CALLBACK" 'index($u)' <<<"$CURRENT" >/dev/null; then
  log "Phase 4: $CALLBACK is already a redirect URI on Entra app $ENTRA_APP_ID"
elif [[ "$SKIP_ENTRA" == true ]]; then
  log "Phase 4 skipped. Register the callback yourself (needs app-owner rights):"
  # shellcheck disable=SC2046
  echo "  az ad app update --id $ENTRA_APP_ID --public-client-redirect-uris $(jq -r '.[]' <<<"$CURRENT" | tr '\n' ' ')$CALLBACK"
else
  log "Phase 4: adding $CALLBACK as a redirect URI on Entra app $ENTRA_APP_ID"
  # shellcheck disable=SC2046
  az ad app update --id "$ENTRA_APP_ID" \
    --public-client-redirect-uris $(jq -r '.[]' <<<"$CURRENT") "$CALLBACK" >/dev/null
  log "Redirect URI added"
fi

cat <<SUMMARY

Deployed.
  MCP endpoint : $PUBLIC_URL/mcp
  Health       : $PUBLIC_URL/healthz
  Key Vault    : $(jq -r .keyVaultName.value <<<"$PHASE3")
  Image        : $IMAGE
  Logs         : az containerapp logs show -g $RESOURCE_GROUP -n ca-pidge-mcp --follow
SUMMARY

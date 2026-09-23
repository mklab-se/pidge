#!/usr/bin/env bash
# Deploy the pidge remote MCP server to Azure.
#
#   deploy/azure/deploy.sh [--skip-build] [--skip-entra] [--skip-certificate]
#
# Idempotent. Phases:
#   1. Bicep: identity, registry, vault, logs, environment (no app yet)
#   2. Build the container image in ACR (cloud build, no local Docker needed)
#   3. Container app, with an optional custom domain:
#        3a. bind the hostname with no certificate (bindingType Disabled)
#        3b. create the managed certificate if it doesn't exist yet, and
#            poll until it provisions
#        3c. redeploy with the certificate id (bindingType SniEnabled)
#      Skipped entirely unless PIDGE_MCP_CUSTOM_DOMAIN is set; steps 3b/3c
#      are skipped once the domain is already SniEnabled (or with
#      --skip-certificate, which asserts that and fails loudly if wrong).
#   4. Register the server's OAuth callback(s) on the pidge Entra app. With a
#      custom domain this only ever prints the command — it touches
#      production auth config for a domain cutover, so a human runs it.
#
# Requires: az CLI logged in to the subscription that owns the resource group.
set -euo pipefail

RESOURCE_GROUP="${PIDGE_RG:-pidge}"
CONTAINER_ENV="cae-${RESOURCE_GROUP}"
APP_NAME="ca-pidge-mcp"
ENTRA_APP_ID="${PIDGE_CLIENT_ID:-e49f90dc-c265-4392-b62f-b26704f9088f}"
ALLOWED_EMAILS="${PIDGE_MCP_ALLOWED_EMAILS:?set PIDGE_MCP_ALLOWED_EMAILS (comma-separated)}"
IMAGE_TAG="${IMAGE_TAG:-$(git rev-parse --short HEAD)}"
CUSTOM_DOMAIN="${PIDGE_MCP_CUSTOM_DOMAIN:-}"
CUTOVER="${PIDGE_MCP_CUTOVER:-}"
SKIP_BUILD=false
SKIP_ENTRA=false
SKIP_CERTIFICATE=false
for arg in "$@"; do
  case "$arg" in
    --skip-build) SKIP_BUILD=true ;;
    --skip-entra) SKIP_ENTRA=true ;;
    --skip-certificate) SKIP_CERTIFICATE=true ;;
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

# Custom-domain env vars for the container app. Without PIDGE_MCP_CUSTOM_DOMAIN
# these all stay empty and the deploy behaves exactly as before.
PUBLIC_URL_OVERRIDE=""
ALT_HOSTS=""
LEGACY_ISSUERS=""
if [[ -n "$CUSTOM_DOMAIN" ]]; then
  EXISTING_FQDN="$(az containerapp show --resource-group "$RESOURCE_GROUP" --name "$APP_NAME" \
    --query properties.configuration.ingress.fqdn -o tsv 2>/dev/null || true)"
  if [[ "$CUTOVER" == "1" ]]; then
    # The custom domain becomes the public URL (OAuth issuer). The old
    # Container Apps FQDN, if the app already exists, keeps working: it's
    # added as an alt host, and its issuer is grandfathered in so tokens
    # minted before the cutover still validate.
    PUBLIC_URL_OVERRIDE="https://$CUSTOM_DOMAIN"
    if [[ -n "$EXISTING_FQDN" ]]; then
      ALT_HOSTS="$EXISTING_FQDN"
      LEGACY_ISSUERS="https://$EXISTING_FQDN"
    fi
  else
    # The Container Apps FQDN stays the public URL; the custom domain is
    # just an additional accepted host.
    ALT_HOSTS="$CUSTOM_DOMAIN"
  fi
fi

# Resolves CERT_ID: the resource id of a SniEnabled managed certificate for
# $CUSTOM_DOMAIN, creating and binding one if necessary. Leaves CERT_ID
# empty (Disabled binding) when $CUSTOM_DOMAIN is unset.
CERT_ID=""
bind_custom_domain_certificate() {
  [[ -z "$CUSTOM_DOMAIN" ]] && return

  local existing_binding
  existing_binding="$(az containerapp show --resource-group "$RESOURCE_GROUP" --name "$APP_NAME" \
    --query "properties.configuration.ingress.customDomains[?name=='$CUSTOM_DOMAIN'].bindingType | [0]" \
    -o tsv 2>/dev/null || true)"

  if [[ "$existing_binding" == "SniEnabled" || "$SKIP_CERTIFICATE" == true ]]; then
    CERT_ID="$(az containerapp show --resource-group "$RESOURCE_GROUP" --name "$APP_NAME" \
      --query "properties.configuration.ingress.customDomains[?name=='$CUSTOM_DOMAIN'].certificateId | [0]" \
      -o tsv 2>/dev/null || true)"
    if [[ -n "$CERT_ID" && "$CERT_ID" != "null" ]]; then
      log "Custom domain $CUSTOM_DOMAIN is already bound with a managed certificate; skipping 3a-3c"
      return
    fi
    if [[ "$SKIP_CERTIFICATE" == true ]]; then
      echo "error: --skip-certificate given but $CUSTOM_DOMAIN has no SniEnabled certificate binding on $APP_NAME" >&2
      exit 1
    fi
    CERT_ID=""
  fi

  log "Phase 3a: binding $CUSTOM_DOMAIN with no certificate (Disabled)"
  az deployment group create \
    --resource-group "$RESOURCE_GROUP" \
    --name pidge-mcp-app \
    --template-file "$TEMPLATE" \
    --parameters allowedEmails="$ALLOWED_EMAILS" vaultAdminObjectId="$DEPLOYER_OID" image="$IMAGE" \
      customDomain="$CUSTOM_DOMAIN" customDomainCertificateId="" \
      publicUrlOverride="$PUBLIC_URL_OVERRIDE" altHosts="$ALT_HOSTS" legacyIssuers="$LEGACY_ISSUERS" \
    --query properties.outputs -o json >/dev/null

  local existing_cert
  existing_cert="$(az containerapp env certificate list \
    --resource-group "$RESOURCE_GROUP" --name "$CONTAINER_ENV" \
    --managed-certificates-only \
    --query "[?name=='pidge-mcp-managed']" -o json)"

  if [[ "$(jq 'length' <<<"$existing_cert")" == "0" ]]; then
    log "Phase 3b: creating managed certificate pidge-mcp-managed for $CUSTOM_DOMAIN"
    az containerapp env certificate create \
      --resource-group "$RESOURCE_GROUP" \
      --name "$CONTAINER_ENV" \
      --certificate-name pidge-mcp-managed \
      --hostname "$CUSTOM_DOMAIN" \
      --validation-method CNAME >/dev/null
  else
    log "Phase 3b: managed certificate pidge-mcp-managed already exists; waiting for it to provision"
  fi

  local waited=0 state=""
  while (( waited < 900 )); do
    state="$(az containerapp env certificate list \
      --resource-group "$RESOURCE_GROUP" --name "$CONTAINER_ENV" \
      --managed-certificates-only \
      --query "[?name=='pidge-mcp-managed'].properties.provisioningState | [0]" -o tsv)"
    [[ "$state" == "Succeeded" ]] && break
    sleep 20
    waited=$((waited + 20))
  done
  if [[ "$state" != "Succeeded" ]]; then
    echo "error: pidge-mcp-managed did not reach provisioningState Succeeded within 15 minutes (last state: $state)" >&2
    exit 1
  fi

  CERT_ID="$(az containerapp env certificate list \
    --resource-group "$RESOURCE_GROUP" --name "$CONTAINER_ENV" \
    --managed-certificates-only \
    --query "[?name=='pidge-mcp-managed'].id | [0]" -o tsv)"
  log "Phase 3c: redeploying $CUSTOM_DOMAIN with certificate $CERT_ID (SniEnabled)"
}

log "Phase 3: container app"
bind_custom_domain_certificate

PHASE3="$(az deployment group create \
  --resource-group "$RESOURCE_GROUP" \
  --name pidge-mcp-app \
  --template-file "$TEMPLATE" \
  --parameters allowedEmails="$ALLOWED_EMAILS" vaultAdminObjectId="$DEPLOYER_OID" image="$IMAGE" \
    customDomain="$CUSTOM_DOMAIN" customDomainCertificateId="$CERT_ID" \
    publicUrlOverride="$PUBLIC_URL_OVERRIDE" altHosts="$ALT_HOSTS" legacyIssuers="$LEGACY_ISSUERS" \
  --query properties.outputs -o json)"
PUBLIC_URL="$(jq -r .publicUrl.value <<<"$PHASE3")"
APP_FQDN="$(jq -r .appFqdn.value <<<"$PHASE3")"

log "Phase 4: Entra callback registration"
CURRENT="$(az ad app show --id "$ENTRA_APP_ID" --query 'publicClient.redirectUris' -o json)"

REQUIRED_CALLBACKS=("https://$APP_FQDN/callback")
if [[ -n "$CUSTOM_DOMAIN" ]]; then
  REQUIRED_CALLBACKS+=("https://$CUSTOM_DOMAIN/callback")
fi

MISSING=()
for cb in "${REQUIRED_CALLBACKS[@]}"; do
  if ! jq -e --arg u "$cb" 'index($u)' <<<"$CURRENT" >/dev/null; then
    MISSING+=("$cb")
  fi
done

if [[ ${#MISSING[@]} -eq 0 ]]; then
  log "Phase 4: all required redirect URIs already registered on Entra app $ENTRA_APP_ID"
elif [[ -n "$CUSTOM_DOMAIN" ]]; then
  log "Phase 4: custom domain callback missing. Register it yourself (needs app-owner rights), keeping every existing redirect URI:"
  # shellcheck disable=SC2046
  echo "  az ad app update --id $ENTRA_APP_ID --public-client-redirect-uris $(jq -r '.[]' <<<"$CURRENT" | tr '\n' ' ')${MISSING[*]}"
elif [[ "$SKIP_ENTRA" == true ]]; then
  log "Phase 4 skipped. Register the callback yourself (needs app-owner rights):"
  # shellcheck disable=SC2046
  echo "  az ad app update --id $ENTRA_APP_ID --public-client-redirect-uris $(jq -r '.[]' <<<"$CURRENT" | tr '\n' ' ')${MISSING[*]}"
else
  log "Phase 4: adding ${MISSING[*]} as redirect URI(s) on Entra app $ENTRA_APP_ID"
  # shellcheck disable=SC2046
  az ad app update --id "$ENTRA_APP_ID" \
    --public-client-redirect-uris $(jq -r '.[]' <<<"$CURRENT") "${MISSING[@]}" >/dev/null
  log "Redirect URI(s) added"
fi

cat <<SUMMARY

Deployed.
  MCP endpoint : $PUBLIC_URL/mcp
  Health       : $PUBLIC_URL/healthz
  Key Vault    : $(jq -r .keyVaultName.value <<<"$PHASE3")
  Image        : $IMAGE
  Logs         : az containerapp logs show -g $RESOURCE_GROUP -n $APP_NAME --follow
SUMMARY

if [[ -n "$CUSTOM_DOMAIN" ]]; then
  cat <<DOMAIN_SUMMARY
  Custom domain: $CUSTOM_DOMAIN ($([[ -n "$CERT_ID" ]] && echo "SniEnabled" || echo "Disabled, no certificate yet"))
  Cutover      : $([[ "$CUTOVER" == "1" ]] && echo "yes, public URL is https://$CUSTOM_DOMAIN" || echo "no, public URL is still $PUBLIC_URL")
DOMAIN_SUMMARY
fi

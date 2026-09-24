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
#      Steps 3a-3c run only for a custom domain that isn't SniEnabled yet;
#      once it is, the live certificate id is reused. --skip-certificate
#      asserts that and fails loudly if it isn't so.
#   4. Register the server's OAuth callback(s) on the pidge Entra app. With a
#      custom domain this only ever prints the command: it touches
#      production auth config for a domain cutover, so a human runs it.
#      Skipped entirely with --skip-entra: the read of the Entra app
#      (needed even to detect what's already registered) requires
#      Microsoft Graph directory-read rights a CI deploy identity should
#      not hold, so --skip-entra must avoid touching Graph at all rather
#      than just skipping the write.
#
# State-preserving: before changing anything, the script reads the live app
# (if it exists) and keeps its custom domain and cutover state unless told
# otherwise explicitly. Leaving an input unset never changes production:
#
#   PIDGE_MCP_CUSTOM_DOMAIN
#     unset/empty  keep the domain bound on the live app, with its
#                  certificate ("keeping bound custom domain X"), or none if
#                  none is bound
#     <domain>     bind this domain (must match the bound one, if any)
#     -            unbind the live domain
#   PIDGE_MCP_CUTOVER
#     unset/empty  keep the live state: if the live public URL is
#                  https://<custom domain>, stay cut over; otherwise don't
#     1            make https://<custom domain> the public URL (OAuth issuer);
#                  the Container Apps FQDN becomes an alt host and a legacy
#                  issuer
#     0            the FQDN is the public URL. If the app was cut over, this
#                  reverts it, and the custom-domain origin becomes a legacy
#                  issuer so tokens minted under it keep validating
#
# PIDGE_MCP_LEGACY_ISSUERS on the live app is carried over (plus whatever the
# run adds), so a grace granted by an earlier cutover or revert isn't lost.
# CI runs with neither input set, so it never changes domain or cutover
# state. See deploy/azure/README.md, "Custom domain".
#
# Requires: az CLI logged in to the subscription that owns the resource
# group, and jq.
set -euo pipefail

RESOURCE_GROUP="${PIDGE_RG:-pidge}"
CONTAINER_ENV="cae-${RESOURCE_GROUP}"
APP_NAME="ca-pidge-mcp"
ENTRA_APP_ID="${PIDGE_CLIENT_ID:-e49f90dc-c265-4392-b62f-b26704f9088f}"
ALLOWED_EMAILS="${PIDGE_MCP_ALLOWED_EMAILS:?set PIDGE_MCP_ALLOWED_EMAILS (comma-separated)}"
IMAGE_TAG="${IMAGE_TAG:-$(git rev-parse --short HEAD)}"
DOMAIN_INPUT="${PIDGE_MCP_CUSTOM_DOMAIN:-}"
CUTOVER_INPUT="${PIDGE_MCP_CUTOVER:-}"
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
die() { echo "error: $*" >&2; exit 1; }

case "$CUTOVER_INPUT" in
  ""|0|1) ;;
  *) die "PIDGE_MCP_CUTOVER must be 1, 0 or unset, got '$CUTOVER_INPUT'" ;;
esac

# --- Live state -------------------------------------------------------------
# Read before anything changes. `list` distinguishes "no app yet" (empty)
# from an az failure (non-zero exit, which stops the script); treating a
# failed read as "no app" would unbind the domain.
LIVE_EXISTS="$(az containerapp list --resource-group "$RESOURCE_GROUP" \
  --query "[?name=='$APP_NAME'].name" -o tsv)"
LIVE_FQDN=""
LIVE_DOMAIN=""
LIVE_BINDING=""
LIVE_CERT_ID=""
LIVE_PUBLIC_URL=""
LIVE_LEGACY_ISSUERS=""
if [[ -n "$LIVE_EXISTS" ]]; then
  LIVE="$(az containerapp show --resource-group "$RESOURCE_GROUP" --name "$APP_NAME" -o json)"
  live_env() {
    jq -r --arg n "$1" \
      '[.properties.template.containers[].env[]? | select(.name == $n) | .value] | first // ""' \
      <<<"$LIVE"
  }
  LIVE_FQDN="$(jq -r '.properties.configuration.ingress.fqdn // ""' <<<"$LIVE")"
  if (( $(jq '.properties.configuration.ingress.customDomains // [] | length' <<<"$LIVE") > 1 )); then
    die "$APP_NAME has more than one custom domain bound; this script manages exactly one"
  fi
  LIVE_DOMAIN="$(jq -r '.properties.configuration.ingress.customDomains[0].name // ""' <<<"$LIVE")"
  LIVE_BINDING="$(jq -r '.properties.configuration.ingress.customDomains[0].bindingType // ""' <<<"$LIVE")"
  LIVE_CERT_ID="$(jq -r '.properties.configuration.ingress.customDomains[0].certificateId // ""' <<<"$LIVE")"
  LIVE_PUBLIC_URL="$(live_env PIDGE_MCP_PUBLIC_URL)"
  LIVE_PUBLIC_URL="${LIVE_PUBLIC_URL%/}"
  LIVE_LEGACY_ISSUERS="$(live_env PIDGE_MCP_LEGACY_ISSUERS)"
fi

# --- Custom domain ----------------------------------------------------------
case "$DOMAIN_INPUT" in
  -)
    CUSTOM_DOMAIN=""
    if [[ -n "$LIVE_DOMAIN" ]]; then
      log "Unbinding custom domain $LIVE_DOMAIN (PIDGE_MCP_CUSTOM_DOMAIN=-)"
    fi
    ;;
  "")
    CUSTOM_DOMAIN="$LIVE_DOMAIN"
    if [[ -n "$LIVE_DOMAIN" ]]; then
      log "Keeping bound custom domain $LIVE_DOMAIN (PIDGE_MCP_CUSTOM_DOMAIN unset; '-' unbinds it)"
    fi
    ;;
  *)
    CUSTOM_DOMAIN="$DOMAIN_INPUT"
    if [[ -n "$LIVE_DOMAIN" && "$LIVE_DOMAIN" != "$CUSTOM_DOMAIN" ]]; then
      die "$APP_NAME has $LIVE_DOMAIN bound, not $CUSTOM_DOMAIN; unbind it first with PIDGE_MCP_CUSTOM_DOMAIN=- (and PIDGE_MCP_CUTOVER=0 if it's the public URL)"
    fi
    ;;
esac

# --- Cutover ----------------------------------------------------------------
# The live app is cut over when its public URL isn't its own FQDN.
LIVE_CUT_OVER=false
if [[ -n "$LIVE_PUBLIC_URL" && -n "$LIVE_FQDN" && "$LIVE_PUBLIC_URL" != "https://$LIVE_FQDN" ]]; then
  LIVE_CUT_OVER=true
fi
case "$CUTOVER_INPUT" in
  1)
    [[ -n "$CUSTOM_DOMAIN" ]] || die "PIDGE_MCP_CUTOVER=1 needs a custom domain (set PIDGE_MCP_CUSTOM_DOMAIN, or bind one first)"
    CUT_OVER=true
    ;;
  0)
    CUT_OVER=false
    ;;
  "")
    if [[ "$LIVE_CUT_OVER" == true ]]; then
      if [[ -z "$CUSTOM_DOMAIN" || "$LIVE_PUBLIC_URL" != "https://$CUSTOM_DOMAIN" ]]; then
        die "the live public URL is $LIVE_PUBLIC_URL, which this run would change; pass PIDGE_MCP_CUTOVER=0 to revert to https://$LIVE_FQDN explicitly"
      fi
      log "Staying cut over: public URL stays $LIVE_PUBLIC_URL (PIDGE_MCP_CUTOVER unset; 0 reverts)"
      CUT_OVER=true
    else
      CUT_OVER=false
    fi
    ;;
esac

# Container app env vars. Legacy issuers accumulate: the live list is kept,
# the run adds the issuer it retires (if any), and the current public URL is
# never listed as its own legacy issuer.
PUBLIC_URL_OVERRIDE=""
ALT_HOSTS=""
ADD_LEGACY=""
if [[ "$CUT_OVER" == true ]]; then
  # The custom domain becomes the public URL (OAuth issuer). The Container
  # Apps FQDN, if the app already exists, keeps working as an alt host and a
  # legacy issuer, so tokens minted under it still validate.
  PUBLIC_URL_OVERRIDE="https://$CUSTOM_DOMAIN"
  if [[ -n "$LIVE_FQDN" ]]; then
    ALT_HOSTS="$LIVE_FQDN"
    ADD_LEGACY="https://$LIVE_FQDN"
  fi
else
  # The Container Apps FQDN is the public URL; a custom domain is just an
  # additional accepted host.
  ALT_HOSTS="$CUSTOM_DOMAIN"
  if [[ "$LIVE_CUT_OVER" == true ]]; then
    log "Reverting cutover (PIDGE_MCP_CUTOVER=0): public URL goes back to https://$LIVE_FQDN; $LIVE_PUBLIC_URL stays a legacy issuer"
    ADD_LEGACY="$LIVE_PUBLIC_URL"
  fi
fi
NEW_PUBLIC_URL="${PUBLIC_URL_OVERRIDE:-${LIVE_FQDN:+https://$LIVE_FQDN}}"
LEGACY_ISSUERS="$({ tr ',' '\n' <<<"$LIVE_LEGACY_ISSUERS"; echo "$ADD_LEGACY"; } \
  | sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//' -e 's:/*$::' \
  | awk -v current="$NEW_PUBLIC_URL" 'NF && $0 != current && !seen[$0]++' \
  | paste -sd, -)"

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

# Resolves CERT_ID: the resource id of a SniEnabled managed certificate for
# $CUSTOM_DOMAIN, creating and binding one if necessary. Leaves CERT_ID
# empty (Disabled binding) when $CUSTOM_DOMAIN is unset.
CERT_ID=""
bind_custom_domain_certificate() {
  [[ -z "$CUSTOM_DOMAIN" ]] && return

  if [[ "$LIVE_DOMAIN" == "$CUSTOM_DOMAIN" && "$LIVE_BINDING" == "SniEnabled" && -n "$LIVE_CERT_ID" ]]; then
    CERT_ID="$LIVE_CERT_ID"
    log "Custom domain $CUSTOM_DOMAIN is already bound with a managed certificate; skipping 3a-3c"
    return
  fi
  if [[ "$SKIP_CERTIFICATE" == true ]]; then
    die "--skip-certificate given but $CUSTOM_DOMAIN has no SniEnabled certificate binding on $APP_NAME"
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
    --query "[?name=='pidge-mcp-managed']" -o json 2>/dev/null || true)"

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
      --query "[?name=='pidge-mcp-managed'].properties.provisioningState | [0]" -o tsv 2>/dev/null || true)"
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

if [[ "$SKIP_ENTRA" == true ]]; then
  log "Phase 4 skipped (--skip-entra)"
else
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
  else
    log "Phase 4: adding ${MISSING[*]} as redirect URI(s) on Entra app $ENTRA_APP_ID"
    # shellcheck disable=SC2046
    az ad app update --id "$ENTRA_APP_ID" \
      --public-client-redirect-uris $(jq -r '.[]' <<<"$CURRENT") "${MISSING[@]}" >/dev/null
    log "Redirect URI(s) added"
  fi
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
  Cutover      : $([[ "$CUT_OVER" == true ]] && echo "yes, public URL is https://$CUSTOM_DOMAIN" || echo "no, public URL is $PUBLIC_URL")
DOMAIN_SUMMARY
fi
if [[ -n "$LEGACY_ISSUERS" ]]; then
  echo "  Legacy issuers: $LEGACY_ISSUERS"
fi

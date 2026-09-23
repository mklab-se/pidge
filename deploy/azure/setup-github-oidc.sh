#!/usr/bin/env bash
# One-time setup for GitHub Actions to deploy pidge-mcp via OIDC (no stored
# Azure credentials). Run this once per environment, by hand, from a
# workstation logged in with `az login` and `gh auth login`.
#
#   PIDGE_MCP_ALLOWED_EMAILS=a@x,b@y deploy/azure/setup-github-oidc.sh
#
# Idempotent: safe to re-run after the identity or role assignments already
# exist. Creates, if missing:
#   - user-assigned identity `id-pidge-deploy` in the resource group
#   - a federated credential trusting GitHub Actions runs from `main`
#   - Contributor and Role Based Access Control Administrator role
#     assignments for that identity, scoped to the resource group (Bicep
#     itself creates role assignments during deploy, hence the latter role)
# and always sets the GitHub repository variables and secret that
# `.github/workflows/deploy-mcp.yml` reads: AZURE_CLIENT_ID, AZURE_TENANT_ID,
# AZURE_SUBSCRIPTION_ID, PIDGE_MCP_ALLOWED_EMAILS (secret), and optionally
# PIDGE_MCP_CUSTOM_DOMAIN.
#
# Requires: az CLI logged in to the subscription that owns the resource
# group, and gh CLI logged in with admin rights on the GitHub repository.
set -euo pipefail

RESOURCE_GROUP="${PIDGE_RG:-pidge}"
IDENTITY_NAME="id-pidge-deploy"
FEDERATED_CRED_NAME="github-main"
REPO="mklab-se/pidge"
GITHUB_ISSUER="https://token.actions.githubusercontent.com"
GITHUB_SUBJECT="repo:${REPO}:ref:refs/heads/main"
GITHUB_AUDIENCE="api://AzureADTokenExchange"
ROLE_CONTRIBUTOR="b24988ac-6180-42a0-ab88-20f7382dd24c"
ROLE_RBAC_ADMIN="f58310d9-a9f6-439a-9e8d-f62e7b41a168"
ALLOWED_EMAILS="${PIDGE_MCP_ALLOWED_EMAILS:?set PIDGE_MCP_ALLOWED_EMAILS (comma-separated) before running this script}"
CUSTOM_DOMAIN="${PIDGE_MCP_CUSTOM_DOMAIN:-}"

log() { printf '\033[1;34m==>\033[0m %s\n' "$*"; }

SUBSCRIPTION_ID="$(az account show --query id -o tsv)"
TENANT_ID="$(az account show --query tenantId -o tsv)"

log "Identity '$IDENTITY_NAME' in resource group '$RESOURCE_GROUP'"
if az identity show --resource-group "$RESOURCE_GROUP" --name "$IDENTITY_NAME" &>/dev/null; then
  log "  already exists"
else
  az identity create \
    --resource-group "$RESOURCE_GROUP" \
    --name "$IDENTITY_NAME" \
    --tags project=pidge environment=spike managed-by=script \
    >/dev/null
  log "  created"
fi

CLIENT_ID="$(az identity show --resource-group "$RESOURCE_GROUP" --name "$IDENTITY_NAME" --query clientId -o tsv)"
PRINCIPAL_ID="$(az identity show --resource-group "$RESOURCE_GROUP" --name "$IDENTITY_NAME" --query principalId -o tsv)"
IDENTITY_ID="$(az identity show --resource-group "$RESOURCE_GROUP" --name "$IDENTITY_NAME" --query id -o tsv)"

log "Federated credential '$FEDERATED_CRED_NAME'"
if az identity federated-credential show \
    --resource-group "$RESOURCE_GROUP" \
    --identity-name "$IDENTITY_NAME" \
    --name "$FEDERATED_CRED_NAME" &>/dev/null; then
  log "  already exists"
else
  az identity federated-credential create \
    --resource-group "$RESOURCE_GROUP" \
    --identity-name "$IDENTITY_NAME" \
    --name "$FEDERATED_CRED_NAME" \
    --issuer "$GITHUB_ISSUER" \
    --subject "$GITHUB_SUBJECT" \
    --audiences "$GITHUB_AUDIENCE" \
    >/dev/null
  log "  created (subject: $GITHUB_SUBJECT)"
fi

SCOPE="/subscriptions/${SUBSCRIPTION_ID}/resourceGroups/${RESOURCE_GROUP}"

assign_role_if_missing() {
  local role_id="$1" role_name="$2" existing
  existing="$(az role assignment list \
    --assignee-object-id "$PRINCIPAL_ID" \
    --role "$role_id" \
    --scope "$SCOPE" \
    --query "[].id" -o tsv)"
  if [[ -n "$existing" ]]; then
    log "Role '$role_name' already assigned"
  else
    az role assignment create \
      --assignee-object-id "$PRINCIPAL_ID" \
      --assignee-principal-type ServicePrincipal \
      --role "$role_id" \
      --scope "$SCOPE" \
      >/dev/null
    log "Role '$role_name' assigned"
  fi
}

assign_role_if_missing "$ROLE_CONTRIBUTOR" "Contributor"
assign_role_if_missing "$ROLE_RBAC_ADMIN" "Role Based Access Control Administrator"

log "GitHub repository variables on $REPO"
gh variable set AZURE_CLIENT_ID --repo "$REPO" --body "$CLIENT_ID" >/dev/null
gh variable set AZURE_TENANT_ID --repo "$REPO" --body "$TENANT_ID" >/dev/null
gh variable set AZURE_SUBSCRIPTION_ID --repo "$REPO" --body "$SUBSCRIPTION_ID" >/dev/null

log "GitHub repository secret PIDGE_MCP_ALLOWED_EMAILS on $REPO"
# Piped via stdin rather than --body so the value never appears as a
# process argument.
printf '%s' "$ALLOWED_EMAILS" | gh secret set PIDGE_MCP_ALLOWED_EMAILS --repo "$REPO" >/dev/null

if [[ -n "$CUSTOM_DOMAIN" ]]; then
  log "GitHub repository variable PIDGE_MCP_CUSTOM_DOMAIN on $REPO"
  gh variable set PIDGE_MCP_CUSTOM_DOMAIN --repo "$REPO" --body "$CUSTOM_DOMAIN" >/dev/null
fi

CUSTOM_DOMAIN_NOTE=""
if [[ -n "$CUSTOM_DOMAIN" ]]; then
  CUSTOM_DOMAIN_NOTE=", PIDGE_MCP_CUSTOM_DOMAIN"
fi

cat <<SUMMARY

Done.
  Identity        : $IDENTITY_NAME ($IDENTITY_ID)
  Client ID       : $CLIENT_ID
  Principal ID    : $PRINCIPAL_ID
  Tenant ID       : $TENANT_ID
  Subscription ID : $SUBSCRIPTION_ID
  Federated cred  : $FEDERATED_CRED_NAME ($GITHUB_SUBJECT)
  Roles           : Contributor, Role Based Access Control Administrator on $SCOPE
  GitHub repo     : $REPO
  Variables set   : AZURE_CLIENT_ID, AZURE_TENANT_ID, AZURE_SUBSCRIPTION_ID$CUSTOM_DOMAIN_NOTE
  Secret set      : PIDGE_MCP_ALLOWED_EMAILS (value not shown)
SUMMARY

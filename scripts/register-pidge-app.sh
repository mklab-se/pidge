#!/usr/bin/env bash
# One-time developer setup: register pidge as a multi-tenant public-client app in Entra.
# Run once. Paste the printed client_id into crates/pidge-client/src/auth/config.rs.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

if ! command -v az >/dev/null 2>&1; then
  cat <<EOF >&2
Azure CLI not found. Install it from https://aka.ms/install-azure-cli, then re-run.

Or follow the manual portal walkthrough in DEVELOPMENT.md.
EOF
  exit 1
fi

if ! az account show >/dev/null 2>&1; then
  echo "Run 'az login' first." >&2
  exit 1
fi

echo "Registering pidge app in your Entra tenant…"
CLIENT_ID=$(az ad app create \
  --display-name "pidge" \
  --sign-in-audience AzureADandPersonalMicrosoftAccount \
  --is-fallback-public-client true \
  --required-resource-accesses "@${SCRIPT_DIR}/pidge-app-permissions.json" \
  --query appId -o tsv)

# Add the native-client redirect URI. The device-code flow itself never
# redirects, but the personal-MSA login endpoint (login.live.com) validates
# that public clients have *some* redirect URI registered and refuses sign-in
# with `invalid_request: ... redirect_uri ...` otherwise. The well-known
# `nativeclient` URI is the documented value for desktop / CLI apps.
echo "Setting public-client redirect URI for personal-MSA sign-in…"
az ad app update --id "${CLIENT_ID}" \
  --public-client-redirect-uris https://login.microsoftonline.com/common/oauth2/nativeclient

cat <<EOF

✔ pidge app registered.
  client_id: ${CLIENT_ID}

Paste this into crates/pidge-client/src/auth/config.rs:

    pub const APP_CLIENT_ID: &str = "${CLIENT_ID}";

Then commit and continue.

EOF

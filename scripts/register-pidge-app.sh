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

# Note on redirect URIs: device-code flow does not redirect, so we
# deliberately leave the public-client redirect URI list empty. An earlier
# version of this script added `nativeclient` to work around a personal-MSA
# `redirect_uri required` error, but that turned out to be the wrong fix —
# adding nativeclient caused Microsoft's anti-phishing UI to trigger on the
# legacy implicit-flow redirect. `--is-fallback-public-client true` (above)
# is the property that actually enables device-code for both work/school
# and personal accounts.

cat <<EOF

✔ pidge app registered.
  client_id: ${CLIENT_ID}

Paste this into crates/pidge-client/src/auth/config.rs:

    pub const APP_CLIENT_ID: &str = "${CLIENT_ID}";

Then commit and continue.

EOF

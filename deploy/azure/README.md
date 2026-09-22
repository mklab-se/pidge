# pidge-mcp on Azure

Remote MCP server for pidge. See `docs/superpowers/specs/2026-09-22-remote-mcp-spike-design.md`
for the design.

## Deploy

```bash
az login                      # subscription that owns the resource group
az group create -n pidge -l swedencentral   # once
deploy/azure/deploy.sh        # idempotent; ~10 min the first time (ACR build)
```

The script deploys `main.bicep` twice (infrastructure, then the app with the
freshly built image), builds the image in ACR, and registers the server's
`/callback` URL on the pidge Entra app. Override the allowlist with
`PIDGE_MCP_ALLOWED_EMAILS=a@x,b@y`.

## Connect a client

Give the harness the MCP URL printed by the script (`https://<fqdn>/mcp`). It
discovers the OAuth endpoints itself, registers, and sends you to Microsoft
sign-in. Only addresses on the allowlist get past the callback.

## Operate

```bash
az containerapp logs show -g pidge -n ca-pidge-mcp --follow
az keyvault secret list --vault-name <kv-name> -o table      # jwt-signing-key, mailbox-*
az keyvault secret delete --vault-name <kv-name> -n mailbox-<addr>   # disconnect a mailbox
```

Rotating `jwt-signing-key` invalidates every client registration and token;
clients simply re-register and sign in again.

## Run locally

```bash
PIDGE_MCP_PUBLIC_URL=http://localhost:8080 \
PIDGE_MCP_ALLOWED_EMAILS=you@example.com \
PIDGE_MCP_SECRETS_DIR=/tmp/pidge-mcp-secrets \
cargo run -p pidge-mcp
```

`PIDGE_MCP_KEYVAULT_URL=https://<kv>.vault.azure.net/` instead of the secrets
dir uses Key Vault through your `az login`.

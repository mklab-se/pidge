# Remote MCP spike: design

**Date:** 2026-09-22
**Status:** superseded by the full-feature design (`2026-09-22-remote-mcp-full-feature-design.md`)
**Crate:** `crates/pidge-mcp` · **Infra:** `deploy/azure/`

## Goal

Prove that pidge can be reached from any MCP-capable harness (Claude.ai, Claude
Cowork, Claude Code, ChatGPT) without a computer of the user's own, with sign-in
restricted to a two-person allowlist, and with the mail logic reused from
`pidge-client` rather than rewritten. Success: add the server as a remote MCP
in Claude Cowork or ChatGPT, sign in, and retrieve the latest e-mail.

## Decisions

| Question | Decision | Why |
|---|---|---|
| Pidge *as* the server or a server *using* pidge? | New crate `pidge-mcp` depending on `pidge-client`; the CLI is untouched. | The CLI's config, keychain and terminal prompts are single-user. `pidge-client` already takes an account on every Graph call. |
| Language | Rust | The asset is `pidge-client` (Graph surface, retry, delta, rendering). A Python port would trade a thinner mail layer for library-provided auth. |
| Auth: harness → server | The server is its own OAuth 2.1 authorization server: RFC 9728 protected-resource metadata, RFC 8414 AS metadata, RFC 7591 dynamic client registration, code + PKCE (S256 only), refresh tokens. Public clients only. | Entra has no dynamic client registration and its tokens are for Graph, not for this server. Every mainstream MCP client speaks exactly this profile. |
| Auth: who is the user? | `/authorize` redirects to Microsoft login (same Entra app as the CLI, `common` authority, so personal MSA accounts work). `/callback` reads `/me`, checks the allowlist, and only then issues a code. | The allowlist is the entire authorization model for now. |
| Mailbox connection | The Microsoft sign-in requests pidge's Graph scopes, so signing in **is** connecting the mailbox: the refresh token is stored keyed by the e-mail. | One step for the user; no separate "connect mailbox" flow needed for the primary account. |
| State | Clients, codes, access and refresh tokens are HS256 JWTs over one key (`typ` claim separates kinds). In memory: pending authorizations (10 min TTL) and redeemed code ids. Persistent: the signing key and one secret per mailbox. | "As little state as possible". The persistent set is exactly what cannot be re-derived. |
| Secret store | Azure Key Vault (RBAC), accessed by a user-assigned managed identity; `SecretStore` trait with a file backend for development. | Cheapest safe home for refresh tokens; no connection strings anywhere. |
| Isolation | The bearer middleware verifies the JWT (issuer, audience = `/mcp`, expiry, allowlist) and attaches `AuthenticatedUser` to the request. rmcp propagates the HTTP parts into tool context; every tool reads the user from there and never from its input. | Cross-user access is structurally impossible, not policy-dependent. |
| Hosting | Container Apps (consumption, scale 0–1), ACR pulls via managed identity, Log Analytics, Bicep in two phases. | Single replica keeps the in-memory OAuth window coherent; scale-to-zero is fine for a Rust binary. |
| Tools | `whoami`, `inbox_latest`, `read_message`: workflow-shaped, plain text, wrapped as untrusted content, body capped. | Enough for the success criterion; the real tool set is a separate design. |

## pidge-client change

`AuthClient` now holds an `Arc<dyn TokenBackend>`. The default,
`LocalBackend`, is the previous behaviour (resolve keychain/file from
`config.yaml`, backfill tenant id). `from_env_with_backend` / `with_backend`
inject another store. `authorize_url`, `exchange_code` and `store_tokens` expose
the pieces of the browser flow a hosted callback needs. No CLI behaviour changed.

## Flow

```
Harness                    pidge-mcp                         Microsoft
  │ POST /mcp (no token)      │                                  │
  │◀── 401 + resource_metadata│                                  │
  │ GET /.well-known/…        │                                  │
  │ POST /register ──────────▶│ client_id = signed metadata      │
  │ GET /authorize ──────────▶│ pending[state] = {client, PKCE}  │
  │◀── 303 login.microsoft…   │                                  │
  │ (user signs in) ─────────────────────────────────────────────▶
  │◀───────────────────────── GET /callback?code&state ◀─────────│
  │                           │ redeem code, GET /me, allowlist? │
  │                           │ store refresh token (Key Vault)  │
  │◀── 303 redirect_uri?code  │                                  │
  │ POST /token (PKCE) ──────▶│ access (1 h) + refresh (30 d)    │
  │ POST /mcp Bearer ────────▶│ tools run as token.sub           │
```

## Known limits of the spike

- Refresh tokens are stateless, so they cannot be revoked individually before
  expiry; removing an address from the allowlist blocks refresh and bearer use.
- Redeemed-code tracking is per process; a restart inside a code's two-minute
  lifetime would allow one replay. Acceptable for a single-replica spike.
- Client registration is deterministic for identical metadata; public clients
  rely on PKCE, not on client identity, so this is by design.
- The Entra app is a public client; the hosted callback is registered as a
  public-client redirect URI. A confidential client with its own secret is the
  textbook shape for a server and is a follow-up.
- Key Vault purge protection is off so the spike can be torn down.
- No custom domain; the Container Apps FQDN is the issuer.

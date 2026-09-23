# Remote MCP — Docs and Distribution Implementation Plan (sub-project 3)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let a local pidge user migrate to the hosted server with one command, publish the server image on every release, and document the whole thing for others.

**Architecture:** `pidge-client` gains a small MCP client (OAuth discovery, dynamic registration, PKCE with the existing localhost callback listener, token storage, JSON-RPC over streamable HTTP) with no clap or terminal code. The CLI adds `pidge mcp connect|status|logout` on top of it. The release workflow builds and pushes the server image to GHCR. A single `docs/mcp.md` covers connecting from each client, self-hosting on Azure, and running the container elsewhere.

**Tech Stack:** Rust 2024 (MSRV 1.88), reqwest 0.13, `pidge_client::auth::browser_flow` (callback listener, PKCE helpers), keyring 4 / file store, clap 4, GitHub Actions `docker/build-push-action@v6` to `ghcr.io`.

**Spec:** `docs/superpowers/specs/2026-09-22-remote-mcp-full-feature-design.md` — Part 3 and §1.5 "Migration from local pidge"

## Global Constraints

- No refresh tokens for Microsoft ever leave the machine: migration connects each mailbox by click-through in the browser (spec §1.5).
- `pidge-client` knows nothing about clap or terminal output; `pidge-core` has no HTTP.
- MCP server tokens are stored like account tokens: OS keychain by default, `--store=file` opt-in at `~/.config/pidge/mcp/<host>.json` mode 0600; never committed (the existing `**/tokens/*.json` ignore pattern is extended to `**/mcp/*.json`).
- The CLI never prints tokens; `--json` output stays machine-readable.
- Container image: `ghcr.io/mklab-se/pidge-mcp:<version>` and `:latest`, linux/amd64, built from `deploy/azure/Dockerfile`, published only on `v*` tags, after the existing `ci` job.
- `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all -- --check`, `cargo test --workspace` stay clean.
- Commit trailer lines: `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>` and `Claude-Session: https://claude.ai/code/session_01Tj3dUYia6V1FHGhQHGMNkB`.

## Review Focus

1. The server's consent interstitial sits between the browser open and Microsoft; `pidge mcp connect` must not time out while the user reads it — expected: the callback listener waits at least 10 minutes (Task 2 test on the listener timeout constant).
2. An expired MCP access token during `pidge mcp status` — expected: refreshed silently via the refresh grant, or a clear "run `pidge mcp connect` again" if the refresh is refused (Task 1 test `refresh_on_expiry`, Task 3 behaviour).
3. A server whose discovery document lacks `registration_endpoint` — expected: a one-line error saying the server does not support dynamic registration (Task 1 test).
4. A local account that is already connected on the server — expected: skipped with "already connected", no browser opened (Task 2 test with a mocked `accounts_list`).
5. The image job runs on a fork or a non-tag push — expected: it doesn't (tag-only trigger inherited from the workflow; Task 4 reviews the job conditions).

---

### Task 1: MCP client in `pidge-client`

**Files:**
- Create: `crates/pidge-client/src/mcp/mod.rs`, `crates/pidge-client/src/mcp/oauth.rs`, `crates/pidge-client/src/mcp/store.rs`, `crates/pidge-client/src/mcp/rpc.rs`
- Modify: `crates/pidge-client/src/lib.rs` (`pub mod mcp;`), `crates/pidge-client/src/auth/browser_flow.rs` (make `wait_for_callback`, `make_code_verifier`, `make_code_challenge`, `make_random` `pub(crate)` and re-export what `mcp/oauth.rs` needs), `.gitignore`
- Test: wiremock tests in each module

**Interfaces:**
- `pub struct McpTokens { pub server: String, pub access_token: String, pub refresh_token: String, pub expires_at: DateTime<Utc>, pub client_id: String }` (serde).
- `pub struct McpTokenStore;` with `load(server_url, storage: TokenStorage) -> Result<Option<McpTokens>>`, `save(&McpTokens, storage)`, `delete(server_url, storage)`; keychain service `"pidge-mcp"`, account = normalised server origin; file path `<config_dir>/pidge/mcp/<host>.json` (host with `:` → `_`).
- `pub async fn discover(http, mcp_url) -> Result<Discovery { resource: String, authorization_endpoint, token_endpoint, registration_endpoint: Option<String> }>` — GET `<origin>/.well-known/oauth-protected-resource/mcp` (fallback `/.well-known/oauth-protected-resource`), then `<as>/.well-known/oauth-authorization-server`.
- `pub async fn register(http, &Discovery, redirect_uri, client_name) -> Result<String /*client_id*/>` — POST `registration_endpoint` with `{redirect_uris:[..], client_name, token_endpoint_auth_method:"none", grant_types:[..], response_types:["code"]}`; error `ClientError::Graph{status:400, message:"server does not support dynamic client registration"}` when `registration_endpoint` is `None`.
- `pub async fn sign_in<F: FnOnce(&str)>(http, mcp_url, client_name, on_open) -> Result<McpTokens>` — discover, bind the localhost listener (reuse `TcpListener::bind("127.0.0.1:0")` + `wait_for_callback` with a 10-minute timeout), register with `http://localhost:<port>`, build the authorize URL (`response_type=code`, `client_id`, `redirect_uri`, `state`, `code_challenge`+`S256`, `resource=<mcp_url>`), call `on_open`, wait for the callback, verify `state`, POST the token endpoint (`grant_type=authorization_code`, `code`, `redirect_uri`, `client_id`, `code_verifier`, `resource`), return tokens.
- `pub async fn refresh(http, &Discovery, &McpTokens) -> Result<McpTokens>`; `pub async fn valid_access_token(http, mcp_url, tokens: &mut McpTokens) -> Result<String>` (refresh when within 60 s of expiry, error `SessionExpired{email: server}` on `invalid_grant`).
- `pub struct McpRpc { http, mcp_url, access_token, session_id: Option<String> }` with `initialize()` (stores `Mcp-Session-Id` if returned; sends `notifications/initialized`), `call_tool(name, args: Value) -> Result<ToolResult { text: String, is_error: bool }>` (concatenates text content blocks; parses both `application/json` and `text/event-stream` bodies by taking the last `data:` line), `list_tools() -> Vec<String>`.

- [ ] **Step 1: Failing tests** (wiremock): discovery happy path and the missing-`registration_endpoint` error; `register` posts the expected JSON; `refresh_on_expiry` (expired tokens → refresh grant called once → new tokens); `McpRpc::call_tool` parses a JSON response and an SSE response; `McpTokenStore` file round-trip in a tempdir with mode 0600.
- [ ] **Step 2: Run to verify failure.** **Step 3: Implement.** **Step 4: Tests, clippy, fmt.** **Step 5: Commit** — `git commit -am "feat(client): MCP client — discovery, registration, PKCE sign-in, token store, JSON-RPC"`

---

### Task 2: `pidge mcp connect`

**Files:**
- Modify: `crates/pidge/src/cli.rs` (`Commands::Mcp { command: McpCommands }`, `McpCommands::Connect { url: String, #[arg(long, default_value="keychain")] store: TokenStorage, #[arg(long)] yes: bool }`, `Status`, `Logout`)
- Create: `crates/pidge/src/commands/mcp.rs`, `crates/pidge/src/commands/mcp_connect.rs`
- Modify: `crates/pidge/src/main.rs` (dispatch), `crates/pidge/src/commands/mod.rs`

**Behaviour of `connect`:**
1. If tokens for the server exist and are valid (or refreshable), skip sign-in; else run `sign_in` (print the URL, open the browser like `account_add.rs::open_browser`), then save tokens.
2. `McpRpc::initialize`, `call_tool("accounts_list", {})` → parse the mailbox lines (`- <addr>` prefix as rendered by the server; keep the parser tolerant: any line containing an `@` token that also appears in the local account list counts as connected).
3. For each local account (`Config::load().accounts`) not connected, in order: `call_tool("accounts_connect", {"email": addr})` → extract the `https://…/connect?state=…` URL from the text → print it, open the browser, then wait for the user to press Enter (skip the prompt with `--yes`, which instead polls `accounts_list` every 5 s for up to 10 minutes until the address appears).
4. Copy settings: if the local `Config.defaults.send` account is connected, `accounts_update {"default_sender": addr}`; for each local trusted sender, `accounts_update {"trust": addr}`.
5. Print the final `accounts_list` text; with `--json` print `{"server", "connected": [..], "skipped": [..]}`.

- [ ] **Step 1: Failing tests** — unit tests for the two pure parsers (connected addresses out of `accounts_list` text; connect URL out of `accounts_connect` text) and a test that the migration loop skips already-connected accounts and calls `accounts_connect` only for the missing ones (drive `run_migration(local_accounts, connected, &mut FakeRpc)` with a trait `McpCalls { async fn call_tool(..) }` so no network is needed).
- [ ] **Step 2–4: implement, verify.** **Step 5: Commit** — `git commit -am "feat(cli): pidge mcp connect migrates local accounts to the hosted server"`

---

### Task 3: `pidge mcp status` and `pidge mcp logout`

**Files:**
- Create: `crates/pidge/src/commands/mcp_status.rs`, `crates/pidge/src/commands/mcp_logout.rs`

**Behaviour:** `status [url]` (url optional when exactly one server is stored; list stored servers otherwise): prints server, token expiry, then the live `accounts_list` text (refreshing the token if needed; on refusal prints "session expired — run `pidge mcp connect <url>`" and exits 3). `logout <url>` deletes the stored tokens. `--json` variants.

- [ ] **Step 1: Failing test** for the "which server" resolution (none, one, many stored). **Step 2–4.** **Step 5: Commit** — `git commit -am "feat(cli): pidge mcp status and logout"`

---

### Task 4: Publish the server image on release

**Files:**
- Modify: `.github/workflows/release.yml` (new job `image`, `needs: ci`, `permissions: { contents: read, packages: write }`, `docker/setup-buildx-action@v3`, `docker/login-action@v3` to `ghcr.io` with `GITHUB_TOKEN`, `docker/metadata-action@v5` producing `ghcr.io/mklab-se/pidge-mcp:${{ github.ref_name }}` (strip the leading `v`) and `latest`, `docker/build-push-action@v6` with `file: deploy/azure/Dockerfile`, `platforms: linux/amd64`, `push: true`, cache `type=gha`)
- Modify: `deploy/azure/README.md` and `docs/mcp.md` (Task 5) to reference the image

- [ ] **Step 1: Write the job**; `actionlint` if available. **Step 2: Commit** — `git commit -am "ci: publish ghcr.io/mklab-se/pidge-mcp on release"`

---

### Task 5: `docs/mcp.md` and README

**Files:**
- Create: `docs/mcp.md`
- Modify: `README.md` (a "Remote MCP server" section linking to `docs/mcp.md`), `CLAUDE.md` (one line for `pidge mcp`)

**Contents of `docs/mcp.md`:** what the server is and the security model in five bullets; connecting from Claude.ai/Cowork/mobile (custom connector, sign in now, register automatically), ChatGPT (chatgpt.com/plugins → + → Create MCP App → public endpoint), Claude Code (`claude mcp add --transport http pidge <url>/mcp`); the tools and prompts (one line each, taken from `deploy/azure/README.md`); migrating from local pidge (`pidge mcp connect`); self-hosting on Azure (the deploy README, the Entra app, the two owner actions: redirect URIs, allowlist), running the container anywhere (`docker run ghcr.io/mklab-se/pidge-mcp:latest` with `PIDGE_MCP_PUBLIC_URL`, `PIDGE_MCP_ALLOWED_EMAILS`, `PIDGE_MCP_SECRETS_DIR` on a persistent volume, behind TLS), and operations (logs, `--convert-check`, `sign_out_everywhere`).

- [ ] **Step 1: Write**; verify every command shown exists (`cargo run -- mcp --help`). **Step 2: Commit** — `git commit -am "docs: remote MCP guide"`

---

## Self-review notes

- Part 3 bullets → Tasks 5 (docs), 4 (image), 2–3 (CLI); §1.5 migration → Task 2.
- Review Focus 1 → Task 1 listener timeout; 2 → Task 1/3; 3 → Task 1; 4 → Task 2; 5 → Task 4.

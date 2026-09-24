# Remote MCP: Platform Implementation Plan (sub-project 2)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Serve the MCP at `https://pidge.mklab.se/mcp` with a managed certificate, emit privacy-safe structured logs, deploy automatically from GitHub via OIDC, and add the hardening the spec lists (purge protection, refresh-token revocation), without breaking the sessions users already have.

**Architecture:** The server learns to accept more than one host and more than one token issuer so the domain cutover can happen without invalidating existing sessions. Revocation rides on a per-user generation counter embedded in tokens and checked against a cached copy of the user record. Logging becomes a JSON line per HTTP request and per tool call, carrying only hashes and outcomes. Infrastructure changes stay in Bicep, with the two-step certificate dance and the GitHub OIDC identity scripted in `deploy/azure/`.

**Tech Stack:** Rust 2024 (MSRV 1.88), axum 0.8, tower-http trace, tracing-subscriber `json` feature, rmcp 3.4 (`ServerHandler::call_tool` override), Bicep (`Microsoft.App/managedEnvironments/managedCertificates`), Azure CLI, GitHub Actions with `azure/login@v2` OIDC.

**Spec:** `docs/superpowers/specs/2026-09-22-remote-mcp-full-feature-design.md`: Part 2 (§2.1–§2.4)

## Global Constraints

- Logs never contain addresses, subjects, bodies, recipient lists or tokens; users appear as the existing 8-hex `user_hash` (spec §2.2).
- Existing sessions must survive the domain cutover: tokens issued under the old issuer stay valid until they expire (spec §2.1 "old hostname keeps working until the new one is verified").
- Identity remains the bearer token; a revoked generation must reject both access and refresh tokens (spec §2.4).
- DNS: only the `pidge` subdomain records exist at GoDaddy (already created); apex and `www` are never touched.
- The Entra app's redirect URI for the new callback (`https://pidge.mklab.se/callback`) must be registered by Kristofer; the deploy script prints the exact command when it is missing and never calls `az ad app update` itself.
- `deploy.sh` stays idempotent; every new Azure resource is declared in `main.bicep`, tagged like the rest.
- `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all -- --check`, `cargo test --workspace` stay clean; CI runs on ubuntu-latest.
- Commit trailer lines: `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>` and `Claude-Session: https://claude.ai/code/session_01Tj3dUYia6V1FHGhQHGMNkB`.

## Review Focus

1. A bearer token minted under the old hostname arrives after cutover. Expected: accepted until its own expiry (Task 1 test `legacy_issuer_tokens_stay_valid`).
2. A refresh token from before a `sign_out_everywhere` arrives. Expected: refused with `invalid_grant`, and an access token from before it is refused with 401 (Task 2 tests).
3. A request with `Host: pidge.mklab.se` and one with the Container Apps FQDN both reach the MCP endpoint during the transition. Expected: both served (Task 1 test `alternate_hosts_are_accepted`).
4. A tool call that fails must still produce exactly one structured log line with `outcome=error` and no message text from Graph (Task 3 test `tool_log_line_has_no_error_text`).
5. The deploy workflow runs twice concurrently. Expected: the second waits (concurrency group) and the smoke test fails the job if `/healthz` or discovery is wrong (Task 5, verified by the controller on the first real run).

---

### Task 1: Alternate hosts and legacy issuers

**Files:**
- Modify: `crates/pidge-mcp/src/config.rs` (`Config` gains `alt_hosts: Vec<String>`, `legacy_issuers: Vec<String>`)
- Modify: `crates/pidge-mcp/src/oauth/jwt.rs` (`Signer::new` takes legacy issuers; `verify_access` accepts them)
- Modify: `crates/pidge-mcp/src/app.rs` (`allowed_hosts` appends `alt_hosts`)
- Modify: `crates/pidge-mcp/src/main.rs` (pass new config through)
- Test: unit tests in `jwt.rs`, `config.rs`; flow test in `oauth/flow_tests.rs`

**Interfaces:**
- Produces: `Config.alt_hosts` from `PIDGE_MCP_ALT_HOSTS` (comma-separated `host[:port]`, default empty); `Config.legacy_issuers` from `PIDGE_MCP_LEGACY_ISSUERS` (comma-separated origins without trailing slash, default empty).
- Produces: `Signer::new(key, issuer, audience)` unchanged; new `Signer::with_legacy_issuers(self, issuers: Vec<String>) -> Self`; `verify_access` accepts `iss ∈ {issuer} ∪ legacy_issuers` and `aud ∈ {audience} ∪ {legacy + "/mcp"}`.

- [ ] **Step 1: Failing tests**

`jwt.rs` tests:
```rust
#[test]
fn legacy_issuer_tokens_stay_valid() {
    let key = random_bytes(32);
    let old = Signer::new(&key, "https://old.test", "https://old.test/mcp");
    let new = Signer::new(&key, "https://new.test", "https://new.test/mcp")
        .with_legacy_issuers(vec!["https://old.test".into()]);
    let token = old.issue_access("jane@example.com", "mail").unwrap();
    assert!(new.verify_access(&token).is_ok(), "old-issuer token accepted during transition");
    let strict = Signer::new(&key, "https://new.test", "https://new.test/mcp");
    assert!(strict.verify_access(&token).is_err(), "without the legacy list it is refused");
}
```
`config.rs` test: with `PIDGE_MCP_ALT_HOSTS="ca-x.example.io, pidge.mklab.se:443"` parsed into two trimmed entries; `PIDGE_MCP_LEGACY_ISSUERS="https://old.test/"` parsed with the trailing slash removed. (Build `Config` through a `Config::from_map(&HashMap<String,String>)` helper that `from_env` calls, so tests don't touch the process environment.)
`flow_tests.rs` test `alternate_hosts_are_accepted`: build the harness with `alt_hosts = vec!["alt.test".into()]`, send `initialize` with `Host: alt.test` and a valid bearer → 200; with `Host: evil.test` → 4xx (rmcp's host check).

- [ ] **Step 2: Run to verify failure**: `cargo test -p pidge-mcp legacy_issuer alternate_hosts from_map`.

- [ ] **Step 3: Implement**

`config.rs`: add fields; `from_map` parses `PIDGE_MCP_ALT_HOSTS` and `PIDGE_MCP_LEGACY_ISSUERS` (split on `,`, trim, drop empties, `trim_end_matches('/')` for issuers; refuse an issuer that is not an absolute http(s) URL).
`jwt.rs`:
```rust
pub struct Signer { encoding, decoding, issuer: String, audience: String, legacy_issuers: Vec<String> }
pub fn with_legacy_issuers(mut self, issuers: Vec<String>) -> Self { self.legacy_issuers = issuers; self }
pub fn verify_access(&self, token: &str) -> Result<AccessClaims> {
    let claims: AccessClaims = self.verify(token, "access", true)?;
    let issuer_ok = claims.iss == self.issuer || self.legacy_issuers.iter().any(|i| *i == claims.iss);
    let audience_ok = claims.aud == self.audience || self.legacy_issuers.iter().any(|i| format!("{i}/mcp") == claims.aud);
    if !issuer_ok { return Err(anyhow!("issuer mismatch")); }
    if !audience_ok { return Err(anyhow!("audience mismatch")); }
    Ok(claims)
}
```
`main.rs`: `Signer::new(..).with_legacy_issuers(config.legacy_issuers.clone())`.
`app.rs::allowed_hosts`: `hosts.extend(state.config.alt_hosts.iter().cloned())`.

- [ ] **Step 4: Run tests, clippy, fmt**: `cargo test -p pidge-mcp && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all`.
- [ ] **Step 5: Commit**: `git commit -am "feat(mcp): alternate hosts and legacy issuers for the domain cutover"`

---

### Task 2: Refresh-token revocation via token generation

**Files:**
- Modify: `crates/pidge-mcp/src/oauth/jwt.rs` (`AccessClaims.gen: u32`, `RefreshClaims.gen: u32`, both `#[serde(default)]`; `issue_access(sub, scope, gen)`, `issue_refresh(sub, client_id, gen)`)
- Modify: `crates/pidge-mcp/src/state.rs` (`generations: Mutex<HashMap<String, u32>>` cache + `pub async fn generation_for(&self, signin) -> u32` loading through `UserStore` on miss; `pub fn set_generation(&self, signin, gen)`)
- Modify: `crates/pidge-mcp/src/oauth/mod.rs` (token endpoint reads the generation for both grants and issues tokens with it; refresh grant refuses `claims.gen != current` with `invalid_grant`)
- Modify: `crates/pidge-mcp/src/oauth/bearer.rs` (after verifying, `if claims.gen != state.generation_for(&claims.sub).await → 401 invalid_token`)
- Modify: `crates/pidge-mcp/src/tools/accounts.rs` (`accounts_update { sign_out_everywhere?: bool }` bumps `record.token_generation`, saves, `set_generation`, invalidates cache; result says "All sessions signed out; clients must sign in again")
- Modify: `crates/pidge-mcp/src/users.rs` (nothing new; `token_generation` already exists)
- Test: `oauth/flow_tests.rs`, `tools/accounts.rs` tests

**Interfaces:**
- Consumes: `UserRecord.token_generation: u32` (exists), `UserStore::load/save`.
- Produces: `AppState::generation_for(&self, signin: &str) -> u32` (0 when no record), `AppState::set_generation`.

- [ ] **Step 1: Failing tests**

`flow_tests.rs`:
```rust
#[tokio::test]
async fn sign_out_everywhere_revokes_access_and_refresh_tokens() {
    // sign in, redeem code → (access, refresh); assert initialize with access is 200
    // call accounts_update { sign_out_everywhere: true } through the tool harness (or bump the record directly + state.set_generation)
    // assert initialize with the old access → 401; POST /token refresh_token grant with the old refresh → 400 invalid_grant
    // sign in again → new tokens work
}
```
`accounts.rs` test: `sign_out_everywhere` increments `token_generation` in the stored record and invalidates the user's read cache.

- [ ] **Step 2: Run to verify failure.**
- [ ] **Step 3: Implement** per the file list. The bearer middleware's generation lookup must be cheap: `generation_for` checks the in-memory map first and only loads the record on a miss (one Key Vault read per user per process lifetime). `set_generation` updates the map.
- [ ] **Step 4: Run tests, clippy, fmt.**
- [ ] **Step 5: Commit**: `git commit -am "feat(mcp): sign_out_everywhere revokes all of a user's tokens"`

---

### Task 3: Structured JSON logs with request and tool lines

**Files:**
- Modify: `Cargo.toml` (workspace `tracing-subscriber` gains features `["env-filter", "json"]`)
- Modify: `crates/pidge-mcp/src/main.rs` (JSON output when `PIDGE_MCP_LOG_FORMAT=json` or when stdout is not a TTY; text otherwise)
- Modify: `crates/pidge-mcp/src/app.rs` (`TraceLayer` `on_response` emits one `info` event `http_request` with `method`, `route` (matched path, `/dl/<redacted>` for downloads), `status`, `latency_ms`)
- Modify: `crates/pidge-mcp/src/tools/mod.rs` (override `call_tool` in the `ServerHandler` impl: time the inner router call, emit one `info` event `tool_call` with `tool`, `user` (hash from the request parts, `"-"` if absent), `duration_ms`, `outcome` (`ok`|`error`|`tool_error`); never the arguments or the result text)
- Modify: `crates/pidge-mcp/src/config.rs` (`log_format: LogFormat { Json, Text }` from `PIDGE_MCP_LOG_FORMAT`, default `Json`)
- Test: `tools/mod.rs` test capturing tracing output for a tool call (use a `tracing_subscriber::fmt` writer into a shared `Vec<u8>` as the existing log-capture tests in the crate do)

**Interfaces:**
- Produces: log event names `http_request` and `tool_call` with the field names above (documented in the README by Task 6).

- [ ] **Step 1: Failing test** `tool_log_line_has_no_error_text`: mount Graph to return 400 with body "secret detail" for `mail_overview`; call the tool through `PidgeMcp::call_tool` (the `ServerHandler` method) with a `request_context`; assert exactly one captured line contains `tool_call` and `tool=mail_overview` (or the JSON equivalent) and `outcome=error`, and that "secret detail" and the address are absent.
- [ ] **Step 2: Run to verify failure.**
- [ ] **Step 3: Implement.** In `tools/mod.rs` the `#[tool_handler]` macro generates `call_tool`; to wrap it, keep the macro on a private inner type or call `self.tool_router.call(...)` yourself: replace `#[tool_handler(router = self.tool_router)]` with a hand-written `call_tool`/`list_tools` (`ToolRouter::call`, `ToolRouter::list_all`, `ToolRouter::get` are public in rmcp 3.4; see how the macro expands in `rmcp-macros-3.4.0/src/tool_handler.rs`) so the timing wrapper sits around `self.tool_router.call(tcc).await`.
- [ ] **Step 4: Run tests, clippy, fmt.**
- [ ] **Step 5: Commit**: `git commit -am "feat(mcp): JSON logs with per-request and per-tool lines"`

---

### Task 4: Custom domain, managed certificate, purge protection (Bicep + deploy script)

**Files:**
- Modify: `deploy/azure/main.bicep`
- Modify: `deploy/azure/deploy.sh`
- Modify: `deploy/azure/README.md` (domain section)

**Interfaces:**
- Bicep params: `customDomain string = ''`, `customDomainCertificateId string = ''`, `publicUrl string = ''` (empty → derived from the environment default domain), `altHosts string = ''`, `legacyIssuers string = ''`.
- Bicep resources: `resource cert 'Microsoft.App/managedEnvironments/managedCertificates@2024-03-01' = if (!empty(customDomain) && empty(customDomainCertificateId))` is NOT used (a managed certificate can only be created after the hostname is bound); instead the script does the certificate step with the CLI and passes the resulting id back as `customDomainCertificateId`.
- App ingress: `customDomains: empty(customDomain) ? [] : [{ name: customDomain, bindingType: empty(customDomainCertificateId) ? 'Disabled' : 'SniEnabled', certificateId: empty(customDomainCertificateId) ? null : customDomainCertificateId }]`.
- Env vars added to the container: `PIDGE_MCP_ALT_HOSTS = altHosts`, `PIDGE_MCP_LEGACY_ISSUERS = legacyIssuers`, `PIDGE_MCP_LOG_FORMAT = 'json'`.
- Key Vault: `enablePurgeProtection: true` (irreversible; the README says so).

- [ ] **Step 1: Bicep changes** as above; `az bicep build --file deploy/azure/main.bicep --stdout >/dev/null` passes.
- [ ] **Step 2: deploy.sh phases**
  - Inputs: `PIDGE_MCP_CUSTOM_DOMAIN` (e.g. `pidge.mklab.se`, optional), `PIDGE_MCP_CUTOVER` (`1` to make the custom domain the public URL; default keeps the Container Apps FQDN as public URL and lists the custom domain in `altHosts`).
  - Phase 3a: deploy the app with `customDomain` bound `Disabled` (when set).
  - Phase 3b: `az containerapp env certificate create -g $RG --name cae-pidge --certificate-name pidge-mcp-managed --hostname $DOMAIN --validation-method CNAME` if no managed certificate with that name exists yet; then poll `az containerapp env certificate list ... --query "[?name=='pidge-mcp-managed'].properties.provisioningState"` until `Succeeded` (timeout 15 min).
  - Phase 3c: deploy again with `customDomainCertificateId=<cert id>` → `SniEnabled`.
  - `publicUrl`: with cutover, `https://$DOMAIN`, `legacyIssuers` = the old `https://<fqdn>`; without cutover, `altHosts` = `$DOMAIN`.
  - Phase 4 prints the `az ad app update` command including BOTH callbacks (old FQDN and custom domain) when the custom domain's callback is missing; never runs it.
- [ ] **Step 3: Dry-run**: `deploy/azure/deploy.sh --skip-build --skip-entra` with `PIDGE_MCP_CUSTOM_DOMAIN=pidge.mklab.se` and no cutover, from the controller's session (the implementer does not deploy); the implementer validates the script with `bash -n` and `shellcheck` if available.
- [ ] **Step 4: Commit**: `git commit -am "feat(deploy): custom domain with managed certificate, purge protection, cutover switch"`

---

### Task 5: CI/CD via GitHub OIDC

**Files:**
- Create: `.github/workflows/deploy-mcp.yml`
- Create: `deploy/azure/setup-github-oidc.sh`
- Modify: `deploy/azure/README.md` (CI/CD section)

**Interfaces:**
- Azure: user-assigned identity `id-pidge-deploy` (Bicep resource, tagged), federated credential `github-main` with issuer `https://token.actions.githubusercontent.com`, subject `repo:mklab-se/pidge:ref:refs/heads/main`, audience `api://AzureADTokenExchange`; role assignments on the resource group: `Contributor` (b24988ac-6180-42a0-ab88-20f7382dd24c) and `Role Based Access Control Administrator` (f58310d9-a9f6-439a-9e8d-f62e7b41a168, with a condition limiting assignable roles to AcrPull and Key Vault Secrets Officer is optional; plain assignment acceptable for this RG).
- GitHub: repository variables `AZURE_CLIENT_ID`, `AZURE_TENANT_ID`, `AZURE_SUBSCRIPTION_ID`; secret `PIDGE_MCP_ALLOWED_EMAILS`; optional variable `PIDGE_MCP_CUSTOM_DOMAIN`.
- Workflow: `on: push: branches: [main], paths: [crates/pidge-mcp/**, crates/pidge-client/**, crates/pidge-core/**, deploy/azure/**, Cargo.toml, Cargo.lock, .github/workflows/deploy-mcp.yml]` + `workflow_dispatch`; `permissions: id-token: write, contents: read`; `concurrency: { group: deploy-mcp, cancel-in-progress: false }`; steps: checkout, `azure/login@v2` with the three variables, install `jq` (present on ubuntu-latest), run `deploy/azure/deploy.sh --skip-entra` with `IMAGE_TAG=${{ github.sha }}` and the env, then smoke test: `curl -fsS $URL/healthz`, discovery JSON has `issuer`, `POST /mcp` without bearer returns 401.
- `setup-github-oidc.sh`: idempotent; creates the identity via Bicep param? Simpler: `az identity create`, `az identity federated-credential create`, `az role assignment create` ×2, then `gh variable set` ×3 and `gh secret set PIDGE_MCP_ALLOWED_EMAILS` from the environment (never echoing it).

- [ ] **Step 1: Write the workflow and script**; `bash -n`; `actionlint` if available.
- [ ] **Step 2: Commit**: `git commit -am "ci: deploy pidge-mcp from main via GitHub OIDC"`
- [ ] **Step 3 (controller):** run `setup-github-oidc.sh`, merge, observe the workflow run on main, confirm the new revision and the smoke test.

---

### Task 6: Docs for the platform

**Files:**
- Modify: `deploy/azure/README.md` (domain, cutover, logs schema, CI/CD, revocation, purge protection)
- Modify: `CLAUDE.md` (one line: deploy workflow + env vars)

- [ ] **Step 1: Write**; **Step 2: Commit**: `git commit -am "docs: platform (domain, logs, CI/CD, revocation)"`

---

## Self-review notes

- §2.1 domain → Task 4 (+ Task 1 for hosts/issuers); §2.2 logs → Task 3; §2.3 CI/CD → Task 5; §2.4 purge protection → Task 4, revocation → Task 2, public client stays (no task).
- Review Focus 1 → Task 1, 2 → Task 2, 3 → Task 1, 4 → Task 3, 5 → Task 5 (controller-verified).
- Controller-only steps: running `setup-github-oidc.sh`, the certificate dance, the cutover, and the Entra redirect URI hand-off.

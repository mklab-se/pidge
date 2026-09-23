//! Shared session-lookup and RPC plumbing for every `pidge mcp` subcommand.
//!
//! `connect`, `status`, and `logout` all need to find a stored session for
//! a server (trying both storage backends, tolerating one being unusable),
//! and `connect`/`status` both need to keep it refreshed across a run. This
//! module holds that shared machinery so it has exactly one implementation
//! instead of three near-identical ones; `mcp_connect` layers its own
//! sign-in/migration/dry-run logic on top, and `mcp_status`/`mcp_logout`
//! use it directly.

use anyhow::Result;

use pidge_client::ClientError;
use pidge_client::mcp::{
    McpRpc, McpTokenStore, McpTokens, StoredServer, ToolResult, normalize_origin,
    valid_access_token,
};
use pidge_core::TokenStorage;

/// Minimal tool-calling surface the migration/dry-run/status logic needs.
/// Implemented for the real [`McpRpc`] (bails on `ToolResult::is_error` via
/// [`check_tool_result`]) and for [`RefreshingRpc`], and for fakes in tests
/// — so that logic can be exercised with no network call.
pub trait McpCalls {
    async fn call_tool(&mut self, name: &str, arguments: serde_json::Value) -> Result<ToolResult>;
}

impl McpCalls for McpRpc {
    async fn call_tool(&mut self, name: &str, arguments: serde_json::Value) -> Result<ToolResult> {
        let result = McpRpc::call_tool(self, name, arguments).await?;
        check_tool_result(name, result)
    }
}

/// Turn a tool-level failure (`ToolResult::is_error`) into an error instead
/// of letting its text be misparsed as if it were a normal list/link. The
/// server reports failures as JSON-RPC errors today, so this only guards
/// against a tool that starts using `isError` for that in the future.
/// Factored out (pure, no network) so it's directly unit-testable.
fn check_tool_result(name: &str, result: ToolResult) -> Result<ToolResult> {
    if result.is_error {
        Err(anyhow::anyhow!("{name} reported an error: {}", result.text))
    } else {
        Ok(result)
    }
}

/// The backends to try, in order, when looking for a stored session:
/// `preferred` first, then the other one as a fallback.
pub(crate) fn candidate_backends(preferred: TokenStorage) -> [TokenStorage; 2] {
    match preferred {
        TokenStorage::Keychain => [TokenStorage::Keychain, TokenStorage::File],
        TokenStorage::File => [TokenStorage::File, TokenStorage::Keychain],
    }
}

pub(crate) fn backend_name(store: TokenStorage) -> &'static str {
    match store {
        TokenStorage::Keychain => "keychain",
        TokenStorage::File => "file",
    }
}

/// Try `load` against each of `candidates` in order, returning the first
/// hit (`Ok(Some(_))`). A miss (`Ok(None)`) moves on to the next candidate.
/// An error only propagates when it comes from the *first* (preferred)
/// candidate; a load error from a later, fallback candidate is treated as a
/// miss instead (with a stderr warning) rather than failing the whole
/// lookup — `--store=file` exists precisely for machines where the
/// keychain/Secret Service isn't usable, and a fallback probe of it must
/// not turn into a hard dependency on it. Generic and synchronous so it's
/// directly unit-testable with no real keychain or network.
pub(crate) fn find_first_hit<T, E: std::fmt::Display>(
    candidates: &[TokenStorage],
    mut load: impl FnMut(TokenStorage) -> Result<Option<T>, E>,
) -> Result<Option<(T, TokenStorage)>, E> {
    for (index, &backend) in candidates.iter().enumerate() {
        match load(backend) {
            Ok(Some(value)) => return Ok(Some((value, backend))),
            Ok(None) => continue,
            Err(e) if index == 0 => return Err(e),
            Err(e) => {
                eprintln!(
                    "warning: could not check the {} backend for a stored session: {e}; trying the next one",
                    backend_name(backend)
                );
                continue;
            }
        }
    }
    Ok(None)
}

/// Run `f` against every one of `candidates`, collecting every result. An
/// error from the *preferred* (first) candidate is fatal; an error from any
/// other candidate is a warning (and that backend is skipped), mirroring
/// [`find_first_hit`]'s tolerance. Unlike `find_first_hit`, this doesn't
/// stop at the first hit — used by `logout`, which needs to know about
/// *every* backend holding tokens for a server, not just the first.
pub(crate) fn try_each_backend<T, E: std::fmt::Display>(
    candidates: &[TokenStorage],
    mut f: impl FnMut(TokenStorage) -> Result<T, E>,
) -> Result<Vec<(TokenStorage, T)>, E> {
    let mut out = Vec::new();
    for (index, &backend) in candidates.iter().enumerate() {
        match f(backend) {
            Ok(value) => out.push((backend, value)),
            Err(e) if index == 0 => return Err(e),
            Err(e) => {
                eprintln!(
                    "warning: could not reach the {} backend: {e}; skipping it",
                    backend_name(backend)
                );
            }
        }
    }
    Ok(out)
}

/// Find a stored session for `url` with no network call and no refresh —
/// just the raw local/keychain lookup, tried in [`candidate_backends`]
/// order. Used directly by `--dry-run` (which must never refresh) and as
/// the first step of [`lookup_session`].
pub(crate) fn find_stored_tokens(
    url: &str,
    preferred: TokenStorage,
) -> Result<Option<(McpTokens, TokenStorage)>> {
    Ok(find_first_hit(&candidate_backends(preferred), |backend| {
        McpTokenStore::load(url, backend)
    })?)
}

/// The backend recorded for `url` in `servers`, if any, else the OS
/// keychain. Used as the default `--store` preference for an explicit-url
/// `status`/`logout`, so they try the backend actually known to hold the
/// session first instead of always defaulting to the keychain and hard
/// -erroring on a machine with no usable one. Pure over an already-loaded
/// index (rather than calling [`McpTokenStore::list`] itself) so: (a) this
/// decision is directly unit-testable with no I/O, and (b) callers that
/// already have the list in hand (or don't need it — an explicit `--store`
/// skips this entirely) don't force a redundant read.
pub(crate) fn preferred_backend_for(url: &str, servers: &[StoredServer]) -> TokenStorage {
    let Ok(origin) = normalize_origin(url) else {
        return TokenStorage::Keychain;
    };
    servers
        .iter()
        .find(|s| s.server == origin)
        .map(|s| s.storage)
        .unwrap_or(TokenStorage::Keychain)
}

/// The outcome of resolving a stored session for one server, with no
/// interactive sign-in ever triggered: present and refreshed, present but
/// irrecoverably expired, or nothing stored in any consulted backend.
pub(crate) enum SessionLookup {
    Found(McpTokens, TokenStorage),
    Expired,
    Absent,
}

/// Look up and, if needed, refresh the stored session for `url`. Shared by
/// `connect` (deciding whether an interactive sign-in is needed) and
/// `status` (which reports `Expired`/`Absent` as their own typed errors
/// instead of falling back to sign-in).
pub(crate) async fn lookup_session(
    http: &reqwest::Client,
    url: &str,
    preferred: TokenStorage,
) -> Result<SessionLookup> {
    let Some((mut tokens, backend)) = find_stored_tokens(url, preferred)? else {
        return Ok(SessionLookup::Absent);
    };
    match refresh_if_needed(http, &mut tokens, backend).await {
        Ok(()) => Ok(SessionLookup::Found(tokens, backend)),
        Err(ClientError::SessionExpired { .. }) => Ok(SessionLookup::Expired),
        Err(e) => Err(e.into()),
    }
}

/// Refresh `tokens` in place if [`McpTokens::needs_refresh`] says it's due,
/// persisting the new tokens through `store` only when the access token
/// actually changed — an unconditional save on every call would mean a
/// keychain write (and on some platforms an access prompt) even when
/// nothing changed. Shared by [`lookup_session`] and [`RefreshingRpc`].
pub(crate) async fn refresh_if_needed(
    http: &reqwest::Client,
    tokens: &mut McpTokens,
    store: TokenStorage,
) -> Result<(), ClientError> {
    let server = tokens.server.clone();
    let previous_access_token = tokens.access_token.clone();
    let access_token = valid_access_token(http, &server, tokens).await?;
    if access_token != previous_access_token {
        McpTokenStore::save(tokens, store)?;
    }
    Ok(())
}

/// Wraps [`McpRpc`] so every [`McpCalls::call_tool`] first makes sure the
/// access token has more than [`McpTokens::needs_refresh`]'s margin left —
/// refreshing (and persisting the refresh through `store`) as needed. An
/// interactive migration can spend up to ten minutes per mailbox waiting on
/// the user, comfortably long enough to run past a token minted at the
/// start of the run, so this check happens immediately before every single
/// call rather than once at the start. A `401` that gets through anyway
/// (e.g. the server revoked the session between calls) is remapped to
/// [`ClientError::SessionExpired`] so it surfaces through
/// `commands::mcp::remap_session_expired`'s `pidge mcp connect <url>` hint
/// instead of a bare Graph error.
pub(crate) struct RefreshingRpc {
    http: reqwest::Client,
    inner: McpRpc,
    tokens: McpTokens,
    store: TokenStorage,
}

impl RefreshingRpc {
    pub(crate) fn new(
        http: reqwest::Client,
        inner: McpRpc,
        tokens: McpTokens,
        store: TokenStorage,
    ) -> Self {
        Self {
            http,
            inner,
            tokens,
            store,
        }
    }

    /// Initialize the underlying MCP session, remapping a bare `401` (the
    /// locally-held token looked valid — it wasn't near its recorded
    /// expiry — but the server had already revoked it, e.g. a revoked
    /// session or a rotated signing key) to [`ClientError::SessionExpired`],
    /// the same way [`McpCalls::call_tool`] remaps a mid-session `401`.
    /// `initialize` runs once, up front, before any `call_tool`, so it
    /// needs this same remap rather than inheriting `call_tool`'s.
    pub(crate) async fn initialize(&mut self) -> Result<()> {
        self.inner
            .initialize()
            .await
            .map_err(|e| remap_401_to_session_expired(&self.tokens.server, e))
    }
}

/// Remap a bare `401` to [`ClientError::SessionExpired`]; every other error
/// passes through unchanged. Factored out of [`RefreshingRpc::initialize`]
/// (whose own error comes from a real network call) so this mapping is
/// directly unit-testable with no network involved. `pub(crate)` since
/// `connect`'s `--dry-run` path also calls a bare `McpRpc::initialize`
/// outside a `RefreshingRpc` (it must never refresh) and needs the same
/// remap (see task-3-rereview.md N2).
pub(crate) fn remap_401_to_session_expired(server: &str, err: ClientError) -> anyhow::Error {
    match err {
        ClientError::Graph { status: 401, .. } => ClientError::SessionExpired {
            email: server.to_string(),
        }
        .into(),
        other => other.into(),
    }
}

impl McpCalls for RefreshingRpc {
    async fn call_tool(&mut self, name: &str, arguments: serde_json::Value) -> Result<ToolResult> {
        let server = self.tokens.server.clone();
        refresh_if_needed(&self.http, &mut self.tokens, self.store).await?;
        // Cheap in-memory assignment regardless of whether a refresh just
        // happened — unlike the token-store write inside
        // `refresh_if_needed`, there's no reason to guard this one.
        self.inner
            .set_access_token(self.tokens.access_token.clone());
        match <McpRpc as McpCalls>::call_tool(&mut self.inner, name, arguments).await {
            Ok(result) => Ok(result),
            Err(e) if is_unauthorized(&e) => {
                Err(ClientError::SessionExpired { email: server }.into())
            }
            Err(e) => Err(e),
        }
    }
}

fn is_unauthorized(err: &anyhow::Error) -> bool {
    matches!(
        err.downcast_ref::<ClientError>(),
        Some(ClientError::Graph { status: 401, .. })
    )
}

/// True if `err`'s chain contains either flavor of "this hosted session is
/// dead, sign in again" — the client's own [`ClientError::SessionExpired`]
/// (a failed refresh grant) or the CLI-level
/// [`ClientError::McpSessionExpired`] a caller may already have remapped it
/// to. Used by `connect`'s poller to stop retrying immediately instead of
/// spending its whole deadline re-POSTing a revoked refresh token.
pub(crate) fn is_session_expired(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        matches!(
            cause.downcast_ref::<ClientError>(),
            Some(ClientError::SessionExpired { .. } | ClientError::McpSessionExpired { .. })
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- check_tool_result -------------------------------------------------

    #[test]
    fn check_tool_result_bails_on_is_error() {
        let result = ToolResult {
            text: "boom".into(),
            is_error: true,
        };
        let err = check_tool_result("accounts_list", result).unwrap_err();
        assert!(err.to_string().contains("boom"), "{err}");
    }

    #[test]
    fn check_tool_result_passes_through_ok_results() {
        let result = ToolResult {
            text: "fine".into(),
            is_error: false,
        };
        assert_eq!(
            check_tool_result("accounts_list", result).unwrap().text,
            "fine"
        );
    }

    // --- candidate_backends -----------------------------------------------

    #[test]
    fn candidate_backends_tries_the_preferred_backend_first() {
        assert_eq!(
            candidate_backends(TokenStorage::Keychain),
            [TokenStorage::Keychain, TokenStorage::File]
        );
        assert_eq!(
            candidate_backends(TokenStorage::File),
            [TokenStorage::File, TokenStorage::Keychain]
        );
    }

    // --- find_first_hit -----------------------------------------------

    #[test]
    fn find_first_hit_returns_the_first_hit_and_does_not_try_the_rest() {
        let candidates = [TokenStorage::Keychain, TokenStorage::File];
        let mut tried = Vec::new();
        let result = find_first_hit(&candidates, |backend| {
            tried.push(backend);
            Ok::<_, anyhow::Error>(if backend == TokenStorage::Keychain {
                Some(42)
            } else {
                None
            })
        })
        .unwrap();

        assert_eq!(result, Some((42, TokenStorage::Keychain)));
        assert_eq!(tried, vec![TokenStorage::Keychain]);
    }

    #[test]
    fn find_first_hit_finds_a_hit_in_the_fallback_backend() {
        let candidates = [TokenStorage::Keychain, TokenStorage::File];
        let result = find_first_hit(&candidates, |backend| {
            Ok::<_, anyhow::Error>(match backend {
                TokenStorage::Keychain => None,
                TokenStorage::File => Some("found"),
            })
        })
        .unwrap();

        assert_eq!(result, Some(("found", TokenStorage::File)));
    }

    #[test]
    fn find_first_hit_propagates_an_error_from_the_preferred_backend() {
        let candidates = [TokenStorage::Keychain, TokenStorage::File];
        let mut tried = Vec::new();
        let err = find_first_hit(&candidates, |backend| {
            tried.push(backend);
            Err::<Option<i32>, _>(anyhow::anyhow!("keychain unavailable"))
        })
        .unwrap_err();

        assert!(err.to_string().contains("keychain unavailable"));
        assert_eq!(
            tried,
            vec![TokenStorage::Keychain],
            "must not try the fallback after a preferred-backend error"
        );
    }

    #[test]
    fn find_first_hit_treats_a_fallback_backend_error_as_a_miss() {
        let candidates = [TokenStorage::Keychain, TokenStorage::File];
        let result = find_first_hit(&candidates, |backend| match backend {
            // miss on the preferred backend
            TokenStorage::Keychain => Ok::<Option<i32>, anyhow::Error>(None),
            TokenStorage::File => Err(anyhow::anyhow!("no secret service running")),
        })
        .unwrap();

        assert_eq!(
            result, None,
            "a fallback-backend error must be reported as an overall miss, not fail the lookup"
        );
    }

    // --- try_each_backend ---------------------------------------------

    #[test]
    fn try_each_backend_collects_every_candidates_result() {
        let candidates = [TokenStorage::Keychain, TokenStorage::File];
        let result = try_each_backend(&candidates, |backend| {
            Ok::<_, anyhow::Error>(backend == TokenStorage::File)
        })
        .unwrap();

        assert_eq!(
            result,
            vec![(TokenStorage::Keychain, false), (TokenStorage::File, true)]
        );
    }

    #[test]
    fn try_each_backend_propagates_an_error_from_the_preferred_backend() {
        let candidates = [TokenStorage::Keychain, TokenStorage::File];
        let err = try_each_backend(&candidates, |_| {
            Err::<bool, _>(anyhow::anyhow!("keychain unavailable"))
        })
        .unwrap_err();
        assert!(err.to_string().contains("keychain unavailable"));
    }

    #[test]
    fn try_each_backend_skips_a_failing_fallback_backend_instead_of_failing() {
        let candidates = [TokenStorage::Keychain, TokenStorage::File];
        let result = try_each_backend(&candidates, |backend| match backend {
            TokenStorage::Keychain => Ok::<bool, anyhow::Error>(true),
            TokenStorage::File => Err(anyhow::anyhow!("no secret service running")),
        })
        .unwrap();

        assert_eq!(result, vec![(TokenStorage::Keychain, true)]);
    }

    // --- preferred_backend_for (N1) -----------------------------------

    #[test]
    fn preferred_backend_for_uses_the_indexed_backend_when_present() {
        let servers = vec![StoredServer {
            server: "https://mcp.example.com".to_string(),
            storage: TokenStorage::File,
        }];
        assert_eq!(
            preferred_backend_for("https://mcp.example.com", &servers),
            TokenStorage::File
        );
    }

    #[test]
    fn preferred_backend_for_defaults_to_keychain_when_not_indexed() {
        assert_eq!(
            preferred_backend_for("https://mcp.example.com", &[]),
            TokenStorage::Keychain
        );
    }

    #[test]
    fn preferred_backend_for_matches_on_normalized_origin_not_the_exact_url() {
        // The index stores the bare origin; a caller passing the full
        // resource url (with a path, e.g. `/mcp`) must still match it.
        let servers = vec![StoredServer {
            server: "https://mcp.example.com".to_string(),
            storage: TokenStorage::File,
        }];
        assert_eq!(
            preferred_backend_for("https://mcp.example.com/mcp", &servers),
            TokenStorage::File
        );
    }

    // --- remap_401_to_session_expired (I2) ---------------------------------

    #[test]
    fn remap_401_to_session_expired_converts_a_bare_401() {
        let err = remap_401_to_session_expired(
            "https://mcp.example.com",
            ClientError::Graph {
                status: 401,
                message: "nope".into(),
            },
        );
        assert!(matches!(
            err.downcast_ref::<ClientError>(),
            Some(ClientError::SessionExpired { .. })
        ));
    }

    #[test]
    fn remap_401_to_session_expired_leaves_other_statuses_alone() {
        let err = remap_401_to_session_expired(
            "https://mcp.example.com",
            ClientError::Graph {
                status: 500,
                message: "boom".into(),
            },
        );
        assert!(matches!(
            err.downcast_ref::<ClientError>(),
            Some(ClientError::Graph { status: 500, .. })
        ));
    }

    // --- is_session_expired ---------------------------------------------

    #[test]
    fn is_session_expired_recognizes_both_error_flavors() {
        let a = anyhow::Error::from(ClientError::SessionExpired {
            email: "a@b.se".into(),
        });
        let b = anyhow::Error::from(ClientError::McpSessionExpired {
            server: "https://mcp.example.com".into(),
        });
        let c = anyhow::anyhow!("something unrelated");
        assert!(is_session_expired(&a));
        assert!(is_session_expired(&b));
        assert!(!is_session_expired(&c));
    }
}

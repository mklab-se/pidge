//! Compile-time constants and runtime overrides for the pidge OAuth app.

/// The pidge app's `client_id` in Microsoft Entra.
///
/// Empty string means "not yet provisioned". Set by `scripts/register-pidge-app.sh`
/// after registering the app in Entra. Until then, set the `PIDGE_CLIENT_ID` env var
/// for development.
pub const APP_CLIENT_ID: &str = "";

/// Microsoft Graph delegated scopes pidge requests at sign-in.
/// Locked in at app registration time — changing them later requires updating
/// the Entra app permissions AND triggering incremental consent on existing accounts.
pub const SCOPES: &[&str] = &[
    "offline_access",
    "User.Read",
    "Mail.ReadWrite",
    "Mail.Send",
    "Calendars.ReadWrite",
];

/// Microsoft identity platform endpoints (common = multi-tenant + personal MSA).
pub const AUTHORITY: &str = "https://login.microsoftonline.com/common";
pub const DEVICE_CODE_URL: &str = "https://login.microsoftonline.com/common/oauth2/v2.0/devicecode";
pub const TOKEN_URL: &str = "https://login.microsoftonline.com/common/oauth2/v2.0/token";

/// Microsoft Graph base URL.
pub const GRAPH_BASE: &str = "https://graph.microsoft.com/v1.0";

/// Resolved client_id: env var wins, otherwise the compile-time constant (if non-empty).
pub fn client_id() -> Option<String> {
    if let Ok(v) = std::env::var("PIDGE_CLIENT_ID") {
        if !v.is_empty() {
            return Some(v);
        }
    }
    if APP_CLIENT_ID.is_empty() {
        None
    } else {
        Some(APP_CLIENT_ID.to_string())
    }
}

/// The space-separated scope string sent to Microsoft.
pub fn scope_string() -> String {
    SCOPES.join(" ")
}

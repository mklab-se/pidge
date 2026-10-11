//! Compile-time constants and runtime overrides for the pidge OAuth app.

/// The pidge app's `client_id` in Microsoft Entra.
///
/// Empty string means "not yet provisioned". Set by `scripts/register-pidge-app.sh`
/// after registering the app in Entra. Until then, set the `PIDGE_CLIENT_ID` env var
/// for development.
pub const APP_CLIENT_ID: &str = "e49f90dc-c265-4392-b62f-b26704f9088f";

/// Microsoft Graph delegated scopes pidge requests at sign-in.
/// Locked in at app registration time; changing them later requires updating
/// the Entra app permissions AND triggering incremental consent on existing
/// accounts.
///
/// `openid` is included so Microsoft returns an `id_token` from the token
/// endpoint; we decode it (no signature check) to extract the user's tenant
/// for the Account record. `profile` is harmless and is what MSAL clients
/// always request alongside `openid`.
pub const SCOPES: &[&str] = &[
    "openid",
    "profile",
    "offline_access",
    "User.Read",
    "Mail.ReadWrite",
    "Mail.Send",
    "Calendars.ReadWrite",
    "MailboxSettings.ReadWrite",
];

/// Scopes added to [`SCOPES`] after accounts were already signed in.
///
/// A refresh may only ask for scopes the account consented to: Microsoft
/// refuses a refresh that asks for more (`invalid_grant`, consent required),
/// which pidge reads as an expired session, signing the account out. So a
/// refresh asks for one of these only when the token's own grant names it
/// (see [`refresh_scope_string`]); only an interactive sign-in asks for them
/// outright, which is where the user can consent.
pub const LATER_SCOPES: &[&str] = &["MailboxSettings.ReadWrite"];

/// Microsoft identity platform endpoints (common = multi-tenant + personal MSA).
pub const AUTHORITY: &str = "https://login.microsoftonline.com/common";
pub const DEVICE_CODE_URL: &str = "https://login.microsoftonline.com/common/oauth2/v2.0/devicecode";
pub const TOKEN_URL: &str = "https://login.microsoftonline.com/common/oauth2/v2.0/token";

/// Microsoft Graph base URL.
pub const GRAPH_BASE: &str = "https://graph.microsoft.com/v1.0";

/// Resolved client_id: env var wins, otherwise the compile-time constant (if non-empty).
pub fn client_id() -> Option<String> {
    if let Ok(v) = std::env::var("PIDGE_CLIENT_ID")
        && !v.is_empty()
    {
        return Some(v);
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

/// Whether a granted scope string (as Microsoft returns it: space-separated,
/// with or without the `https://graph.microsoft.com/` prefix) includes `scope`.
pub fn grants(granted: &str, scope: &str) -> bool {
    granted.split_whitespace().any(|g| {
        g.rsplit('/')
            .next()
            .unwrap_or(g)
            .eq_ignore_ascii_case(scope)
    })
}

/// The scope string to refresh a token with: `requested` (the sign-in
/// scopes) without any of [`LATER_SCOPES`] the token's grant lacks. A token
/// stored before grants were recorded (`granted` is `None`) predates every
/// later scope, so it is refreshed with the original set.
pub fn refresh_scope_string(requested: &str, granted: Option<&str>) -> String {
    requested
        .split_whitespace()
        .filter(|s| {
            !LATER_SCOPES.iter().any(|l| l.eq_ignore_ascii_case(s))
                || granted.is_some_and(|g| grants(g, s))
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grants_reads_microsofts_scope_spellings() {
        let g = "openid https://graph.microsoft.com/Mail.ReadWrite https://graph.microsoft.com/MailboxSettings.ReadWrite";
        assert!(grants(g, "MailboxSettings.ReadWrite"));
        assert!(grants(
            "mailboxsettings.readwrite",
            "MailboxSettings.ReadWrite"
        ));
        assert!(!grants(g, "Calendars.ReadWrite"));
    }

    #[test]
    fn a_refresh_never_asks_for_a_later_scope_the_token_was_not_granted() {
        let all = scope_string();
        assert!(all.contains("MailboxSettings.ReadWrite"));
        // A token from before the scope existed: refreshed with the old set.
        let old = refresh_scope_string(&all, None);
        assert!(!old.contains("MailboxSettings"), "{old}");
        assert!(old.contains("Mail.ReadWrite") && old.contains("offline_access"));
        let old = refresh_scope_string(&all, Some("User.Read Mail.ReadWrite Mail.Send"));
        assert!(!old.contains("MailboxSettings"), "{old}");
        // Granted at a later sign-in: kept.
        let new = refresh_scope_string(
            &all,
            Some("https://graph.microsoft.com/MailboxSettings.ReadWrite"),
        );
        assert_eq!(new, all);
    }
}

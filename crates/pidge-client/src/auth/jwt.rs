//! Minimal JWT decoder — extracts the `tid` (tenant_id) and `oid` (object id)
//! claims from id_tokens.
//!
//! We don't verify the signature: we trust the token because we just received it
//! over TLS from `login.microsoftonline.com`. The decoder is base64url-without-padding,
//! which is what Microsoft uses.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::Deserialize;

#[derive(Deserialize)]
struct Claims {
    #[serde(default)]
    tid: Option<String>,
}

/// The immutable identity of a Microsoft account, from its ID token: the
/// tenant (`tid`) and the account's object id within it (`oid`). Unlike the
/// profile's `mail` or `userPrincipalName`, these can't be edited to look
/// like another account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdTokenClaims {
    pub tid: String,
    pub oid: String,
}

#[derive(Deserialize)]
struct IdentityClaims {
    #[serde(default)]
    tid: Option<String>,
    #[serde(default)]
    oid: Option<String>,
}

/// Extract `tid` and `oid` from an ID token. `None` if the JWT is malformed
/// or either claim is missing or empty. The signature is not verified (see
/// the module docs): only use this on a token just received from Microsoft's
/// token endpoint over TLS.
pub fn extract_id_claims(jwt: &str) -> Option<IdTokenClaims> {
    let mid = jwt.split('.').nth(1)?;
    let bytes = URL_SAFE_NO_PAD.decode(mid).ok()?;
    let claims: IdentityClaims = serde_json::from_slice(&bytes).ok()?;
    match (claims.tid, claims.oid) {
        (Some(tid), Some(oid)) if !tid.is_empty() && !oid.is_empty() => {
            Some(IdTokenClaims { tid, oid })
        }
        _ => None,
    }
}

/// Extract the `tid` claim from a JWT. Returns `None` if the JWT is malformed
/// or if `tid` is missing.
pub fn extract_tenant_id(jwt: &str) -> Option<String> {
    let mid = jwt.split('.').nth(1)?;
    let bytes = URL_SAFE_NO_PAD.decode(mid).ok()?;
    let claims: Claims = serde_json::from_slice(&bytes).ok()?;
    claims.tid
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_jwt(payload_json: &str) -> String {
        let header = URL_SAFE_NO_PAD.encode(r#"{"alg":"RS256","typ":"JWT"}"#);
        let payload = URL_SAFE_NO_PAD.encode(payload_json);
        let signature = URL_SAFE_NO_PAD.encode("dummy-signature");
        format!("{header}.{payload}.{signature}")
    }

    #[test]
    fn extracts_tid_from_valid_jwt() {
        let jwt = make_jwt(
            r#"{"tid":"11111111-2222-3333-4444-555555555555","iss":"https://login.microsoftonline.com/.."}"#,
        );
        assert_eq!(
            extract_tenant_id(&jwt),
            Some("11111111-2222-3333-4444-555555555555".to_string())
        );
    }

    #[test]
    fn returns_none_when_tid_is_missing() {
        let jwt = make_jwt(r#"{"iss":"https://login.microsoftonline.com/.."}"#);
        assert_eq!(extract_tenant_id(&jwt), None);
    }

    #[test]
    fn extracts_tid_and_oid_together() {
        let jwt = make_jwt(r#"{"tid":"t-1","oid":"o-1","email":"x@example.com"}"#);
        assert_eq!(
            extract_id_claims(&jwt),
            Some(IdTokenClaims {
                tid: "t-1".into(),
                oid: "o-1".into()
            })
        );
    }

    #[test]
    fn id_claims_need_both_tid_and_oid() {
        assert_eq!(extract_id_claims(&make_jwt(r#"{"tid":"t-1"}"#)), None);
        assert_eq!(extract_id_claims(&make_jwt(r#"{"oid":"o-1"}"#)), None);
        assert_eq!(
            extract_id_claims(&make_jwt(r#"{"tid":"","oid":"o-1"}"#)),
            None
        );
        assert_eq!(extract_id_claims("not-a-jwt"), None);
    }

    #[test]
    fn returns_none_for_malformed_jwt() {
        assert_eq!(extract_tenant_id("not-a-jwt"), None);
    }

    #[test]
    fn returns_none_for_non_base64_middle_segment() {
        assert_eq!(
            extract_tenant_id("a.this-isn't-base64-because-of-?-char.b"),
            None
        );
    }
}

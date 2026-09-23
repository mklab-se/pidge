//! Every piece of OAuth state the server hands out is a signed, self-describing
//! token, so the server needs no database: registered clients, authorization
//! codes, access tokens and refresh tokens are all HS256 JWTs over one secret
//! key. The `typ` claim keeps the kinds from being swapped for one another.

use anyhow::{Context, Result, anyhow};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{Duration, Utc};
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation};
use rand::{Rng, rng};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::users::user_hash_long;

pub const ACCESS_TOKEN_TTL: Duration = Duration::hours(1);
pub const REFRESH_TOKEN_TTL: Duration = Duration::days(30);
pub const AUTH_CODE_TTL: Duration = Duration::minutes(2);
pub const DOWNLOAD_TTL: Duration = Duration::minutes(15);

#[derive(Debug, Serialize, Deserialize)]
pub struct ClientClaims {
    pub typ: String,
    pub iat: i64,
    pub redirect_uris: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_name: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CodeClaims {
    pub typ: String,
    pub jti: String,
    pub exp: i64,
    /// The signed-in user's e-mail (lower-cased). Doubles as the mailbox id.
    pub sub: String,
    pub client_id: String,
    pub redirect_uri: String,
    pub code_challenge: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AccessClaims {
    pub typ: String,
    pub jti: String,
    pub iss: String,
    pub aud: String,
    pub sub: String,
    pub exp: i64,
    pub iat: i64,
    pub scope: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RefreshClaims {
    pub typ: String,
    pub jti: String,
    pub sub: String,
    pub client_id: String,
    pub exp: i64,
}

/// A `mail_attachment` download link: anyone holding it may fetch this one
/// attachment until `exp`, as long as the user still owns the mailbox. The
/// payload is readable by whoever sees the URL, so it names the user and
/// mailbox only by [`user_hash_long`]; `/dl` resolves them against the allowlist
/// and the user's record. The content type rides along so serving it needs
/// no extra Graph call.
#[derive(Debug, Serialize, Deserialize)]
pub struct DownloadClaims {
    pub typ: String,
    pub jti: String,
    pub exp: i64,
    /// [`user_hash_long`] of the signed-in user the link was minted for.
    pub uh: String,
    /// [`user_hash_long`] of the mailbox holding the message.
    pub mh: String,
    pub message_id: String,
    pub attachment_id: String,
    pub filename: String,
    pub content_type: String,
}

#[derive(Clone)]
pub struct Signer {
    encoding: EncodingKey,
    decoding: DecodingKey,
    issuer: String,
    audience: String,
}

impl Signer {
    /// `key` is the raw secret; `issuer` is the public origin and `audience`
    /// the protected resource URL (both baked into access tokens).
    pub fn new(key: &[u8], issuer: impl Into<String>, audience: impl Into<String>) -> Self {
        Self {
            encoding: EncodingKey::from_secret(key),
            decoding: DecodingKey::from_secret(key),
            issuer: issuer.into(),
            audience: audience.into(),
        }
    }

    /// A fresh 256-bit key, base64url-encoded for storage in a secret store.
    pub fn generate_key() -> String {
        URL_SAFE_NO_PAD.encode(random_bytes(32))
    }

    pub fn decode_key(stored: &str) -> Result<Vec<u8>> {
        let bytes = URL_SAFE_NO_PAD
            .decode(stored.trim())
            .context("signing key is not base64url")?;
        if bytes.len() < 32 {
            return Err(anyhow!("signing key is shorter than 256 bits"));
        }
        Ok(bytes)
    }

    fn sign<T: Serialize>(&self, claims: &T) -> Result<String> {
        Ok(jsonwebtoken::encode(
            &Header::new(Algorithm::HS256),
            claims,
            &self.encoding,
        )?)
    }

    fn verify<T: DeserializeOwned>(
        &self,
        token: &str,
        expect_typ: &str,
        with_exp: bool,
    ) -> Result<T> {
        let mut validation = Validation::new(Algorithm::HS256);
        validation.validate_aud = false;
        validation.validate_exp = with_exp;
        validation.required_spec_claims.clear();
        if with_exp {
            validation.required_spec_claims.insert("exp".into());
        }
        let data = jsonwebtoken::decode::<serde_json::Value>(token, &self.decoding, &validation)?;
        let typ = data
            .claims
            .get("typ")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        if typ != expect_typ {
            return Err(anyhow!(
                "token type mismatch: expected {expect_typ}, got {typ:?}"
            ));
        }
        Ok(serde_json::from_value(data.claims)?)
    }

    pub fn issue_client(
        &self,
        redirect_uris: Vec<String>,
        client_name: Option<String>,
    ) -> Result<String> {
        self.sign(&ClientClaims {
            typ: "client".into(),
            iat: Utc::now().timestamp(),
            redirect_uris,
            client_name,
        })
    }

    pub fn verify_client(&self, client_id: &str) -> Result<ClientClaims> {
        self.verify(client_id, "client", false)
    }

    pub fn issue_code(
        &self,
        sub: &str,
        client_id: &str,
        redirect_uri: &str,
        code_challenge: &str,
    ) -> Result<String> {
        self.sign(&CodeClaims {
            typ: "code".into(),
            jti: random_id(),
            exp: (Utc::now() + AUTH_CODE_TTL).timestamp(),
            sub: sub.into(),
            client_id: client_id.into(),
            redirect_uri: redirect_uri.into(),
            code_challenge: code_challenge.into(),
        })
    }

    pub fn verify_code(&self, code: &str) -> Result<CodeClaims> {
        self.verify(code, "code", true)
    }

    pub fn issue_access(&self, sub: &str, scope: &str) -> Result<String> {
        let now = Utc::now();
        self.sign(&AccessClaims {
            typ: "access".into(),
            jti: random_id(),
            iss: self.issuer.clone(),
            aud: self.audience.clone(),
            sub: sub.into(),
            iat: now.timestamp(),
            exp: (now + ACCESS_TOKEN_TTL).timestamp(),
            scope: scope.into(),
        })
    }

    pub fn verify_access(&self, token: &str) -> Result<AccessClaims> {
        let claims: AccessClaims = self.verify(token, "access", true)?;
        if claims.iss != self.issuer {
            return Err(anyhow!("issuer mismatch"));
        }
        if claims.aud != self.audience {
            return Err(anyhow!("audience mismatch"));
        }
        Ok(claims)
    }

    pub fn issue_refresh(&self, sub: &str, client_id: &str) -> Result<String> {
        self.sign(&RefreshClaims {
            typ: "refresh".into(),
            jti: random_id(),
            sub: sub.into(),
            client_id: client_id.into(),
            exp: (Utc::now() + REFRESH_TOKEN_TTL).timestamp(),
        })
    }

    pub fn verify_refresh(&self, token: &str) -> Result<RefreshClaims> {
        self.verify(token, "refresh", true)
    }

    /// A [`DOWNLOAD_TTL`] link to `attachment` of `message_id` in `account`.
    pub fn issue_download(
        &self,
        sub: &str,
        account: &str,
        message_id: &str,
        attachment: &pidge_core::Attachment,
    ) -> Result<String> {
        self.issue_download_with_ttl(sub, account, message_id, attachment, DOWNLOAD_TTL)
    }

    pub(crate) fn issue_download_with_ttl(
        &self,
        sub: &str,
        account: &str,
        message_id: &str,
        attachment: &pidge_core::Attachment,
        ttl: Duration,
    ) -> Result<String> {
        self.sign(&DownloadClaims {
            typ: "download".into(),
            jti: random_id(),
            exp: (Utc::now() + ttl).timestamp(),
            uh: user_hash_long(sub),
            mh: user_hash_long(account),
            message_id: message_id.into(),
            attachment_id: attachment.id.clone(),
            filename: attachment.name.clone(),
            content_type: attachment.content_type.clone(),
        })
    }

    pub fn verify_download(&self, token: &str) -> Result<DownloadClaims> {
        self.verify(token, "download", true)
    }
}

pub fn random_bytes(n: usize) -> Vec<u8> {
    let mut buf = vec![0u8; n];
    rng().fill_bytes(&mut buf);
    buf
}

/// 128 bits of randomness, base64url. Used for `jti`, `state`, PKCE verifiers.
pub fn random_id() -> String {
    URL_SAFE_NO_PAD.encode(random_bytes(16))
}

/// RFC 7636 `S256` challenge for a verifier.
pub fn pkce_challenge(verifier: &str) -> String {
    use sha2::{Digest, Sha256};
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signer() -> Signer {
        Signer::new(
            &random_bytes(32),
            "https://issuer.test",
            "https://issuer.test/mcp",
        )
    }

    #[test]
    fn client_round_trip() {
        let s = signer();
        let id = s
            .issue_client(vec!["https://app.test/cb".into()], Some("App".into()))
            .unwrap();
        let claims = s.verify_client(&id).unwrap();
        assert_eq!(claims.redirect_uris, vec!["https://app.test/cb"]);
        assert_eq!(claims.client_name.as_deref(), Some("App"));
    }

    #[test]
    fn token_kinds_are_not_interchangeable() {
        let s = signer();
        let access = s.issue_access("jane@example.com", "mail").unwrap();
        assert!(s.verify_code(&access).is_err());
        assert!(s.verify_refresh(&access).is_err());
        assert!(s.verify_client(&access).is_err());
        let refresh = s.issue_refresh("jane@example.com", "cid").unwrap();
        assert!(s.verify_access(&refresh).is_err());
    }

    #[test]
    fn access_token_binds_issuer_and_audience() {
        let a = signer();
        let key = random_bytes(32);
        let b = Signer::new(&key, "https://issuer.test", "https://issuer.test/mcp");
        let c = Signer::new(&key, "https://issuer.test", "https://other.test/mcp");
        let token = b.issue_access("jane@example.com", "mail").unwrap();
        assert!(b.verify_access(&token).is_ok());
        assert!(a.verify_access(&token).is_err(), "different key");
        assert!(c.verify_access(&token).is_err(), "different audience");
    }

    fn attachment() -> pidge_core::Attachment {
        pidge_core::Attachment {
            id: "A1".into(),
            name: "report.pdf".into(),
            content_type: "application/pdf".into(),
            size_bytes: 10,
            is_inline: false,
            content_id: None,
        }
    }

    #[test]
    fn download_round_trip_carries_the_attachment_and_a_15_minute_expiry() {
        let s = signer();
        let token = s
            .issue_download("jane@example.com", "work@example.com", "M1", &attachment())
            .unwrap();
        let c = s.verify_download(&token).unwrap();
        assert_eq!(c.typ, "download");
        assert_eq!(c.uh, crate::users::user_hash_long("jane@example.com"));
        assert_eq!(c.mh, crate::users::user_hash_long("work@example.com"));
        assert_eq!(c.uh.len(), 16, "64-bit hash");
        assert_eq!(c.message_id, "M1");
        assert_eq!(c.attachment_id, "A1");
        assert_eq!(c.filename, "report.pdf");
        assert_eq!(c.content_type, "application/pdf");
        let ttl = c.exp - Utc::now().timestamp();
        assert!((14 * 60..=15 * 60).contains(&ttl), "{ttl}");
    }

    #[test]
    fn download_tokens_carry_no_address() {
        let s = signer();
        let token = s
            .issue_download("jane@example.com", "work@example.com", "M1", &attachment())
            .unwrap();
        let payload = token.split('.').nth(1).unwrap();
        let json = String::from_utf8(URL_SAFE_NO_PAD.decode(payload).unwrap()).unwrap();
        for part in ["jane", "work", "example", "@"] {
            assert!(!json.contains(part), "payload leaks {part}: {json}");
        }
    }

    #[test]
    fn download_tokens_are_their_own_kind_and_expire() {
        let s = signer();
        let access = s.issue_access("jane@example.com", "mail").unwrap();
        assert!(s.verify_download(&access).is_err());
        let download = s
            .issue_download("jane@example.com", "jane@example.com", "M1", &attachment())
            .unwrap();
        assert!(s.verify_access(&download).is_err());
        assert!(s.verify_refresh(&download).is_err());
        let expired = s
            .issue_download_with_ttl(
                "jane@example.com",
                "jane@example.com",
                "M1",
                &attachment(),
                Duration::minutes(-5),
            )
            .unwrap();
        assert!(s.verify_download(&expired).is_err());
    }

    #[test]
    fn pkce_matches_rfc_7636_example() {
        assert_eq!(
            pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn key_round_trips_through_storage_encoding() {
        let stored = Signer::generate_key();
        let bytes = Signer::decode_key(&stored).unwrap();
        assert_eq!(bytes.len(), 32);
        assert!(Signer::decode_key("dG9vc2hvcnQ").is_err());
    }
}

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ring::hmac;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    env,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};
use thiserror::Error;

const DEFAULT_ISSUER: &str = "vibequest-web";
const DEFAULT_AUDIENCE: &str = "vibequest-core";
const CLOCK_SKEW_SECONDS: i64 = 5;
const MAX_ASSERTION_TTL_SECONDS: i64 = 120;

#[derive(Clone, Debug)]
pub struct AuthVerifier {
    issuer: String,
    audience: String,
    keys: BTreeMap<String, Vec<u8>>,
    identity_key: Vec<u8>,
    replay_cache: Arc<Mutex<BTreeMap<String, i64>>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthenticatedPrincipal {
    pub user_id: String,
    pub provider: String,
    pub provider_subject: String,
    pub email: Option<String>,
    pub name: Option<String>,
    pub assertion_id: String,
}

#[derive(Clone, Debug, Deserialize)]
struct AssertionHeader {
    alg: String,
    typ: String,
    kid: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct AssertionClaims {
    iss: String,
    aud: String,
    sub: String,
    provider: String,
    provider_sub: String,
    email: Option<String>,
    name: Option<String>,
    iat: i64,
    exp: i64,
    jti: String,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum AuthError {
    #[error("authentication is not configured")]
    NotConfigured,
    #[error("assertion is malformed")]
    Malformed,
    #[error("assertion header is invalid")]
    InvalidHeader,
    #[error("assertion key is unknown")]
    UnknownKey,
    #[error("assertion signature is invalid")]
    InvalidSignature,
    #[error("assertion issuer is invalid")]
    InvalidIssuer,
    #[error("assertion audience is invalid")]
    InvalidAudience,
    #[error("assertion provider is invalid")]
    InvalidProvider,
    #[error("assertion subject is invalid")]
    InvalidSubject,
    #[error("assertion issue time is invalid")]
    InvalidIssuedAt,
    #[error("assertion expiry is invalid")]
    InvalidExpiry,
    #[error("assertion lifetime is invalid")]
    InvalidLifetime,
    #[error("assertion has already been used")]
    Replayed,
    #[error("assertion replay cache is unavailable")]
    ReplayCacheUnavailable,
    #[error("assertion replay identifier is invalid")]
    InvalidAssertionId,
    #[error("assertion key configuration is invalid")]
    InvalidKeyConfiguration,
}

impl AuthVerifier {
    pub fn from_env() -> Result<Self, AuthError> {
        let raw_keys = env::var("CORE_ASSERTION_KEYS").unwrap_or_default();
        let raw_identity_key = env::var("IDENTITY_DERIVATION_SECRET").unwrap_or_default();
        if raw_keys.trim().is_empty() && raw_identity_key.trim().is_empty() {
            return Ok(Self::disabled());
        }

        Self::from_key_config(
            raw_keys,
            raw_identity_key,
            env::var("CORE_ASSERTION_ISSUER").unwrap_or_else(|_| DEFAULT_ISSUER.to_string()),
            env::var("CORE_ASSERTION_AUDIENCE").unwrap_or_else(|_| DEFAULT_AUDIENCE.to_string()),
        )
    }

    pub fn disabled() -> Self {
        Self {
            issuer: DEFAULT_ISSUER.to_string(),
            audience: DEFAULT_AUDIENCE.to_string(),
            keys: BTreeMap::new(),
            identity_key: Vec::new(),
            replay_cache: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }

    pub fn from_key_config(
        raw_keys: String,
        raw_identity_key: String,
        issuer: String,
        audience: String,
    ) -> Result<Self, AuthError> {
        let mut keys = BTreeMap::new();

        for entry in raw_keys
            .split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            let (kid, encoded) = entry
                .split_once(':')
                .ok_or(AuthError::InvalidKeyConfiguration)?;
            if kid.is_empty()
                || kid.len() > 64
                || !kid
                    .bytes()
                    .all(|value| value.is_ascii_alphanumeric() || b"._-".contains(&value))
            {
                return Err(AuthError::InvalidKeyConfiguration);
            }
            let bytes = URL_SAFE_NO_PAD
                .decode(encoded)
                .map_err(|_| AuthError::InvalidKeyConfiguration)?;
            if bytes.len() < 32 || keys.insert(kid.to_string(), bytes).is_some() {
                return Err(AuthError::InvalidKeyConfiguration);
            }
        }

        let identity_key = URL_SAFE_NO_PAD
            .decode(raw_identity_key)
            .map_err(|_| AuthError::InvalidKeyConfiguration)?;
        if identity_key.len() < 32 {
            return Err(AuthError::InvalidKeyConfiguration);
        }

        if issuer.trim().is_empty() || audience.trim().is_empty() {
            return Err(AuthError::InvalidKeyConfiguration);
        }

        Ok(Self {
            issuer,
            audience,
            keys,
            identity_key,
            replay_cache: Arc::new(Mutex::new(BTreeMap::new())),
        })
    }

    fn consume_assertion_id(
        &self,
        assertion_id: &str,
        expires_at: i64,
        now: i64,
    ) -> Result<(), AuthError> {
        const MAX_REPLAY_ENTRIES: usize = 100_000;

        let mut cache = self
            .replay_cache
            .lock()
            .map_err(|_| AuthError::ReplayCacheUnavailable)?;
        cache.retain(|_, expiry| *expiry > now - CLOCK_SKEW_SECONDS);
        if cache.contains_key(assertion_id) {
            return Err(AuthError::Replayed);
        }
        if cache.len() >= MAX_REPLAY_ENTRIES {
            return Err(AuthError::ReplayCacheUnavailable);
        }
        cache.insert(assertion_id.to_string(), expires_at);

        Ok(())
    }

    pub fn configured(&self) -> bool {
        !self.keys.is_empty()
    }

    pub fn verify_now(&self, assertion: &str) -> Result<AuthenticatedPrincipal, AuthError> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| AuthError::InvalidIssuedAt)?
            .as_secs() as i64;

        self.verify(assertion, now)
    }

    pub fn verify(&self, assertion: &str, now: i64) -> Result<AuthenticatedPrincipal, AuthError> {
        if !self.configured() {
            return Err(AuthError::NotConfigured);
        }

        let mut parts = assertion.split('.');
        let header_part = parts.next().ok_or(AuthError::Malformed)?;
        let payload_part = parts.next().ok_or(AuthError::Malformed)?;
        let signature_part = parts.next().ok_or(AuthError::Malformed)?;
        if parts.next().is_some() || assertion.len() > 8_192 {
            return Err(AuthError::Malformed);
        }

        let header: AssertionHeader = decode_json(header_part)?;
        if header.alg != "HS256" || header.typ != "JWT" {
            return Err(AuthError::InvalidHeader);
        }
        let key = self.keys.get(&header.kid).ok_or(AuthError::UnknownKey)?;
        let signature = URL_SAFE_NO_PAD
            .decode(signature_part)
            .map_err(|_| AuthError::Malformed)?;
        let signing_input = format!("{header_part}.{payload_part}");
        let verification_key = hmac::Key::new(hmac::HMAC_SHA256, key);
        hmac::verify(&verification_key, signing_input.as_bytes(), &signature)
            .map_err(|_| AuthError::InvalidSignature)?;

        let claims: AssertionClaims = decode_json(payload_part)?;
        if claims.iss != self.issuer {
            return Err(AuthError::InvalidIssuer);
        }
        if claims.aud != self.audience {
            return Err(AuthError::InvalidAudience);
        }
        if claims.provider != "google" {
            return Err(AuthError::InvalidProvider);
        }
        if !valid_user_id(&claims.sub) || !valid_provider_subject(&claims.provider_sub) {
            return Err(AuthError::InvalidSubject);
        }
        let expected_subject = derive_opaque_user_id(&claims.provider_sub, &self.identity_key);
        if expected_subject != claims.sub {
            return Err(AuthError::InvalidSubject);
        }
        if claims.iat > now + CLOCK_SKEW_SECONDS || claims.iat < now - MAX_ASSERTION_TTL_SECONDS {
            return Err(AuthError::InvalidIssuedAt);
        }
        if claims.exp <= now - CLOCK_SKEW_SECONDS {
            return Err(AuthError::InvalidExpiry);
        }
        if claims.exp <= claims.iat || claims.exp - claims.iat > MAX_ASSERTION_TTL_SECONDS {
            return Err(AuthError::InvalidLifetime);
        }
        if claims.jti.is_empty() || claims.jti.len() > 128 {
            return Err(AuthError::InvalidAssertionId);
        }
        self.consume_assertion_id(&claims.jti, claims.exp, now)?;

        Ok(AuthenticatedPrincipal {
            user_id: claims.sub,
            provider: claims.provider,
            provider_subject: claims.provider_sub,
            email: claims.email.filter(|value| value.len() <= 320),
            name: claims.name.filter(|value| value.len() <= 200),
            assertion_id: claims.jti,
        })
    }
}

fn decode_json<T: for<'de> Deserialize<'de>>(encoded: &str) -> Result<T, AuthError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| AuthError::Malformed)?;
    serde_json::from_slice(&bytes).map_err(|_| AuthError::Malformed)
}

fn valid_user_id(value: &str) -> bool {
    value.len() == 36
        && value.starts_with("usr_")
        && value[4..]
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

fn valid_provider_subject(value: &str) -> bool {
    !value.is_empty() && value.len() <= 255 && value.bytes().all(|byte| byte.is_ascii_graphic())
}

fn derive_opaque_user_id(provider_subject: &str, identity_key: &[u8]) -> String {
    let key = hmac::Key::new(hmac::HMAC_SHA256, identity_key);
    let digest = hmac::sign(&key, format!("google:{provider_subject}").as_bytes());
    let encoded = URL_SAFE_NO_PAD.encode(digest.as_ref());

    format!("usr_{}", &encoded[..32])
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const KEY_ID: &str = "test-key";
    const KEY_BYTES: &[u8] = b"0123456789abcdef0123456789abcdef";

    fn verifier() -> AuthVerifier {
        AuthVerifier::from_key_config(
            format!("{KEY_ID}:{}", URL_SAFE_NO_PAD.encode(KEY_BYTES)),
            URL_SAFE_NO_PAD.encode(KEY_BYTES),
            DEFAULT_ISSUER.to_string(),
            DEFAULT_AUDIENCE.to_string(),
        )
        .expect("valid verifier")
    }

    fn assertion(overrides: serde_json::Value) -> String {
        let now = 1_800_000_000_i64;
        let mut claims = json!({
            "iss": DEFAULT_ISSUER,
            "aud": DEFAULT_AUDIENCE,
            "sub": derive_opaque_user_id("google-subject-01", KEY_BYTES),
            "provider": "google",
            "provider_sub": "google-subject-01",
            "email": "learner@example.com",
            "name": "Learner",
            "iat": now,
            "exp": now + 60,
            "jti": "assertion-01"
        });
        let source = overrides.as_object().expect("object overrides");
        for (key, value) in source {
            claims[key] = value.clone();
        }
        sign(claims)
    }

    fn sign(claims: serde_json::Value) -> String {
        let header = URL_SAFE_NO_PAD.encode(
            serde_json::to_vec(&json!({
                "alg": "HS256",
                "typ": "JWT",
                "kid": KEY_ID
            }))
            .expect("header"),
        );
        let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).expect("claims"));
        let input = format!("{header}.{payload}");
        let key = hmac::Key::new(hmac::HMAC_SHA256, KEY_BYTES);
        let signature = hmac::sign(&key, input.as_bytes());

        format!("{input}.{}", URL_SAFE_NO_PAD.encode(signature.as_ref()))
    }

    #[test]
    fn accepts_valid_google_assertion() {
        let principal = verifier()
            .verify(&assertion(json!({})), 1_800_000_010)
            .expect("valid assertion");

        assert_eq!(
            principal.user_id,
            derive_opaque_user_id("google-subject-01", KEY_BYTES)
        );
        assert_eq!(principal.provider_subject, "google-subject-01");
    }

    #[test]
    fn rejects_replayed_assertion_id() {
        let verifier = verifier();
        let token = assertion(json!({}));

        verifier
            .verify(&token, 1_800_000_010)
            .expect("first use is valid");
        assert_eq!(
            verifier.verify(&token, 1_800_000_011),
            Err(AuthError::Replayed)
        );
    }

    #[test]
    fn rejects_tampered_assertion() {
        let mut token = assertion(json!({}));
        token.push('x');

        assert_eq!(
            verifier().verify(&token, 1_800_000_010),
            Err(AuthError::InvalidSignature)
        );
    }

    #[test]
    fn rejects_subject_not_derived_from_provider_identity() {
        let token = assertion(json!({
            "sub": derive_opaque_user_id("another-google-subject", KEY_BYTES)
        }));

        assert_eq!(
            verifier().verify(&token, 1_800_000_010),
            Err(AuthError::InvalidSubject)
        );
    }

    #[test]
    fn rejects_expired_assertion() {
        let token = assertion(json!({ "exp": 1_799_999_900 }));

        assert_eq!(
            verifier().verify(&token, 1_800_000_010),
            Err(AuthError::InvalidExpiry)
        );
    }

    #[test]
    fn rejects_wrong_audience_and_issuer() {
        assert_eq!(
            verifier().verify(&assertion(json!({ "aud": "another-core" })), 1_800_000_010,),
            Err(AuthError::InvalidAudience)
        );
        assert_eq!(
            verifier().verify(&assertion(json!({ "iss": "another-web" })), 1_800_000_010,),
            Err(AuthError::InvalidIssuer)
        );
    }

    #[test]
    fn rejects_future_and_overlong_assertions() {
        assert_eq!(
            verifier().verify(
                &assertion(json!({ "iat": 1_800_000_100, "exp": 1_800_000_160 })),
                1_800_000_010,
            ),
            Err(AuthError::InvalidIssuedAt)
        );
        assert_eq!(
            verifier().verify(&assertion(json!({ "exp": 1_800_000_500 })), 1_800_000_010,),
            Err(AuthError::InvalidLifetime)
        );
    }
}

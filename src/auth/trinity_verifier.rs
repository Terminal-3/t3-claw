//! Verifier for Trinity-derived ID tokens (T3-TS-031).
//!
//! Trinity-issued tokens are compact JWS values signed with `ES256K`
//! (RFC 8812 secp256k1 ECDSA for JOSE). `jsonwebtoken` 9.3.1 does not
//! implement `ES256K`, so this module performs the verification by
//! hand against the `k256` crate.
//!
//! The wire shape mirrors `tee-contract-session::cryptography::ecdsa::
//! eth::jws_sign_secp256k1`:
//!
//! - Signing-input is `base64url(header) || "." || base64url(payload)`.
//! - The signed digest is `SHA-256` of the signing-input bytes.
//! - The signature is a raw `r || s` byte string (64 bytes,
//!   low-S normalised). No EIP-191 framing, no recovery byte.
//!
//! The JWKS published by Trinity is a single-key set with `kty = EC`,
//! `crv = secp256k1`, `kid = "tee-eoa-v1"`, and `(x, y)` reassembled
//! into a 65-byte uncompressed SEC1 public key. Discovery and JWKS
//! responses are cached for 5 minutes to match Trinity's
//! `Cache-Control: public, max-age=300`.
//!
//! Spec: `docs/specs/T3-TS-031-trinity-derived-tokens-for-t3-claw.md`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use k256::ecdsa::signature::hazmat::PrehashVerifier;
use k256::ecdsa::{Signature, VerifyingKey};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::sync::RwLock;

use crate::config::TrinityVerifierConfig;

/// JWKS / discovery cache TTL — matches Trinity's published
/// `Cache-Control: public, max-age=300`.
const CACHE_TTL: Duration = Duration::from_secs(300);

/// Errors the verifier returns. One variant per distinct failure
/// reason so callers can distinguish "this is a forged token" from
/// "I cannot reach the issuer right now".
#[derive(Debug, thiserror::Error)]
pub enum TrinityVerifyError {
    #[error("malformed JWS: {0}")]
    MalformedJws(&'static str),
    #[error("malformed header: {0}")]
    MalformedHeader(String),
    #[error("malformed payload: {0}")]
    MalformedPayload(String),
    #[error("base64 decode failed: {0}")]
    Base64(String),
    #[error("unsupported algorithm: {0}")]
    UnsupportedAlgorithm(String),
    #[error("missing `kid` in header")]
    MissingKid,
    #[error("unknown key id: {0}")]
    UnknownKid(String),
    #[error("invalid JWK: {0}")]
    InvalidJwk(String),
    #[error("signature verification failed")]
    InvalidSignature,
    #[error("invalid issuer: expected {expected}, got {actual}")]
    InvalidIssuer { expected: String, actual: String },
    #[error("invalid audience: expected {expected}, got {actual}")]
    InvalidAudience { expected: String, actual: String },
    #[error("token expired at {exp}, now {now}")]
    Expired { exp: i64, now: i64 },
    #[error("token not yet valid: nbf {nbf} > now {now}")]
    NotYetValid { nbf: i64, now: i64 },
    #[error("discovery fetch failed: {0}")]
    DiscoveryFetch(String),
    #[error("jwks fetch failed: {0}")]
    JwksFetch(String),
    #[error("user has no provisioned local identity for did {did}")]
    UserNotProvisioned { did: String },
}

/// Claims surfaced from a Trinity-issued ID token.
///
/// Field names follow the JWT / OIDC convention so RP-side code can
/// reason about them without translation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrinityClaims {
    pub iss: String,
    /// The user's Trinity DID (`did:t3n:…`).
    pub sub: String,
    pub aud: String,
    pub exp: i64,
    pub nbf: i64,
    pub iat: Option<i64>,
    pub jti: Option<String>,
    /// Authenticator type recorded at session creation. v1 emits the
    /// sentinel `"unknown"`; RP code MUST treat that as a valid
    /// placeholder and not gate any logic on a specific value.
    pub auth_method: Option<String>,
    pub org_dids: Vec<String>,
    pub roles: Vec<String>,
    pub session_ref: Option<String>,
}

#[derive(Debug, Deserialize)]
struct JwsHeader {
    alg: String,
    #[serde(default)]
    kid: Option<String>,
    #[serde(default, rename = "typ")]
    _typ: Option<String>,
}

#[derive(Debug, Deserialize)]
struct JwsPayload {
    iss: String,
    sub: String,
    aud: String,
    exp: i64,
    #[serde(default)]
    nbf: Option<i64>,
    #[serde(default)]
    iat: Option<i64>,
    #[serde(default)]
    jti: Option<String>,
    #[serde(default)]
    auth_method: Option<String>,
    #[serde(default)]
    org_dids: Vec<String>,
    #[serde(default)]
    roles: Vec<String>,
    #[serde(default)]
    session_ref: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DiscoveryDoc {
    #[serde(default)]
    jwks_uri: Option<String>,
}

#[derive(Debug, Deserialize)]
struct JwkSet {
    #[serde(default)]
    keys: Vec<Jwk>,
}

#[derive(Debug, Deserialize)]
struct Jwk {
    kty: String,
    #[serde(default)]
    crv: Option<String>,
    #[serde(default)]
    kid: Option<String>,
    #[serde(default)]
    alg: Option<String>,
    #[serde(default)]
    x: Option<String>,
    #[serde(default)]
    y: Option<String>,
}

#[derive(Clone)]
struct CachedKey {
    verifying_key: VerifyingKey,
}

#[derive(Clone)]
struct CachedDiscovery {
    jwks_uri: String,
    fetched_at: Instant,
}

/// Trinity ID-token verifier.
///
/// Holds the trust configuration, an HTTP client for discovery /
/// JWKS fetches, and a 5-minute cache of both. Cheap to clone.
#[derive(Clone)]
pub struct TrinityVerifier {
    cfg: TrinityVerifierConfig,
    http: reqwest::Client,
    discovery_cache: Arc<RwLock<Option<CachedDiscovery>>>,
    jwks_cache: Arc<RwLock<HashMap<String, CachedKey>>>,
    jwks_fetched_at: Arc<RwLock<Option<Instant>>>,
}

impl TrinityVerifier {
    pub fn new(cfg: TrinityVerifierConfig, http: reqwest::Client) -> Self {
        Self {
            cfg,
            http,
            discovery_cache: Arc::new(RwLock::new(None)),
            jwks_cache: Arc::new(RwLock::new(HashMap::new())),
            jwks_fetched_at: Arc::new(RwLock::new(None)),
        }
    }

    pub fn issuer(&self) -> &str {
        &self.cfg.issuer
    }

    pub fn audience(&self) -> &str {
        &self.cfg.audience
    }

    /// Seed the in-memory JWKS cache with `(kid, verifying_key)` and
    /// mark the cache as freshly populated. Test-only helper — the
    /// production path always loads keys via the discovery doc.
    #[cfg(test)]
    pub(crate) async fn seed_key(&self, kid: &str, verifying_key: VerifyingKey) {
        let mut cache = self.jwks_cache.write().await;
        cache.insert(kid.to_string(), CachedKey { verifying_key });
        drop(cache);
        *self.jwks_fetched_at.write().await = Some(Instant::now());
    }

    /// Verify a compact JWS produced by Trinity's `client-token`
    /// interface. On success returns the parsed claims; on failure a
    /// distinct error per reason.
    pub async fn verify(&self, raw_jws: &str) -> Result<TrinityClaims, TrinityVerifyError> {
        let now = current_unix_seconds();
        self.verify_at(raw_jws, now).await
    }

    /// Verify with a caller-supplied "now" — exposed so tests can pin
    /// the clock without monkey-patching `SystemTime`.
    pub async fn verify_at(
        &self,
        raw_jws: &str,
        now_secs: i64,
    ) -> Result<TrinityClaims, TrinityVerifyError> {
        let (header_b64, payload_b64, sig_b64) = split_jws(raw_jws)?;

        let header_bytes = b64url_decode(header_b64)?;
        let header: JwsHeader = serde_json::from_slice(&header_bytes)
            .map_err(|e| TrinityVerifyError::MalformedHeader(e.to_string()))?;

        if header.alg != "ES256K" {
            return Err(TrinityVerifyError::UnsupportedAlgorithm(header.alg));
        }
        let kid = header.kid.ok_or(TrinityVerifyError::MissingKid)?;

        let payload_bytes = b64url_decode(payload_b64)?;
        let payload: JwsPayload = serde_json::from_slice(&payload_bytes)
            .map_err(|e| TrinityVerifyError::MalformedPayload(e.to_string()))?;

        let signature_bytes = b64url_decode(sig_b64)?;
        if signature_bytes.len() != 64 {
            return Err(TrinityVerifyError::MalformedJws(
                "signature is not 64 bytes",
            ));
        }
        let signature = Signature::from_slice(&signature_bytes)
            .map_err(|_| TrinityVerifyError::InvalidSignature)?;

        // Signing input is the base64url-encoded header + "." + base64url-encoded
        // payload bytes — mirrors `jws_sign_secp256k1` in the contracts repo
        // (see `cryptography/src/ecdsa/eth.rs:20`).
        let mut signing_input = Vec::with_capacity(header_b64.len() + 1 + payload_b64.len());
        signing_input.extend_from_slice(header_b64.as_bytes());
        signing_input.push(b'.');
        signing_input.extend_from_slice(payload_b64.as_bytes());
        let digest: [u8; 32] = Sha256::digest(&signing_input).into();

        let verifying_key = self.get_verifying_key(&kid).await?;
        verifying_key
            .verify_prehash(&digest, &signature)
            .map_err(|_| TrinityVerifyError::InvalidSignature)?;

        if payload.iss != self.cfg.issuer {
            return Err(TrinityVerifyError::InvalidIssuer {
                expected: self.cfg.issuer.clone(),
                actual: payload.iss,
            });
        }
        if payload.aud != self.cfg.audience {
            return Err(TrinityVerifyError::InvalidAudience {
                expected: self.cfg.audience.clone(),
                actual: payload.aud,
            });
        }
        let nbf = payload.nbf.unwrap_or(payload.iat.unwrap_or(payload.exp));
        if now_secs < nbf {
            return Err(TrinityVerifyError::NotYetValid { nbf, now: now_secs });
        }
        if now_secs >= payload.exp {
            return Err(TrinityVerifyError::Expired {
                exp: payload.exp,
                now: now_secs,
            });
        }

        Ok(TrinityClaims {
            iss: payload.iss,
            sub: payload.sub,
            aud: payload.aud,
            exp: payload.exp,
            nbf,
            iat: payload.iat,
            jti: payload.jti,
            auth_method: payload.auth_method,
            org_dids: payload.org_dids,
            roles: payload.roles,
            session_ref: payload.session_ref,
        })
    }

    /// Returns the verifying key for `kid`. On cache miss or expiry
    /// the JWKS is refreshed via the discovery doc.
    async fn get_verifying_key(&self, kid: &str) -> Result<VerifyingKey, TrinityVerifyError> {
        if let Some(key) = self.cached_key_if_fresh(kid).await {
            return Ok(key);
        }
        self.refresh_jwks().await?;
        self.cached_key_if_fresh(kid)
            .await
            .ok_or_else(|| TrinityVerifyError::UnknownKid(kid.to_string()))
    }

    async fn cached_key_if_fresh(&self, kid: &str) -> Option<VerifyingKey> {
        let fetched_at = *self.jwks_fetched_at.read().await;
        let fresh = fetched_at.map(|t| t.elapsed() < CACHE_TTL).unwrap_or(false);
        if !fresh {
            return None;
        }
        let cache = self.jwks_cache.read().await;
        cache.get(kid).map(|c| c.verifying_key)
    }

    async fn refresh_jwks(&self) -> Result<(), TrinityVerifyError> {
        let jwks_uri = self.resolve_jwks_uri().await?;
        let response = self
            .http
            .get(&jwks_uri)
            .send()
            .await
            .map_err(|e| TrinityVerifyError::JwksFetch(e.to_string()))?;
        if !response.status().is_success() {
            return Err(TrinityVerifyError::JwksFetch(format!(
                "{} returned {}",
                jwks_uri,
                response.status()
            )));
        }
        let body: JwkSet = response
            .json()
            .await
            .map_err(|e| TrinityVerifyError::JwksFetch(e.to_string()))?;

        let mut next = HashMap::new();
        for jwk in body.keys {
            if jwk.kty != "EC" || jwk.crv.as_deref() != Some("secp256k1") {
                continue;
            }
            if let Some(alg) = jwk.alg.as_deref()
                && alg != "ES256K"
            {
                continue;
            }
            let Some(kid) = jwk.kid.clone() else {
                continue;
            };
            let key = jwk_to_verifying_key(&jwk)?;
            next.insert(kid, CachedKey { verifying_key: key });
        }
        *self.jwks_cache.write().await = next;
        *self.jwks_fetched_at.write().await = Some(Instant::now());
        Ok(())
    }

    async fn resolve_jwks_uri(&self) -> Result<String, TrinityVerifyError> {
        if let Some(cached) = self.discovery_cache.read().await.clone()
            && cached.fetched_at.elapsed() < CACHE_TTL
        {
            return Ok(cached.jwks_uri);
        }

        let response = self
            .http
            .get(&self.cfg.discovery_url)
            .send()
            .await
            .map_err(|e| TrinityVerifyError::DiscoveryFetch(e.to_string()))?;
        if !response.status().is_success() {
            return Err(TrinityVerifyError::DiscoveryFetch(format!(
                "{} returned {}",
                self.cfg.discovery_url,
                response.status()
            )));
        }
        let doc: DiscoveryDoc = response
            .json()
            .await
            .map_err(|e| TrinityVerifyError::DiscoveryFetch(e.to_string()))?;
        let jwks_uri = doc.jwks_uri.unwrap_or_else(|| {
            format!(
                "{}/.well-known/jwks.json",
                self.cfg.issuer.trim_end_matches('/')
            )
        });
        *self.discovery_cache.write().await = Some(CachedDiscovery {
            jwks_uri: jwks_uri.clone(),
            fetched_at: Instant::now(),
        });
        Ok(jwks_uri)
    }
}

fn split_jws(raw: &str) -> Result<(&str, &str, &str), TrinityVerifyError> {
    let mut parts = raw.split('.');
    let header = parts
        .next()
        .ok_or(TrinityVerifyError::MalformedJws("missing header"))?;
    let payload = parts
        .next()
        .ok_or(TrinityVerifyError::MalformedJws("missing payload"))?;
    let signature = parts
        .next()
        .ok_or(TrinityVerifyError::MalformedJws("missing signature"))?;
    if parts.next().is_some() {
        return Err(TrinityVerifyError::MalformedJws("unexpected extra segment"));
    }
    Ok((header, payload, signature))
}

fn b64url_decode(s: &str) -> Result<Vec<u8>, TrinityVerifyError> {
    URL_SAFE_NO_PAD
        .decode(s)
        .map_err(|e| TrinityVerifyError::Base64(e.to_string()))
}

/// Rebuild a verifying key from the `x` / `y` JWK fields. Trinity
/// publishes both coordinates as 32-byte base64url-no-pad strings; we
/// concatenate them under the SEC1 uncompressed prefix `0x04`.
fn jwk_to_verifying_key(jwk: &Jwk) -> Result<VerifyingKey, TrinityVerifyError> {
    let x_b64 = jwk
        .x
        .as_deref()
        .ok_or_else(|| TrinityVerifyError::InvalidJwk("missing x".to_string()))?;
    let y_b64 = jwk
        .y
        .as_deref()
        .ok_or_else(|| TrinityVerifyError::InvalidJwk("missing y".to_string()))?;
    let x = b64url_decode(x_b64)?;
    let y = b64url_decode(y_b64)?;
    if x.len() != 32 || y.len() != 32 {
        return Err(TrinityVerifyError::InvalidJwk(
            "x and y must be 32 bytes each".to_string(),
        ));
    }
    let mut sec1 = Vec::with_capacity(65);
    sec1.push(0x04);
    sec1.extend_from_slice(&x);
    sec1.extend_from_slice(&y);
    VerifyingKey::from_sec1_bytes(&sec1).map_err(|e| TrinityVerifyError::InvalidJwk(e.to_string()))
}

fn current_unix_seconds() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use k256::ecdsa::SigningKey;
    use k256::ecdsa::signature::hazmat::PrehashSigner;
    use rand::rngs::OsRng;
    use serde_json::json;

    /// Build a JWS signed by `signing_key` over the given claims with
    /// header `{ alg: ES256K, typ: JWT, kid }`.
    fn make_jws(signing_key: &SigningKey, kid: &str, claims: &serde_json::Value) -> String {
        let header = json!({ "alg": "ES256K", "typ": "JWT", "kid": kid });
        let header_b64 = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&header).unwrap());
        let payload_b64 = URL_SAFE_NO_PAD.encode(serde_json::to_vec(claims).unwrap());
        let signing_input = format!("{header_b64}.{payload_b64}");
        let digest: [u8; 32] = Sha256::digest(signing_input.as_bytes()).into();
        let signature: Signature = signing_key.sign_prehash(&digest).unwrap();
        let sig_b64 = URL_SAFE_NO_PAD.encode(signature.to_bytes());
        format!("{header_b64}.{payload_b64}.{sig_b64}")
    }

    fn jwk_for(verifying_key: &VerifyingKey, kid: &str) -> serde_json::Value {
        let point = verifying_key.to_encoded_point(false);
        let bytes = point.as_bytes();
        assert_eq!(bytes.len(), 65);
        let x_b64 = URL_SAFE_NO_PAD.encode(&bytes[1..33]);
        let y_b64 = URL_SAFE_NO_PAD.encode(&bytes[33..65]);
        json!({
            "kty": "EC",
            "crv": "secp256k1",
            "kid": kid,
            "alg": "ES256K",
            "use": "sig",
            "x": x_b64,
            "y": y_b64,
        })
    }

    fn seed_verifier(
        cfg: TrinityVerifierConfig,
        verifying_key: &VerifyingKey,
        kid: &str,
    ) -> TrinityVerifier {
        let verifier = TrinityVerifier::new(cfg, reqwest::Client::new());
        let key = CachedKey {
            verifying_key: *verifying_key,
        };
        let mut cache = HashMap::new();
        cache.insert(kid.to_string(), key);
        // Pre-seed both caches so verify_at never touches the network.
        *verifier.jwks_cache.try_write().unwrap() = cache;
        *verifier.jwks_fetched_at.try_write().unwrap() = Some(Instant::now());
        verifier
    }

    fn happy_cfg() -> TrinityVerifierConfig {
        TrinityVerifierConfig {
            issuer: "did:t3n:trinity-cluster-test".to_string(),
            audience: "claw-acme".to_string(),
            discovery_url: "https://example.invalid/.well-known/openid-configuration".to_string(),
        }
    }

    fn happy_claims() -> serde_json::Value {
        json!({
            "iss": "did:t3n:trinity-cluster-test",
            "aud": "claw-acme",
            "sub": "did:t3n:0000000000000000000000000000000000000000deadbeef",
            "iat": 1000,
            "nbf": 1000,
            "exp": 2000,
            "jti": "abc123",
            "auth_method": "unknown",
            "session_ref": "deadbeef",
            "roles": ["user"],
            "org_dids": ["did:t3n:acme"],
        })
    }

    #[tokio::test]
    async fn verify_happy_path() {
        let sk = SigningKey::random(&mut OsRng);
        let vk = *sk.verifying_key();
        let kid = "tee-eoa-v1";
        let jws = make_jws(&sk, kid, &happy_claims());
        let verifier = seed_verifier(happy_cfg(), &vk, kid);

        let claims = verifier.verify_at(&jws, 1500).await.expect("ok");
        assert_eq!(claims.iss, "did:t3n:trinity-cluster-test");
        assert_eq!(claims.aud, "claw-acme");
        assert_eq!(
            claims.sub,
            "did:t3n:0000000000000000000000000000000000000000deadbeef"
        );
        assert_eq!(claims.auth_method.as_deref(), Some("unknown"));
        assert_eq!(claims.org_dids, vec!["did:t3n:acme".to_string()]);
    }

    #[tokio::test]
    async fn verify_rejects_tampered_signature() {
        let sk = SigningKey::random(&mut OsRng);
        let vk = *sk.verifying_key();
        let kid = "tee-eoa-v1";
        let jws = make_jws(&sk, kid, &happy_claims());

        // Flip a bit in the signature segment.
        let parts: Vec<&str> = jws.split('.').collect();
        let mut sig_bytes = URL_SAFE_NO_PAD.decode(parts[2]).unwrap();
        sig_bytes[0] ^= 0x01;
        let bad_sig = URL_SAFE_NO_PAD.encode(&sig_bytes);
        let tampered = format!("{}.{}.{}", parts[0], parts[1], bad_sig);

        let verifier = seed_verifier(happy_cfg(), &vk, kid);
        let err = verifier
            .verify_at(&tampered, 1500)
            .await
            .expect_err("should fail");
        assert!(matches!(err, TrinityVerifyError::InvalidSignature));
    }

    #[tokio::test]
    async fn verify_rejects_wrong_issuer() {
        let sk = SigningKey::random(&mut OsRng);
        let vk = *sk.verifying_key();
        let kid = "tee-eoa-v1";
        let mut claims = happy_claims();
        claims["iss"] = json!("did:t3n:trinity-cluster-other");
        let jws = make_jws(&sk, kid, &claims);
        let verifier = seed_verifier(happy_cfg(), &vk, kid);

        let err = verifier
            .verify_at(&jws, 1500)
            .await
            .expect_err("should fail");
        assert!(matches!(err, TrinityVerifyError::InvalidIssuer { .. }));
    }

    #[tokio::test]
    async fn verify_rejects_wrong_audience() {
        let sk = SigningKey::random(&mut OsRng);
        let vk = *sk.verifying_key();
        let kid = "tee-eoa-v1";
        let mut claims = happy_claims();
        claims["aud"] = json!("claw-globex");
        let jws = make_jws(&sk, kid, &claims);
        let verifier = seed_verifier(happy_cfg(), &vk, kid);

        let err = verifier
            .verify_at(&jws, 1500)
            .await
            .expect_err("should fail");
        assert!(matches!(err, TrinityVerifyError::InvalidAudience { .. }));
    }

    #[tokio::test]
    async fn verify_rejects_expired() {
        let sk = SigningKey::random(&mut OsRng);
        let vk = *sk.verifying_key();
        let kid = "tee-eoa-v1";
        let jws = make_jws(&sk, kid, &happy_claims());
        let verifier = seed_verifier(happy_cfg(), &vk, kid);

        let err = verifier
            .verify_at(&jws, 2500)
            .await
            .expect_err("should fail");
        assert!(matches!(err, TrinityVerifyError::Expired { .. }));
    }

    #[tokio::test]
    async fn verify_rejects_not_yet_valid() {
        let sk = SigningKey::random(&mut OsRng);
        let vk = *sk.verifying_key();
        let kid = "tee-eoa-v1";
        let jws = make_jws(&sk, kid, &happy_claims());
        let verifier = seed_verifier(happy_cfg(), &vk, kid);

        let err = verifier
            .verify_at(&jws, 500)
            .await
            .expect_err("should fail");
        assert!(matches!(err, TrinityVerifyError::NotYetValid { .. }));
    }

    #[tokio::test]
    async fn verify_rejects_unknown_kid() {
        let sk = SigningKey::random(&mut OsRng);
        let vk = *sk.verifying_key();
        let jws = make_jws(&sk, "tee-eoa-v2", &happy_claims());

        let verifier = seed_verifier(happy_cfg(), &vk, "tee-eoa-v1");
        // Force the refresh path to no-op by pretending the cache is fresh
        // but for the wrong kid. Since the jwks_uri is unreachable we
        // expect a discovery-fetch error rather than `UnknownKid`.
        let err = verifier
            .verify_at(&jws, 1500)
            .await
            .expect_err("should fail");
        assert!(matches!(
            err,
            TrinityVerifyError::DiscoveryFetch(_) | TrinityVerifyError::UnknownKid(_)
        ));
    }

    #[tokio::test]
    async fn verify_rejects_wrong_algorithm() {
        // Hand-craft a header claiming RS256 — verifier must refuse
        // before ever touching the signature.
        let header = json!({ "alg": "RS256", "typ": "JWT", "kid": "tee-eoa-v1" });
        let header_b64 = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&header).unwrap());
        let payload_b64 = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&happy_claims()).unwrap());
        let sig_b64 = URL_SAFE_NO_PAD.encode([0u8; 64]);
        let jws = format!("{header_b64}.{payload_b64}.{sig_b64}");

        let sk = SigningKey::random(&mut OsRng);
        let vk = *sk.verifying_key();
        let verifier = seed_verifier(happy_cfg(), &vk, "tee-eoa-v1");
        let err = verifier
            .verify_at(&jws, 1500)
            .await
            .expect_err("should fail");
        assert!(matches!(err, TrinityVerifyError::UnsupportedAlgorithm(_)));
    }

    #[tokio::test]
    async fn jwks_cache_refresh_on_ttl_expiry() {
        // Build a verifier whose JWKS endpoint is served by a local
        // mock — we verify two tokens with a TTL-expiry in between
        // and assert the mock saw two requests.
        let sk = SigningKey::random(&mut OsRng);
        let vk = *sk.verifying_key();
        let kid = "tee-eoa-v1";

        // Tiny in-process mock using `wiremock` would be ideal but
        // we avoid that dep — instead drive the cache state manually
        // to exercise the refresh path.
        let cfg = happy_cfg();
        let verifier = TrinityVerifier::new(cfg.clone(), reqwest::Client::new());

        // Seed a fresh entry, then artificially age the
        // `jwks_fetched_at` marker so the next `verify` would
        // refresh.
        {
            let mut cache = verifier.jwks_cache.write().await;
            cache.insert(kid.to_string(), CachedKey { verifying_key: vk });
        }
        *verifier.jwks_fetched_at.write().await = Some(Instant::now());

        let jws = make_jws(&sk, kid, &happy_claims());
        // Within TTL — no refresh required, succeeds.
        verifier.verify_at(&jws, 1500).await.expect("within TTL");

        // Age the cache past TTL. With no reachable discovery URL we
        // expect a fetch failure on the refresh path.
        *verifier.jwks_fetched_at.write().await =
            Some(Instant::now() - (CACHE_TTL + Duration::from_secs(1)));

        let err = verifier
            .verify_at(&jws, 1500)
            .await
            .expect_err("refresh should fail with unreachable issuer");
        assert!(matches!(
            err,
            TrinityVerifyError::DiscoveryFetch(_) | TrinityVerifyError::JwksFetch(_)
        ));
    }

    #[tokio::test]
    async fn jwk_to_verifying_key_round_trips() {
        let sk = SigningKey::random(&mut OsRng);
        let vk = *sk.verifying_key();
        let jwk_json = jwk_for(&vk, "tee-eoa-v1");
        let jwk: Jwk = serde_json::from_value(jwk_json).unwrap();
        let recovered = jwk_to_verifying_key(&jwk).expect("rebuild");
        assert_eq!(
            recovered.to_encoded_point(false).as_bytes(),
            vk.to_encoded_point(false).as_bytes()
        );
    }
}

/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 * http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

//! Client-side node-auth JWT minting (issue NVIDIA/infra-controller#355).
//!
//! The node self-signs short-lived ES256 JWTs with the private key of its
//! EXISTING mTLS client certificate; the certificate chain rides along in the
//! token's `x5c` header (RFC 7515 §4.1.6) so the API can verify it against the
//! same root CA its TLS listener already trusts. No new key material, no key
//! storage, and no refresh RPC: a fresh token is minted locally whenever the
//! cached one nears expiry, and key "rotation" happens for free when the
//! client certificate renews.
//!
//! [`BearerAuthService`] is the tower middleware that stamps the current token
//! onto each outgoing request's `Authorization` header. Minting is best-effort:
//! if the cert/key files are missing or unreadable (e.g. before first
//! registration), requests simply carry no bearer header and the channel's
//! mTLS client cert remains the only credential.

use std::io::Cursor;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::task::{Context, Poll};
use std::time::{SystemTime, UNIX_EPOCH};

use data_encoding::BASE64;
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use p256::pkcs8::{EncodePrivateKey, LineEnding};
use serde::Serialize;
use tower::Service;
use x509_parser::prelude::{FromDer, GeneralName, X509Certificate};

/// `aud` claim stamped on all node-auth tokens.
pub const NODE_JWT_AUDIENCE: &str = "nico-api";

/// Lifetime of minted tokens. Deliberately short: tokens cost nothing to
/// re-mint locally, so a leaked one ages out in minutes.
pub const NODE_JWT_TTL_SECS: u64 = 300;

/// Maximum certificates carried in a node-auth JWT's `x5c` header.
///
/// Node certificates normally include a leaf and issuing CA. This leaves room
/// for additional intermediates while bounding the verifier's work on an
/// untrusted request.
pub const NODE_JWT_MAX_X5C_CERTIFICATES: usize = 4;

/// Avoid repeatedly warning from the per-request token-minting path for the
/// same persistent certificate-bundle configuration error.
const OVER_LIMIT_WARNING_INTERVAL_SECS: u64 = NODE_JWT_TTL_SECS;

static LAST_OVER_LIMIT_WARNING_AT: AtomicU64 = AtomicU64::new(0);

/// A cached token is reused until it has less than this long left, then
/// re-minted. Comfortably above per-request latency, comfortably below TTL.
const REMINT_MARGIN_SECS: u64 = 60;

#[derive(Debug, thiserror::Error)]
pub enum NodeJwtError {
    #[error("could not read client certificate or key: {0}")]
    Io(#[from] std::io::Error),
    #[error("client certificate file contains no parsable certificate")]
    NoCertificate,
    #[error("client certificate is not usable for node JWTs: {0}")]
    BadCertificate(String),
    #[error("client private key is not a usable EC key: {0}")]
    BadKey(String),
    #[error("client private key does not match the certificate's public key")]
    KeyCertMismatch,
    /// The client certificate bundle exceeds the node-auth JWT header limit.
    #[error("client certificate file has {actual} certificates; maximum is {maximum}")]
    TooManyCertificates { actual: usize, maximum: usize },
    #[error("JWT signing failed: {0}")]
    Sign(#[from] jsonwebtoken::errors::Error),
    #[error("system clock is before the UNIX epoch")]
    Clock,
}

#[derive(Clone)]
struct CachedToken {
    token: String,
    expires_at: u64,
}

/// Mints and caches node-auth JWTs from the node's existing mTLS client
/// certificate and private key files.
///
/// Cert and key are re-read from disk on every mint, so certificate renewal
/// (which rewrites both files in place) is picked up automatically on the
/// next re-mint without any coordination.
pub struct NodeJwtMinter {
    cert_path: String,
    key_path: String,
    cached: RwLock<Option<CachedToken>>,
}

/// Manual impl so the cached token (a live credential) never lands in debug
/// output of the client config.
impl std::fmt::Debug for NodeJwtMinter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NodeJwtMinter")
            .field("cert_path", &self.cert_path)
            .field("key_path", &self.key_path)
            .finish_non_exhaustive()
    }
}

/// Claims carried by a node-auth JWT. `sub` duplicates the certificate's
/// SPIFFE URI SAN; the server derives identity from the *verified certificate*
/// and only cross-checks `sub` against it, so a forged `sub` buys nothing.
#[derive(Debug, Serialize)]
struct NodeClaims<'a> {
    sub: &'a str,
    aud: &'a str,
    iat: u64,
    exp: u64,
}

impl NodeJwtMinter {
    /// Mints tokens for [`NODE_JWT_AUDIENCE`].
    #[must_use]
    pub fn new(cert_path: String, key_path: String) -> Arc<Self> {
        Arc::new(Self {
            cert_path,
            key_path,
            cached: RwLock::new(None),
        })
    }

    /// Returns a currently-valid token, re-minting if the cached one is
    /// missing or close to expiry. Returns `None` when minting is impossible
    /// — e.g. the cert/key files don't exist yet — so callers degrade
    /// gracefully to mTLS-only. Transient failures log at debug; an
    /// over-limit certificate bundle logs at most once every five minutes per
    /// process because it requires an operator configuration change.
    pub fn current(&self) -> Option<String> {
        self.current_with_expiry().map(|(token, _)| token)
    }

    /// Like [`current`](Self::current), but also returns the token's expiry
    /// (unix seconds) — used by the agent's local API to tell co-located
    /// consumers when to re-fetch.
    pub fn current_with_expiry(&self) -> Option<(String, u64)> {
        let now = unix_now().ok()?;
        // Poisoning recovers rather than disabling the cache. Here it would
        // only cost a re-mint per call, since minting doesn't depend on the
        // cache — but silently doing that forever is worse than continuing to
        // use a `CachedToken` no panicking reader could have torn.
        if let Some(cached) = self
            .cached
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
            && cached.expires_at > now + REMINT_MARGIN_SECS
        {
            return Some((cached.token.clone(), cached.expires_at));
        }
        match self.mint(now) {
            Ok(minted) => {
                let result = (minted.token.clone(), minted.expires_at);
                *self
                    .cached
                    .write()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(minted);
                Some(result)
            }
            Err(error @ NodeJwtError::TooManyCertificates { .. }) => {
                if should_log_over_limit_warning(now, &LAST_OVER_LIMIT_WARNING_AT) {
                    tracing::warn!(
                        target: "node_auth",
                        cert_path = %self.cert_path,
                        %error,
                        "node-auth: client certificate bundle cannot mint node JWT; continuing with mTLS only"
                    );
                }
                None
            }
            Err(error) => {
                tracing::debug!(
                    target: "node_auth",
                    cert_path = %self.cert_path,
                    %error,
                    "node-auth: could not mint node JWT; continuing with mTLS only"
                );
                None
            }
        }
    }

    fn mint(&self, now: u64) -> Result<CachedToken, NodeJwtError> {
        let cert_pem = std::fs::read(&self.cert_path)?;
        let key_pem = std::fs::read_to_string(&self.key_path)?;

        let chain = rustls_pemfile::certs(&mut Cursor::new(&cert_pem[..]))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| NodeJwtError::BadCertificate(e.to_string()))?;
        let leaf = chain.first().ok_or(NodeJwtError::NoCertificate)?;
        if chain.len() > NODE_JWT_MAX_X5C_CERTIFICATES {
            return Err(NodeJwtError::TooManyCertificates {
                actual: chain.len(),
                maximum: NODE_JWT_MAX_X5C_CERTIFICATES,
            });
        }
        let sub = spiffe_uri_from_cert(leaf.as_ref())?;
        let secret = parse_secret_key(&key_pem)?;

        // Certificate renewal rewrites the two files in sequence, so a mint
        // landing in between can pair a new certificate with the old key.
        // Signing would still succeed and the API would reject the result,
        // and the bad token would sit in the cache for a re-mint margin. Catch
        // the mismatch here instead: returning an error means no bearer header
        // on this request and a fresh attempt on the next one.
        if !key_matches_certificate(&secret, leaf.as_ref())? {
            return Err(NodeJwtError::KeyCertMismatch);
        }

        let mut header = Header::new(Algorithm::ES256);
        header.typ = Some("JWT".to_string());
        header.x5c = Some(chain.iter().map(|c| BASE64.encode(c.as_ref())).collect());

        let expires_at = now + NODE_JWT_TTL_SECS;
        let claims = NodeClaims {
            sub: &sub,
            aud: NODE_JWT_AUDIENCE,
            iat: now,
            exp: expires_at,
        };
        let token = jsonwebtoken::encode(&header, &claims, &ec_encoding_key(&secret)?)?;
        Ok(CachedToken { token, expires_at })
    }
}

fn unix_now() -> Result<u64, NodeJwtError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .map_err(|_| NodeJwtError::Clock)
}

fn should_log_over_limit_warning(now: u64, last_warning_at: &AtomicU64) -> bool {
    let mut last = last_warning_at.load(Ordering::Relaxed);
    loop {
        if last != 0 && now.saturating_sub(last) < OVER_LIMIT_WARNING_INTERVAL_SECS {
            return false;
        }
        match last_warning_at.compare_exchange_weak(last, now, Ordering::Relaxed, Ordering::Relaxed)
        {
            Ok(_) => return true,
            Err(current) => last = current,
        }
    }
}

/// Extracts the single SPIFFE URI SAN from the certificate — the same field
/// the server's authn layer maps to a machine principal for mTLS.
fn spiffe_uri_from_cert(der: &[u8]) -> Result<String, NodeJwtError> {
    let (_, cert) = X509Certificate::from_der(der)
        .map_err(|e| NodeJwtError::BadCertificate(format!("X.509 parse error: {e}")))?;
    let san = cert
        .subject_alternative_name()
        .map_err(|e| NodeJwtError::BadCertificate(format!("bad SAN extension: {e}")))?
        .ok_or_else(|| NodeJwtError::BadCertificate("no SAN extension".to_string()))?;
    // Exactly one, not merely the first. The doc comment says "single" and the
    // server enforces that (`carbide_authn::validate_x509_certificate` applies
    // the single-URI-SAN rule to the verified leaf), so picking whichever came
    // first would let a client stamp `sub` from one SAN while the server
    // derived identity from another — a disagreement the server would reject
    // as a subject mismatch, reported here as a signing failure rather than a
    // malformed certificate. Refuse to guess.
    let mut spiffe_uris = san
        .value
        .general_names
        .iter()
        .filter_map(|name| match name {
            GeneralName::URI(uri) if uri.starts_with("spiffe://") => Some(uri.to_string()),
            _ => None,
        });
    let uri = spiffe_uris
        .next()
        .ok_or_else(|| NodeJwtError::BadCertificate("no SPIFFE URI SAN".to_string()))?;
    if spiffe_uris.next().is_some() {
        return Err(NodeJwtError::BadCertificate(
            "certificate carries more than one SPIFFE URI SAN".to_string(),
        ));
    }
    Ok(uri)
}

/// Parses a Vault-issued SEC1 or PKCS#8 client key.
fn parse_secret_key(key_pem: &str) -> Result<p256::SecretKey, NodeJwtError> {
    if key_pem.contains("BEGIN EC PRIVATE KEY") {
        p256::SecretKey::from_sec1_pem(key_pem).map_err(|e| NodeJwtError::BadKey(e.to_string()))
    } else {
        <p256::SecretKey as p256::pkcs8::DecodePrivateKey>::from_pkcs8_pem(key_pem)
            .map_err(|e| NodeJwtError::BadKey(e.to_string()))
    }
}

/// Whether `secret`'s public half is the one certified by `leaf_der`.
///
/// Both encodings the node may hold are accepted: Vault issues SEC1 ("EC
/// PRIVATE KEY") and renewal may leave PKCS#8. The comparison is on the
/// uncompressed SEC1 point, which is exactly what an EC `SubjectPublicKeyInfo`
/// carries.
fn key_matches_certificate(
    secret: &p256::SecretKey,
    leaf_der: &[u8],
) -> Result<bool, NodeJwtError> {
    let (_, cert) = X509Certificate::from_der(leaf_der)
        .map_err(|e| NodeJwtError::BadCertificate(e.to_string()))?;
    let certified = &cert.public_key().subject_public_key.data;
    let from_key = {
        use p256::elliptic_curve::sec1::ToSec1Point as _;
        secret.public_key().to_sec1_point(false)
    };

    Ok(certified.as_ref() == from_key.as_bytes())
}

/// Builds an ES256 signing key from an already-validated client key.
fn ec_encoding_key(secret: &p256::SecretKey) -> Result<EncodingKey, NodeJwtError> {
    let pkcs8 = secret
        .to_pkcs8_pem(LineEnding::LF)
        .map_err(|e| NodeJwtError::BadKey(e.to_string()))?;
    EncodingKey::from_ec_pem(pkcs8.as_bytes()).map_err(|e| NodeJwtError::BadKey(e.to_string()))
}

/// A source of node-auth bearer tokens for outgoing requests. Implemented by
/// [`NodeJwtMinter`] (holds the key, signs locally) and
/// [`SocketTokenSource`](crate::node_token_socket::SocketTokenSource)
/// (fetches from the dpu-agent's local API — the caller never sees the key).
///
/// `current` runs on the request path, so an implementation must return
/// promptly and must never wait on the network or on a remote peer — return
/// `None` instead. Cheap local work is permitted and [`NodeJwtMinter`] does
/// it: on a cold or near-expiry cache it reads the cert and key from disk and
/// signs, roughly once per token lifetime. `SocketTokenSource` is the shape to
/// copy for anything costlier — its fetch runs in a background task and the
/// request path only ever reads the cache.
pub trait NodeTokenProvider: Send + Sync + std::fmt::Debug {
    /// Returns a currently-valid token, or `None` if one isn't available
    /// (the request then proceeds with whatever else the channel carries).
    fn current(&self) -> Option<String>;
}

impl NodeTokenProvider for NodeJwtMinter {
    fn current(&self) -> Option<String> {
        NodeJwtMinter::current(self)
    }
}

/// Tower middleware that injects `Authorization: Bearer <jwt>` onto each
/// request when a [`NodeTokenProvider`] is configured. A `None` provider is a
/// no-op, so the same client construction path serves both token and
/// mTLS-only modes.
#[derive(Clone)]
pub struct BearerAuthService<S> {
    inner: S,
    minter: Option<Arc<dyn NodeTokenProvider>>,
}

impl<S> BearerAuthService<S> {
    pub fn new(inner: S, minter: Option<Arc<dyn NodeTokenProvider>>) -> Self {
        Self { inner, minter }
    }
}

impl<S, B> Service<hyper::Request<B>> for BearerAuthService<S>
where
    S: Service<hyper::Request<B>>,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = S::Future;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut req: hyper::Request<B>) -> Self::Future {
        if let Some(value) = self
            .minter
            .as_ref()
            .and_then(|minter| minter.current())
            .and_then(|token| hyper::http::HeaderValue::from_str(&format!("Bearer {token}")).ok())
        {
            req.headers_mut()
                .insert(hyper::header::AUTHORIZATION, value);
        }
        self.inner.call(req)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicU64;

    use carbide_test_support::{Check, check_values};
    use jsonwebtoken::{DecodingKey, Validation};
    use serde::Deserialize;

    use super::*;

    #[derive(Debug, Deserialize)]
    struct Claims {
        sub: String,
        exp: u64,
        iat: u64,
    }

    const SPIFFE_URI: &str = "spiffe://forge.local/forge-system/machine/fm100xtest";

    /// Self-signed leaf with a SPIFFE URI SAN plus its PKCS#8 key PEM —
    /// stand-ins for the Vault-issued client cert/key pair on a node.
    fn cert_and_key() -> (String, String) {
        let mut params = rcgen::CertificateParams::default();
        params.subject_alt_names = vec![rcgen::SanType::URI(
            rcgen::string::Ia5String::try_from(SPIFFE_URI.to_string()).expect("uri"),
        )];
        let key = rcgen::KeyPair::generate().expect("key pair");
        let cert = params.self_signed(&key).expect("certificate");
        (cert.pem(), key.serialize_pem())
    }

    fn write_temp(dir: &tempfile::TempDir, name: &str, contents: &str) -> String {
        let path = dir.path().join(name);
        std::fs::write(&path, contents).expect("write");
        path.to_string_lossy().into_owned()
    }

    fn decode_against_own_cert(token: &str) -> Claims {
        // Validate exactly as the server does: pull the leaf from x5c, take
        // its SPKI EC point, verify the signature with it.
        let header = jsonwebtoken::decode_header(token).expect("header");
        assert_eq!(header.alg, Algorithm::ES256);
        let chain = header.x5c_der().expect("x5c decodes").expect("x5c present");
        let (_, cert) = X509Certificate::from_der(&chain[0]).expect("leaf parses");
        let decoding_key = DecodingKey::from_ec_der(&cert.public_key().subject_public_key.data);
        let mut validation = Validation::new(Algorithm::ES256);
        validation.set_audience(&[NODE_JWT_AUDIENCE]);
        jsonwebtoken::decode::<Claims>(token, &decoding_key, &validation)
            .expect("token validates")
            .claims
    }

    /// Certificate renewal rewrites cert and key in sequence, so a mint can
    /// land on a new certificate paired with the previous key. That signs
    /// cleanly but the API cannot verify it, so it must never be minted or
    /// cached — the next attempt, on a consistent pair, should succeed.
    #[test]
    fn a_certificate_from_a_different_key_is_never_minted_or_cached() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (cert_pem, _) = cert_and_key();
        let (_, other_key_pem) = cert_and_key();

        let cert_path = write_temp(&dir, "cert.pem", &cert_pem);
        let key_path = write_temp(&dir, "key.pem", &other_key_pem);
        let minter = NodeJwtMinter::new(cert_path.clone(), key_path.clone());

        assert!(
            minter.current().is_none(),
            "a token the API could not verify must not be minted"
        );

        // Renewal completes: the matching pair lands, in place. The *same*
        // minter has to recover — a fresh one would only show that a good pair
        // mints, not that the instance which just refused a mismatch stopped
        // refusing. It can, because the mismatch path caches nothing.
        let (matching_cert, matching_key) = cert_and_key();
        std::fs::write(&key_path, &matching_key).expect("write key");
        std::fs::write(&cert_path, &matching_cert).expect("write cert");
        assert!(
            minter.current().is_some(),
            "a consistent cert/key pair must mint normally"
        );
    }

    #[test]
    fn mints_a_token_signed_by_the_client_cert_key() {
        let (cert_pem, key_pem) = cert_and_key();
        let dir = tempfile::tempdir().expect("tempdir");
        let minter = NodeJwtMinter::new(
            write_temp(&dir, "cert.pem", &cert_pem),
            write_temp(&dir, "key.pem", &key_pem),
        );

        let token = minter.current().expect("token minted");
        let claims = decode_against_own_cert(&token);
        assert_eq!(claims.sub, SPIFFE_URI);
        assert_eq!(claims.exp - claims.iat, NODE_JWT_TTL_SECS);
    }

    #[test]
    fn certificate_chain_length_must_fit_the_node_jwt_header() {
        #[derive(Debug, Eq, PartialEq)]
        enum Outcome {
            Mints,
            TooManyCertificates { actual: usize, maximum: usize },
        }

        let (cert_pem, key_pem) = cert_and_key();
        let dir = tempfile::tempdir().expect("tempdir");

        check_values(
            [
                Check {
                    scenario: "the maximum certificate count is minted",
                    input: NODE_JWT_MAX_X5C_CERTIFICATES,
                    expect: Outcome::Mints,
                },
                Check {
                    scenario: "one certificate above the limit is rejected",
                    input: NODE_JWT_MAX_X5C_CERTIFICATES + 1,
                    expect: Outcome::TooManyCertificates {
                        actual: NODE_JWT_MAX_X5C_CERTIFICATES + 1,
                        maximum: NODE_JWT_MAX_X5C_CERTIFICATES,
                    },
                },
            ],
            |certificate_count| {
                let minter = NodeJwtMinter::new(
                    write_temp(&dir, "cert.pem", &cert_pem.repeat(certificate_count)),
                    write_temp(&dir, "key.pem", &key_pem),
                );

                match minter.mint(unix_now().expect("system clock")) {
                    Ok(_) => Outcome::Mints,
                    Err(NodeJwtError::TooManyCertificates { actual, maximum }) => {
                        Outcome::TooManyCertificates { actual, maximum }
                    }
                    Err(error) => panic!("unexpected node-token mint error: {error:?}"),
                }
            },
        );
    }

    #[test]
    fn over_limit_bundle_warning_is_rate_limited() {
        check_values(
            [
                Check {
                    scenario: "the first process-wide failure is reported",
                    input: (0, 1_000),
                    expect: true,
                },
                Check {
                    scenario: "a failure one second later is suppressed",
                    input: (1_000, 1_001),
                    expect: false,
                },
                Check {
                    scenario: "a failure just before the interval ends is suppressed",
                    input: (1_000, 1_000 + OVER_LIMIT_WARNING_INTERVAL_SECS - 1),
                    expect: false,
                },
                Check {
                    scenario: "a failure at the interval boundary is reported",
                    input: (1_000, 1_000 + OVER_LIMIT_WARNING_INTERVAL_SECS),
                    expect: true,
                },
            ],
            |(last_warning_at, now)| {
                should_log_over_limit_warning(now, &AtomicU64::new(last_warning_at))
            },
        );
    }

    #[test]
    fn sec1_key_pem_is_accepted() {
        // Vault PKI hands out SEC1-encoded EC keys; re-encode the test key the
        // same way and make sure minting still works.
        use p256::pkcs8::DecodePrivateKey;
        let (cert_pem, key_pem) = cert_and_key();
        let secret = p256::SecretKey::from_pkcs8_pem(&key_pem).expect("pkcs8 parses");
        let sec1_pem = secret
            .to_sec1_pem(LineEnding::LF)
            .expect("sec1 encodes")
            .to_string();
        assert!(sec1_pem.contains("BEGIN EC PRIVATE KEY"));

        let dir = tempfile::tempdir().expect("tempdir");
        let minter = NodeJwtMinter::new(
            write_temp(&dir, "cert.pem", &cert_pem),
            write_temp(&dir, "key.pem", &sec1_pem),
        );
        let token = minter.current().expect("token minted from SEC1 key");
        assert_eq!(decode_against_own_cert(&token).sub, SPIFFE_URI);
    }

    #[test]
    fn token_is_cached_until_near_expiry() {
        let (cert_pem, key_pem) = cert_and_key();
        let dir = tempfile::tempdir().expect("tempdir");
        let minter = NodeJwtMinter::new(
            write_temp(&dir, "cert.pem", &cert_pem),
            write_temp(&dir, "key.pem", &key_pem),
        );
        let first = minter.current().expect("token");

        // Remove the material a mint would need. Comparing two live mints
        // proves nothing: `iat`/`exp` come from whole seconds, so two fresh
        // mints in the same second are byte-identical and the assertion would
        // hold with the cache removed entirely. Only a token served without
        // readable files can have come from the cache.
        std::fs::remove_file(dir.path().join("cert.pem")).expect("remove cert");
        std::fs::remove_file(dir.path().join("key.pem")).expect("remove key");

        let second = minter
            .current()
            .expect("fresh token must be served from cache");
        assert_eq!(first, second);
    }

    #[test]
    fn missing_files_yield_none_not_panic() {
        let minter = NodeJwtMinter::new(
            "/nonexistent/cert.pem".to_string(),
            "/nonexistent/key.pem".to_string(),
        );
        assert!(minter.current().is_none());
    }
}

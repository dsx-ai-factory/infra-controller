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

//! Machine Identity module for JWT-SVID token generation and management.
//!
//! This module handles signing JWT-SVID tokens for machine identity verification.
//! [`crypto`] holds AES envelope helpers for `tenant_identity_config` ciphertext.
//! [`token_exchange`] implements RFC 8693 HTTP calls to a tenant token endpoint.
mod crypto;
mod token_exchange;

use std::collections::BTreeMap;
use std::fmt;

use base64::Engine;
pub(crate) use crypto::{
    MachineIdentityStoredSecretDecryptionFailed, ReencryptBlobOutcome,
    StoredMachineIdentitySecretKind, decrypt_machine_identity_ciphertext,
    decrypt_token_delegation_encrypted_blob, machine_identity_encryption_secret,
    reencrypt_ciphertext_if_needed, token_delegation_credentials,
};
use jsonwebtoken::{EncodingKey, Header, encode};
use model::tenant::identity_config::TENANT_IDENTITY_SIGNING_JWT_ALG;
use p256::PublicKey;
use p256::elliptic_curve::sec1::ToSec1Point;
use p256::pkcs8::DecodePublicKey;
use serde_json::Value;
pub(crate) use token_exchange::{token_exchange_http_client, token_exchange_request};

/// Error type for JWT-SVID signing.
#[derive(Debug, thiserror::Error)]
pub(crate) enum SignError {
    #[error("invalid JSON payload: {0}")]
    InvalidPayload(String),
    #[error("encode error: {0}")]
    Encode(#[from] jsonwebtoken::errors::Error),
}

/// Options for signing (e.g. future overrides for expiry, audience).
#[derive(Debug, Default, Clone)]
pub(crate) struct SignOptions {}

/// Abstraction for signing JWT-SVID tokens. Key loading and metadata (e.g. from DB)
/// stay outside: the caller builds a signer and passes it here.
pub(crate) trait Signer: Send + Sync {
    /// Signs the given JSON payload (JWT claims) and returns the signed JWT string.
    fn sign(&self, payload: &Value, opts: &SignOptions) -> Result<String, SignError>;
}

/// ES256 signer (ECDSA P-256 + SHA-256). Holds key material and key_id only;
/// no I/O or DB access.
pub(crate) struct Es256Signer {
    key_id: String,
    encoding_key: EncodingKey,
}

impl Es256Signer {
    /// Builds an ES256 signer from PEM-encoded EC P-256 private key and key id (`kid`).
    pub(crate) fn new(key: &[u8], key_id: impl AsRef<str>) -> Result<Self, SignError> {
        let encoding_key = EncodingKey::from_ec_pem(key).map_err(SignError::Encode)?;
        Ok(Self {
            key_id: key_id.as_ref().to_string(),
            encoding_key,
        })
    }
}

impl Signer for Es256Signer {
    fn sign(&self, payload: &Value, _opts: &SignOptions) -> Result<String, SignError> {
        let claims = payload
            .as_object()
            .ok_or_else(|| SignError::InvalidPayload("payload must be a JSON object".to_string()))?
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect::<BTreeMap<_, _>>();

        let mut header = Header::new(jsonwebtoken::Algorithm::ES256);
        header.kid = Some(self.key_id.clone());
        let token = encode(&header, &claims, &self.encoding_key)?;
        Ok(token)
    }
}

/// Failure building a RFC 7517 JWK / JWKS JSON value from a tenant public key PEM.
#[derive(Debug)]
pub(crate) struct JwkBuildError(String);

impl fmt::Display for JwkBuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl std::error::Error for JwkBuildError {}

/// JWK `use` (RFC 7517 / SPIFFE bundle) for the tenant signing public key in `GetJWKS`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum JwkPublicKeyUse {
    /// RFC 7517 `sig` — OIDC-style JWT signature verification (`/.well-known/jwks.json`).
    OidcSignature,
    /// SPIFFE bundle `jwt-svid` — JWT-SVID validation (SPIFFE Trust Domain and Bundle §4.2.2).
    SpiffeJwtSvid,
}

impl JwkPublicKeyUse {
    /// Wire value for the JWK `use` parameter.
    #[must_use]
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::OidcSignature => "sig",
            Self::SpiffeJwtSvid => "jwt-svid",
        }
    }
}

/// Maps `public_pem` (SPKI PEM) into one RFC 7517 JWK JSON object.
pub(crate) fn public_pem_to_jwk_value(
    public_key_pem: &str,
    kid: &str,
    algorithm: &str,
    jwk_key_use: JwkPublicKeyUse,
) -> Result<Value, JwkBuildError> {
    if algorithm != TENANT_IDENTITY_SIGNING_JWT_ALG {
        return Err(JwkBuildError(format!(
            "JWKS is only implemented for {TENANT_IDENTITY_SIGNING_JWT_ALG} (got {algorithm:?})"
        )));
    }

    let pk = PublicKey::from_public_key_pem(public_key_pem.trim())
        .map_err(|e| JwkBuildError(format!("failed to parse signing public key PEM: {e}")))?;
    let encoded = pk.to_sec1_point(false);
    let x = encoded
        .x()
        .ok_or_else(|| JwkBuildError("EC public key missing x coordinate".into()))?;
    let y = encoded.y().ok_or_else(|| {
        JwkBuildError("EC public key missing y coordinate — expected uncompressed SEC1".into())
    })?;

    let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;

    Ok(serde_json::json!({
        "kty": "EC",
        "use": jwk_key_use.as_str(),
        "crv": "P-256",
        "kid": kid,
        "x": b64.encode(x),
        "y": b64.encode(y),
        "alg": algorithm,
    }))
}

/// Serializes `{"keys":[ ... ]}` as compact UTF-8 JSON for gRPC [`rpc::forge::Jwks::jwks`].
pub(crate) fn jwks_document_string(keys: &[Value]) -> Result<String, JwkBuildError> {
    if keys.is_empty() {
        return Err(JwkBuildError(
            "JWKS document requires at least one verification key".into(),
        ));
    }
    let doc = serde_json::json!({ "keys": keys });
    serde_json::to_string(&doc).map_err(|e| JwkBuildError(format!("serialize JWKS document: {e}")))
}

#[cfg(test)]
mod tests {
    use p256::SecretKey;
    use p256::pkcs8::{DecodePrivateKey, EncodePublicKey};

    use super::*;

    fn sign(payload: &Value, key: &[u8]) -> Result<String, SignError> {
        let signer = Es256Signer::new(key, "default")?;
        signer.sign(payload, &SignOptions::default())
    }

    /// Returns an EC P-256 private key in PKCS#8 PEM format (standard encoding), generated at test time.
    fn ec_p256_private_key_pem() -> Vec<u8> {
        let key_pair = rcgen::KeyPair::generate().expect("generate test key");
        key_pair.serialize_pem().into_bytes()
    }

    #[test]
    fn sign_returns_jwt_svid_for_valid_object_payload_and_key() {
        let payload = serde_json::json!({
            "sub": "spiffe://example.org/machine/123",
            "iss": "https://carbide/v1/org/org-id",
            "aud": ["service-a"],
            "exp": 1678886400,
            "iat": 1678882800,
        });
        let key = ec_p256_private_key_pem();
        let result = sign(&payload, &key);
        assert!(result.is_ok(), "sign should succeed: {:?}", result.err());
        let token = result.unwrap();
        // JWT has three base64url parts separated by dots
        let parts: Vec<&str> = token.split('.').collect();
        assert_eq!(parts.len(), 3, "JWT-SVID must have 3 segments");
    }

    #[test]
    fn sign_returns_invalid_payload_error_when_payload_is_not_an_object() {
        let payload = serde_json::json!(["not", "an", "object"]);
        let key = ec_p256_private_key_pem();
        let result = sign(&payload, &key);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err, SignError::InvalidPayload(_)),
            "expected InvalidPayload, got {:?}",
            err
        );
    }

    #[test]
    fn sign_returns_invalid_payload_error_when_payload_is_primitive() {
        let payload = serde_json::json!("a string");
        let key = ec_p256_private_key_pem();
        let result = sign(&payload, &key);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), SignError::InvalidPayload(_)));
    }

    #[test]
    fn sign_returns_encode_error_for_invalid_key() {
        let payload = serde_json::json!({ "sub": "test" });
        let key = b"not valid PEM";
        let result = sign(&payload, key);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), SignError::Encode(_)));
    }

    #[test]
    fn es256_signer_implements_signer_trait() {
        let key = ec_p256_private_key_pem();
        let signer = Es256Signer::new(&key, "test-key-1").expect("create signer");
        assert_eq!(signer.key_id, "test-key-1");
        let payload = serde_json::json!({ "sub": "spiffe://example.org/machine/456" });
        let token = signer
            .sign(&payload, &SignOptions::default())
            .expect("sign");
        let parts: Vec<&str> = token.split('.').collect();
        assert_eq!(parts.len(), 3);
    }

    #[test]
    fn public_pem_to_jwk_es256() {
        let key_pair = rcgen::KeyPair::generate().expect("generate test key pair");
        let private_pem = key_pair.serialize_pem();
        let sk = SecretKey::from_pkcs8_pem(&private_pem).expect("parse PKCS#8 private PEM");
        let pk = sk.public_key();
        let pem = pk
            .to_public_key_pem(p256::pkcs8::LineEnding::LF)
            .expect("public key PEM");
        let jwk = public_pem_to_jwk_value(
            &pem,
            "test-kid",
            TENANT_IDENTITY_SIGNING_JWT_ALG,
            JwkPublicKeyUse::OidcSignature,
        )
        .expect("jwk");
        assert_eq!(jwk["kty"], "EC");
        assert_eq!(jwk["use"], JwkPublicKeyUse::OidcSignature.as_str());
        assert_eq!(jwk["crv"], "P-256");
        assert_eq!(jwk["kid"], "test-kid");
        assert_eq!(jwk["alg"], TENANT_IDENTITY_SIGNING_JWT_ALG);
        assert!(
            !jwk["x"].as_str().unwrap_or("").is_empty()
                && !jwk["y"].as_str().unwrap_or("").is_empty()
        );
    }

    #[test]
    fn public_pem_to_jwk_spiffe_uses_jwt_svid_key_use() {
        let key_pair = rcgen::KeyPair::generate().expect("generate test key pair");
        let private_pem = key_pair.serialize_pem();
        let sk = SecretKey::from_pkcs8_pem(&private_pem).expect("parse PKCS#8 private PEM");
        let pk = sk.public_key();
        let pem = pk
            .to_public_key_pem(p256::pkcs8::LineEnding::LF)
            .expect("public key PEM");
        let jwk = public_pem_to_jwk_value(
            &pem,
            "test-kid",
            TENANT_IDENTITY_SIGNING_JWT_ALG,
            JwkPublicKeyUse::SpiffeJwtSvid,
        )
        .expect("jwk");
        assert_eq!(jwk["use"], JwkPublicKeyUse::SpiffeJwtSvid.as_str());
    }
}

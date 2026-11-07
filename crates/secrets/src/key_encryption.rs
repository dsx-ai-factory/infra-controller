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

//! Generic key encryption and signing key generation utilities.
//!
//! # Envelope format (scheme version 1)
//!
//! Stored in the DB as **standard base64** of a UTF-8 JSON document:
//!
//! ```json
//! {"scheme_version":1,"key_id":"…","nonce":[…],"ciphertext":[…]}
//! ```
//!
//! `nonce` (12 bytes) and `ciphertext` are JSON arrays of byte values (`serde_json` defaults).
//!
//! - **scheme_version** `1`: AES-256-GCM; key material is 32 bytes from base64-decoding the
//!   configured encryption secret (`openssl rand -base64 32`).
//! - **key_id**: map key under `machine_identity.encryption_keys` (e.g. `kv1`). Decrypt loads
//!   credentials by this envelope field. Encrypt uses site `current_encryption_key_id`.

use aes_gcm::Aes256Gcm;
use aes_gcm::aead::{Aead, KeyInit};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use p256::SecretKey;
use p256::elliptic_curve::Generate;
use p256::pkcs8::{EncodePrivateKey, EncodePublicKey, LineEnding};
use rand::rngs::SysRng;
use rand_core::TryRng;
use serde::{Deserialize, Serialize};

/// Scheme version 1: AES-256-GCM, 32-byte key from base64-decoded encryption secret, envelope below.
pub const SCHEME_VERSION_V1: u8 = 1;

/// 32-byte AES-256 key material (after decoding from the stored credential, if applicable).
pub type Aes256Key = [u8; 32];

/// Error type for key encryption and generation operations.
#[derive(Debug, thiserror::Error)]
pub enum KeyEncryptionError {
    #[error("key generation failed: {0}")]
    KeyGen(String),
    #[error("encryption failed: {0}")]
    Encrypt(String),
    #[error("decryption failed: {0}")]
    Decrypt(String),
}

/// Decodes machine-identity encryption key material from its **stored** form: standard base64 of
/// exactly 32 random bytes (e.g. `openssl rand -base64 32`). Callers that already hold key bytes
/// should use those directly as [`Aes256Key`].
pub fn aes256_key_from_stored_secret(stored: &str) -> Result<Aes256Key, KeyEncryptionError> {
    let trimmed = stored.trim();
    let raw = BASE64.decode(trimmed).map_err(|e| {
        KeyEncryptionError::Encrypt(format!("encryption secret is not valid base64: {e}"))
    })?;
    raw.try_into().map_err(|v: Vec<u8>| {
        KeyEncryptionError::Encrypt(format!(
            "encryption secret must decode to exactly 32 bytes for AES-256 (got {} bytes); use e.g. `openssl rand -base64 32`",
            v.len()
        ))
    })
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EncryptionEnvelopeV1 {
    scheme_version: u8,
    key_id: String,
    nonce: [u8; 12],
    ciphertext: Vec<u8>,
}

#[derive(Debug, Clone)]
struct ParsedEnvelopeV1 {
    key_id: String,
    nonce: [u8; 12],
    ciphertext: Vec<u8>,
}

fn envelope_json_bytes(
    key_id: impl AsRef<str>,
    nonce: &[u8; 12],
    ciphertext: &[u8],
) -> Result<Vec<u8>, KeyEncryptionError> {
    let key_id = key_id.as_ref();
    let kid = key_id.as_bytes();
    if kid.is_empty() || kid.len() > 255 {
        return Err(KeyEncryptionError::Encrypt(
            "encryption_key_id (envelope) must be 1..=255 UTF-8 bytes".into(),
        ));
    }
    let env = EncryptionEnvelopeV1 {
        scheme_version: SCHEME_VERSION_V1,
        key_id: key_id.to_string(),
        nonce: *nonce,
        ciphertext: ciphertext.to_vec(),
    };
    serde_json::to_vec(&env).map_err(|e| KeyEncryptionError::Encrypt(e.to_string()))
}

fn parse_envelope(data: &[u8]) -> Result<ParsedEnvelopeV1, KeyEncryptionError> {
    let env: EncryptionEnvelopeV1 =
        serde_json::from_slice(data).map_err(|e| KeyEncryptionError::Decrypt(e.to_string()))?;
    if env.scheme_version != SCHEME_VERSION_V1 {
        return Err(KeyEncryptionError::Decrypt(
            "unsupported scheme_version".into(),
        ));
    }
    if env.key_id.is_empty() || env.key_id.len() > 255 {
        return Err(KeyEncryptionError::Decrypt(
            "envelope key_id must be 1..=255 UTF-8 bytes".into(),
        ));
    }
    Ok(ParsedEnvelopeV1 {
        key_id: env.key_id,
        nonce: env.nonce,
        ciphertext: env.ciphertext,
    })
}

fn parse_envelope_from_base64(
    encrypted_base64: &str,
) -> Result<ParsedEnvelopeV1, KeyEncryptionError> {
    let combined = BASE64
        .decode(encrypted_base64.trim())
        .map_err(|e| KeyEncryptionError::Decrypt(e.to_string()))?;
    parse_envelope(&combined)
}

/// Returns the `key_id` embedded in a stored envelope (standard base64 of JSON v1).
pub fn envelope_key_id(encrypted_base64: &str) -> Result<String, KeyEncryptionError> {
    Ok(parse_envelope_from_base64(encrypted_base64)?.key_id)
}

/// Encrypts plaintext with AES-256-GCM using envelope v1.
///
/// `encryption_secret` is raw 32-byte key material.
/// `encryption_key_id` must match the entry under `machine_identity.encryption_keys` and site
/// `current_encryption_key_id` (1..=255 UTF-8 bytes). Callers may pass any `T: AsRef<str>` (e.g. a
/// non-empty encryption-key id newtype from the API model).
/// Returns standard base64 of the UTF-8 JSON envelope (safe for `TEXT` columns).
pub fn encrypt(
    plaintext: &[u8],
    encryption_secret: &Aes256Key,
    encryption_key_id: impl AsRef<str>,
) -> Result<String, KeyEncryptionError> {
    let cipher = Aes256Gcm::new_from_slice(encryption_secret)
        .map_err(|e| KeyEncryptionError::Encrypt(e.to_string()))?;
    let mut nonce = [0u8; 12];
    SysRng.try_fill_bytes(&mut nonce).map_err(|e| {
        KeyEncryptionError::Encrypt(format!("OS RNG failed while generating AES-GCM nonce: {e}"))
    })?;
    let ciphertext = cipher
        .encrypt(&nonce.into(), plaintext)
        .map_err(|e| KeyEncryptionError::Encrypt(e.to_string()))?;
    let envelope = envelope_json_bytes(encryption_key_id.as_ref(), &nonce, &ciphertext)?;
    Ok(BASE64.encode(&envelope))
}

/// Decrypts a DB value produced by [`encrypt`]: JSON envelope v1, same [`Aes256Key`] as used for encrypt.
pub fn decrypt(
    encrypted_base64: &str,
    encryption_secret: &Aes256Key,
) -> Result<Vec<u8>, KeyEncryptionError> {
    let envelope = parse_envelope_from_base64(encrypted_base64)?;
    let cipher = Aes256Gcm::new_from_slice(encryption_secret)
        .map_err(|e| KeyEncryptionError::Decrypt(e.to_string()))?;
    let nonce_ga = aes_gcm::aead::generic_array::GenericArray::from_slice(&envelope.nonce);
    cipher
        .decrypt(nonce_ga, envelope.ciphertext.as_slice())
        .map_err(|e| KeyEncryptionError::Decrypt(e.to_string()))
}

/// Generates an ES256 (ECDSA P-256) signing key pair (PKCS#8 private + SPKI public PEM via `p256`).
///
/// The public PEM matches `p256::PublicKey::from_public_key_pem` (same as carbide-api JWKS).
/// Returns (private_key_pem_bytes, public_key_pem).
pub fn generate_es256_key_pair() -> Result<(Vec<u8>, String), KeyEncryptionError> {
    let secret_key = SecretKey::try_generate_from_rng(&mut SysRng).map_err(|e| {
        KeyEncryptionError::KeyGen(format!("Could not generate SecretKey from system RNG: {e}"))
    })?;
    let private_pem = secret_key
        .to_pkcs8_pem(LineEnding::LF)
        .map_err(|e| KeyEncryptionError::KeyGen(e.to_string()))?;
    let private_pem_bytes = private_pem.as_bytes().to_vec();
    let public_pem = secret_key
        .public_key()
        .to_public_key_pem(LineEnding::LF)
        .map_err(|e| KeyEncryptionError::KeyGen(e.to_string()))?;
    Ok((private_pem_bytes, public_pem))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_aes256_key() -> Aes256Key {
        [0u8; 32]
    }

    #[test]
    fn encrypt_decrypt_roundtrip_v1() {
        let plaintext = b"secret data";
        let key = test_aes256_key();
        let encrypted = encrypt(plaintext, &key, "kv1").unwrap();
        let decrypted = decrypt(&encrypted, &key).unwrap();
        assert_eq!(decrypted, plaintext);
        let raw = BASE64.decode(encrypted).unwrap();
        assert_eq!(raw.first(), Some(&b'{'));
    }

    #[test]
    fn stored_secret_wrong_length_errors() {
        let short = BASE64.encode([0u8; 16]);
        let err = aes256_key_from_stored_secret(&short).unwrap_err();
        assert!(err.to_string().contains("32"));
    }

    #[test]
    fn encrypt_decrypt_token_delegation_json_utf8_roundtrip() {
        let key = test_aes256_key();
        let json = r#"{"client_id":"c","client_secret":"s"}"#;
        let enc = encrypt(json.as_bytes(), &key, "kv1").unwrap();
        let plain = decrypt(&enc, &key).unwrap();
        let out = String::from_utf8(plain).unwrap();
        assert_eq!(out, json);
    }

    #[test]
    fn decrypt_rejects_plaintext_token_delegation_json() {
        let key = test_aes256_key();
        let plaintext_json = r#"{"client_id":"c","client_secret":"s"}"#;
        assert!(decrypt(plaintext_json, &key).is_err());
    }

    #[test]
    fn envelope_key_id_reads_embedded_key_id() {
        let key = test_aes256_key();
        let encrypted = encrypt(b"secret", &key, "kv2").unwrap();
        assert_eq!(envelope_key_id(&encrypted).unwrap(), "kv2");
    }
}

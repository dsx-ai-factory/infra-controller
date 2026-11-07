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

use aes_gcm::aead::{Aead, Payload};
use aes_gcm::{Aes256Gcm, KeyInit, Nonce};

use crate::KmsError;

/// The byte length of an AES-256-GCM nonce.
const NONCE_LEN: usize = 12;

/// Encrypt plaintext with AES-256-GCM under a random 12-byte nonce, binding
/// `aad` into the authentication tag. Returns `(ciphertext, nonce)`.
///
/// The associated data is authenticated but not encrypted: decryption fails
/// unless the same bytes are presented again. Callers use this to tie a
/// ciphertext to its storage location (the secrets table passes the row's
/// `path`), so a ciphertext copied onto another row will not decrypt. Pass
/// `b""` when there is no context to bind, which is what DEK wrapping does.
pub fn encrypt(
    key: &[u8; 32],
    plaintext: &[u8],
    aad: &[u8],
) -> Result<(Vec<u8>, Vec<u8>), KmsError> {
    let cipher = Aes256Gcm::new(key.into());
    let nonce_bytes: [u8; NONCE_LEN] = rand::random();
    let nonce = Nonce::from_slice(&nonce_bytes);
    let ciphertext = cipher
        .encrypt(
            nonce,
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .map_err(|_| KmsError::EncryptionFailed("AES-256-GCM encryption failed".to_string()))?;
    Ok((ciphertext, nonce_bytes.to_vec()))
}

/// Decrypt AES-256-GCM ciphertext. The key, nonce, and `aad` must all match
/// what [`encrypt`] was given, or decryption fails.
pub fn decrypt(
    key: &[u8; 32],
    nonce: &[u8],
    ciphertext: &[u8],
    aad: &[u8],
) -> Result<Vec<u8>, KmsError> {
    if nonce.len() != NONCE_LEN {
        return Err(KmsError::DecryptionFailed(format!(
            "invalid nonce length: expected {NONCE_LEN} bytes, got {}",
            nonce.len()
        )));
    }
    let cipher = Aes256Gcm::new(key.into());
    let nonce = Nonce::from_slice(nonce);
    cipher
        .decrypt(
            nonce,
            Payload {
                msg: ciphertext,
                aad,
            },
        )
        .map_err(|_| KmsError::DecryptionFailed("AES-256-GCM decryption failed".to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_key() -> [u8; 32] {
        let mut key = [0u8; 32];
        for (i, byte) in key.iter_mut().enumerate() {
            *byte = i as u8;
        }
        key
    }

    fn other_key() -> [u8; 32] {
        let mut key = [0u8; 32];
        for (i, byte) in key.iter_mut().enumerate() {
            *byte = (i as u8).wrapping_add(100);
        }
        key
    }

    // Verifies that encrypt then decrypt produces the original plaintext.
    #[test]
    fn encrypt_decrypt_round_trip() {
        let key = test_key();
        let plaintext = b"hello, secrets!";

        let (ciphertext, nonce) = encrypt(&key, plaintext, b"").expect("encrypt");
        let decrypted = decrypt(&key, &nonce, &ciphertext, b"").expect("decrypt");

        assert_eq!(decrypted, plaintext);
    }

    // Verifies that decrypting with the wrong key fails.
    #[test]
    fn wrong_key_fails_decryption() {
        let key = test_key();
        let wrong_key = other_key();
        let plaintext = b"sensitive data";

        let (ciphertext, nonce) = encrypt(&key, plaintext, b"").expect("encrypt");
        let result = decrypt(&wrong_key, &nonce, &ciphertext, b"");

        assert!(result.is_err());
    }

    // Verifies that encrypting the same plaintext twice produces different
    // ciphertext (a new random nonce every call).
    #[test]
    fn different_nonces_produce_different_ciphertext() {
        let key = test_key();
        let plaintext = b"same plaintext";

        let (ct1, nonce1) = encrypt(&key, plaintext, b"").expect("encrypt 1");
        let (ct2, nonce2) = encrypt(&key, plaintext, b"").expect("encrypt 2");

        assert_ne!(nonce1, nonce2);
        assert_ne!(ct1, ct2);
    }

    // Verifies that an invalid nonce length returns an error.
    #[test]
    fn invalid_nonce_length_errors() {
        let key = test_key();
        let result = decrypt(&key, &[0u8; 11], &[], b"");
        assert!(result.is_err());
    }

    // Verifies that the associated data is bound into the ciphertext: the
    // same bytes decrypt it, different bytes do not.
    #[test]
    fn aad_mismatch_fails_decryption() {
        let key = test_key();
        let plaintext = b"bound to a path";

        let (ciphertext, nonce) =
            encrypt(&key, plaintext, b"machines/bmc/a/root").expect("encrypt");

        let ok = decrypt(&key, &nonce, &ciphertext, b"machines/bmc/a/root").expect("same aad");
        assert_eq!(ok, plaintext);

        let swapped = decrypt(&key, &nonce, &ciphertext, b"machines/bmc/b/root");
        assert!(
            swapped.is_err(),
            "ciphertext moved to another path must not decrypt"
        );
    }
}

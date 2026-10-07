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

use std::collections::HashMap;
use std::path::PathBuf;

use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use zeroize::{Zeroize, Zeroizing};

use crate::{EncryptedDek, KmsBackend, KmsError, crypto};

/// Where to load a base64-encoded 256-bit key from.
///
/// `Env` and `File` keep key material out of the carbide config file, which is
/// Debug-logged at startup and served on the web debug page. `Value` inlines
/// the key in config instead -- handy for tests and local dev, but it lands in
/// those debug surfaces, so use `Env` or `File` for real keys.
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(untagged)]
pub enum KeySource {
    /// Load the key from an environment variable.
    Env { env: String },
    /// Load the key from a file path.
    File { file: PathBuf },
    /// Use the base64-encoded key inlined directly in config. Convenient for
    /// tests and local dev; avoid for real keys, since config is Debug-logged.
    Value { value: String },
}

/// IntegratedKmsProvider implements KmsBackend using
/// local key material. Keys are configured as an
/// explicit kek_id → key_source mapping. This is the
/// default backend when no external KMS is configured.
pub struct IntegratedKmsProvider {
    keys: HashMap<String, [u8; 32]>,
}

impl Drop for IntegratedKmsProvider {
    fn drop(&mut self) {
        for val in self.keys.values_mut() {
            val.zeroize();
        }
    }
}

/// Decode a base64-encoded 256-bit key. The intermediate buffer is zeroized.
fn decode_key(encoded: &str) -> Result<[u8; 32], KmsError> {
    let mut bytes = BASE64
        .decode(encoded.trim())
        .map_err(|e| KmsError::Other(format!("invalid base64 key: {e}")))?;
    let len = bytes.len();
    let result = bytes
        .as_slice()
        .try_into()
        .map_err(|_| KmsError::Other(format!("invalid key length: expected 32 bytes, got {len}")));
    bytes.zeroize();
    result
}

/// Load a key from the given source. The base64 string read from the
/// environment or file is zeroized along with the decoded buffer.
fn resolve_key_source(source: &KeySource) -> Result<[u8; 32], KmsError> {
    match source {
        KeySource::Env { env } => {
            let val =
                Zeroizing::new(std::env::var(env).map_err(|_| {
                    KmsError::Other(format!("environment variable {env:?} not set"))
                })?);
            decode_key(&val)
        }
        KeySource::File { file } => {
            warn_if_key_file_is_open(file);
            let val =
                Zeroizing::new(std::fs::read_to_string(file).map_err(|e| {
                    KmsError::Other(format!("failed to read key file {file:?}: {e}"))
                })?);
            decode_key(&val)
        }
        KeySource::Value { value } => decode_key(value),
    }
}

/// Warn when a key file is readable by group or other. The provider still
/// loads the key -- deployments mount these files in ways we cannot always
/// predict -- but the warning gives operators a clear signal to fix the mode.
#[cfg(unix)]
fn warn_if_key_file_is_open(file: &std::path::Path) {
    use std::os::unix::fs::MetadataExt;
    if let Ok(meta) = std::fs::metadata(file) {
        let mode = meta.mode() & 0o077;
        if mode != 0 {
            tracing::warn!(
                file = %file.display(),
                mode = format!("{:o}", meta.mode() & 0o777),
                "KEK file is readable by group/other; consider mode 0600"
            );
        }
    }
}

#[cfg(not(unix))]
fn warn_if_key_file_is_open(_file: &std::path::Path) {}

impl IntegratedKmsProvider {
    /// IntegratedKmsProvider::from_config builds a
    /// provider from a map of kek_id to key source.
    pub fn from_config(key_map: &HashMap<String, KeySource>) -> Result<Self, KmsError> {
        if key_map.is_empty() {
            return Err(KmsError::Other("no KMS keys configured".to_string()));
        }

        let mut keys = HashMap::with_capacity(key_map.len());
        for (kek_id, source) in key_map {
            let key = resolve_key_source(source)?;
            tracing::info!(kek_id = %kek_id, "loaded KEK");
            keys.insert(kek_id.clone(), key);
        }

        Ok(Self { keys })
    }

    /// IntegratedKmsProvider::new creates a provider
    /// from pre-loaded keys (for tests).
    pub fn new(keys: HashMap<String, [u8; 32]>) -> Self {
        Self { keys }
    }
}

#[async_trait]
impl KmsBackend for IntegratedKmsProvider {
    async fn encrypt_dek(&self, kek_id: &str, dek: &[u8; 32]) -> Result<EncryptedDek, KmsError> {
        let kek = self
            .keys
            .get(kek_id)
            .ok_or_else(|| KmsError::KeyNotFound(kek_id.to_string()))?;
        let (ciphertext, nonce) = crypto::encrypt(kek, dek, b"")?;
        Ok(EncryptedDek { ciphertext, nonce })
    }

    async fn decrypt_dek(
        &self,
        kek_id: &str,
        encrypted: &EncryptedDek,
    ) -> Result<Zeroizing<[u8; 32]>, KmsError> {
        let kek = self
            .keys
            .get(kek_id)
            .ok_or_else(|| KmsError::KeyNotFound(kek_id.to_string()))?;
        let mut plaintext = crypto::decrypt(kek, &encrypted.nonce, &encrypted.ciphertext, b"")?;
        let len = plaintext.len();
        let dek: [u8; 32] = plaintext
            .as_slice()
            .try_into()
            .map_err(|_| KmsError::Other(format!("DEK has wrong length: {len}")))?;
        plaintext.zeroize();
        Ok(Zeroizing::new(dek))
    }

    fn can_decrypt_kek(&self, kek_id: &str) -> bool {
        self.keys.contains_key(kek_id)
    }

    fn kek_ids(&self) -> Vec<String> {
        self.keys.keys().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use serial_test::serial;

    use super::*;

    fn make_test_key(seed: u8) -> [u8; 32] {
        let mut key = [0u8; 32];
        for (i, byte) in key.iter_mut().enumerate() {
            *byte = seed.wrapping_add(i as u8);
        }
        key
    }

    fn encode_key(key: &[u8; 32]) -> String {
        BASE64.encode(key)
    }

    fn make_provider(kek_id: &str, key: [u8; 32]) -> IntegratedKmsProvider {
        let mut keys = HashMap::new();
        keys.insert(kek_id.to_string(), key);
        IntegratedKmsProvider::new(keys)
    }

    // Verifies that encrypt_dek then decrypt_dek
    // recovers the original DEK.
    #[tokio::test]
    async fn encrypt_decrypt_dek_round_trip() {
        let kek = make_test_key(1);
        let provider = make_provider("my-key", kek);

        let dek: [u8; 32] = rand::random();
        let encrypted = provider.encrypt_dek("my-key", &dek).await.expect("encrypt");
        let decrypted = provider
            .decrypt_dek("my-key", &encrypted)
            .await
            .expect("decrypt");

        assert_eq!(*decrypted, dek);
    }

    // Verifies that decrypting a DEK with the wrong
    // kek_id returns an error.
    #[tokio::test]
    async fn decrypt_dek_wrong_kek_id_errors() {
        let provider = make_provider("my-key", make_test_key(1));

        let dek: [u8; 32] = rand::random();
        let encrypted = provider.encrypt_dek("my-key", &dek).await.expect("encrypt");

        let result = provider.decrypt_dek("nonexistent", &encrypted).await;
        assert!(result.is_err());
    }

    // Verifies that can_decrypt_kek returns true for
    // known keys and false for unknown.
    #[test]
    fn can_decrypt_kek_known_and_unknown() {
        let provider = make_provider("my-key", make_test_key(1));

        assert!(provider.can_decrypt_kek("my-key"));
        assert!(!provider.can_decrypt_kek("unknown"));
    }

    // Verifies that generate_and_wrap_dek produces
    // a valid DEK that can be unwrapped.
    #[tokio::test]
    async fn generate_and_wrap_dek_round_trip() {
        let provider = make_provider("my-key", make_test_key(1));

        let (dek, wrapped) = provider
            .generate_and_wrap_dek("my-key")
            .await
            .expect("generate");
        let unwrapped = provider
            .decrypt_dek("my-key", &wrapped)
            .await
            .expect("unwrap");

        assert_eq!(*dek, *unwrapped);
    }

    // Verifies that from_config loads a key from each KeySource variant and
    // rejects sources whose key material is malformed. Loading a key under a
    // kek_id is observable through can_decrypt_kek, so each success row yields
    // whether the configured key is present.
    #[test]
    fn from_config_loads_each_key_source_and_rejects_bad_material() {
        use carbide_test_support::Outcome::*;
        use carbide_test_support::scenarios;

        // A key read from a file: write valid base64 to a temp path the File
        // source then points at. The tempdir lives for the whole table.
        let dir = tempfile::tempdir().expect("tempdir");
        let file_path = dir.path().join("test-key");
        std::fs::write(&file_path, encode_key(&make_test_key(2))).expect("write");

        scenarios!(
            run = |source: KeySource| {
                let key_map = HashMap::from([("key".to_string(), source)]);
                IntegratedKmsProvider::from_config(&key_map)
                    .map(|p| p.can_decrypt_kek("key"))
                    // KmsError isn't PartialEq; the failing rows assert only that
                    // malformed material is rejected, so carry the message as a String.
                    .map_err(|e| e.to_string())
            };
            "a valid key loads from any source" {
                KeySource::File { file: file_path } => Yields(true),
                KeySource::Value { value: encode_key(&make_test_key(3)) } => Yields(true),
            }
            "malformed key material is rejected" {
                // Not valid base64.
                KeySource::Value { value: "not-valid-base64!!!".to_string() } => Fails,
                // Valid base64, but only 16 bytes -- not a 256-bit key.
                KeySource::Value { value: BASE64.encode([0u8; 16]) } => Fails,
            }
        );
    }

    // Verifies that from_config loads keys from env vars. Kept separate from the
    // key-source table because it mutates process-wide environment and so must
    // run serially.
    #[test]
    #[serial]
    fn from_config_env_source() {
        let key = make_test_key(1);
        // SAFETY: Initial lint enablement: `#[serial]` excludes other participating
        // tests, but it does not prove Unix process-wide exclusion from environment
        // readers. This needs owner review.
        unsafe { std::env::set_var("TEST_KMS_KEY_1", encode_key(&key)) };

        let mut key_map = HashMap::new();
        key_map.insert(
            "my-key".to_string(),
            KeySource::Env {
                env: "TEST_KMS_KEY_1".to_string(),
            },
        );

        let provider = IntegratedKmsProvider::from_config(&key_map).expect("from_config");
        assert!(provider.can_decrypt_kek("my-key"));

        // SAFETY: Initial lint enablement: `#[serial]` excludes other participating
        // tests, but it does not prove Unix process-wide exclusion from environment
        // readers. This needs owner review.
        unsafe { std::env::remove_var("TEST_KMS_KEY_1") };
    }

    // Verifies that from_config errors when no keys are provided. Kept separate
    // from the key-source table: this exercises an empty map rather than a single
    // source variant.
    #[test]
    fn from_config_empty_errors() {
        let result = IntegratedKmsProvider::from_config(&HashMap::new());
        assert!(result.is_err());
    }
}

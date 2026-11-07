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

//! MultiKmsProvider aggregates multiple KMS backends, routing
//! writes to the active provider and decrypts to whichever provider
//! owns the kek_id.

use std::sync::Arc;

use async_trait::async_trait;
use zeroize::Zeroizing;

use crate::{EncryptedDek, KmsBackend, KmsError};

/// MultiKmsProvider routes KMS operations across
/// multiple backends. New writes go to the active
/// provider. Decrypts are routed to the provider
/// that owns the kek_id (via can_decrypt_kek).
pub struct MultiKmsProvider {
    active: Arc<dyn KmsBackend>,
    providers: Vec<Arc<dyn KmsBackend>>,
}

impl MultiKmsProvider {
    /// MultiKmsProvider::new creates a multi-provider
    /// from an active provider (for writes) and a list
    /// of all providers (for decrypts). The active
    /// provider should also be in the providers list.
    pub fn new(active: Arc<dyn KmsBackend>, providers: Vec<Arc<dyn KmsBackend>>) -> Self {
        Self { active, providers }
    }

    /// find_provider_for_kek locates the provider
    /// that owns the given kek_id.
    fn find_provider_for_kek(&self, kek_id: &str) -> Result<&Arc<dyn KmsBackend>, KmsError> {
        self.providers
            .iter()
            .find(|p| p.can_decrypt_kek(kek_id))
            .ok_or_else(|| {
                KmsError::KeyNotFound(format!("no KMS provider found for kek_id {kek_id:?}"))
            })
    }
}

#[async_trait]
impl KmsBackend for MultiKmsProvider {
    async fn encrypt_dek(&self, kek_id: &str, dek: &[u8; 32]) -> Result<EncryptedDek, KmsError> {
        self.active.encrypt_dek(kek_id, dek).await
    }

    async fn decrypt_dek(
        &self,
        kek_id: &str,
        encrypted: &EncryptedDek,
    ) -> Result<Zeroizing<[u8; 32]>, KmsError> {
        self.find_provider_for_kek(kek_id)?
            .decrypt_dek(kek_id, encrypted)
            .await
    }

    fn can_decrypt_kek(&self, kek_id: &str) -> bool {
        self.providers.iter().any(|p| p.can_decrypt_kek(kek_id))
    }

    fn kek_ids(&self) -> Vec<String> {
        self.providers.iter().flat_map(|p| p.kek_ids()).collect()
    }

    async fn generate_and_wrap_dek(
        &self,
        kek_id: &str,
    ) -> Result<(Zeroizing<[u8; 32]>, EncryptedDek), KmsError> {
        self.active.generate_and_wrap_dek(kek_id).await
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::IntegratedKmsProvider;

    fn make_test_key(seed: u8) -> [u8; 32] {
        let mut key = [0u8; 32];
        for (i, byte) in key.iter_mut().enumerate() {
            *byte = seed.wrapping_add(i as u8);
        }
        key
    }

    fn make_integrated(kek_id: &str, key: [u8; 32]) -> Arc<dyn KmsBackend> {
        let mut keys = HashMap::new();
        keys.insert(kek_id.to_string(), key);
        Arc::new(IntegratedKmsProvider::new(keys))
    }

    // Verifies that writes go to the active provider
    // and can be read back.
    #[tokio::test]
    async fn writes_go_to_active() {
        let provider_a = make_integrated("key-a", make_test_key(1));
        let provider_b = make_integrated("key-b", make_test_key(2));

        let multi = MultiKmsProvider::new(provider_a.clone(), vec![provider_a, provider_b]);

        let dek: [u8; 32] = rand::random();
        let encrypted = multi.encrypt_dek("key-a", &dek).await.expect("encrypt");
        let decrypted = multi
            .decrypt_dek("key-a", &encrypted)
            .await
            .expect("decrypt");
        assert_eq!(*decrypted, dek);
    }

    // Verifies that decrypts route to the correct
    // provider based on kek_id.
    #[tokio::test]
    async fn decrypt_routes_to_correct_provider() {
        let key_a = make_test_key(1);
        let key_b = make_test_key(2);
        let provider_a = make_integrated("key-a", key_a);
        let provider_b = make_integrated("key-b", key_b);

        // Active is provider_a, but provider_b also participates in decrypts.
        let multi = MultiKmsProvider::new(provider_a.clone(), vec![provider_a, provider_b.clone()]);

        // Encrypt directly with provider_b.
        let dek: [u8; 32] = rand::random();
        let encrypted = provider_b
            .encrypt_dek("key-b", &dek)
            .await
            .expect("encrypt");

        // Multi should route the decrypt to provider_b based on kek_id.
        let decrypted = multi
            .decrypt_dek("key-b", &encrypted)
            .await
            .expect("decrypt");
        assert_eq!(*decrypted, dek);
    }

    // Verifies that can_decrypt_kek returns true if
    // any provider owns the kek_id.
    #[test]
    fn can_decrypt_kek_across_providers() {
        let provider_a = make_integrated("key-a", make_test_key(1));
        let provider_b = make_integrated("key-b", make_test_key(2));

        let multi = MultiKmsProvider::new(provider_a.clone(), vec![provider_a, provider_b]);

        assert!(multi.can_decrypt_kek("key-a"));
        assert!(multi.can_decrypt_kek("key-b"));
        assert!(!multi.can_decrypt_kek("unknown"));
    }

    // Verifies that decrypt_dek errors for an
    // unknown kek_id.
    #[tokio::test]
    async fn decrypt_unknown_kek_id_errors() {
        let provider = make_integrated("key-a", make_test_key(1));
        let multi = MultiKmsProvider::new(provider.clone(), vec![provider]);

        let dek: [u8; 32] = rand::random();
        let encrypted = multi.encrypt_dek("key-a", &dek).await.expect("encrypt");

        let result = multi.decrypt_dek("unknown-key", &encrypted).await;
        assert!(result.is_err());
    }

    // Verifies that generate_and_wrap_dek goes
    // through the active provider.
    #[tokio::test]
    async fn generate_and_wrap_uses_active() {
        let provider_a = make_integrated("key-a", make_test_key(1));
        let provider_b = make_integrated("key-b", make_test_key(2));

        let multi = MultiKmsProvider::new(provider_a.clone(), vec![provider_a, provider_b]);

        let (dek, wrapped) = multi
            .generate_and_wrap_dek("key-a")
            .await
            .expect("generate");
        let unwrapped = multi.decrypt_dek("key-a", &wrapped).await.expect("unwrap");
        assert_eq!(*dek, *unwrapped);
    }

    // Verifies that when two providers claim the
    // same kek_id, the first in the list wins.
    #[tokio::test]
    async fn duplicate_kek_id_uses_first_provider() {
        let key_a = make_test_key(1);
        let key_b = make_test_key(2);

        // Both providers claim "shared-key" but with different key material.
        let provider_a = make_integrated("shared-key", key_a);
        let provider_b = make_integrated("shared-key", key_b);

        let multi = MultiKmsProvider::new(provider_a.clone(), vec![provider_a.clone(), provider_b]);

        // Encrypt with provider_a (via multi's active).
        let dek: [u8; 32] = rand::random();
        let encrypted = multi
            .encrypt_dek("shared-key", &dek)
            .await
            .expect("encrypt");

        // Decrypt should route to provider_a (first in list), which works.
        let decrypted = multi
            .decrypt_dek("shared-key", &encrypted)
            .await
            .expect("decrypt");
        assert_eq!(*decrypted, dek);
    }
}

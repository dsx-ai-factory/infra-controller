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

//! carbide_kms_provider provides a KmsBackend trait
//! for envelope encryption key management. Two
//! implementations are included:
//! - IntegratedKmsProvider: local key material.
//! - TransitKmsProvider: Vault/OpenBao Transit.

use async_trait::async_trait;
use zeroize::Zeroizing;

pub mod crypto;
pub mod providers;

pub use providers::integrated::{IntegratedKmsProvider, KeySource};
pub use providers::multi::MultiKmsProvider;
pub use providers::transit::{DEFAULT_TRANSIT_MOUNT, TransitKmsProvider};

/// EncryptedDek holds a wrapped Data Encryption Key
/// and the nonce used to wrap it. For Transit backends,
/// the nonce is empty (managed internally).
#[derive(Debug)]
pub struct EncryptedDek {
    /// ciphertext contains the encrypted DEK bytes.
    pub ciphertext: Vec<u8>,
    /// nonce contains the nonce used for wrapping
    /// (empty for Transit backends).
    pub nonce: Vec<u8>,
}

/// KmsError represents errors from a KMS backend.
#[derive(Debug, thiserror::Error)]
pub enum KmsError {
    #[error("key not found: {0}")]
    KeyNotFound(String),

    #[error("encryption failed: {0}")]
    EncryptionFailed(String),

    #[error("decryption failed: {0}")]
    DecryptionFailed(String),

    #[error("{0}")]
    Other(String),
}

/// KmsBackend abstracts key encryption key operations
/// for envelope encryption. Implementations handle
/// wrapping and unwrapping DEKs using KEKs identified
/// by kek_id strings.
#[async_trait]
pub trait KmsBackend: Send + Sync {
    /// encrypt_dek wraps a DEK using the KEK
    /// identified by kek_id.
    async fn encrypt_dek(&self, kek_id: &str, dek: &[u8; 32]) -> Result<EncryptedDek, KmsError>;

    /// decrypt_dek unwraps an encrypted DEK using
    /// the KEK identified by kek_id.
    async fn decrypt_dek(
        &self,
        kek_id: &str,
        encrypted: &EncryptedDek,
    ) -> Result<Zeroizing<[u8; 32]>, KmsError>;

    /// can_decrypt_kek returns whether this backend
    /// can decrypt DEKs wrapped by the given kek_id.
    fn can_decrypt_kek(&self, kek_id: &str) -> bool;

    /// kek_ids returns every kek_id this backend answers for. Startup
    /// validation uses this to reject a kek_id configured in more than one
    /// provider, which would make unwraps depend on provider order.
    fn kek_ids(&self) -> Vec<String>;

    /// generate_and_wrap_dek generates a fresh DEK and
    /// wraps it in a single operation. The default
    /// generates locally and delegates to encrypt_dek.
    /// Transit overrides this to use GenerateDataKey.
    async fn generate_and_wrap_dek(
        &self,
        kek_id: &str,
    ) -> Result<(Zeroizing<[u8; 32]>, EncryptedDek), KmsError> {
        let dek = Zeroizing::new(rand::random::<[u8; 32]>());
        let wrapped = self.encrypt_dek(kek_id, &dek).await?;
        Ok((dek, wrapped))
    }
}

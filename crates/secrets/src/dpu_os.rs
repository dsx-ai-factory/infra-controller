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

use rand::rngs::SysRng;
use rand_core::TryRng;
use sha_crypt::{PasswordHashRef, PasswordHasher, PasswordVerifier, ShaCrypt};

use crate::SecretsError;
use crate::credentials::{CredentialKey, CredentialManager, Credentials};

/// Resolve the optional site-wide DPU OS password to a stable SHA-512 crypt hash.
///
/// The password remains a recoverable credential. Its randomly salted installer
/// hash is created once in the same backend, so API restarts and concurrent
/// replicas use the same DPUFlavor. Store errors and inconsistent hashes fail
/// initialization without exposing either credential. Password rotation is not
/// supported; the credential API only permits initial creation.
pub async fn password_hash(
    manager: &dyn CredentialManager,
) -> Result<Option<String>, SecretsError> {
    let Some(Credentials::UsernamePassword { password, .. }) =
        manager.get_credentials(&CredentialKey::DpuOs).await?
    else {
        return Ok(None);
    };

    let cached = manager.get_credentials(&CredentialKey::DpuOsHash).await?;
    let hash = if let Some(Credentials::UsernamePassword { password: hash, .. }) = cached {
        hash
    } else {
        // Twelve random bytes encode to crypt(3)'s maximum of 16 salt characters.
        let mut salt = [0u8; 12];
        SysRng.try_fill_bytes(&mut salt).map_err(|_| {
            SecretsError::GenericError(eyre::eyre!("could not generate the DPU OS password salt"))
        })?;
        let generated = ShaCrypt::SHA512
            .hash_password_with_salt(password.as_bytes(), &salt)
            .map_err(|_| {
                SecretsError::GenericError(eyre::eyre!("could not hash the DPU OS password"))
            })?
            .to_string();
        // create_credentials is insert-only. A competing replica's salt wins;
        // reread that durable value instead of rendering our losing candidate.
        if let Err(error) = manager
            .create_credentials(
                &CredentialKey::DpuOsHash,
                &Credentials::new("ubuntu", &generated),
            )
            .await
        {
            match manager
                .get_credentials_from_writer(&CredentialKey::DpuOsHash)
                .await?
            {
                Some(Credentials::UsernamePassword { password: hash, .. }) => hash,
                None => return Err(error),
            }
        } else {
            generated
        }
    };

    let valid = hash.starts_with("$6$")
        && PasswordHashRef::new(&hash).is_ok_and(|parsed| {
            ShaCrypt::SHA512
                .verify_password(password.as_bytes(), parsed)
                .is_ok()
        });
    if !valid {
        return Err(SecretsError::GenericError(eyre::eyre!(
            "stored DPU OS password hash does not match its credential; direct password changes are unsupported"
        )));
    }
    Ok(Some(hash))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;

    use super::*;
    use crate::credentials::CredentialWriter;
    use crate::test_support::credentials::TestCredentialManager;

    #[tokio::test]
    async fn optional_password_hash_is_persistent_and_matches_the_password() {
        let store = TestCredentialManager::default();
        assert_eq!(password_hash(&store).await.unwrap(), None);
        store
            .create_credentials(
                &CredentialKey::DpuOs,
                &Credentials::new("ubuntu", "example-password"),
            )
            .await
            .unwrap();
        let first = password_hash(&store).await.unwrap().unwrap();
        assert!(first.starts_with("$6$"));
        let salt = first
            .rsplit('$')
            .nth(1)
            .expect("crypt hash contains a salt");
        assert_eq!(salt.len(), 16, "salt must fit Linux crypt(3)'s limit");
        assert!(!first.contains("example-password"));
        assert_eq!(password_hash(&store).await.unwrap().as_ref(), Some(&first));

        store
            .set_credentials(
                &CredentialKey::DpuOs,
                &Credentials::new("ubuntu", "different-password"),
            )
            .await
            .unwrap();
        let error = password_hash(&store).await.unwrap_err().to_string();
        assert!(error.contains("does not match"));
        assert!(!error.contains("different-password"));
    }

    #[tokio::test]
    async fn concurrent_initialization_uses_one_persisted_salt() {
        let store = TestCredentialManager::default();
        store
            .create_credentials(
                &CredentialKey::DpuOs,
                &Credentials::new("ubuntu", "example-password"),
            )
            .await
            .unwrap();
        store
            .set_credentials_sleep_time_ms
            .store(10, Ordering::Release);
        let (first, second) = tokio::join!(password_hash(&store), password_hash(&store));
        assert_eq!(first.unwrap(), second.unwrap());
    }

    #[tokio::test]
    async fn failed_hash_persistence_is_retried_before_provisioning() {
        let store = TestCredentialManager::default();
        store
            .create_credentials(
                &CredentialKey::DpuOs,
                &Credentials::new("ubuntu", "example-password"),
            )
            .await
            .unwrap();
        store.set_create_credentials_failure(true);
        assert!(password_hash(&store).await.is_err());
        store.set_create_credentials_failure(false);
        assert!(password_hash(&store).await.unwrap().is_some());
    }
}

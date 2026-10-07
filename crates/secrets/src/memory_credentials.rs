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

use async_trait::async_trait;
use tokio::sync::RwLock;

use crate::SecretsError;
use crate::credentials::{
    CredentialKey, CredentialManager, CredentialReader, CredentialWriter, Credentials,
};

/// An in-memory credential store that implements both reading and writing.
///
/// Useful in dev/test environments where vault is unavailable. Credentials are
/// held in process memory and lost when the process exits.
#[derive(Default)]
pub struct MemoryCredentialStore {
    store: RwLock<HashMap<String, Credentials>>,
}

#[async_trait]
impl CredentialReader for MemoryCredentialStore {
    async fn get_credentials(
        &self,
        key: &CredentialKey,
    ) -> Result<Option<Credentials>, SecretsError> {
        let store = self.store.read().await;
        Ok(store.get(key.to_key_str().as_ref()).cloned())
    }
}

#[async_trait]
impl CredentialWriter for MemoryCredentialStore {
    async fn get_credentials_from_writer(
        &self,
        key: &CredentialKey,
    ) -> Result<Option<Credentials>, SecretsError> {
        CredentialReader::get_credentials(self, key).await
    }

    async fn set_credentials(
        &self,
        key: &CredentialKey,
        credentials: &Credentials,
    ) -> Result<(), SecretsError> {
        let mut store = self.store.write().await;
        store.insert(key.to_key_str().into_owned(), credentials.clone());
        Ok(())
    }

    async fn create_credentials(
        &self,
        key: &CredentialKey,
        credentials: &Credentials,
    ) -> Result<(), SecretsError> {
        let mut store = self.store.write().await;
        let key_str = key.to_key_str().into_owned();
        if store.contains_key(&key_str) {
            return Err(SecretsError::GenericError(eyre::eyre!(
                "credential already exists at {key_str}"
            )));
        }
        store.insert(key_str, credentials.clone());
        Ok(())
    }

    async fn delete_credentials(&self, key: &CredentialKey) -> Result<(), SecretsError> {
        let mut store = self.store.write().await;
        store.remove(key.to_key_str().as_ref());
        Ok(())
    }
}

impl CredentialManager for MemoryCredentialStore {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credentials::CredentialKey;

    fn cred(username: &str, password: &str) -> Credentials {
        Credentials::UsernamePassword {
            username: username.to_string(),
            password: password.to_string(),
        }
    }

    fn key(fabric: &str) -> CredentialKey {
        CredentialKey::UfmAuth {
            fabric: fabric.to_string(),
        }
    }

    #[tokio::test]
    async fn get_returns_none_when_empty() {
        let store = MemoryCredentialStore::default();
        let result = store.get_credentials(&key("fabric")).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn set_and_get_roundtrip() {
        let store = MemoryCredentialStore::default();
        let k = key("fabric");
        let c = cred("user", "pass");
        store.set_credentials(&k, &c).await.unwrap();
        let result = store.get_credentials(&k).await.unwrap();
        assert_eq!(result, Some(c));
    }

    #[tokio::test]
    async fn set_overwrites_existing() {
        let store = MemoryCredentialStore::default();
        let k = key("fabric");
        store.set_credentials(&k, &cred("u1", "p1")).await.unwrap();
        store.set_credentials(&k, &cred("u2", "p2")).await.unwrap();
        let result = store.get_credentials(&k).await.unwrap();
        assert_eq!(result, Some(cred("u2", "p2")));
    }

    #[tokio::test]
    async fn create_inserts_new_credential() {
        let store = MemoryCredentialStore::default();
        let k = key("fabric");
        let c = cred("user", "pass");
        store.create_credentials(&k, &c).await.unwrap();
        let result = store.get_credentials(&k).await.unwrap();
        assert_eq!(result, Some(c));
    }

    #[tokio::test]
    async fn create_rejects_duplicate() {
        let store = MemoryCredentialStore::default();
        let k = key("fabric");
        let c = cred("user", "pass");
        store.create_credentials(&k, &c).await.unwrap();
        let result = store.create_credentials(&k, &c).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn delete_removes_credential() {
        let store = MemoryCredentialStore::default();
        let k = key("fabric");
        store
            .set_credentials(&k, &cred("user", "pass"))
            .await
            .unwrap();
        store.delete_credentials(&k).await.unwrap();
        let result = store.get_credentials(&k).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn delete_is_idempotent() {
        let store = MemoryCredentialStore::default();
        let k = key("fabric");
        // Deleting a non-existent key should not error.
        store.delete_credentials(&k).await.unwrap();
    }

    #[tokio::test]
    async fn different_keys_are_independent() {
        let store = MemoryCredentialStore::default();
        let k1 = key("fabric-a");
        let k2 = key("fabric-b");
        let c1 = cred("u1", "p1");
        let c2 = cred("u2", "p2");
        store.set_credentials(&k1, &c1).await.unwrap();
        store.set_credentials(&k2, &c2).await.unwrap();
        assert_eq!(store.get_credentials(&k1).await.unwrap(), Some(c1));
        assert_eq!(store.get_credentials(&k2).await.unwrap(), Some(c2));
    }
}

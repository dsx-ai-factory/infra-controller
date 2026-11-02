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
use std::sync::atomic;
use std::sync::atomic::{AtomicBool, AtomicU32};

use async_trait::async_trait;
use tokio::sync::Mutex;

use crate::credentials::{CredentialKey, Credentials};
use crate::{CredentialManager, CredentialReader, CredentialWriter, SecretsError};

#[derive(Default)]
pub struct TestCredentialManager {
    credentials: Mutex<HashMap<String, Credentials>>,
    fallback_credentials: Option<Credentials>,
    pub set_credentials_sleep_time_ms: AtomicU32,
    create_credentials_failure: AtomicBool,
    delete_credentials_failure: AtomicBool,
    set_credentials_failure: AtomicBool,
}

impl TestCredentialManager {
    /// Construct a TestCredentialManager which falls back on a default set of credentials if we
    /// can't find matching ones set via set_credentials()
    pub fn new(fallback_credentials: Credentials) -> Self {
        Self {
            credentials: Mutex::new(HashMap::new()),
            fallback_credentials: Some(fallback_credentials),
            set_credentials_sleep_time_ms: Default::default(),
            create_credentials_failure: Default::default(),
            delete_credentials_failure: Default::default(),
            set_credentials_failure: Default::default(),
        }
    }

    /// Makes `delete_credentials` return an error without removing the stored
    /// credential.
    pub fn set_delete_credentials_failure(&self, fail: bool) {
        self.delete_credentials_failure
            .store(fail, atomic::Ordering::Release);
    }

    /// Makes `set_credentials` return an error without persisting the credential.
    /// Models a credential-store write failure (e.g. Vault unreachable) so the
    /// caller can exercise a persist-failure path without touching a real store.
    pub fn set_set_credentials_failure(&self, fail: bool) {
        self.set_credentials_failure
            .store(fail, atomic::Ordering::Release);
    }

    /// Makes `create_credentials` return an error without persisting the
    /// credential.
    pub fn set_create_credentials_failure(&self, fail: bool) {
        self.create_credentials_failure
            .store(fail, atomic::Ordering::Release);
    }
}

#[async_trait]
impl CredentialReader for TestCredentialManager {
    async fn get_credentials(
        &self,
        key: &CredentialKey,
    ) -> Result<Option<Credentials>, SecretsError> {
        let credentials = self.credentials.lock().await;
        let cred = credentials
            .get(key.to_key_str().as_ref())
            .or(self.fallback_credentials.as_ref());

        Ok(cred.cloned())
    }
}

#[async_trait]
impl CredentialWriter for TestCredentialManager {
    async fn get_credentials_from_writer(
        &self,
        key: &CredentialKey,
    ) -> Result<Option<Credentials>, SecretsError> {
        let credentials = self.credentials.lock().await;
        Ok(credentials.get(key.to_key_str().as_ref()).cloned())
    }

    async fn set_credentials(
        &self,
        key: &CredentialKey,
        credentials: &Credentials,
    ) -> Result<(), SecretsError> {
        let sleep_ms = self
            .set_credentials_sleep_time_ms
            .load(atomic::Ordering::Acquire);
        if sleep_ms > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(sleep_ms as _)).await;
        }
        if self.set_credentials_failure.load(atomic::Ordering::Acquire) {
            return Err(SecretsError::GenericError(eyre::eyre!(
                "test credential set failure"
            )));
        }
        let mut data = self.credentials.lock().await;
        data.insert(key.to_key_str().to_string(), credentials.clone());
        Ok(())
    }

    async fn create_credentials(
        &self,
        key: &CredentialKey,
        credentials: &Credentials,
    ) -> Result<(), SecretsError> {
        let sleep_ms = self
            .set_credentials_sleep_time_ms
            .load(atomic::Ordering::Acquire);
        if sleep_ms > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(sleep_ms as _)).await;
        }
        if self
            .create_credentials_failure
            .load(atomic::Ordering::Acquire)
        {
            return Err(SecretsError::GenericError(eyre::eyre!(
                "test credential create failure"
            )));
        }
        let mut data = self.credentials.lock().await;
        let key_str = key.to_key_str();
        if data.contains_key(key_str.as_ref()) {
            return Err(SecretsError::GenericError(eyre::eyre!(
                "secret already exists with key {key_str}"
            )));
        }

        data.insert(key_str.to_string(), credentials.clone());
        Ok(())
    }

    async fn delete_credentials(&self, key: &CredentialKey) -> Result<(), SecretsError> {
        if self
            .delete_credentials_failure
            .load(atomic::Ordering::Acquire)
        {
            return Err(SecretsError::GenericError(eyre::eyre!(
                "test credential delete failure"
            )));
        }

        let mut data = self.credentials.lock().await;
        let _ = data.remove(key.to_key_str().as_ref());

        Ok(())
    }
}

impl CredentialManager for TestCredentialManager {}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn writer_readback_ignores_reader_fallback() {
        let fallback = Credentials::UsernamePassword {
            username: "fallback".to_string(),
            password: "fallback-password".to_string(),
        };

        let manager = TestCredentialManager::new(fallback);
        let key = CredentialKey::switch_nvos_site_admin(1);

        assert_eq!(
            manager.get_credentials_from_writer(&key).await.unwrap(),
            None
        );

        let persisted = Credentials::UsernamePassword {
            username: "nvos-admin".to_string(),
            password: "target-password".to_string(),
        };

        manager.set_credentials(&key, &persisted).await.unwrap();

        assert_eq!(
            manager.get_credentials_from_writer(&key).await.unwrap(),
            Some(persisted)
        );
    }
}

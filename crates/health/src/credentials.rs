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

use std::time::Duration;

use carbide_secrets::FileCredentialsConfig;
use carbide_secrets::credentials::{
    BmcCredentialType, CredentialKey, CredentialReader, Credentials,
};
use carbide_secrets::local_credentials::{CredentialValue, FileCredentialsWatcher};
use mac_address::MacAddress;
use tokio::sync::OnceCell;

use crate::HealthError;
use crate::bmc::{BoxFuture, CredentialProvider};
use crate::config::StaticBmcEndpoint;
use crate::endpoint::{BmcAddr, BmcCredentials};

pub(crate) struct StaticCredentialProvider {
    key: CredentialKey,
    username: Option<CredentialValue>,
    password: CredentialValue,
    password_present: bool,
    fields: OnceCell<FileCredentialsWatcher>,
    poll_interval: Duration,
}

impl StaticCredentialProvider {
    pub(crate) fn new(config: &StaticBmcEndpoint, mac: MacAddress) -> Self {
        Self {
            key: CredentialKey::BmcCredentials {
                credential_type: BmcCredentialType::BmcRoot {
                    bmc_mac_address: mac,
                },
            },
            username: config
                .username_file
                .as_ref()
                .map(|path| CredentialValue::File(path.clone()))
                .or_else(|| config.username.clone().map(CredentialValue::Inline)),
            // The shared pair type requires a string. Restore `None` below
            // when the target did not configure a password source.
            password: config
                .password_file
                .as_ref()
                .map(|path| CredentialValue::File(path.clone()))
                .unwrap_or_else(|| {
                    CredentialValue::Inline(config.password.clone().unwrap_or_default())
                }),
            password_present: config.password.is_some() || config.password_file.is_some(),
            fields: OnceCell::new(),
            poll_interval: FileCredentialsConfig::default().poll_interval(),
        }
    }

    #[cfg(test)]
    pub(crate) fn from_config(config: &StaticBmcEndpoint, poll_interval: Duration) -> Self {
        let mut provider = Self::new(config, config.mac.parse().expect("fixture MAC parses"));
        provider.poll_interval = poll_interval;
        provider
    }
}

impl CredentialProvider for StaticCredentialProvider {
    fn reload_before_use(&self) -> bool {
        true
    }

    fn fetch_credentials<'a>(
        &'a self,
        _endpoint: &'a BmcAddr,
    ) -> BoxFuture<'a, Result<BmcCredentials, HealthError>> {
        Box::pin(async move {
            let fields = self
                .fields
                .get_or_try_init(|| async {
                    let username = self.username.clone().ok_or_else(|| {
                        HealthError::GenericError("missing static endpoint username".to_string())
                    })?;
                    FileCredentialsWatcher::from_fields(
                        self.key.clone(),
                        username,
                        self.password.clone(),
                        self.poll_interval,
                    )
                    .await
                    .map_err(HealthError::CredentialSource)
                })
                .await?;
            match fields
                .get_credentials(&self.key)
                .await
                .map_err(HealthError::CredentialSource)?
            {
                Some(Credentials::UsernamePassword { username, password }) => {
                    Ok(BmcCredentials::UsernamePassword {
                        username,
                        password: self.password_present.then_some(password),
                    })
                }
                None => Err(HealthError::GenericError(
                    "missing watched static endpoint credentials".to_string(),
                )),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::Yields;
    use carbide_test_support::{Case, check_cases_async};

    use super::*;

    #[tokio::test]
    async fn password_absence_is_distinct_from_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let username = dir.path().join("username");
        let password = dir.path().join("password");
        std::fs::write(&username, "admin").unwrap();
        std::fs::write(&password, "").unwrap();
        check_cases_async([
            Case { scenario: "omitted password", input: None, expect: Yields(None) },
            Case { scenario: "empty password file", input: Some(password), expect: Yields(Some(String::new())) },
        ], |password_file| {
            let username = username.clone();
            async move {
                let config: StaticBmcEndpoint = serde_json::from_value(serde_json::json!({
                    "ip": "127.0.0.1", "mac": "00:11:22:33:44:55", "username_file": username, "password_file": password_file,
                })).unwrap();
                let provider = StaticCredentialProvider::new(&config, config.mac.parse().unwrap());
                let addr = BmcAddr { ip: config.ip, mac: None, port: None };
                match provider.fetch_credentials(&addr).await.map_err(|_| ())? {
                    BmcCredentials::UsernamePassword { username, password } if username == "admin" => Ok(password),
                    _ => Err(()),
                }
            }
        }).await;
    }
}

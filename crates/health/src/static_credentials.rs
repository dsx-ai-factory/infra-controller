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
use std::sync::Arc;
use std::time::Duration;

use carbide_secrets::credentials::{
    BmcCredentialType, CredentialKey, CredentialReader, Credentials,
};
use carbide_secrets::local_credentials::{FileCredentialsConfig, StructuredCredentialReader};

use crate::HealthError;
use crate::bmc::{BoxFuture, CredentialProvider};
use crate::endpoint::{BmcAddr, BmcCredentials};

mod snapshot;

pub(crate) use snapshot::{BmcCredentialConfig, CredentialOverrides};
use snapshot::{BmcCredentialSnapshot, BmcFileCredentialReader};

type Reader = StructuredCredentialReader<BmcCredentialSnapshot>;

/// Shares one eagerly initialized secrets reader across static endpoints.
pub(crate) struct StaticCredentialProvider {
    targets: HashMap<BmcAddr, BmcFileCredentialReader>,
}

fn credential_key(addr: &BmcAddr) -> Result<CredentialKey, HealthError> {
    let mac = addr.mac.ok_or_else(|| {
        HealthError::GenericError("static credentials require a MAC address".into())
    })?;
    Ok(CredentialKey::BmcCredentials {
        credential_type: BmcCredentialType::BmcRoot {
            bmc_mac_address: mac,
        },
    })
}

impl StaticCredentialProvider {
    pub(crate) async fn new(
        path: PathBuf,
        targets: HashMap<BmcAddr, BmcCredentialConfig>,
    ) -> Result<Self, HealthError> {
        let reader = Arc::new(Reader::new_json(FileCredentialsConfig {
            path: Some(path.clone()), poll_interval: Some(Duration::from_secs(60)), ..Default::default()
        }).await.map_err(|error| {
            tracing::error!(path = %path.display(), %error, "failed to load static credential file at startup");
            HealthError::CredentialError(error)
        })?);
        let mut adapters = HashMap::with_capacity(targets.len());
        for (addr, config) in targets {
            let key = credential_key(&addr)?;
            let adapter = BmcFileCredentialReader::new(
                reader.clone(),
                addr.mac.ok_or_else(|| {
                    HealthError::GenericError("static credentials require a MAC address".into())
                })?,
                config,
            );
            adapter.get_credentials(&key).await.map_err(|error| {
                tracing::error!(path = %path.display(), endpoint = ?addr, %error, "invalid static credentials at startup");
                HealthError::CredentialError(error)
            })?;
            adapters.insert(addr, adapter);
        }
        Ok(Self { targets: adapters })
    }
}

impl CredentialProvider for StaticCredentialProvider {
    fn reload_before_use(&self) -> bool {
        true
    }

    fn fetch_credentials<'a>(
        &'a self,
        endpoint: &'a BmcAddr,
    ) -> BoxFuture<'a, Result<BmcCredentials, HealthError>> {
        Box::pin(async move {
            let reader = self.targets.get(endpoint).ok_or_else(|| {
                HealthError::GenericError("static endpoint has no configured credentials".into())
            })?;
            let credentials = reader
                .get_credentials(&credential_key(endpoint)?)
                .await
                .map_err(HealthError::CredentialError)?
                .ok_or_else(|| {
                    HealthError::GenericError(
                        "static endpoint credential key is unsupported".into(),
                    )
                })?;
            let Credentials::UsernamePassword { username, password } = credentials;
            Ok(BmcCredentials::UsernamePassword {
                username,
                password: Some(password),
            })
        })
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    pub(crate) fn write_credentials(path: &std::path::Path, password: &str) {
        std::fs::write(
            path,
            serde_json::json!({"profiles": {"test": {"username": "admin", "password": password}}})
                .to_string(),
        )
        .unwrap();
    }

    pub(crate) async fn provider_for_credentials_file(
        addr: BmcAddr,
        path: PathBuf,
    ) -> Arc<StaticCredentialProvider> {
        let reader = Reader::new_json(FileCredentialsConfig {
            path: Some(path.clone()),
            poll_interval: Some(Duration::from_millis(50)),
            ..Default::default()
        })
        .await
        .expect("test reader");
        let config = BmcCredentialConfig {
            profile: Some("test".into()),
            inline: CredentialOverrides::default(),
        };
        Arc::new(StaticCredentialProvider {
            targets: HashMap::from([(
                addr.clone(),
                BmcFileCredentialReader::new(Arc::new(reader), addr.mac.unwrap(), config),
            )]),
        })
    }

    pub(crate) async fn wait_for_password(
        provider: &dyn CredentialProvider,
        addr: &BmcAddr,
        expected: &str,
    ) {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if matches!(provider.fetch_credentials(addr).await,
                    Ok(BmcCredentials::UsernamePassword { password: Some(actual), .. }) if actual == expected)
                { break; }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await.expect("shared reader reloads credentials");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn endpoints_with_the_same_mac_keep_their_own_inline_credentials() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.json");
        std::fs::write(&path, "{}").unwrap();
        let targets = (1..=2)
            .map(|n| {
                let addr = BmcAddr {
                    ip: format!("192.0.2.{n}").parse().unwrap(),
                    port: None,
                    mac: Some("00:11:22:33:44:55".parse().unwrap()),
                };
                (
                    addr,
                    BmcCredentialConfig {
                        profile: None,
                        inline: CredentialOverrides {
                            username: Some(format!("user-{n}")),
                            password: Some(format!("password-{n}")),
                        },
                    },
                )
            })
            .collect();
        let provider = StaticCredentialProvider::new(path, targets).await.unwrap();
        for n in 1..=2 {
            let addr = BmcAddr {
                ip: format!("192.0.2.{n}").parse().unwrap(),
                port: None,
                mac: Some("00:11:22:33:44:55".parse().unwrap()),
            };
            assert!(
                provider.fetch_credentials(&addr).await.unwrap()
                    == BmcCredentials::UsernamePassword {
                        username: format!("user-{n}"),
                        password: Some(format!("password-{n}")),
                    }
            );
        }
    }
}

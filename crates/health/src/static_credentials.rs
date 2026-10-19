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
use std::time::Duration;

use carbide_secrets::credentials::CredentialReader;
use carbide_secrets::local_credentials::{
    BmcCredentialKey, BmcCredentialSnapshot, FileCredentialsConfig, StructuredCredentialReader,
};

use crate::HealthError;
use crate::bmc::{BoxFuture, CredentialProvider};
use crate::endpoint::{BmcAddr, BmcCredentials};

type Reader = StructuredCredentialReader<BmcCredentialSnapshot>;

/// Shares one eagerly initialized secrets reader across static endpoints.
pub(crate) struct StaticCredentialProvider {
    targets: HashMap<BmcAddr, BmcCredentialKey>,
    reader: Reader,
}

impl StaticCredentialProvider {
    pub(crate) async fn new(
        path: PathBuf,
        targets: HashMap<BmcAddr, BmcCredentialKey>,
    ) -> Result<Self, HealthError> {
        let reader = Reader::new_json(FileCredentialsConfig {
            path: Some(path.clone()), poll_interval: Some(Duration::from_secs(60)), ..Default::default()
        }).await.map_err(|error| {
            tracing::error!(path = %path.display(), %error, "failed to load static credential file at startup");
            HealthError::CredentialError(error)
        })?;
        for key in targets.values() {
            reader.get_credentials(key).await.map_err(|error| {
                tracing::error!(path = %path.display(), %error, "invalid static credential profile at startup");
                HealthError::CredentialError(error)
            })?;
        }
        Ok(Self { targets, reader })
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
            let key = self.targets.get(endpoint).ok_or_else(|| {
                HealthError::GenericError("static endpoint has no configured credentials".into())
            })?;
            let fields = self
                .reader
                .get_credentials(key)
                .await
                .map_err(HealthError::CredentialError)?
                .unwrap_or_default();
            let username = fields.username.ok_or_else(|| {
                HealthError::GenericError(format!(
                    "no username configured for static BMC {}",
                    key.mac
                ))
            })?;
            Ok(BmcCredentials::UsernamePassword {
                username,
                password: fields.password,
            })
        })
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::sync::Arc;

    use carbide_secrets::local_credentials::CredentialOverrides;

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
        let key = BmcCredentialKey {
            mac: addr.mac.unwrap(),
            profile: Some("test".into()),
            inline: CredentialOverrides::default(),
        };
        Arc::new(StaticCredentialProvider {
            targets: HashMap::from([(addr, key)]),
            reader,
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
    use carbide_secrets::local_credentials::CredentialOverrides;

    use super::*;

    #[tokio::test]
    async fn optional_password_and_inline_fallbacks_preserve_absence() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.json");
        std::fs::write(
            &path,
            r#"{
            "overrides": {
                "00:11:22:33:44:02": {"password": ""},
                "00:11:22:33:44:03": {"password": "file-password"}
            }
        }"#,
        )
        .unwrap();
        let fallback = |password: Option<&str>| CredentialOverrides {
            username: Some("admin".into()),
            password: password.map(str::to_owned),
        };
        let addr = |mac: &str| BmcAddr {
            ip: "192.0.2.1".parse().unwrap(),
            port: None,
            mac: Some(mac.parse().unwrap()),
        };
        let provider = StaticCredentialProvider::new(
            path,
            HashMap::from([
                (addr("00:11:22:33:44:01"), fallback(None)),
                (addr("00:11:22:33:44:02"), fallback(Some("inline"))),
                (addr("00:11:22:33:44:04"), fallback(Some("inline"))),
                (addr("00:11:22:33:44:03"), CredentialOverrides::default()),
            ])
            .into_iter()
            .map(|(addr, inline)| {
                let key = BmcCredentialKey {
                    mac: addr.mac.unwrap(),
                    profile: None,
                    inline,
                };
                (addr, key)
            })
            .collect(),
        )
        .await
        .unwrap();
        for (mac, password) in [
            ("00:11:22:33:44:01", None),
            ("00:11:22:33:44:02", Some("")),
            ("00:11:22:33:44:04", Some("inline")),
        ] {
            let addr = BmcAddr {
                ip: "192.0.2.1".parse().unwrap(),
                port: None,
                mac: Some(mac.parse().unwrap()),
            };
            assert!(
                provider.fetch_credentials(&addr).await.unwrap()
                    == BmcCredentials::UsernamePassword {
                        username: "admin".into(),
                        password: password.map(str::to_owned),
                    },
                "{mac}"
            );
        }
        let addr = BmcAddr {
            ip: "192.0.2.1".parse().unwrap(),
            port: None,
            mac: Some("00:11:22:33:44:03".parse().unwrap()),
        };
        assert!(
            provider.fetch_credentials(&addr).await.is_err(),
            "password-only entry requires a username fallback"
        );
    }
}

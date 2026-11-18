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

use carbide_secrets::local_credentials::{FileCredentialsConfig, StructuredCredentialReader};
use mac_address::MacAddress;

use crate::HealthError;
use crate::bmc::{BoxFuture, CredentialProvider};
use crate::endpoint::{BmcAddr, BmcCredentials};

mod snapshot;

use snapshot::BmcCredentialSnapshot;

type Reader = StructuredCredentialReader<BmcCredentialSnapshot>;

#[derive(Debug)]
struct CredentialReaderError(carbide_secrets::SecretsError);

impl std::fmt::Display for CredentialReaderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, f)
    }
}

impl std::error::Error for CredentialReaderError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match &self.0 {
            carbide_secrets::SecretsError::GenericError(report) => Some(report.as_ref()),
            _ => None,
        }
    }
}

impl From<carbide_secrets::SecretsError> for HealthError {
    fn from(error: carbide_secrets::SecretsError) -> Self {
        Self::CredentialError(Box::new(CredentialReaderError(error)))
    }
}

impl From<snapshot::ResolutionError> for HealthError {
    fn from(error: snapshot::ResolutionError) -> Self {
        Self::StaticCredentialResolution(Box::new(error))
    }
}

/// Shares one eagerly initialized secrets reader across static endpoints.
pub(crate) struct StaticCredentialProvider {
    reader: Reader,
    targets: HashMap<MacAddress, Option<String>>,
}

impl StaticCredentialProvider {
    pub(crate) async fn new(
        path: PathBuf,
        configs: Vec<(MacAddress, Option<String>)>,
    ) -> Result<Self, HealthError> {
        let reader = Reader::new(FileCredentialsConfig {
            path: Some(path.clone()), poll_interval: Some(Duration::from_secs(60)), ..Default::default()
        }).await.map_err(|error| {
            tracing::error!(path = %path.display(), %error, "failed to load static credential file at startup");
            HealthError::from(error)
        })?;
        let snapshot = reader.snapshot();
        let mut targets = HashMap::with_capacity(configs.len());
        for (mac, config) in configs {
            snapshot.resolve(mac, config.as_deref()).map_err(|error| {
                tracing::error!(path = %path.display(), mac_address = %mac, %error, "invalid static credentials at startup");
                HealthError::from(error)
            })?;
            targets.insert(mac, config);
        }
        Ok(Self { reader, targets })
    }
}

impl CredentialProvider for StaticCredentialProvider {
    fn fetch_credentials<'a>(
        &'a self,
        endpoint: &'a BmcAddr,
    ) -> BoxFuture<'a, Result<BmcCredentials, HealthError>> {
        Box::pin(async move {
            let mac = endpoint.mac.ok_or_else(|| {
                HealthError::GenericError("static credentials require a MAC address".into())
            })?;
            let config = self.targets.get(&mac).ok_or_else(|| {
                HealthError::GenericError("static endpoint has no configured credentials".into())
            })?;
            self.reader
                .snapshot()
                .resolve(mac, config.as_deref())
                .map_err(HealthError::from)
        })
    }
}

#[cfg(test)]
mod tests {
    use std::error::Error as _;

    use super::*;

    #[tokio::test]
    async fn fetch_preserves_resolution_error_sources() {
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), "{}").unwrap();
        let reader = Reader::new(FileCredentialsConfig {
            path: Some(file.path().into()),
            ..Default::default()
        })
        .await
        .unwrap();
        let mac = "00:11:22:33:44:55".parse().unwrap();
        let addr = BmcAddr {
            ip: "192.0.2.1".parse().unwrap(),
            port: None,
            mac: Some(mac),
        };
        let mut provider = StaticCredentialProvider {
            reader,
            targets: HashMap::new(),
        };
        for profile in [None, Some("missing")] {
            provider.targets.insert(mac, profile.map(str::to_owned));
            let error = provider
                .fetch_credentials(&addr)
                .await
                .err()
                .expect("invalid credentials fail lookup");
            assert!(matches!(&error, HealthError::StaticCredentialResolution(_)));
            let source = error
                .source()
                .unwrap()
                .downcast_ref::<snapshot::ResolutionError>()
                .expect("original resolution error remains available");
            match source {
                snapshot::ResolutionError::UnknownProfile(name) => {
                    assert_eq!(Some(name.as_str()), profile)
                }
                snapshot::ResolutionError::MissingUsername(actual) => {
                    assert!(profile.is_none());
                    assert_eq!(*actual, mac);
                }
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::sync::Arc;

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
        let reader = Reader::new(FileCredentialsConfig {
            path: Some(path.clone()),
            poll_interval: Some(Duration::from_millis(50)),
            ..Default::default()
        })
        .await
        .expect("test reader");
        Arc::new(StaticCredentialProvider {
            reader,
            targets: HashMap::from([(addr.mac.unwrap(), Some("test".into()))]),
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

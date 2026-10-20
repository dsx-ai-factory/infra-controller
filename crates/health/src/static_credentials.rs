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
            HealthError::CredentialError(error)
        })?;
        let snapshot = reader.snapshot();
        let mut targets = HashMap::with_capacity(configs.len());
        for (mac, config) in configs {
            snapshot.resolve(mac, config.as_deref()).map_err(|error| {
                tracing::error!(path = %path.display(), mac_address = %mac, %error, "invalid static credentials at startup");
                HealthError::GenericError(error.to_string())
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
                .map_err(|error| HealthError::GenericError(error.to_string()))
        })
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

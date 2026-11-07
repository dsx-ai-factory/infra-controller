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
use std::fs;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::SystemTime;

use chrono::{DateTime, Utc};
use nonempty::NonEmpty;
use tonic::Status;

use crate::forge_api_client::FailOverOn;
use crate::forge_tls_client::{
    ApiConfig, ForgeClientConfig, ForgeTlsClient, NmxCClientT, RetryConfig,
};
use crate::protos::nmx_c_client::NmxCApiClient;

impl NmxCApiClient {
    pub fn new(nmx_c_config: &ApiConfig<'_>) -> Self {
        Self::build(NmxCTlsConnectionProvider {
            urls: NonEmpty::from((
                nmx_c_config.url.to_string(),
                nmx_c_config.additional_urls.to_vec(),
            )),
            client_config: nmx_c_config.client_config.clone(),
            retry_config: nmx_c_config.retry_config,
            last_connection_index: 0.into(),
            fail_over_on: FailOverOn::ConnectionError,
        })
    }
}

#[derive(Debug)]
pub struct NmxCTlsConnectionProvider {
    pub urls: NonEmpty<String>,
    pub client_config: ForgeClientConfig,
    pub retry_config: RetryConfig,
    pub fail_over_on: FailOverOn,
    pub last_connection_index: AtomicUsize,
}

impl NmxCTlsConnectionProvider {
    fn current_endpoint_url(&self) -> &str {
        // SAFETY: last_connection_index is always modulo urls.len()
        self.urls
            .get(self.last_connection_index.load(Ordering::SeqCst))
            .unwrap()
    }

    fn next_endpoint_url(&self) -> &str {
        let connection_index = self
            .last_connection_index
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |current_index| {
                Some((current_index + 1) % self.urls.len())
            })
            .unwrap(); // SAFETY: we always return Some(), so this will always succeed.
        // SAFETY: connection_index is always modulo urls.len()
        self.urls.get(connection_index).unwrap()
    }
}

#[async_trait::async_trait]
impl tonic_client_wrapper::ConnectionProvider<NmxCClientT> for NmxCTlsConnectionProvider {
    async fn provide_connection(&self) -> Result<NmxCClientT, Status> {
        let mut url = if self.urls.len() <= 1 {
            self.urls.first()
        } else {
            match self.fail_over_on {
                FailOverOn::ConnectionError => self.current_endpoint_url(),
                FailOverOn::EveryApiCall => self.next_endpoint_url(),
            }
        };

        let mut retries = 0;
        loop {
            match ForgeTlsClient::retry_build_nmx_c(
                &ApiConfig::new(url, &self.client_config).with_retry_config(RetryConfig {
                    // We do our own retry counting
                    retries: 1,
                    interval: self.retry_config.interval,
                }),
            )
            .await
            .map_err(Into::into)
            {
                Ok(client) => return Ok(client),
                Err(e) => {
                    retries += 1;
                    if retries > self.retry_config.retries {
                        return Err(e);
                    }
                    url = self.next_endpoint_url();
                }
            }
        }
    }

    async fn connection_is_stale(&self, last_connected: SystemTime) -> bool {
        if matches!(self.fail_over_on, FailOverOn::EveryApiCall) {
            // We can switch between API instances on every API call by just always considering the
            // connection to be stale.
            return true;
        }

        if let Some(ref client_cert) = self.client_config.client_cert {
            if let Ok(mtime) = fs::metadata(&client_cert.cert_path).and_then(|m| m.modified()) {
                if mtime > last_connected {
                    let old_cert_date = DateTime::<Utc>::from(last_connected);
                    let new_cert_date = DateTime::<Utc>::from(mtime);
                    tracing::info!(
                        cert_path = &client_cert.cert_path,
                        %old_cert_date,
                        %new_cert_date,
                        "ForgeApiClient: Reconnecting to pick up newer client certificate"
                    );
                    true
                } else {
                    false
                }
            } else if let Ok(mtime) = fs::metadata(&client_cert.key_path).and_then(|m| m.modified())
            {
                // Just in case the cert and key are created some amount of time apart and we
                // last constructed a client with the new cert but the old key...
                if mtime > last_connected {
                    let old_key_date = DateTime::<Utc>::from(last_connected);
                    let new_key_date = DateTime::<Utc>::from(mtime);
                    tracing::info!(
                        key_path = &client_cert.key_path,
                        %old_key_date,
                        %new_key_date,
                        "ForgeApiClient: Reconnecting to pick up newer client key"
                    );
                    true
                } else {
                    false
                }
            } else {
                false
            }
        } else {
            false
        }
    }

    fn connection_url(&self) -> &str {
        self.current_endpoint_url()
    }
}

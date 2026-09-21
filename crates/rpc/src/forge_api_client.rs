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
use std::fmt::{self, Display};
use std::fs;
use std::str::FromStr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::SystemTime;

use chrono::{DateTime, Utc};
use nonempty::NonEmpty;
use tonic::Status;

use crate::forge_tls_client::{
    ApiConfig, ForgeClientConfig, ForgeClientT, ForgeTlsClient, RetryConfig,
};
pub use crate::protos::forge_api_client::ForgeApiClient;

pub const EXPECTED_SWITCH_UPDATE_MASK_HEADER: &str = "x-nico-expected-switch-update-mask";

/// Field accepted by the sparse expected-switch update contract.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ExpectedSwitchUpdateField {
    /// BMC username.
    BmcUsername,

    /// BMC password.
    BmcPassword,

    /// Switch serial number.
    SwitchSerialNumber,

    /// NVOS MAC addresses.
    NvosMacAddresses,

    /// NVOS username.
    NvosUsername,

    /// NVOS password.
    NvosPassword,

    /// Metadata name.
    MetadataName,

    /// Metadata description.
    MetadataDescription,

    /// Metadata labels.
    MetadataLabels,

    /// Rack ID.
    RackId,

    /// BMC IP address.
    BmcIpAddress,

    /// NVOS IP address.
    NvosIpAddress,

    /// BMC credential-retention setting.
    BmcRetainCredentials,
}

impl Display for ExpectedSwitchUpdateField {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::BmcUsername => "bmc_username",
            Self::BmcPassword => "bmc_password",
            Self::SwitchSerialNumber => "switch_serial_number",
            Self::NvosMacAddresses => "nvos_mac_addresses",
            Self::NvosUsername => "nvos_username",
            Self::NvosPassword => "nvos_password",
            Self::MetadataName => "metadata.name",
            Self::MetadataDescription => "metadata.description",
            Self::MetadataLabels => "metadata.labels",
            Self::RackId => "rack_id",
            Self::BmcIpAddress => "bmc_ip_address",
            Self::NvosIpAddress => "nvos_ip_address",
            Self::BmcRetainCredentials => "bmc_retain_credentials",
        })
    }
}

impl FromStr for ExpectedSwitchUpdateField {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "bmc_username" => Ok(Self::BmcUsername),
            "bmc_password" => Ok(Self::BmcPassword),
            "switch_serial_number" => Ok(Self::SwitchSerialNumber),
            "nvos_mac_addresses" => Ok(Self::NvosMacAddresses),
            "nvos_username" => Ok(Self::NvosUsername),
            "nvos_password" => Ok(Self::NvosPassword),
            "metadata.name" => Ok(Self::MetadataName),
            "metadata.description" => Ok(Self::MetadataDescription),
            "metadata.labels" => Ok(Self::MetadataLabels),
            "rack_id" => Ok(Self::RackId),
            "bmc_ip_address" => Ok(Self::BmcIpAddress),
            "nvos_ip_address" => Ok(Self::NvosIpAddress),
            "bmc_retain_credentials" => Ok(Self::BmcRetainCredentials),
            _ => Err(format!("unknown expected-switch update field: {value}")),
        }
    }
}

/// Builds the sparse-update field mask represented by `switch`.
pub fn expected_switch_update_mask(
    switch: &crate::protos::forge::ExpectedSwitch,
) -> Vec<ExpectedSwitchUpdateField> {
    let mut fields = Vec::new();

    if !switch.bmc_username.is_empty() || !switch.bmc_password.is_empty() {
        fields.extend([
            ExpectedSwitchUpdateField::BmcUsername,
            ExpectedSwitchUpdateField::BmcPassword,
        ]);
    }

    if !switch.switch_serial_number.is_empty() {
        fields.push(ExpectedSwitchUpdateField::SwitchSerialNumber);
    }

    if !switch.nvos_mac_addresses.is_empty() {
        fields.push(ExpectedSwitchUpdateField::NvosMacAddresses);
    }

    if switch.nvos_username.is_some() || switch.nvos_password.is_some() {
        fields.extend([
            ExpectedSwitchUpdateField::NvosUsername,
            ExpectedSwitchUpdateField::NvosPassword,
        ]);
    }

    if let Some(metadata) = &switch.metadata {
        if !metadata.name.is_empty() {
            fields.push(ExpectedSwitchUpdateField::MetadataName);
        }

        if !metadata.description.is_empty() {
            fields.push(ExpectedSwitchUpdateField::MetadataDescription);
        }

        if !metadata.labels.is_empty() {
            fields.push(ExpectedSwitchUpdateField::MetadataLabels);
        }
    }

    if switch.rack_id.is_some() {
        fields.push(ExpectedSwitchUpdateField::RackId);
    }

    if !switch.bmc_ip_address.is_empty() {
        fields.push(ExpectedSwitchUpdateField::BmcIpAddress);
    }

    if switch.nvos_ip_address.is_some() {
        fields.push(ExpectedSwitchUpdateField::NvosIpAddress);
    }

    if switch.bmc_retain_credentials.is_some() {
        fields.push(ExpectedSwitchUpdateField::BmcRetainCredentials);
    }

    fields
}

impl ForgeApiClient {
    pub fn new(api_config: &ApiConfig<'_>) -> Self {
        Self::build(ForgeTlsConnectionProvider {
            urls: NonEmpty::from((
                api_config.url.to_string(),
                api_config.additional_urls.to_vec(),
            )),
            client_config: api_config.client_config.clone(),
            retry_config: api_config.retry_config,
            last_connection_index: 0.into(),
            fail_over_on: FailOverOn::ConnectionError,
        })
    }

    pub fn new_with_failover_behavior(
        api_config: &ApiConfig<'_>,
        fail_over_on: FailOverOn,
    ) -> Self {
        Self::build(ForgeTlsConnectionProvider {
            urls: NonEmpty::from((
                api_config.url.to_string(),
                api_config.additional_urls.to_vec(),
            )),
            client_config: api_config.client_config.clone(),
            retry_config: api_config.retry_config,
            last_connection_index: 0.into(),
            fail_over_on,
        })
    }

    /// Applies the named `ExpectedSwitch` fields without replacing omitted fields.
    /// Uses the legacy update RPC and metadata header for CLI compatibility.
    pub async fn update_expected_switch_with_mask(
        &self,
        switch: crate::protos::forge::ExpectedSwitch,
        update_mask: &[ExpectedSwitchUpdateField],
    ) -> Result<(), Status> {
        let update_mask = update_mask
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(",")
            .parse()
            .map_err(|error| Status::invalid_argument(format!("invalid update mask: {error}")))?;

        let mut request = tonic::Request::new(switch);
        request
            .metadata_mut()
            .insert(EXPECTED_SWITCH_UPDATE_MASK_HEADER, update_mask);

        self.connection()
            .await?
            .update_expected_switch(request)
            .await?;

        Ok(())
    }
}

#[derive(Debug)]
pub struct ForgeTlsConnectionProvider {
    pub urls: NonEmpty<String>,
    pub client_config: ForgeClientConfig,
    pub retry_config: RetryConfig,
    pub fail_over_on: FailOverOn,
    pub last_connection_index: AtomicUsize,
}

#[derive(Debug, Clone, Copy)]
/// Determines when ForgeTlsConnectionProvider should select the next server in the list, if
/// configured for multiple carbide-api servers.
pub enum FailOverOn {
    /// Fail over whenever there is a failure connecting to carbide-api. Note that fail-back is not
    /// (yet) supported.
    ConnectionError,
    /// Select a new carbide-api instance on every call to carbide-api. This is currently only
    /// needed by tests, where we intentionally want to vary the connection to emulate what a load
    /// balancer would do.
    EveryApiCall,
}

impl ForgeTlsConnectionProvider {
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
impl tonic_client_wrapper::ConnectionProvider<ForgeClientT> for ForgeTlsConnectionProvider {
    async fn provide_connection(&self) -> Result<ForgeClientT, Status> {
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
            match ForgeTlsClient::retry_build(
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

#[cfg(test)]
mod tests {
    use carbide_test_support::{Check, check_values};

    use super::*;
    use crate::protos::forge::ExpectedSwitch;

    #[test]
    fn expected_switch_update_field_round_trips() {
        let field = ExpectedSwitchUpdateField::NvosUsername;

        assert_eq!(field.to_string(), "nvos_username");
        assert_eq!("nvos_username".parse(), Ok(field));
        assert!("unknown".parse::<ExpectedSwitchUpdateField>().is_err());
    }

    #[test]
    fn expected_switch_update_mask_contains_only_provided_fields() {
        use ExpectedSwitchUpdateField as Field;

        check_values(
            [
                Check {
                    scenario: "empty patch",
                    input: ExpectedSwitch::default(),
                    expect: Vec::new(),
                },
                Check {
                    scenario: "paired credentials",
                    input: ExpectedSwitch {
                        bmc_username: "bmc-admin".to_string(),
                        bmc_password: "bmc-password".to_string(),
                        nvos_username: Some("nvos-admin".to_string()),
                        nvos_password: Some("nvos-password".to_string()),
                        ..Default::default()
                    },
                    expect: vec![
                        Field::BmcUsername,
                        Field::BmcPassword,
                        Field::NvosUsername,
                        Field::NvosPassword,
                    ],
                },
                Check {
                    scenario: "switch endpoint fields",
                    input: ExpectedSwitch {
                        switch_serial_number: "serial".to_string(),
                        nvos_mac_addresses: vec!["00:11:22:33:44:55".to_string()],
                        bmc_ip_address: "192.0.2.1".to_string(),
                        nvos_ip_address: Some("192.0.2.2".to_string()),
                        bmc_retain_credentials: Some(true),
                        ..Default::default()
                    },
                    expect: vec![
                        Field::SwitchSerialNumber,
                        Field::NvosMacAddresses,
                        Field::BmcIpAddress,
                        Field::NvosIpAddress,
                        Field::BmcRetainCredentials,
                    ],
                },
            ],
            |switch| expected_switch_update_mask(&switch),
        );
    }
}

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

use std::net::SocketAddr;
use std::sync::Arc;

use arc_swap::ArcSwap;
use async_trait::async_trait;
use carbide_secrets::credentials::{CredentialKey, CredentialReader, Credentials};
use carbide_utils::HostPortPair;
use carbide_uuid::machine::MachineId;
use eyre::eyre;

use crate::IPMITool;
use crate::metrics::{IpmiCommand, count_ipmi_command};

/// HTTP-based IPMI implementation for testing with bmc-mock.
/// Sends JSON requests to bmc_proxy which routes to appropriate machine.
pub(super) struct IPMIToolHttpImpl {
    bmc_proxy: Arc<ArcSwap<Option<HostPortPair>>>,
    credential_reader: Arc<dyn CredentialReader>,
}

impl IPMIToolHttpImpl {
    pub(super) fn new(
        bmc_proxy: Arc<ArcSwap<Option<HostPortPair>>>,
        credential_reader: Arc<dyn CredentialReader>,
    ) -> Self {
        Self {
            bmc_proxy,
            credential_reader,
        }
    }

    /// The wire action string bmc-mock's `/ipmi` endpoint expects for each
    /// command -- the counterpart of the real runner's `command_args`.
    fn wire_action(command: IpmiCommand) -> &'static str {
        match command {
            IpmiCommand::ChassisPowerReset => "chassis_power_reset",
            IpmiCommand::DpuLegacyPowerReset => "dpu_legacy_boot",
            IpmiCommand::BmcColdReset => "bmc_cold_reset",
        }
    }

    fn request_target(
        proxy: Option<&HostPortPair>,
        bmc_address: SocketAddr,
    ) -> (String, Option<String>) {
        match proxy {
            Some(proxy) => {
                let host = proxy.url_host().unwrap_or_else(|| "127.0.0.1".into());
                let port = proxy.port().unwrap_or(443);
                (
                    format!("https://{host}:{port}/ipmi"),
                    Some(format!("host={}", bmc_address.ip())),
                )
            }
            None => {
                // The caller supplies an IPMI port; the mock serves HTTPS on 443.
                let https_address = SocketAddr::new(bmc_address.ip(), 443);
                (format!("https://{https_address}/ipmi"), None)
            }
        }
    }

    async fn execute_action(
        &self,
        command: IpmiCommand,
        bmc_address: SocketAddr,
        credential_key: &CredentialKey,
    ) -> Result<(), eyre::Report> {
        let action = Self::wire_action(command);
        let proxy = self.bmc_proxy.load();
        let (url, forwarded_header) = Self::request_target(proxy.as_ref().as_ref(), bmc_address);

        let credentials = self
            .credential_reader
            .get_credentials(credential_key)
            .await
            .map_err(|e| {
                eyre!("secret engine getting credentials for key {credential_key:#?}: {e:#?}")
            })?
            .ok_or_else(|| eyre!("no credentials for key {credential_key:#?} found"))?;
        let Credentials::UsernamePassword { username, password } = credentials;

        let client = reqwest::Client::builder()
            .danger_accept_invalid_certs(true)
            .build()
            .map_err(|e| eyre!("failed to create HTTP client: {}", e))?;

        let mut request = client
            .post(&url)
            .basic_auth(username, Some(password))
            .json(&serde_json::json!({"action": action}));

        if let Some(header) = forwarded_header {
            request = request.header("Forwarded", header);
        }

        // Everything from here on is a dispatched command: the counter covers
        // the wire attempt and its response, not the credential lookup or
        // client construction above (a command that was never sent must not
        // move the metric).
        let result = async {
            let resp = request
                .send()
                .await
                .map_err(|e| eyre!("HTTP request to {} failed: {}", url, e))?;

            if !resp.status().is_success() {
                return Err(eyre!("HTTP error: {}", resp.status()));
            }

            #[derive(serde::Deserialize)]
            struct IpmiHttpResponse {
                success: bool,
                error: Option<String>,
            }

            let body: IpmiHttpResponse = resp
                .json()
                .await
                .map_err(|e| eyre!("failed to parse response: {}", e))?;

            if !body.success {
                return Err(eyre!(
                    "IPMI action failed: {}",
                    body.error.unwrap_or_else(|| "unknown error".to_string())
                ));
            }

            Ok(())
        }
        .await;
        count_ipmi_command(command, &result);
        result
    }
}

#[async_trait]
impl IPMITool for IPMIToolHttpImpl {
    async fn bmc_cold_reset(
        &self,
        bmc_address: SocketAddr,
        credential_key: &CredentialKey,
    ) -> Result<(), eyre::Report> {
        self.execute_action(IpmiCommand::BmcColdReset, bmc_address, credential_key)
            .await
    }

    async fn restart(
        &self,
        _machine_id: &MachineId,
        bmc_address: SocketAddr,
        legacy_boot: bool,
        credential_key: &CredentialKey,
    ) -> Result<(), eyre::Report> {
        if legacy_boot
            && self
                .execute_action(
                    IpmiCommand::DpuLegacyPowerReset,
                    bmc_address,
                    credential_key,
                )
                .await
                .is_ok()
        {
            return Ok(());
        }
        // Fall through to chassis_power_reset if legacy_boot fails or is false
        self.execute_action(IpmiCommand::ChassisPowerReset, bmc_address, credential_key)
            .await
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;

    use super::*;

    #[test]
    fn request_targets_preserve_hosts_ports_and_forwarded_headers() {
        struct Target {
            proxy: Option<&'static str>,
            bmc_address: &'static str,
        }

        value_scenarios!(run = |Target { proxy, bmc_address }| {
            let proxy = proxy.map(|proxy| proxy.parse().expect("valid proxy fixture"));
            let (url, forwarded_header) = IPMIToolHttpImpl::request_target(
                proxy.as_ref(),
                bmc_address.parse().expect("valid BMC socket fixture"),
            );
            let url = reqwest::Url::parse(&url).expect("HTTP IPMI target must be a valid URL");
            (url.to_string(), forwarded_header)
        };
            "IPv6 proxies preserve explicit and default HTTPS ports" {
                Target { proxy: Some("[2001:db8::1]:8443"), bmc_address: "[2001:db8::2]:623" }
                    => ("https://[2001:db8::1]:8443/ipmi".into(), Some("host=2001:db8::2".into())),
                Target { proxy: Some("2001:db8::1"), bmc_address: "[2001:db8::2]:623" }
                    => ("https://[2001:db8::1]/ipmi".into(), Some("host=2001:db8::2".into())),
            }

            "direct BMC requests use HTTPS rather than the IPMI port, without forwarding" {
                Target { proxy: None, bmc_address: "[2001:db8::2]:623" }
                    => ("https://[2001:db8::2]/ipmi".into(), None),
                Target { proxy: None, bmc_address: "192.0.2.2:623" }
                    => ("https://192.0.2.2/ipmi".into(), None),
            }

            "existing proxy forms keep their host and port defaults" {
                Target { proxy: Some("192.0.2.1:8443"), bmc_address: "192.0.2.2:623" }
                    => ("https://192.0.2.1:8443/ipmi".into(), Some("host=192.0.2.2".into())),
                Target { proxy: Some("bmc-proxy.example"), bmc_address: "192.0.2.2:623" }
                    => ("https://bmc-proxy.example/ipmi".into(), Some("host=192.0.2.2".into())),
                Target { proxy: Some(":8443"), bmc_address: "192.0.2.2:623" }
                    => ("https://127.0.0.1:8443/ipmi".into(), Some("host=192.0.2.2".into())),
            }
        );
    }
}

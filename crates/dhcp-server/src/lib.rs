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

pub mod cache;
pub mod errors;
pub mod metrics;
pub mod modes;
pub mod packet_handler;
pub mod packet_handler_v6;
mod rpc;
pub mod util;

use ::rpc::forge_tls_client::ForgeClientConfig;
use carbide_rpc_utils::dhcp::{
    DhcpConfig, DhcpDataError, DhcpV4Config, DhcpV6ServerId, HostConfig,
};

use crate::errors::DhcpError;

/// Runtime configuration shared by the DHCPv4 and DHCPv6 packet paths.
#[derive(Debug, Clone)]
pub struct Config {
    pub(crate) dhcp_config: DhcpConfig,
    pub(crate) host_config: Option<HostConfig>,
    pub(crate) relay_response_port: u16,
    pub(crate) forge_client_config: ForgeClientConfig,
}

impl Config {
    /// `ipv4` returns this generation's DHCPv4 settings, or `None` when
    /// DHCPv4 is disabled. Incomplete IPv4 configuration is an error.
    pub fn ipv4(&self) -> Result<Option<DhcpV4Config>, DhcpDataError> {
        self.dhcp_config.ipv4()
    }

    /// Return this generation's DHCPv6 identity, after retained identity has
    /// been applied by the configuration loader.
    pub fn server_identifier(&self) -> Result<DhcpV6ServerId, DhcpDataError> {
        self.dhcp_config.server_identifier()
    }

    /// Return the preferred and valid stateful DHCPv6 lifetimes in seconds.
    /// Both must be nonzero, and the preferred lifetime must not exceed the valid lifetime.
    pub fn stateful_lifetimes(&self) -> Result<(u32, u32), DhcpError> {
        let preferred_lifetime = self.dhcp_config.dhcpv6_preferred_lifetime_secs;
        let valid_lifetime = self.dhcp_config.dhcpv6_valid_lifetime_secs;
        if preferred_lifetime == 0 || valid_lifetime == 0 || preferred_lifetime > valid_lifetime {
            return Err(DhcpError::InvalidDhcpV6Lifetimes {
                preferred_lifetime_secs: preferred_lifetime,
                valid_lifetime_secs: valid_lifetime,
            });
        }
        Ok((preferred_lifetime, valid_lifetime))
    }

    /// Build one immutable server configuration for a listener generation.
    pub fn new(
        dhcp_config: DhcpConfig,
        host_config: Option<HostConfig>,
        relay_response_port: u16,
        forge_client_config: ForgeClientConfig,
    ) -> Self {
        Self {
            dhcp_config,
            host_config,
            relay_response_port,
            forge_client_config,
        }
    }

    /// Return the DPU-provided host configuration when the server runs in DPU mode.
    pub fn host_config(&self) -> Option<&HostConfig> {
        self.host_config.as_ref()
    }
}

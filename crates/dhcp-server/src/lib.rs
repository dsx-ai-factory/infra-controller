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
use carbide_rpc_utils::dhcp::{DhcpConfig, HostConfig};

/// Runtime configuration shared by the DHCPv4 and DHCPv6 packet paths.
#[derive(Debug, Clone)]
pub struct Config {
    pub(crate) dhcp_config: DhcpConfig,
    pub(crate) host_config: Option<HostConfig>,
    pub(crate) relay_response_port: u16,
    pub(crate) forge_client_config: ForgeClientConfig,
}

impl Config {
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

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
    DhcpConfig, DhcpDataError, DhcpV4Config, DhcpV6ServerId, HostConfig, ServiceConfig,
};
use tokio::net::UdpSocket;
use tokio_util::sync::CancellationToken;

use crate::errors::DhcpError;

/// Validates stateful DHCPv6 lifetimes when a host interface requests an address because
/// stateless-only and service-only generations do not consume them.
pub fn validate_stateful_lifetimes_for_host(
    dhcp_config: &DhcpConfig,
    host_config: Option<&HostConfig>,
) -> Result<(), DhcpError> {
    if host_config.is_some_and(|host| {
        host.host_ip_addresses.values().any(|interface| {
            interface
                .ipv6
                .as_ref()
                .is_some_and(|ipv6| ipv6.address.is_some())
        })
    }) {
        stateful_lifetimes(dhcp_config)?;
    }
    Ok(())
}

/// Returns validated stateful DHCPv6 lifetimes so admission and packet encoding enforce the same
/// constraints.
fn stateful_lifetimes(dhcp_config: &DhcpConfig) -> Result<(u32, u32), DhcpError> {
    let preferred_lifetime = dhcp_config.dhcpv6_preferred_lifetime_secs;
    let valid_lifetime = dhcp_config.dhcpv6_valid_lifetime_secs;
    if preferred_lifetime == 0 || valid_lifetime == 0 || preferred_lifetime > valid_lifetime {
        return Err(DhcpError::InvalidDhcpV6Lifetimes {
            preferred_lifetime_secs: preferred_lifetime,
            valid_lifetime_secs: valid_lifetime,
        });
    }
    Ok((preferred_lifetime, valid_lifetime))
}

/// Runtime configuration shared by the DHCPv4 and DHCPv6 packet paths.
#[derive(Debug, Clone)]
pub struct Config {
    pub(crate) dhcp_config: DhcpConfig,
    pub(crate) host_config: Option<HostConfig>,
    pub(crate) relay_response_port: u16,
    pub(crate) forge_client_config: ForgeClientConfig,
    pub(crate) service_config: ServiceConfig,
    pub(crate) reply_retirement: ReplyRetirement,
}

/// Reports whether an encoded DHCP response reached the socket or was suppressed after its listener
/// generation retired.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplyStatus {
    /// The datagram reached the listener socket.
    Sent,
    /// Generation cancellation won before the socket write completed.
    Retired,
    /// Protocol semantics required silence, so no datagram existed to send.
    NoReply,
}

/// Couples each final socket write to generation cancellation so listener shutdown can retire or
/// finish every response before replacement.
#[derive(Debug, Clone)]
pub(crate) struct ReplyRetirement {
    cancellation: CancellationToken,
}

impl ReplyRetirement {
    /// Creates an active send guard whose cancellation retires every response carrying the cloned
    /// token.
    pub(crate) fn new(cancellation: CancellationToken) -> Self {
        Self { cancellation }
    }

    /// Races one socket write against generation cancellation and prioritizes ready cancellation so
    /// listener shutdown can retire unsent responses.
    pub(crate) async fn send_to(
        &self,
        socket: &UdpSocket,
        packet: &[u8],
        destination: std::net::SocketAddr,
    ) -> Result<ReplyStatus, std::io::Error> {
        tokio::select! {
            biased;
            _ = self.cancellation.cancelled() => Ok(ReplyStatus::Retired),
            result = socket.send_to(packet, destination) => result.map(|_| ReplyStatus::Sent),
        }
    }
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
        stateful_lifetimes(&self.dhcp_config)
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
            service_config: ServiceConfig::default(),
            reply_retirement: ReplyRetirement::new(CancellationToken::new()),
        }
    }

    /// Installs the complete service reservation replacement on this immutable listener generation.
    pub fn with_service_config(mut self, service_config: ServiceConfig) -> Self {
        self.service_config = service_config;
        self
    }

    /// Associates every response with the generation cancellation token so reload retires in-flight
    /// sends.
    pub fn with_reply_retirement(mut self, cancellation: CancellationToken) -> Self {
        self.reply_retirement = ReplyRetirement::new(cancellation);
        self
    }

    /// Returns the service reservation index used by DPU mode for exact interface and MAC lookup.
    pub(crate) fn service_config(&self) -> &ServiceConfig {
        &self.service_config
    }

    /// Return the DPU-provided host configuration when the server runs in DPU mode.
    pub fn host_config(&self) -> Option<&HostConfig> {
        self.host_config.as_ref()
    }
}

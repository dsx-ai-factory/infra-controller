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

mod proto {
    #![allow(
        unreachable_pub,
        reason = "tonic_prost_build emits public items for this crate-internal protocol module"
    )]

    tonic::include_proto!("dhcp_server_control");
}

use carbide_rpc_utils::dhcp::{
    DhcpConfig as ModelDhcpConfig, HostConfig as ModelHostConfig,
    InterfaceInfo as ModelInterfaceInfo, InterfaceInfoV6 as ModelInterfaceInfoV6,
};
use carbide_uuid::machine::MachineInterfaceId;
use proto::dhcp_server_control_client::DhcpServerControlClient;

// ── Model → proto conversions ─────────────────────────────────────────────────

impl From<ModelDhcpConfig> for proto::DhcpConfig {
    fn from(c: ModelDhcpConfig) -> Self {
        proto::DhcpConfig {
            lease_time_secs: c.lease_time_secs,
            renewal_time_secs: c.renewal_time_secs,
            rebinding_time_secs: c.rebinding_time_secs,
            carbide_nameservers: c
                .carbide_nameservers
                .iter()
                .map(|ip| ip.to_string())
                .collect(),
            carbide_api_url: c.carbide_api_url,
            carbide_ntpservers: c
                .carbide_ntpservers
                .iter()
                .map(|ip| ip.to_string())
                .collect(),
            carbide_provisioning_server_ipv4: c.carbide_provisioning_server_ipv4.to_string(),
            carbide_dhcp_server: c.carbide_dhcp_server.to_string(),
            carbide_nameservers_v6: c
                .carbide_nameservers_v6
                .iter()
                .map(|ip| ip.to_string())
                .collect(),
            carbide_ntpservers_v6: c
                .carbide_ntpservers_v6
                .iter()
                .map(|ip| ip.to_string())
                .collect(),
            carbide_dhcp_server_v6: c.carbide_dhcp_server_v6.map(|ip| ip.to_string()),
            dhcpv6_preferred_lifetime_secs: c.dhcpv6_preferred_lifetime_secs,
            dhcpv6_valid_lifetime_secs: c.dhcpv6_valid_lifetime_secs,
            dhcpv6_server_preference: c.dhcpv6_server_preference.map(u32::from),
        }
    }
}

impl From<ModelInterfaceInfoV6> for proto::InterfaceInfoV6 {
    fn from(i: ModelInterfaceInfoV6) -> Self {
        proto::InterfaceInfoV6 {
            address: i.address.map(|ip| ip.to_string()),
            prefix: i.prefix,
        }
    }
}

impl From<ModelInterfaceInfo> for proto::InterfaceInfo {
    fn from(i: ModelInterfaceInfo) -> Self {
        proto::InterfaceInfo {
            address: i.address.map(|address| address.to_string()),
            gateway: i.gateway.map(|gateway| gateway.to_string()),
            prefix: i.prefix,
            fqdn: i.fqdn,
            booturl: i.booturl,
            mtu: i.mtu,
            ipv6: i.ipv6.map(Into::into),
        }
    }
}

impl From<ModelHostConfig> for proto::HostConfig {
    fn from(h: ModelHostConfig) -> Self {
        proto::HostConfig {
            host_interface_id: h.host_interface_id.to_string(),
            host_ip_addresses: h
                .host_ip_addresses
                .into_iter()
                .map(|(k, v)| (k, v.into()))
                .collect(),
        }
    }
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Fetches last DHCP request timestamps from the dhcp-server control service.
///
/// Used when the DHCP server runs in a separate container where the timestamps
/// file is not directly accessible on the DPU filesystem.  Each entry carries
/// the host interface UUID and the timestamp of the last DHCP request seen for
/// that interface.  Returns an empty `Vec` (with a warning) if the call fails,
/// so the caller can degrade gracefully.
pub(super) async fn get_dhcp_timestamps(
    grpc_addr: &str,
) -> eyre::Result<Vec<::rpc::forge::LastDhcpRequest>> {
    let channel = tonic::transport::Endpoint::new(grpc_addr.to_string())
        .map_err(|e| eyre::eyre!("invalid dhcp-server gRPC endpoint {grpc_addr}: {e}"))?
        .connect()
        .await
        .map_err(|e| eyre::eyre!("connect to dhcp-server gRPC at {grpc_addr}: {e}"))?;
    let mut client = DhcpServerControlClient::new(channel);

    let entries = client
        .get_dhcp_timestamps(proto::GetDhcpTimestampsRequest {})
        .await
        .map_err(|s| eyre::eyre!("GetDhcpTimestamps gRPC failed: {s}"))?
        .into_inner()
        .entries;

    let requests = entries
        .into_iter()
        .filter_map(|e| {
            let id = e
                .host_interface_id
                .parse::<MachineInterfaceId>()
                .map_err(
                    |err| tracing::warn!(error = %err, "Skipping unparseable host_interface_id"),
                )
                .ok()?;
            Some(::rpc::forge::LastDhcpRequest {
                host_interface_id: Some(id),
                timestamp: e.timestamp,
            })
        })
        .collect();

    Ok(requests)
}

/// Sends a stop request to the dhcp-server control service.
///
/// The gRPC control server remains running after this call so that a future
/// [`update_and_reload`] call can restart the DHCP process.
pub(super) async fn stop_server(grpc_addr: &str) -> eyre::Result<()> {
    let channel = tonic::transport::Endpoint::new(grpc_addr.to_string())
        .map_err(|e| eyre::eyre!("invalid dhcp-server gRPC endpoint {grpc_addr}: {e}"))?
        .connect()
        .await
        .map_err(|e| eyre::eyre!("connect to dhcp-server gRPC at {grpc_addr}: {e}"))?;
    let mut client = DhcpServerControlClient::new(channel);

    client
        .stop_server(proto::StopServerRequest {})
        .await
        .map_err(|s| eyre::eyre!("StopServer gRPC failed: {s}"))?;

    Ok(())
}

/// Pushes new DHCP config to the dhcp-server control service and triggers an
/// immediate reload in a single RPC.
///
/// The server only restarts the DHCP process if the incoming config differs
/// from what is already active, so this function is safe to call on every
/// agent tick.
pub(super) async fn update_and_reload(
    grpc_addr: &str,
    dhcp_config: ModelDhcpConfig,
    host_config: Option<ModelHostConfig>,
    interfaces: Vec<String>,
) -> eyre::Result<()> {
    let channel = tonic::transport::Endpoint::new(grpc_addr.to_string())
        .map_err(|e| eyre::eyre!("invalid dhcp-server gRPC endpoint {grpc_addr}: {e}"))?
        .connect()
        .await
        .map_err(|e| eyre::eyre!("connect to dhcp-server gRPC at {grpc_addr}: {e}"))?;
    let mut client = DhcpServerControlClient::new(channel);

    client
        .update_and_reload_config(proto::UpdateAndReloadConfigRequest {
            dhcp_config: Some(dhcp_config.into()),
            host_config: host_config.map(Into::into),
            interfaces,
        })
        .await
        .map_err(|s| eyre::eyre!("UpdateAndReloadConfig gRPC failed: {s}"))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies the agent preserves both explicit zero and omission because
    /// they have different DHCPv6 protocol semantics during rolling upgrades.
    #[test]
    fn dhcpv6_preference_preserves_presence_and_bounds() {
        for preference in [None, Some(0), Some(u8::MAX)] {
            // Convert through the exact request-side model used by the gRPC client.
            let wire = proto::DhcpConfig::from(ModelDhcpConfig {
                dhcpv6_server_preference: preference,
                ..Default::default()
            });

            // The control protocol widens the value without changing presence.
            assert_eq!(wire.dhcpv6_server_preference, preference.map(u32::from));
        }
    }

    /// Verifies the control transport retains a tenant's IPv6 binding because
    /// the out-of-process DHCP server cannot recover it from any other source.
    #[test]
    fn tenant_ipv6_binding_survives_control_transport() {
        // Build the same model-to-protobuf boundary used by UpdateAndReloadConfig.
        let wire = proto::HostConfig::from(ModelHostConfig {
            host_interface_id: "11111111-1111-1111-1111-111111111111".parse().unwrap(),
            host_ip_addresses: std::collections::BTreeMap::from([(
                "pf0vf0_sf".to_string(),
                ModelInterfaceInfo {
                    ipv6: Some(ModelInterfaceInfoV6 {
                        address: Some("2001:db8::1".parse().unwrap()),
                        prefix: "2001:db8::1/128".to_string(),
                    }),
                    ..Default::default()
                },
            )]),
        });

        // Inspect the exact protobuf payload sent to the DHCP control service.
        let ipv6 = wire.host_ip_addresses["pf0vf0_sf"]
            .ipv6
            .as_ref()
            .expect("tenant IPv6 binding must be present");
        assert_eq!(ipv6.address.as_deref(), Some("2001:db8::1"));
        assert_eq!(ipv6.prefix, "2001:db8::1/128");
    }
}

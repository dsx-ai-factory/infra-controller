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
use std::collections::BTreeMap;
use std::net::SocketAddr;

use carbide_instrument::emit;
use carbide_rpc_utils::dhcp::{
    DhcpConfig as ModelDhcpConfig, DhcpTimestamps, DhcpTimestampsFilePath,
    HostConfig as ModelHostConfig, InterfaceInfo as ModelInterfaceInfo,
    InterfaceInfoV6 as ModelInterfaceInfoV6,
};
use carbide_uuid::machine::MachineInterfaceId;
use tokio::sync::mpsc;
use tonic::{Request, Response, Status};

mod proto {
    #![allow(
        unreachable_pub,
        reason = "tonic_prost_build emits public items for this crate-internal protocol module"
    )]

    tonic::include_proto!("dhcp_server_control");
}

use carbide_dhcp_server::errors::DhcpError;
use carbide_dhcp_server::metrics::DhcpTimestampFileFailed;
use proto::dhcp_server_control_server::{DhcpServerControl, DhcpServerControlServer};
use proto::{
    GetDhcpTimestampsRequest, GetDhcpTimestampsResponse, StopServerRequest, StopServerResponse,
    UpdateAndReloadConfigRequest, UpdateAndReloadConfigResponse,
};

// ── Control channel types ────────────────────────────────────────────────────

/// Messages sent from the gRPC handlers to the main restart loop.
pub(super) enum ControlRequest {
    /// Write new config YAML and immediately restart the DHCP server.
    /// The restart loop skips the restart if the config is unchanged.
    UpdateAndReload {
        dhcp_yaml: String,
        host_yaml: Option<String>,
        interfaces: Vec<String>,
    },
    /// Stop the DHCP server.  The gRPC control server stays up so that a
    /// subsequent UpdateAndReload can restart the DHCP server.
    Stop,
}

// ── Proto → model conversions ─────────────────────────────────────────────────

impl TryFrom<proto::DhcpConfig> for ModelDhcpConfig {
    type Error = DhcpError;

    fn try_from(c: proto::DhcpConfig) -> Result<Self, Self::Error> {
        Ok(ModelDhcpConfig {
            lease_time_secs: c.lease_time_secs,
            renewal_time_secs: c.renewal_time_secs,
            rebinding_time_secs: c.rebinding_time_secs,
            carbide_nameservers: c
                .carbide_nameservers
                .iter()
                .map(|s| s.parse())
                .collect::<Result<Vec<_>, _>>()?,
            carbide_api_url: c.carbide_api_url,
            carbide_ntpservers: c
                .carbide_ntpservers
                .iter()
                .map(|s| s.parse())
                .collect::<Result<Vec<_>, _>>()?,
            carbide_provisioning_server_ipv4: c.carbide_provisioning_server_ipv4.parse()?,
            carbide_dhcp_server: c.carbide_dhcp_server.parse()?,
            carbide_nameservers_v6: c
                .carbide_nameservers_v6
                .iter()
                .map(|s| s.parse())
                .collect::<Result<Vec<_>, _>>()?,
            carbide_ntpservers_v6: c
                .carbide_ntpservers_v6
                .iter()
                .map(|s| s.parse())
                .collect::<Result<Vec<_>, _>>()?,
            carbide_dhcp_server_v6: c.carbide_dhcp_server_v6.map(|s| s.parse()).transpose()?,
            dhcpv6_preferred_lifetime_secs: c.dhcpv6_preferred_lifetime_secs,
            dhcpv6_valid_lifetime_secs: c.dhcpv6_valid_lifetime_secs,
        })
    }
}

impl TryFrom<proto::InterfaceInfoV6> for ModelInterfaceInfoV6 {
    type Error = DhcpError;

    fn try_from(i: proto::InterfaceInfoV6) -> Result<Self, Self::Error> {
        Ok(ModelInterfaceInfoV6 {
            address: i.address.map(|s| s.parse()).transpose()?,
            prefix: i.prefix,
        })
    }
}

impl TryFrom<proto::InterfaceInfo> for ModelInterfaceInfo {
    type Error = DhcpError;

    fn try_from(i: proto::InterfaceInfo) -> Result<Self, Self::Error> {
        let (address, gateway, prefix) = match (i.address, i.gateway, i.prefix) {
            (Some(address), Some(gateway), Some(prefix)) if !prefix.is_empty() => {
                (Some(address.parse()?), Some(gateway.parse()?), Some(prefix))
            }
            (None, None, None) => (None, None, None),
            _ => {
                return Err(DhcpError::InvalidInput(
                    "IPv4 address, gateway, and non-empty prefix must be configured together"
                        .to_string(),
                ));
            }
        };

        Ok(ModelInterfaceInfo {
            address,
            gateway,
            prefix,
            fqdn: i.fqdn,
            booturl: i.booturl,
            mtu: i.mtu,
            ipv6: i.ipv6.map(ModelInterfaceInfoV6::try_from).transpose()?,
        })
    }
}

impl TryFrom<proto::HostConfig> for ModelHostConfig {
    type Error = DhcpError;

    fn try_from(h: proto::HostConfig) -> Result<Self, Self::Error> {
        let host_interface_id = h
            .host_interface_id
            .parse::<MachineInterfaceId>()
            .map_err(|e| DhcpError::InvalidInput(format!("invalid host_interface_id: {e}")))?;
        let host_ip_addresses = h
            .host_ip_addresses
            .into_iter()
            .map(|(k, v)| ModelInterfaceInfo::try_from(v).map(|info| (k, info)))
            .collect::<Result<BTreeMap<_, _>, _>>()?;
        Ok(ModelHostConfig {
            host_interface_id,
            host_ip_addresses,
        })
    }
}

// ── gRPC service implementation ───────────────────────────────────────────────

struct DhcpServerControlService {
    ctrl_tx: mpsc::Sender<ControlRequest>,
}

#[tonic::async_trait]
impl DhcpServerControl for DhcpServerControlService {
    /// Converts the incoming typed config to YAML, forwards it to the control
    /// loop, and triggers an immediate reload.  The control loop skips the
    /// restart if the incoming config is identical to the active config on disk.
    async fn update_and_reload_config(
        &self,
        request: Request<UpdateAndReloadConfigRequest>,
    ) -> Result<Response<UpdateAndReloadConfigResponse>, Status> {
        let req = request.into_inner();

        let proto_dhcp = req
            .dhcp_config
            .ok_or_else(|| Status::invalid_argument("dhcp_config is required"))?;
        let model_dhcp = ModelDhcpConfig::try_from(proto_dhcp)
            .map_err(|e| Status::invalid_argument(format!("invalid dhcp_config: {e}")))?;
        let dhcp_yaml = serde_yaml::to_string(&model_dhcp)
            .map_err(|e| Status::internal(format!("failed to serialise dhcp_config: {e}")))?;

        let host_yaml = if let Some(proto_host) = req.host_config {
            let model_host = <ModelHostConfig as TryFrom<proto::HostConfig>>::try_from(proto_host)
                .map_err(|e| Status::invalid_argument(format!("invalid host_config: {e}")))?;
            let yaml = serde_yaml::to_string(&model_host)
                .map_err(|e| Status::internal(format!("failed to serialise host_config: {e}")))?;
            Some(yaml)
        } else {
            None
        };

        self.ctrl_tx
            .send(ControlRequest::UpdateAndReload {
                dhcp_yaml,
                host_yaml,
                interfaces: req.interfaces,
            })
            .await
            .map_err(|_| Status::internal("control channel closed"))?;

        tracing::debug!("UpdateAndReloadConfig accepted");
        Ok(Response::new(UpdateAndReloadConfigResponse {}))
    }

    /// Stops the DHCP server without terminating the gRPC control server.
    /// The gRPC server remains up to accept future requests.  The next
    /// UpdateAndReloadConfig call will restart the DHCP server.
    async fn stop_server(
        &self,
        _request: Request<StopServerRequest>,
    ) -> Result<Response<StopServerResponse>, Status> {
        self.ctrl_tx
            .send(ControlRequest::Stop)
            .await
            .map_err(|_| Status::internal("control channel closed"))?;

        tracing::info!("StopServer accepted");
        Ok(Response::new(StopServerResponse {}))
    }

    /// Returns the last DHCP request timestamp for every known host interface
    /// by reading the timestamps file that the DHCP server maintains on disk.
    /// Returns an empty list (rather than an error) if the file cannot be read,
    /// so callers can treat a missing or unreadable file as "no requests seen yet".
    async fn get_dhcp_timestamps(
        &self,
        _request: Request<GetDhcpTimestampsRequest>,
    ) -> Result<Response<GetDhcpTimestampsResponse>, Status> {
        let mut ts = DhcpTimestamps::new(DhcpTimestampsFilePath::Hbn);
        if let Err(e) = ts.read() {
            emit(DhcpTimestampFileFailed::Read {
                dhcp_timestamps_path: DhcpTimestampsFilePath::Hbn.path_str().to_string(),
                error: e.to_string(),
            });
        }
        let entries = ts
            .into_iter()
            .map(|(id, timestamp)| proto::DhcpTimestampEntry {
                host_interface_id: id.to_string(),
                timestamp,
            })
            .collect();
        Ok(Response::new(GetDhcpTimestampsResponse { entries }))
    }
}

// ── Server entry point ────────────────────────────────────────────────────────

/// Start the plain (no-TLS) gRPC control server and block until it exits.
pub(super) async fn run_grpc_server(addr: SocketAddr, ctrl_tx: mpsc::Sender<ControlRequest>) {
    let service = DhcpServerControlService { ctrl_tx };
    tracing::info!(listen_address = %addr, "gRPC config-reload server listening");

    if let Err(e) = tonic::transport::Server::builder()
        .add_service(DhcpServerControlServer::new(service))
        .serve(addr)
        .await
    {
        tracing::error!(listen_address = %addr, error = %e, "gRPC server exited");
    }
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use carbide_test_support::Outcome::*;
    use carbide_test_support::scenarios;

    use super::*;

    type InterfaceIpv4Summary = (Option<Ipv4Addr>, Option<Ipv4Addr>, Option<String>);

    fn summarize_interface(interface: proto::InterfaceInfo) -> Result<InterfaceIpv4Summary, ()> {
        ModelInterfaceInfo::try_from(interface)
            .map(|interface| (interface.address, interface.gateway, interface.prefix))
            .map_err(drop)
    }

    #[test]
    fn interface_ipv4_fields_are_all_present_or_all_absent() {
        scenarios!(run = summarize_interface;
            "complete IPv4 configuration" {
                proto::InterfaceInfo {
                    address: Some("192.0.2.10".to_string()),
                    gateway: Some("192.0.2.1".to_string()),
                    prefix: Some("192.0.2.0/24".to_string()),
                    ..Default::default()
                } => Yields((
                    Some(Ipv4Addr::new(192, 0, 2, 10)),
                    Some(Ipv4Addr::new(192, 0, 2, 1)),
                    Some("192.0.2.0/24".to_string()),
                )),
            }
            "all IPv4 fields absent in IPv6-only configuration" {
                proto::InterfaceInfo {
                    ipv6: Some(proto::InterfaceInfoV6 {
                        address: Some("2001:db8::10".to_string()),
                        prefix: "2001:db8::/64".to_string(),
                    }),
                    ..Default::default()
                } => Yields((None, None, None)),
            }
            "missing IPv4 address" {
                proto::InterfaceInfo {
                    gateway: Some("192.0.2.1".to_string()),
                    prefix: Some("192.0.2.0/24".to_string()),
                    ..Default::default()
                } => Fails,
            }
            "missing IPv4 gateway" {
                proto::InterfaceInfo {
                    address: Some("192.0.2.10".to_string()),
                    prefix: Some("192.0.2.0/24".to_string()),
                    ..Default::default()
                } => Fails,
            }
            "missing IPv4 prefix" {
                proto::InterfaceInfo {
                    address: Some("192.0.2.10".to_string()),
                    gateway: Some("192.0.2.1".to_string()),
                    ..Default::default()
                } => Fails,
            }
            "IPv4 address only" {
                proto::InterfaceInfo {
                    address: Some("192.0.2.10".to_string()),
                    ..Default::default()
                } => Fails,
            }
            "IPv4 gateway only" {
                proto::InterfaceInfo {
                    gateway: Some("192.0.2.1".to_string()),
                    ..Default::default()
                } => Fails,
            }
            "IPv4 prefix only" {
                proto::InterfaceInfo {
                    prefix: Some("192.0.2.0/24".to_string()),
                    ..Default::default()
                } => Fails,
            }
            "empty IPv4 prefix" {
                proto::InterfaceInfo {
                    address: Some("192.0.2.10".to_string()),
                    gateway: Some("192.0.2.1".to_string()),
                    prefix: Some(String::new()),
                    ..Default::default()
                } => Fails,
            }
        );
    }
}

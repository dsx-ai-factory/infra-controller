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
use tokio::sync::{mpsc, oneshot};
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
    /// Stage replacement YAML and apply it when interfaces are supplied.
    /// A running server restarts when configuration or interfaces change.
    UpdateAndReload {
        dhcp_yaml: String,
        host_yaml: Option<String>,
        interfaces: Vec<String>,
        applied: oneshot::Sender<Result<(), ApplyError>>,
    },
    /// Stop the DHCP server.  The gRPC control server stays up so that a
    /// subsequent UpdateAndReload can restart the DHCP server.
    Stop,
}

/// Separates invalid caller configuration from failures reading or applying it.
#[derive(Debug, thiserror::Error)]
pub(super) enum ApplyError {
    #[error("{0}")]
    InvalidConfig(#[source] DhcpError),
    #[error("{0}")]
    Internal(#[from] DhcpError),
}

impl From<ApplyError> for Status {
    fn from(error: ApplyError) -> Self {
        match error {
            ApplyError::InvalidConfig(error) => Status::invalid_argument(error.to_string()),
            ApplyError::Internal(error) => Status::internal(error.to_string()),
        }
    }
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
            carbide_provisioning_server_ipv4: (!c.carbide_provisioning_server_ipv4.is_empty())
                .then(|| c.carbide_provisioning_server_ipv4.parse())
                .transpose()?,
            carbide_provisioning_server_ipv6: c
                .carbide_provisioning_server_ipv6
                .map(|address| address.parse())
                .transpose()?,
            carbide_dhcp_server: (!c.carbide_dhcp_server.is_empty())
                .then(|| c.carbide_dhcp_server.parse())
                .transpose()?,
            dhcpv6_server_id: c.dhcpv6_server_id.map(TryInto::try_into).transpose()?,
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
            dhcpv6_server_preference: c
                .dhcpv6_server_preference
                .map(u8::try_from)
                .transpose()
                .map_err(|_| {
                    DhcpError::InvalidInput(
                        "DHCPv6 server preference must be between 0 and 255".to_string(),
                    )
                })?,
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
    /// Wait for the control loop to persist and apply the config, not for sockets
    /// to become ready. A timeout can leave an already-started apply in progress;
    /// retrying the complete replacement is safe.
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
        model_dhcp
            .ipv4()
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

        let (applied, result) = oneshot::channel();
        self.ctrl_tx
            .try_send(ControlRequest::UpdateAndReload {
                dhcp_yaml,
                host_yaml,
                interfaces: req.interfaces,
                applied,
            })
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => {
                    Status::resource_exhausted("config update queue full")
                }
                mpsc::error::TrySendError::Closed(_) => {
                    Status::unavailable("control channel closed")
                }
            })?;

        // Four queued updates and one active update bound accepted work. The
        // caller waits at most 30 seconds, including time in that queue.
        tokio::time::timeout(std::time::Duration::from_secs(30), result)
            .await
            .map_err(|_| Status::deadline_exceeded("config apply exceeded 30 seconds"))?
            .map_err(|_| Status::unavailable("config apply stopped without a result"))?
            .map_err(Status::from)?;

        tracing::debug!("UpdateAndReloadConfig applied");
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
    use std::time::Duration;

    use carbide_test_support::Outcome::*;
    use carbide_test_support::scenarios;

    use super::*;

    #[test]
    fn control_rpc_rejects_invalid_config_and_applies_a_valid_retry() {
        let directory = tempfile::tempdir().unwrap();
        let live = directory.path().join("dhcp.yaml");
        let host = directory.path().join("host.yaml");
        let staged = directory.path().join("dhcp.yaml_new");
        let staged_host = directory.path().join("host.yaml_new");
        let sidecar = directory.path().join("dhcp.yaml.duid");
        let args = crate::command_line::Args {
            interfaces: Vec::new(),
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            relay_response_port: 67,
            dhcp_config: live.display().to_string(),
            host_config: Some(host.display().to_string()),
            forge_root_ca_path: None,
            client_cert_path: None,
            client_key_path: None,
            mode: crate::command_line::ServerMode::Controller,
            grpc_listen_addr: None,
            metrics_listen_addr: None,
            validate_config: None,
        };
        let reservation = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = reservation.local_addr().unwrap();
        let endpoint =
            tonic::transport::Endpoint::from_shared(format!("http://{address}")).unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        drop(reservation);
        runtime.block_on(async {
            let exercise = async {
                let channel = loop {
                    if let Ok(channel) = endpoint.connect().await {
                        break channel;
                    }
                    tokio::time::sleep(Duration::from_millis(25)).await;
                };
                let mut client = tonic::client::Grpc::new(channel);
                let path = tonic::codegen::http::uri::PathAndQuery::from_static(
                    "/dhcp_server_control.DhcpServerControl/UpdateAndReloadConfig",
                );
                let request = |dhcpv6_server_id| UpdateAndReloadConfigRequest {
                    dhcp_config: Some(proto::DhcpConfig {
                        lease_time_secs: 600,
                        dhcpv6_server_id,
                        ..Default::default()
                    }),
                    host_config: None,
                    interfaces: vec!["lo".to_string()],
                };

                client.ready().await.unwrap();
                let rejected: Result<Response<UpdateAndReloadConfigResponse>, Status> = client
                    .unary(
                        Request::new(request(None)),
                        path.clone(),
                        tonic_prost::ProstCodec::default(),
                    )
                    .await;
                // Missing identity passes RPC conversion and fails in the control loop.
                let status = rejected.unwrap_err();
                assert_eq!(status.code(), tonic::Code::InvalidArgument);
                assert!(status.message().contains("DHCPv6 server identifier"));
                for path in [&live, &host, &staged, &staged_host, &sidecar] {
                    assert!(!path.exists(), "rejected update wrote {}", path.display());
                }

                let identity =
                    carbide_rpc_utils::dhcp::DhcpV6ServerId::from_remote_id("test-dpu").unwrap();
                let mut multiple_interfaces = request(Some(identity.as_bytes().to_vec()));
                multiple_interfaces
                    .interfaces
                    .push("second-interface".to_string());
                client.ready().await.unwrap();
                let rejected: Result<Response<UpdateAndReloadConfigResponse>, Status> = client
                    .unary(
                        Request::new(multiple_interfaces),
                        path.clone(),
                        tonic_prost::ProstCodec::default(),
                    )
                    .await;
                let status = rejected.unwrap_err();
                assert_eq!(status.code(), tonic::Code::InvalidArgument);
                assert!(status.message().contains("only 1 is supported"));
                for path in [&live, &host, &staged, &staged_host, &sidecar] {
                    assert!(
                        !path.exists(),
                        "rejected interfaces wrote {}",
                        path.display()
                    );
                }

                client.ready().await.unwrap();
                let _: Response<UpdateAndReloadConfigResponse> = client
                    .unary(
                        Request::new(request(Some(identity.as_bytes().to_vec()))),
                        path.clone(),
                        tonic_prost::ProstCodec::default(),
                    )
                    .await
                    .unwrap();
                let applied: ModelDhcpConfig =
                    serde_yaml::from_str(&std::fs::read_to_string(&live).unwrap()).unwrap();
                assert_eq!(applied.ipv4().unwrap(), None);
                assert_eq!(applied.lease_time_secs, 600);
                assert_eq!(applied.dhcpv6_server_id.as_ref(), Some(&identity));
                assert_eq!(std::fs::read(&sidecar).unwrap(), identity.as_bytes());
                assert!(!staged.exists());
                assert!(!staged_host.exists());

                // Stored corruption is a server failure even though the caller
                // can provide a valid replacement identity.
                std::fs::write(&sidecar, b"corrupt identity").unwrap();
                client.ready().await.unwrap();
                let rejected: Result<Response<UpdateAndReloadConfigResponse>, Status> = client
                    .unary(
                        Request::new(request(Some(identity.as_bytes().to_vec()))),
                        path,
                        tonic_prost::ProstCodec::default(),
                    )
                    .await;
                let status = rejected.unwrap_err();
                assert_eq!(status.code(), tonic::Code::Internal);
                assert!(status.message().contains("dhcp.yaml.duid"));
                assert_eq!(std::fs::read(&sidecar).unwrap(), b"corrupt identity");
            };
            tokio::select! {
                result = crate::run_with_grpc_control(args, address, 0) => {
                    panic!("control loop exited during configuration updates: {result:?}");
                }
                result = tokio::time::timeout(Duration::from_secs(10), exercise) => {
                    result.expect("configuration RPC sequence did not finish");
                }
            }
        });
        // Stop every background task before its configuration directory is removed.
        drop(runtime);
    }

    #[tokio::test(start_paused = true)]
    async fn update_waits_for_apply_and_returns_its_failure() {
        let identity = carbide_rpc_utils::dhcp::DhcpV6ServerId::from_remote_id("test-dpu").unwrap();
        for applied_result in [
            Ok(()),
            Err(ApplyError::Internal(DhcpError::IoError(
                std::io::Error::other("identity storage unavailable"),
            ))),
        ] {
            let (ctrl_tx, mut ctrl_rx) = mpsc::channel(1);
            let service = DhcpServerControlService { ctrl_tx };
            let request = Request::new(UpdateAndReloadConfigRequest {
                dhcp_config: Some(if applied_result.is_ok() {
                    proto::DhcpConfig {
                        dhcpv6_server_id: Some(identity.as_bytes().to_vec()),
                        ..Default::default()
                    }
                } else {
                    proto::DhcpConfig {
                        carbide_provisioning_server_ipv4: "192.0.2.2".to_string(),
                        carbide_dhcp_server: "192.0.2.1".to_string(),
                        ..Default::default()
                    }
                }),
                host_config: None,
                interfaces: vec!["lo".to_string()],
            });
            let update = service.update_and_reload_config(request);
            tokio::pin!(update);
            let control = tokio::select! {
                result = &mut update => panic!("update returned before apply: {result:?}"),
                control = ctrl_rx.recv() => control.unwrap(),
            };
            assert!(
                tokio::time::timeout(std::time::Duration::from_millis(10), &mut update)
                    .await
                    .is_err()
            );
            let ControlRequest::UpdateAndReload {
                dhcp_yaml, applied, ..
            } = control
            else {
                panic!("expected update control request");
            };
            if applied_result.is_ok() {
                let config: ModelDhcpConfig = serde_yaml::from_str(&dhcp_yaml).unwrap();
                assert_eq!(config.carbide_provisioning_server_ipv4, None);
                assert_eq!(config.carbide_dhcp_server, None);
                assert_eq!(config.dhcpv6_server_id.as_ref(), Some(&identity));
            }
            let expected_error = applied_result.as_ref().err().map(ToString::to_string);
            applied.send(applied_result).unwrap();
            match (expected_error, update.await) {
                (None, Ok(_)) => {}
                (Some(message), Err(status)) => {
                    assert_eq!(status.code(), tonic::Code::Internal);
                    assert_eq!(status.message(), message);
                }
                (_, result) => panic!("unexpected apply response: {result:?}"),
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn update_reports_queue_and_completion_failures() {
        enum Failure {
            Full,
            Closed,
            DroppedResult,
            Deadline,
        }
        for (failure, expected) in [
            (Failure::Full, tonic::Code::ResourceExhausted),
            (Failure::Closed, tonic::Code::Unavailable),
            (Failure::DroppedResult, tonic::Code::Unavailable),
            (Failure::Deadline, tonic::Code::DeadlineExceeded),
        ] {
            let (ctrl_tx, mut ctrl_rx) = mpsc::channel(1);
            let service = DhcpServerControlService { ctrl_tx };
            let update =
                service.update_and_reload_config(Request::new(UpdateAndReloadConfigRequest {
                    dhcp_config: Some(proto::DhcpConfig {
                        carbide_provisioning_server_ipv4: "192.0.2.2".to_string(),
                        carbide_dhcp_server: "192.0.2.1".to_string(),
                        ..Default::default()
                    }),
                    host_config: None,
                    interfaces: vec!["lo".to_string()],
                }));
            tokio::pin!(update);
            let status = match failure {
                Failure::Full => {
                    service.ctrl_tx.try_send(ControlRequest::Stop).unwrap();
                    update.await.unwrap_err()
                }
                Failure::Closed => {
                    drop(ctrl_rx);
                    update.await.unwrap_err()
                }
                Failure::DroppedResult | Failure::Deadline => {
                    let control = tokio::select! {
                        result = &mut update => panic!("update returned before completion: {result:?}"),
                        control = ctrl_rx.recv() => control.unwrap(),
                    };
                    let ControlRequest::UpdateAndReload { applied, .. } = control else {
                        panic!("expected update");
                    };
                    if matches!(failure, Failure::DroppedResult) {
                        drop(applied);
                        update.await.unwrap_err()
                    } else {
                        tokio::time::advance(Duration::from_secs(30)).await;
                        let status = update.await.unwrap_err();
                        assert!(
                            applied.is_closed(),
                            "expired caller must abandon its queued work"
                        );
                        status
                    }
                }
            };
            assert_eq!(status.code(), expected);
        }
    }

    type InterfaceIpv4Summary = (Option<Ipv4Addr>, Option<Ipv4Addr>, Option<String>);

    fn summarize_interface(interface: proto::InterfaceInfo) -> Result<InterfaceIpv4Summary, ()> {
        ModelInterfaceInfo::try_from(interface)
            .map(|interface| (interface.address, interface.gateway, interface.prefix))
            .map_err(drop)
    }

    // IPv6 provisioning changes must not swap the distinct DHCPv4 server and
    // provisioning addresses as the control request becomes a model.
    #[test]
    fn provisioning_ipv6_requires_an_ipv6_address_when_present() {
        scenarios!(run = |address: Option<&str>| {
                ModelDhcpConfig::try_from(proto::DhcpConfig {
                    carbide_provisioning_server_ipv4: "192.0.2.10".to_string(),
                    carbide_dhcp_server: "192.0.2.1".to_string(),
                    carbide_provisioning_server_ipv6: address.map(str::to_string),
                    ..Default::default()
                })
                    .map(|config| {
                        assert_eq!(config.carbide_dhcp_server, Some(Ipv4Addr::new(192, 0, 2, 1)));
                        assert_eq!(
                            config.carbide_provisioning_server_ipv4,
                            Some(Ipv4Addr::new(192, 0, 2, 10)),
                        );
                        config.carbide_provisioning_server_ipv6
                    })
                    .map_err(drop)
            };
            "optional IPv6 provisioning source" {
                None => Yields(None),
                Some("2001:db8::80") => Yields(Some("2001:db8::80".parse().unwrap())),
            }
            "invalid IPv6 provisioning source" {
                Some("192.0.2.10") => Fails,
                Some("") => Fails,
            }
        );
    }

    /// Verifies the control boundary accepts the complete Preference range,
    /// preserves legacy omission, and rejects values the packet cannot encode.
    #[test]
    fn dhcpv6_preference_validates_control_protocol_range() {
        scenarios!(run = |preference| {
                let config = proto::DhcpConfig {
                    carbide_provisioning_server_ipv4: "192.0.2.10".to_string(),
                    carbide_dhcp_server: "192.0.2.1".to_string(),
                    dhcpv6_server_preference: preference,
                    ..Default::default()
                };
                ModelDhcpConfig::try_from(config)
                    .map(|config| config.dhcpv6_server_preference)
                    .map_err(drop)
            };
            "legacy omission" {
                // A missing field must keep omitting the wire option.
                None => Yields(None),
            }
            "valid configured values" {
                // Explicit zero is distinct from omission even though both are effective zero.
                Some(0) => Yields(Some(0)),
                // The upper protocol boundary must survive the widened control field.
                Some(255) => Yields(Some(255)),
            }
            "out of range" {
                // Values above one octet cannot be encoded as DHCPv6 Preference.
                Some(256) => Fails,
            }
        );
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

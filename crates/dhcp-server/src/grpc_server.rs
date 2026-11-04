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
    InterfaceInfoV6 as ModelInterfaceInfoV6, ServiceConfig as ModelServiceConfig,
    ServiceInterfaceReservations as ModelServiceInterfaceReservations,
    ServiceLink as ModelServiceLink, ServiceLinkV4 as ModelServiceLinkV4,
    ServiceLinkV6 as ModelServiceLinkV6, ServiceReservation as ModelServiceReservation,
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
use carbide_dhcp_server::modes::dpu::get_host_config;
use carbide_dhcp_server::validate_stateful_lifetimes_for_host;
use proto::dhcp_server_control_server::{DhcpServerControl, DhcpServerControlServer};
use proto::{
    GetDhcpTimestampsRequest, GetDhcpTimestampsResponse, StopServerRequest, StopServerResponse,
    UpdateAndReloadConfigRequest, UpdateAndReloadConfigResponse,
};

// ── Control channel types ────────────────────────────────────────────────────

/// Messages sent from the gRPC handlers to the main restart loop.
pub(super) enum ControlRequest {
    /// Stages replacement YAML and applies it using supplied interfaces, or remembered interfaces
    /// when a running server's service reservations change.
    UpdateAndReload {
        dhcp_yaml: String,
        host_yaml: Option<String>,
        interfaces: Vec<String>,
        service_config: ModelServiceConfig,
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

impl TryFrom<proto::ServiceLinkV4> for ModelServiceLinkV4 {
    type Error = DhcpError;

    /// Parses a DHCPv4 service link while deferring point-to-point topology validation to
    /// ServiceConfig.
    fn try_from(link: proto::ServiceLinkV4) -> Result<Self, Self::Error> {
        Ok(Self {
            address: link.address.parse()?,
            prefix: link.prefix.parse().map_err(|error| {
                DhcpError::InvalidInput(format!("invalid IPv4 service prefix: {error}"))
            })?,
            gateway: link.gateway.parse()?,
        })
    }
}

impl TryFrom<proto::ServiceLinkV6> for ModelServiceLinkV6 {
    type Error = DhcpError;

    /// Parses a DHCPv6 service link while deferring point-to-point topology validation to
    /// ServiceConfig.
    fn try_from(link: proto::ServiceLinkV6) -> Result<Self, Self::Error> {
        Ok(Self {
            address: link.address.parse()?,
            prefix: link.prefix.parse().map_err(|error| {
                DhcpError::InvalidInput(format!("invalid IPv6 service prefix: {error}"))
            })?,
            hbn_peer: link.hbn_peer.parse()?,
        })
    }
}

impl TryFrom<proto::ServiceReservation> for ModelServiceReservation {
    type Error = DhcpError;

    /// Converts stable service identity and requires one explicit address-family link so incomplete
    /// reservations never enter the control queue.
    fn try_from(reservation: proto::ServiceReservation) -> Result<Self, Self::Error> {
        // Require the caller to choose one family before parsing the shared identity fields.
        let link = match reservation.link {
            Some(proto::service_reservation::Link::Ipv4(link)) => {
                ModelServiceLink::Ipv4(link.try_into()?)
            }
            Some(proto::service_reservation::Link::Ipv6(link)) => {
                ModelServiceLink::Ipv6(link.try_into()?)
            }
            None => {
                return Err(DhcpError::InvalidInput(
                    "service reservation link is required".to_string(),
                ));
            }
        };
        Ok(Self {
            attachment_id: reservation.attachment_id.parse().map_err(|error| {
                DhcpError::InvalidInput(format!("invalid service attachment_id: {error}"))
            })?,
            interface_ordinal: reservation.interface_ordinal,
            internal_uuid: reservation.internal_uuid.parse().map_err(|error| {
                DhcpError::InvalidInput(format!("invalid service internal_uuid: {error}"))
            })?,
            mac_address: reservation.mac_address.parse().map_err(|error| {
                DhcpError::InvalidInput(format!("invalid service mac_address: {error}"))
            })?,
            link,
        })
    }
}

impl TryFrom<proto::ServiceConfig> for ModelServiceConfig {
    type Error = DhcpError;

    /// Builds the immutable exact-match service index so duplicate slots and invalid links fail
    /// before reload admission.
    fn try_from(config: proto::ServiceConfig) -> Result<Self, Self::Error> {
        // Convert every group before indexing so malformed nested values cannot leave a partial replacement.
        let groups = config
            .interfaces
            .into_iter()
            .map(|group| {
                Ok(ModelServiceInterfaceReservations {
                    receiving_interface: group.receiving_interface,
                    reservations: group
                        .reservations
                        .into_iter()
                        .map(TryInto::try_into)
                        .collect::<Result<_, DhcpError>>()?,
                })
            })
            .collect::<Result<_, DhcpError>>()?;
        Ok(ModelServiceConfig::try_new(groups)?)
    }
}

// ── gRPC service implementation ───────────────────────────────────────────────

struct DhcpServerControlService {
    ctrl_tx: mpsc::Sender<ControlRequest>,
    retained_host_config_path: Option<String>,
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
        let model_host = req
            .host_config
            .map(<ModelHostConfig as TryFrom<proto::HostConfig>>::try_from)
            .transpose()
            .map_err(|e| Status::invalid_argument(format!("invalid host_config: {e}")))?;
        // Omission retains the durable host file, so lifetime admission must inspect that state before transferring queue ownership.
        let retained_host = if model_host.is_none() {
            match get_host_config(self.retained_host_config_path.clone()).await {
                Ok(host) => host,
                Err(DhcpError::MissingArgument(_)) => None,
                Err(DhcpError::IoError(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                    None
                }
                Err(error) => {
                    return Err(Status::internal(format!(
                        "failed to load retained host_config: {error}"
                    )));
                }
            }
        } else {
            None
        };
        validate_stateful_lifetimes_for_host(
            &model_dhcp,
            model_host.as_ref().or(retained_host.as_ref()),
        )
        .map_err(|e| Status::invalid_argument(format!("invalid dhcp_config: {e}")))?;

        let dhcp_yaml = serde_yaml::to_string(&model_dhcp)
            .map_err(|e| Status::internal(format!("failed to serialise dhcp_config: {e}")))?;
        let host_yaml = model_host
            .map(|host| serde_yaml::to_string(&host))
            .transpose()
            .map_err(|e| Status::internal(format!("failed to serialise host_config: {e}")))?;

        // The request is a full replacement, so protobuf omission and an explicit empty message both clear reservations.
        let service_config = req
            .service_config
            .map(ModelServiceConfig::try_from)
            .transpose()
            .map_err(|e| Status::invalid_argument(format!("invalid service_config: {e}")))?
            .unwrap_or_default();
        service_config
            .validate_receiving_interfaces(&req.interfaces)
            .map_err(|e| Status::invalid_argument(format!("invalid service_config: {e}")))?;

        let (applied, result) = oneshot::channel();
        self.ctrl_tx
            .try_send(ControlRequest::UpdateAndReload {
                dhcp_yaml,
                host_yaml,
                interfaces: req.interfaces,
                service_config,
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
pub(super) async fn run_grpc_server(
    addr: SocketAddr,
    ctrl_tx: mpsc::Sender<ControlRequest>,
    retained_host_config_path: Option<String>,
) {
    let service = DhcpServerControlService {
        ctrl_tx,
        retained_host_config_path,
    };
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
    use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4};
    use std::num::NonZeroUsize;
    use std::sync::Arc;
    use std::time::Duration;

    use carbide_instrument::testing::MetricsCapture;
    use carbide_test_support::Outcome::*;
    use carbide_test_support::scenarios;
    use dhcproto::v4::{DhcpOption, Message, MessageType};
    use dhcproto::{Decodable, Decoder, Encodable, Encoder};
    use lru::LruCache;
    use tokio::net::UdpSocket;
    use tokio::sync::Mutex;

    use super::*;

    /// Builds one valid protobuf service reservation so RPC rejection cases alter only their
    /// targeted caller error.
    fn service_reservation() -> proto::ServiceReservation {
        proto::ServiceReservation {
            attachment_id: "11111111-1111-1111-1111-111111111111".to_string(),
            interface_ordinal: 0,
            internal_uuid: "22222222-2222-2222-2222-222222222222".to_string(),
            mac_address: "02:00:00:00:00:01".to_string(),
            link: Some(proto::service_reservation::Link::Ipv4(
                proto::ServiceLinkV4 {
                    address: "192.0.2.11".to_string(),
                    prefix: "192.0.2.10/31".to_string(),
                    gateway: "192.0.2.10".to_string(),
                },
            )),
        }
    }

    /// Builds one stateful IPv6 host interface so admission tests exercise lifetime validation
    /// rather than host conversion failures.
    fn stateful_host_config() -> proto::HostConfig {
        proto::HostConfig {
            host_interface_id: "55555555-5555-5555-5555-555555555555".to_string(),
            host_ip_addresses: [(
                "lo".to_string(),
                proto::InterfaceInfo {
                    ipv6: Some(proto::InterfaceInfoV6 {
                        address: Some("2001:db8::2".to_string()),
                        prefix: "2001:db8::/64".to_string(),
                    }),
                    ..Default::default()
                },
            )]
            .into(),
        }
    }

    /// Exchanges one service DISCOVER through the live DPU listener so control-path tests observe
    /// the published reservation rather than only its files.
    async fn exchange_service_packet(
        listener: SocketAddrV4,
        receiver: &UdpSocket,
        transaction_id: u32,
    ) -> Message {
        // Address the reply through loopback giaddr so the test can receive it without binding the privileged client port.
        let mut request = Message::new(
            Ipv4Addr::UNSPECIFIED,
            Ipv4Addr::UNSPECIFIED,
            Ipv4Addr::UNSPECIFIED,
            Ipv4Addr::LOCALHOST,
            &[0x02, 0, 0, 0, 0, 1],
        );
        request
            .opts_mut()
            .insert(DhcpOption::MessageType(MessageType::Discover));
        request.set_xid(transaction_id);
        let mut encoded = Vec::new();
        request.encode(&mut Encoder::new(&mut encoded)).unwrap();
        let sender = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);

        // Retry while the applied generation binds its socket after the control RPC returns.
        loop {
            sender.send_to(&encoded, listener).await.unwrap();
            let mut response = [0; 1500];
            if let Ok(Ok((length, _))) =
                tokio::time::timeout(Duration::from_millis(50), receiver.recv_from(&mut response))
                    .await
            {
                let response = Message::decode(&mut Decoder::new(&response[..length])).unwrap();
                if response.xid() == transaction_id {
                    return response;
                }
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "service-bearing DPU generation did not answer DHCP"
            );
        }
    }

    /// Verifies protobuf conversion preserves a complete IPv6 service link so family-aware
    /// validation receives its typed identity.
    #[test]
    fn service_config_converts_ipv6_wire_link() {
        // Replace only the link family and ordinal while retaining the valid shared identity fixture.
        let mut reservation = service_reservation();
        reservation.interface_ordinal = 7;
        reservation.link = Some(proto::service_reservation::Link::Ipv6(
            proto::ServiceLinkV6 {
                address: "2001:db8::11".to_string(),
                prefix: "2001:db8::10/127".to_string(),
                hbn_peer: "2001:db8::10".to_string(),
            },
        ));

        // Convert the complete wire replacement through the same path used before queue admission.
        let config = ModelServiceConfig::try_from(proto::ServiceConfig {
            interfaces: vec![proto::ServiceInterfaceReservations {
                receiving_interface: "lo".to_string(),
                reservations: vec![reservation],
            }],
        })
        .unwrap();
        let mac_address = "02:00:00:00:00:01".parse().unwrap();
        let reservation = config.get("lo", &mac_address).unwrap();

        // The typed reservation must retain the exact IPv6 endpoint contract and stable identities.
        assert_eq!(
            reservation.attachment_id.to_string(),
            "11111111-1111-1111-1111-111111111111"
        );
        assert_eq!(
            reservation.internal_uuid.to_string(),
            "22222222-2222-2222-2222-222222222222"
        );
        assert_eq!(reservation.interface_ordinal, 7);
        assert!(matches!(
            &reservation.link,
            ModelServiceLink::Ipv6(link)
                if link.address == "2001:db8::11".parse::<Ipv6Addr>().unwrap()
                    && link.prefix
                        == "2001:db8::10/127"
                            .parse::<ipnetwork::Ipv6Network>()
                            .unwrap()
                    && link.hbn_peer == "2001:db8::10".parse::<Ipv6Addr>().unwrap()
        ));
    }

    /// Verifies malformed service replacements return INVALID_ARGUMENT before they consume
    /// control-loop queue capacity.
    #[tokio::test]
    async fn service_config_validation_precedes_control_queue_admission() {
        struct Case {
            name: &'static str,
            service_config: proto::ServiceConfig,
        }

        // Remove the oneof while retaining valid identity fields to isolate required-link validation.
        let mut missing_link = service_reservation();
        missing_link.link = None;
        // Corrupt only the attachment identity to prove protobuf conversion rejects it before semantic validation.
        let mut malformed_attachment_id = service_reservation();
        malformed_attachment_id.attachment_id = "not-a-uuid".to_string();
        // Corrupt only the internal identity so it cannot be confused with attachment parsing.
        let mut malformed_internal_uuid = service_reservation();
        malformed_internal_uuid.internal_uuid = "not-a-uuid".to_string();
        // Corrupt only the client address to prove an unparseable MAC never enters exact lookup state.
        let mut malformed_mac = service_reservation();
        malformed_mac.mac_address = "not-a-mac".to_string();
        // Preserve the exact lookup key while changing both stable identities to isolate duplicate-MAC validation.
        let valid = service_reservation();
        let mut duplicate_mac = valid.clone();
        duplicate_mac.attachment_id = "33333333-3333-3333-3333-333333333333".to_string();
        duplicate_mac.internal_uuid = "44444444-4444-4444-4444-444444444444".to_string();
        // Reverse only the service endpoint so parsing succeeds before topology validation rejects it.
        let mut wrong_address = service_reservation();
        let Some(proto::service_reservation::Link::Ipv4(link)) = &mut wrong_address.link else {
            unreachable!();
        };
        link.address = "192.0.2.10".to_string();
        let cases = [
            // Missing oneof data must fail before semantic validation because no address family can serve the client.
            Case {
                name: "missing service link",
                service_config: proto::ServiceConfig {
                    interfaces: vec![proto::ServiceInterfaceReservations {
                        receiving_interface: "lo".to_string(),
                        reservations: vec![missing_link],
                    }],
                },
            },
            // An invalid attachment UUID must fail before semantic validation can accept its link.
            Case {
                name: "malformed attachment UUID",
                service_config: proto::ServiceConfig {
                    interfaces: vec![proto::ServiceInterfaceReservations {
                        receiving_interface: "lo".to_string(),
                        reservations: vec![malformed_attachment_id],
                    }],
                },
            },
            // An invalid internal UUID must fail independently from the valid attachment identity.
            Case {
                name: "malformed internal UUID",
                service_config: proto::ServiceConfig {
                    interfaces: vec![proto::ServiceInterfaceReservations {
                        receiving_interface: "lo".to_string(),
                        reservations: vec![malformed_internal_uuid],
                    }],
                },
            },
            // An invalid MAC must fail before a string can become an exact reservation key.
            Case {
                name: "malformed MAC",
                service_config: proto::ServiceConfig {
                    interfaces: vec![proto::ServiceInterfaceReservations {
                        receiving_interface: "lo".to_string(),
                        reservations: vec![malformed_mac],
                    }],
                },
            },
            // Duplicate exact keys must not reach a queue where input order could select an arbitrary attachment.
            Case {
                name: "duplicate service MAC",
                service_config: proto::ServiceConfig {
                    interfaces: vec![proto::ServiceInterfaceReservations {
                        receiving_interface: "lo".to_string(),
                        reservations: vec![valid, duplicate_mac],
                    }],
                },
            },
            // A reversed /31 endpoint must fail before reload because packet serving assumes base-plus-one ownership.
            Case {
                name: "invalid service topology",
                service_config: proto::ServiceConfig {
                    interfaces: vec![proto::ServiceInterfaceReservations {
                        receiving_interface: "lo".to_string(),
                        reservations: vec![wrong_address],
                    }],
                },
            },
            // A reservation for an unprovisioned listener must fail without querying live Kubernetes interface state.
            Case {
                name: "unprovisioned receiving interface",
                service_config: proto::ServiceConfig {
                    interfaces: vec![proto::ServiceInterfaceReservations {
                        receiving_interface: "pf0sf99".to_string(),
                        reservations: vec![service_reservation()],
                    }],
                },
            },
        ];

        // Submit each invalid replacement with otherwise complete DHCP settings.
        for case in cases {
            let (ctrl_tx, mut ctrl_rx) = mpsc::channel(1);
            let service = DhcpServerControlService {
                ctrl_tx,
                retained_host_config_path: None,
            };
            let result = service
                .update_and_reload_config(Request::new(UpdateAndReloadConfigRequest {
                    dhcp_config: Some(proto::DhcpConfig {
                        carbide_provisioning_server_ipv4: "192.0.2.2".to_string(),
                        carbide_dhcp_server: "192.0.2.1".to_string(),
                        ..Default::default()
                    }),
                    host_config: None,
                    interfaces: vec!["lo".to_string()],
                    service_config: Some(case.service_config),
                }))
                .await;

            // INVALID_ARGUMENT and an untouched queue prove rejection happened before reload ownership transferred.
            assert_eq!(
                result.unwrap_err().code(),
                tonic::Code::InvalidArgument,
                "{}",
                case.name
            );
            assert!(
                matches!(ctrl_rx.try_recv(), Err(mpsc::error::TryRecvError::Empty)),
                "{}",
                case.name
            );
        }
    }

    /// Verifies stateful DHCPv6 lifetime errors are rejected before queue admission so invalid host
    /// updates cannot stage files or disturb a live generation.
    #[tokio::test]
    async fn stateful_ipv6_lifetime_validation_precedes_control_queue_admission() {
        struct Case {
            name: &'static str,
            preferred_lifetime: u32,
            valid_lifetime: u32,
        }

        let cases = [
            // A zero preferred lifetime is invalid even when the valid lifetime could otherwise serve the address.
            Case {
                name: "zero preferred lifetime",
                preferred_lifetime: 0,
                valid_lifetime: 600,
            },
            // A preferred lifetime beyond the valid lifetime violates the address-expiry ordering independently of zero handling.
            Case {
                name: "preferred lifetime exceeds valid lifetime",
                preferred_lifetime: 601,
                valid_lifetime: 600,
            },
        ];

        for case in cases {
            // Attach an explicit IPv6 address because SLAAC-only and service-only updates do not consume stateful lifetimes.
            let host_config = stateful_host_config();
            let (ctrl_tx, mut ctrl_rx) = mpsc::channel(1);
            let service = DhcpServerControlService {
                ctrl_tx,
                retained_host_config_path: None,
            };

            // Submit through the public handler where validation must precede serialization and queue ownership transfer.
            let result = service
                .update_and_reload_config(Request::new(UpdateAndReloadConfigRequest {
                    dhcp_config: Some(proto::DhcpConfig {
                        dhcpv6_preferred_lifetime_secs: case.preferred_lifetime,
                        dhcpv6_valid_lifetime_secs: case.valid_lifetime,
                        ..Default::default()
                    }),
                    host_config: Some(host_config),
                    interfaces: vec!["lo".to_string()],
                    service_config: None,
                }))
                .await;

            // Caller rejection and an empty channel prove no invalid replacement reached staging or application.
            let status = result.unwrap_err();
            assert_eq!(status.code(), tonic::Code::InvalidArgument, "{}", case.name);
            assert!(status.message().contains("invalid DHCPv6 lifetimes"));
            assert!(
                matches!(ctrl_rx.try_recv(), Err(mpsc::error::TryRecvError::Empty)),
                "{}",
                case.name
            );
        }
    }

    /// Verifies omitted host data is validated against retained state before queue admission so an
    /// invalid DHCP-only update cannot stage beside a stateful host.
    #[tokio::test]
    async fn retained_stateful_host_lifetime_validation_precedes_control_queue_admission() {
        // Persist the stateful host snapshot that protobuf omission promises to retain.
        let directory = tempfile::tempdir().unwrap();
        let retained_host_path = directory.path().join("host.yaml");
        let retained_host =
            <ModelHostConfig as TryFrom<proto::HostConfig>>::try_from(stateful_host_config())
                .unwrap();
        tokio::fs::write(
            &retained_host_path,
            serde_yaml::to_string(&retained_host).unwrap(),
        )
        .await
        .unwrap();
        let (ctrl_tx, mut ctrl_rx) = mpsc::channel(1);
        let service = DhcpServerControlService {
            ctrl_tx,
            retained_host_config_path: Some(retained_host_path.display().to_string()),
        };

        // Omit host_config while submitting zero lifetimes through the public handler.
        let result = service
            .update_and_reload_config(Request::new(UpdateAndReloadConfigRequest {
                dhcp_config: Some(proto::DhcpConfig {
                    dhcpv6_preferred_lifetime_secs: 0,
                    dhcpv6_valid_lifetime_secs: 600,
                    ..Default::default()
                }),
                host_config: None,
                interfaces: vec!["lo".to_string()],
                service_config: None,
            }))
            .await;

        // Rejection and an untouched queue prove the retained file participated before application ownership transferred.
        let status = result.unwrap_err();
        assert_eq!(status.code(), tonic::Code::InvalidArgument);
        assert!(status.message().contains("invalid DHCPv6 lifetimes"));
        assert!(matches!(
            ctrl_rx.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
    }

    /// Verifies omitted and explicitly empty protobuf service messages publish the same full-clear
    /// replacement so wire presence cannot retain reservations.
    #[tokio::test]
    async fn omitted_and_empty_service_configs_publish_the_same_clear() {
        let cases = [
            // Proto omission must retain full-replacement semantics instead of behaving like an absent update.
            ("omitted service config", None),
            // An explicit empty message must produce the same empty model as omission.
            (
                "explicitly empty service config",
                Some(proto::ServiceConfig::default()),
            ),
        ];

        // Submit each wire representation through validation and queue admission.
        for (name, service_config) in cases {
            let (ctrl_tx, mut ctrl_rx) = mpsc::channel(1);
            let service = DhcpServerControlService {
                ctrl_tx,
                retained_host_config_path: None,
            };
            let update =
                service.update_and_reload_config(Request::new(UpdateAndReloadConfigRequest {
                    dhcp_config: Some(proto::DhcpConfig {
                        carbide_provisioning_server_ipv4: "192.0.2.2".to_string(),
                        carbide_dhcp_server: "192.0.2.1".to_string(),
                        ..Default::default()
                    }),
                    host_config: None,
                    interfaces: vec!["lo".to_string()],
                    service_config,
                }));
            tokio::pin!(update);
            let control = tokio::select! {
                result = &mut update => panic!("{name} returned before application: {result:?}"),
                control = ctrl_rx.recv() => control.unwrap(),
            };
            let ControlRequest::UpdateAndReload {
                service_config,
                applied,
                ..
            } = control
            else {
                panic!("{name} did not publish an update");
            };

            // Both forms must publish an empty replacement and complete with the existing applied-acknowledgement contract.
            assert_eq!(service_config, ModelServiceConfig::default(), "{name}");
            applied.send(Ok(())).unwrap();
            update.await.unwrap();
        }
    }

    /// Verifies clean service-only DPU startup and proves an invalid full replacement preserves
    /// usable host and service configuration.
    #[test]
    fn control_rpc_publishes_service_only_dpu_and_rejects_atomically() {
        // Serialize this live OFFER producer with process-global metric-delta assertions in the binary.
        let _metrics_guard = MetricsCapture::start();

        // Reserve unprivileged loopback ports while leaving host.yaml absent to exercise clean service-only startup.
        let directory = tempfile::tempdir().unwrap();
        let live = directory.path().join("dhcp.yaml");
        let host = directory.path().join("host.yaml");
        let staged = directory.path().join("dhcp.yaml_new");
        let staged_host = directory.path().join("host.yaml_new");
        let sidecar = directory.path().join("dhcp.yaml.duid");
        let listener_reservation = std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let SocketAddr::V4(listener_address) = listener_reservation.local_addr().unwrap() else {
            panic!("loopback listener must have an IPv4 address");
        };
        let reply_socket = std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let reply_address = reply_socket.local_addr().unwrap();
        reply_socket.set_nonblocking(true).unwrap();
        let args = crate::command_line::Args {
            interfaces: Vec::new(),
            listen_addr: listener_address,
            relay_response_port: reply_address.port(),
            dhcp_config: live.display().to_string(),
            host_config: Some(host.display().to_string()),
            forge_root_ca_path: None,
            client_cert_path: None,
            client_key_path: None,
            mode: crate::command_line::ServerMode::Dpu,
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
        drop(listener_reservation);
        runtime.block_on(async {
            let control_args = args.clone();
            let reply_receiver = UdpSocket::from_std(reply_socket).unwrap();
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
                    service_config: None,
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
                // Enable DHCPv4 and attach one service reservation so success starts a service-bearing generation.
                client.ready().await.unwrap();
                let mut service_bearing_request = request(Some(identity.as_bytes().to_vec()));
                let service_dhcp = service_bearing_request.dhcp_config.as_mut().unwrap();
                service_dhcp.carbide_provisioning_server_ipv4 = "192.0.2.2".to_string();
                service_dhcp.carbide_dhcp_server = "192.0.2.1".to_string();
                let service_config = proto::ServiceConfig {
                    interfaces: vec![proto::ServiceInterfaceReservations {
                        receiving_interface: "lo".to_string(),
                        reservations: vec![service_reservation()],
                    }],
                };
                service_bearing_request.service_config = Some(service_config.clone());

                // Publish the service-only replacement through the public RPC and wait for applied acknowledgement.
                let _: Response<UpdateAndReloadConfigResponse> = client
                    .unary(
                        Request::new(service_bearing_request),
                        path.clone(),
                        tonic_prost::ProstCodec::default(),
                    )
                    .await
                    .unwrap();

                // Reopen the durable DHCP and identity state to prove successful publication committed the replacement.
                let applied: ModelDhcpConfig =
                    serde_yaml::from_str(&std::fs::read_to_string(&live).unwrap()).unwrap();
                let applied_ipv4 = applied.ipv4().unwrap().unwrap();
                assert_eq!(
                    applied_ipv4.provisioning_server,
                    "192.0.2.2".parse::<Ipv4Addr>().unwrap()
                );
                assert_eq!(
                    applied_ipv4.server,
                    "192.0.2.1".parse::<Ipv4Addr>().unwrap()
                );
                assert_eq!(applied.lease_time_secs, 600);
                assert_eq!(applied.dhcpv6_server_id.as_ref(), Some(&identity));
                assert_eq!(std::fs::read(&sidecar).unwrap(), identity.as_bytes());
                assert!(!staged.exists());
                assert!(!staged_host.exists());
                assert!(
                    !host.exists(),
                    "service-only publication must not create host.yaml"
                );

                // Observe the reservation through the live DPU listener before challenging its replacement.
                let response = exchange_service_packet(listener_address, &reply_receiver, 1).await;
                assert_eq!(response.yiaddr(), "192.0.2.11".parse::<Ipv4Addr>().unwrap());
                assert_eq!(
                    response.opts().msg_type(),
                    Some(MessageType::Offer),
                    "service-only DPU must answer from the applied reservation"
                );

                // Add a host on a separate logical interface because a service-configured interface intentionally fails closed to host fallback.
                let applied_host_config = proto::HostConfig {
                    host_interface_id: "55555555-5555-5555-5555-555555555555".to_string(),
                    host_ip_addresses: [(
                        "host0".to_string(),
                        proto::InterfaceInfo {
                            address: Some("198.51.100.11".to_string()),
                            gateway: Some("198.51.100.1".to_string()),
                            prefix: Some("198.51.100.0/24".to_string()),
                            fqdn: "tenant-host.example.com".to_string(),
                            ..Default::default()
                        },
                    )]
                    .into(),
                };
                let mut full_request = request(Some(identity.as_bytes().to_vec()));
                let full_dhcp = full_request.dhcp_config.as_mut().unwrap();
                full_dhcp.carbide_provisioning_server_ipv4 = "192.0.2.2".to_string();
                full_dhcp.carbide_dhcp_server = "192.0.2.1".to_string();
                full_request.host_config = Some(applied_host_config);
                full_request.service_config = Some(service_config.clone());

                // Publish the combined host and service replacement before constructing its invalid successor.
                client.ready().await.unwrap();
                let _: Response<UpdateAndReloadConfigResponse> = client
                    .unary(
                        Request::new(full_request),
                        path.clone(),
                        tonic_prost::ProstCodec::default(),
                    )
                    .await
                    .unwrap();

                // Combine valid DHCP and host changes with an ambiguous service replacement after the full replacement is applied.
                let applied_yaml = std::fs::read_to_string(&live).unwrap();
                let applied_host_yaml = std::fs::read_to_string(&host).unwrap();
                let first = service_reservation();
                // Change both UUIDs while preserving the MAC so earlier identity checks pass and duplicate-MAC validation selects the rejection.
                let mut duplicate_mac = first.clone();
                duplicate_mac.attachment_id = "33333333-3333-3333-3333-333333333333".to_string();
                duplicate_mac.internal_uuid = "44444444-4444-4444-4444-444444444444".to_string();
                let mut invalid_combined = request(Some(identity.as_bytes().to_vec()));
                invalid_combined
                    .dhcp_config
                    .as_mut()
                    .unwrap()
                    .lease_time_secs = 601;
                invalid_combined.host_config = Some(proto::HostConfig {
                    host_interface_id: "66666666-6666-6666-6666-666666666666".to_string(),
                    host_ip_addresses: [(
                        "host0".to_string(),
                        proto::InterfaceInfo {
                            address: Some("198.51.100.12".to_string()),
                            gateway: Some("198.51.100.1".to_string()),
                            prefix: Some("198.51.100.0/24".to_string()),
                            fqdn: "replacement-host.example.com".to_string(),
                            ..Default::default()
                        },
                    )]
                    .into(),
                });
                invalid_combined.service_config = Some(proto::ServiceConfig {
                    interfaces: vec![proto::ServiceInterfaceReservations {
                        receiving_interface: "lo".to_string(),
                        reservations: vec![first, duplicate_mac],
                    }],
                });
                client.ready().await.unwrap();
                let rejected: Result<Response<UpdateAndReloadConfigResponse>, Status> = client
                    .unary(
                        Request::new(invalid_combined),
                        path.clone(),
                        tonic_prost::ProstCodec::default(),
                    )
                    .await;

                // Prequeue rejection leaves both applied files, the identity, and the live reservation untouched.
                assert_eq!(rejected.unwrap_err().code(), tonic::Code::InvalidArgument);
                assert_eq!(std::fs::read_to_string(&live).unwrap(), applied_yaml);
                assert_eq!(std::fs::read_to_string(&host).unwrap(), applied_host_yaml);
                assert_eq!(std::fs::read(&sidecar).unwrap(), identity.as_bytes());
                let response = exchange_service_packet(listener_address, &reply_receiver, 2).await;
                assert_eq!(response.yiaddr(), "192.0.2.11".parse::<Ipv4Addr>().unwrap());

                // Reopen the retained files and construct a DISCOVER for the retained host interface.
                let retained_config =
                    crate::load_config(&args, &args.dhcp_config, args.host_config.clone(), false)
                        .await
                        .unwrap()
                        .with_service_config(ModelServiceConfig::try_from(service_config).unwrap());
                let mut host_discover = Message::new(
                    Ipv4Addr::UNSPECIFIED,
                    Ipv4Addr::UNSPECIFIED,
                    Ipv4Addr::UNSPECIFIED,
                    Ipv4Addr::UNSPECIFIED,
                    &[0x02, 0, 0, 0, 0, 9],
                );
                host_discover
                    .opts_mut()
                    .insert(DhcpOption::MessageType(MessageType::Discover));
                let mut encoded_host_discover = Vec::new();
                host_discover
                    .encode(&mut Encoder::new(&mut encoded_host_discover))
                    .unwrap();
                let mut machine_cache =
                    Arc::new(Mutex::new(LruCache::new(NonZeroUsize::new(1).unwrap())));

                // Process the DISCOVER through the production DPU packet path after rejecting the replacement.
                let host_response = carbide_dhcp_server::packet_handler::process_packet(
                    &encoded_host_discover,
                    SocketAddr::from(([127, 0, 0, 1], 68)),
                    &retained_config,
                    "host0",
                    &carbide_dhcp_server::modes::dpu::Dpu {},
                    &mut machine_cache,
                )
                .await
                .unwrap();

                // The retained address proves the invalid replacement did not disrupt host DHCP service.
                let decoded_host_response =
                    Message::decode(&mut Decoder::new(host_response.encoded_packet())).unwrap();
                assert_eq!(
                    decoded_host_response.yiaddr(),
                    "198.51.100.11".parse::<Ipv4Addr>().unwrap()
                );

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
                result = crate::run_with_grpc_control(control_args, address, 0) => {
                    panic!("control loop exited during configuration updates: {result:?}");
                }
                result = tokio::time::timeout(Duration::from_secs(15), exercise) => {
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
            let service = DhcpServerControlService {
                ctrl_tx,
                retained_host_config_path: None,
            };
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
                service_config: None,
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
            let service = DhcpServerControlService {
                ctrl_tx,
                retained_host_config_path: None,
            };
            let update =
                service.update_and_reload_config(Request::new(UpdateAndReloadConfigRequest {
                    dhcp_config: Some(proto::DhcpConfig {
                        carbide_provisioning_server_ipv4: "192.0.2.2".to_string(),
                        carbide_dhcp_server: "192.0.2.1".to_string(),
                        ..Default::default()
                    }),
                    host_config: None,
                    interfaces: vec!["lo".to_string()],
                    service_config: None,
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

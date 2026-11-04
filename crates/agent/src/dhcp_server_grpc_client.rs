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

use std::future::Future;
use std::time::Duration;

use carbide_rpc_utils::dhcp::{
    DhcpConfig as ModelDhcpConfig, HostConfig as ModelHostConfig,
    InterfaceInfo as ModelInterfaceInfo, InterfaceInfoV6 as ModelInterfaceInfoV6,
    ServiceConfig as ModelServiceConfig,
    ServiceDhcpInterfaceReservations as ModelServiceDhcpInterfaceReservations,
    ServiceDhcpReservation as ModelServiceDhcpReservation, ServiceLink as ModelServiceLink,
    ServiceLinkV4 as ModelServiceLinkV4, ServiceLinkV6 as ModelServiceLinkV6,
};
use carbide_uuid::machine::MachineInterfaceId;
#[cfg(test)]
pub(super) use proto::UpdateAndReloadConfigRequest;
use proto::dhcp_server_control_client::DhcpServerControlClient;
use tonic::transport::{Channel, Endpoint};

const DHCP_CONTROL_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const DHCP_CONTROL_REQUEST_TIMEOUT: Duration = Duration::from_secs(35);

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
            carbide_provisioning_server_ipv4: c
                .carbide_provisioning_server_ipv4
                .map(|ip| ip.to_string())
                .unwrap_or_default(),
            carbide_provisioning_server_ipv6: c
                .carbide_provisioning_server_ipv6
                .map(|address| address.to_string()),
            carbide_dhcp_server: c
                .carbide_dhcp_server
                .map(|ip| ip.to_string())
                .unwrap_or_default(),
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
            dhcpv6_server_id: c.dhcpv6_server_id.map(Into::into),
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
            booturl_v4: i.booturl_v4,
            booturl_v6: i.booturl_v6,
            dns_servers_v4: i.dns_servers_v4,
            dns_servers_v6: i.dns_servers_v6,
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

impl From<ModelServiceLinkV4> for proto::ServiceLinkV4 {
    fn from(link: ModelServiceLinkV4) -> Self {
        Self {
            address: link.address.to_string(),
            prefix: link.prefix.to_string(),
            gateway: link.gateway.to_string(),
        }
    }
}

impl From<ModelServiceLinkV6> for proto::ServiceLinkV6 {
    fn from(link: ModelServiceLinkV6) -> Self {
        Self {
            address: link.address.to_string(),
            prefix: link.prefix.to_string(),
            hbn_peer: link.hbn_peer.to_string(),
        }
    }
}

impl From<ModelServiceDhcpReservation> for proto::ServiceReservation {
    fn from(reservation: ModelServiceDhcpReservation) -> Self {
        let link = match reservation.link {
            ModelServiceLink::Ipv4(link) => proto::service_reservation::Link::Ipv4(link.into()),
            ModelServiceLink::Ipv6(link) => proto::service_reservation::Link::Ipv6(link.into()),
        };
        Self {
            attachment_id: reservation.attachment_id.to_string(),
            interface_ordinal: reservation.interface_ordinal,
            internal_uuid: reservation.internal_uuid.to_string(),
            mac_address: reservation.mac_address.to_string(),
            link: Some(link),
        }
    }
}

impl From<ModelServiceDhcpInterfaceReservations> for proto::ServiceInterfaceReservations {
    fn from(group: ModelServiceDhcpInterfaceReservations) -> Self {
        Self {
            receiving_interface: group.receiving_interface,
            reservations: group.reservations.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<ModelServiceConfig> for proto::ServiceConfig {
    fn from(config: ModelServiceConfig) -> Self {
        Self {
            interfaces: config
                .into_interface_groups()
                .into_iter()
                .map(Into::into)
                .collect(),
        }
    }
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Waits for one complete DHCP control unary RPC within the request deadline so client readiness,
/// response data, and trailers cannot stall the sequential agent main loop.
async fn complete_unary<T>(
    operation: &str,
    call: impl Future<Output = Result<T, tonic::Status>>,
) -> eyre::Result<T> {
    tokio::time::timeout(DHCP_CONTROL_REQUEST_TIMEOUT, call)
        .await
        .map_err(|_| {
            eyre::eyre!(
                "{operation} gRPC timed out after {} seconds",
                DHCP_CONTROL_REQUEST_TIMEOUT.as_secs()
            )
        })?
        .map_err(|status| eyre::eyre!("{operation} gRPC failed: {status}"))
}

/// Connects to the DHCP control service within five seconds so DNS resolution and transport setup
/// cannot stall the sequential agent main loop.
async fn connect_client(grpc_addr: &str) -> eyre::Result<DhcpServerControlClient<Channel>> {
    let endpoint = Endpoint::new(grpc_addr.to_string())
        .map_err(|error| eyre::eyre!("invalid dhcp-server gRPC endpoint {grpc_addr}: {error}"))?
        .connect_timeout(DHCP_CONTROL_CONNECT_TIMEOUT);
    let channel = tokio::time::timeout(DHCP_CONTROL_CONNECT_TIMEOUT, endpoint.connect())
        .await
        .map_err(|_| {
            eyre::eyre!(
                "connect to dhcp-server gRPC at {grpc_addr} timed out after {} seconds",
                DHCP_CONTROL_CONNECT_TIMEOUT.as_secs()
            )
        })?
        .map_err(|error| eyre::eyre!("connect to dhcp-server gRPC at {grpc_addr}: {error}"))?;
    Ok(DhcpServerControlClient::new(channel))
}

/// Fetches last DHCP request timestamps from the dhcp-server control service.
///
/// Used when the DHCP server runs in a separate container where the timestamps
/// file is not directly accessible on the DPU filesystem.  Each entry carries
/// the host interface UUID and the timestamp of the last DHCP request seen for
/// that interface. Connection and RPC failures are returned to the caller.
pub(super) async fn get_dhcp_timestamps(
    grpc_addr: &str,
) -> eyre::Result<Vec<::rpc::forge::LastDhcpRequest>> {
    let mut client = connect_client(grpc_addr).await?;

    let entries = complete_unary(
        "GetDhcpTimestamps",
        client.get_dhcp_timestamps(proto::GetDhcpTimestampsRequest {}),
    )
    .await?
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
    let mut client = connect_client(grpc_addr).await?;

    complete_unary(
        "StopServer",
        client.stop_server(proto::StopServerRequest {}),
    )
    .await?;

    Ok(())
}

/// Pushes DHCP settings, optional host settings, complete service reservations, and listener names
/// to the dhcp-server control service and triggers an immediate reload in a single RPC.
/// Omitting `host_config` retains the existing host file. Omitting `service_config` clears service
/// reservations.
///
/// Matching configuration and interfaces do not restart a running server, so
/// this function is safe to call on every agent tick. Success acknowledges
/// application, not listener readiness. An empty interface list stages file-only
/// changes, but a changed service replacement applies on remembered listeners.
pub(super) async fn update_and_reload(
    grpc_addr: &str,
    dhcp_config: ModelDhcpConfig,
    host_config: Option<ModelHostConfig>,
    service_config: Option<ModelServiceConfig>,
    interfaces: Vec<String>,
) -> eyre::Result<()> {
    let mut client = connect_client(grpc_addr).await?;

    complete_unary(
        "UpdateAndReloadConfig",
        client.update_and_reload_config(proto::UpdateAndReloadConfigRequest {
            dhcp_config: Some(dhcp_config.into()),
            host_config: host_config.map(Into::into),
            interfaces,
            service_config: service_config.map(Into::into),
        }),
    )
    .await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;
    use std::net::Ipv6Addr;
    use std::sync::Arc;

    use axum::Router;
    use axum::response::IntoResponse;
    use axum::routing::post;
    use carbide_test_support::Outcome::Yields;
    use carbide_test_support::{Case, check_cases_async, value_scenarios};
    use mac_address::MacAddress;
    use tokio::sync::Notify;

    use super::*;

    /// Verifies every DHCP control operation fails after 35 seconds when its request reaches the
    /// server but response headers stall, preventing any unbounded RPC from blocking reconciliation.
    #[tokio::test]
    async fn control_calls_time_out_when_response_headers_stall() {
        check_cases_async(
            [
                // Configuration submissions must retain their complete-call deadline.
                "UpdateAndReloadConfig",
                // Stop requests must remain bounded when the server withholds acknowledgement.
                "StopServer",
                // Timestamp reads must not indefinitely delay subsequent network reconciliation.
                "GetDhcpTimestamps",
            ]
            .map(|operation| Case {
                scenario: operation,
                input: operation,
                expect: Yields(format!("{operation} gRPC timed out after 35 seconds")),
            }),
            |operation| async move {
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                    .await
                    .expect("bind DHCP control mock");
                let address = listener.local_addr().expect("DHCP control mock address");
                let request_received = Arc::new(Notify::new());
                let handler_signal = request_received.clone();

                // Receive the RPC before withholding headers so the request deadline is active.
                let app = Router::new().route(
                    &format!("/dhcp_server_control.DhcpServerControl/{operation}"),
                    post(move || {
                        let signal = handler_signal.clone();
                        async move {
                            signal.notify_one();
                            std::future::pending::<axum::response::Response>().await
                        }
                    }),
                );
                let server = tokio::spawn(async move { axum::serve(listener, app).await });

                // Call each production function to protect its deadline wiring; the mock does not
                // validate the update payload because it withholds all response headers.
                let request = tokio::spawn(async move {
                    let grpc_address = format!("http://{address}");
                    match operation {
                        "UpdateAndReloadConfig" => {
                            update_and_reload(
                                &grpc_address,
                                ModelDhcpConfig::default(),
                                None,
                                None,
                                vec![],
                            )
                            .await
                        }
                        "StopServer" => stop_server(&grpc_address).await,
                        "GetDhcpTimestamps" => get_dhcp_timestamps(&grpc_address).await.map(drop),
                        _ => unreachable!("unknown DHCP control operation: {operation}"),
                    }
                });

                // Freeze time after request receipt to exclude connection setup from this proof.
                tokio::time::timeout(Duration::from_secs(5), request_received.notified())
                    .await
                    .expect("mock must receive the RPC before the test watchdog expires");
                tokio::time::pause();
                tokio::time::advance(DHCP_CONTROL_REQUEST_TIMEOUT).await;
                tokio::task::yield_now().await;
                tokio::time::resume();

                // Compare each production call's timeout error against its table expectation.
                let error = tokio::time::timeout(Duration::from_secs(5), request)
                    .await
                    .expect("DHCP control task must finish")
                    .expect("DHCP control task must not panic")
                    .expect_err("pending response headers must time out");
                server.abort();
                let _ = server.await;
                Ok::<_, ()>(format!("{error:#}"))
            },
        )
        .await;
    }

    /// Verifies a generated StopServer call fails after 35 seconds when its message is decoded but
    /// trailers remain pending, keeping the complete unary deadline effective through grpc-status.
    #[tokio::test]
    async fn stop_server_times_out_when_response_trailers_stall() -> eyre::Result<()> {
        let trailers_polled = Arc::new(Notify::new());
        let handler_signal = trailers_polled.clone();

        // Pass the body directly to Tonic so its second poll observes the client's trailer wait.
        let transport = tower::service_fn(move |_request| {
            let signal = handler_signal.clone();
            async move {
                Ok::<_, Infallible>(
                    crate::tests::respond_then_stall_before_trailers((), signal).into_response(),
                )
            }
        });
        let mut client = DhcpServerControlClient::new(transport);
        let request = tokio::spawn(async move {
            complete_unary(
                "StopServer",
                client.stop_server(proto::StopServerRequest {}),
            )
            .await
        });

        // Wait until Tonic consumes the message and requests the missing trailers.
        tokio::time::timeout(Duration::from_secs(5), trailers_polled.notified())
            .await
            .expect("client must reach its trailer wait");
        // Decoding the message must leave the unary call pending until its trailers arrive.
        assert!(
            !request.is_finished(),
            "decoded message must not complete the unary call before trailers"
        );
        // Freeze time only after Tonic consumes the message and requests the missing trailers.
        tokio::time::pause();
        tokio::time::advance(DHCP_CONTROL_REQUEST_TIMEOUT).await;
        tokio::task::yield_now().await;
        tokio::time::resume();

        // The generated unary call must be bounded through response-trailer completion.
        let error = tokio::time::timeout(Duration::from_secs(5), request)
            .await
            .expect("StopServer task must finish")
            .expect("StopServer task must not panic")
            .expect_err("pending trailers must time out");
        assert_eq!(
            format!("{error:#}"),
            "StopServer gRPC timed out after 35 seconds"
        );

        Ok(())
    }

    // The control request must preserve absent IPv4 fields and keep the DHCP
    // server address separate from the provisioning address when both exist.
    #[test]
    fn control_request_preserves_ipv4_presence_and_identity() {
        let id = carbide_rpc_utils::dhcp::DhcpV6ServerId::from_remote_id("dpu-remote-id").unwrap();
        value_scenarios!(run = |config| {
                let wire = proto::DhcpConfig::from(config);
                (
                    wire.carbide_dhcp_server,
                    wire.carbide_provisioning_server_ipv4,
                    wire.dhcpv6_server_id,
                )
            };
            "IPv6-only configuration" {
                ModelDhcpConfig {
                    dhcpv6_server_id: Some(id.clone()),
                    ..Default::default()
                } => (String::new(), String::new(), Some(id.as_bytes().to_vec())),
            }
            "complete IPv4 configuration" {
                ModelDhcpConfig {
                    carbide_dhcp_server: Some("192.0.2.1".parse().unwrap()),
                    carbide_provisioning_server_ipv4: Some("192.0.2.2".parse().unwrap()),
                    ..Default::default()
                } => ("192.0.2.1".to_string(), "192.0.2.2".to_string(), None),
            }
        );
    }

    #[test]
    fn provisioning_ipv6_preserves_presence_in_control_request() {
        value_scenarios!(run = |address: Option<Ipv6Addr>| {
                proto::DhcpConfig::from(ModelDhcpConfig {
                    carbide_provisioning_server_ipv6: address,
                    ..Default::default()
                }).carbide_provisioning_server_ipv6
            };
            "optional IPv6 provisioning source" {
                None => None,
                Some("2001:db8::80".parse().unwrap()) => Some("2001:db8::80".to_string()),
            }
        );
    }

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

    /// Verifies the agent's outbound control conversion preserves empty listeners and every
    /// `ServiceDhcpReservation` field because the server cannot reconstruct them from host
    /// configuration.
    #[test]
    fn service_reservations_survive_control_transport() {
        let service_config = ModelServiceConfig::try_new(vec![
            // Keep the first listener empty because its presence prevents host fallback after detach.
            ModelServiceDhcpInterfaceReservations {
                receiving_interface: "d_iface_svc_1".to_string(),
                reservations: vec![],
            },
            // Populate the second listener with both families to exercise the complete wire union.
            ModelServiceDhcpInterfaceReservations {
                receiving_interface: "d_iface_svc_2".to_string(),
                reservations: vec![
                    // The IPv4 binding drives the DHCP response fields used by service clients.
                    ModelServiceDhcpReservation {
                        attachment_id: "11111111-1111-1111-1111-111111111111"
                            .parse()
                            .expect("IPv4 attachment UUID"),
                        interface_ordinal: 3,
                        internal_uuid: "22222222-2222-2222-2222-222222222222"
                            .parse()
                            .expect("IPv4 internal UUID"),
                        mac_address: MacAddress::new([0x02, 0, 0, 0, 0, 1]),
                        link: ModelServiceLink::Ipv4(ModelServiceLinkV4 {
                            address: "192.0.2.11".parse().expect("IPv4 service address"),
                            prefix: "192.0.2.10/31".parse().expect("IPv4 service prefix"),
                            gateway: "192.0.2.10".parse().expect("IPv4 HBN gateway"),
                        }),
                    },
                    // Add an IPv6 binding to exercise transport of the independent fixed-link variant.
                    ModelServiceDhcpReservation {
                        attachment_id: "33333333-3333-3333-3333-333333333333"
                            .parse()
                            .expect("IPv6 attachment UUID"),
                        interface_ordinal: 4,
                        internal_uuid: "44444444-4444-4444-4444-444444444444"
                            .parse()
                            .expect("IPv6 internal UUID"),
                        mac_address: MacAddress::new([0x02, 0, 0, 0, 0, 2]),
                        link: ModelServiceLink::Ipv6(ModelServiceLinkV6 {
                            address: "2001:db8::11".parse().expect("IPv6 service address"),
                            prefix: "2001:db8::10/127".parse().expect("IPv6 service prefix"),
                            hbn_peer: "2001:db8::10".parse().expect("IPv6 HBN peer"),
                        }),
                    },
                ],
            },
        ])
        .expect("valid shared-slot service reservations");

        // Convert with the exact model-to-protobuf logic used by UpdateAndReloadConfig.
        let wire = proto::ServiceConfig::from(service_config);

        // Assert empty and occupied listeners remain distinct before inspecting reservation fields.
        let [empty_group, group] = wire.interfaces.as_slice() else {
            panic!("expected two service listeners: {wire:?}");
        };
        assert_eq!(empty_group.receiving_interface, "d_iface_svc_1");
        assert!(empty_group.reservations.is_empty());
        assert_eq!(group.receiving_interface, "d_iface_svc_2");
        let [ipv4, ipv6] = group.reservations.as_slice() else {
            panic!("expected two service reservations: {group:?}");
        };
        // Verify every field in the IPv4 `ServiceReservation`, including its `ServiceLinkV4`
        // values.
        assert_eq!(ipv4.attachment_id, "11111111-1111-1111-1111-111111111111");
        assert_eq!(ipv4.interface_ordinal, 3);
        assert_eq!(ipv4.internal_uuid, "22222222-2222-2222-2222-222222222222");
        assert_eq!(ipv4.mac_address, "02:00:00:00:00:01");
        assert_eq!(
            ipv4.link,
            Some(proto::service_reservation::Link::Ipv4(
                proto::ServiceLinkV4 {
                    address: "192.0.2.11".to_string(),
                    prefix: "192.0.2.10/31".to_string(),
                    gateway: "192.0.2.10".to_string(),
                }
            ))
        );
        // Verify every field in the IPv6 `ServiceReservation`, including its independent
        // `ServiceLinkV6` values.
        assert_eq!(ipv6.attachment_id, "33333333-3333-3333-3333-333333333333");
        assert_eq!(ipv6.interface_ordinal, 4);
        assert_eq!(ipv6.internal_uuid, "44444444-4444-4444-4444-444444444444");
        assert_eq!(ipv6.mac_address, "02:00:00:00:00:02");
        assert_eq!(
            ipv6.link,
            Some(proto::service_reservation::Link::Ipv6(
                proto::ServiceLinkV6 {
                    address: "2001:db8::11".to_string(),
                    prefix: "2001:db8::10/127".to_string(),
                    hbn_peer: "2001:db8::10".to_string(),
                }
            ))
        );
    }
}

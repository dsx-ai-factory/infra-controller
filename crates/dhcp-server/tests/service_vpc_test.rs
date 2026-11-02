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

mod common;

use std::collections::BTreeMap;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::Arc;
use std::time::Duration;

use carbide_dhcp_server::cache::CacheEntry;
use carbide_dhcp_server::modes::DhcpMode;
use carbide_dhcp_server::modes::dpu::Dpu;
use carbide_dhcp_server::{Config, ReplyStatus, packet_handler};
use carbide_instrument::testing::{MetricsCapture, capture_logs_async};
use carbide_rpc_utils::dhcp::{
    HostConfig, InterfaceInfo, ServiceConfig, ServiceInterfaceReservations, ServiceLink,
    ServiceLinkV4, ServiceLinkV6, ServiceReservation,
};
use common::{base_dhcp_config, machine_cache};
use dhcproto::v4::{Decodable, Decoder, DhcpOption, Message, MessageType, OptionCode};
use dhcproto::{Encodable, Encoder};
use lru::LruCache;
use rpc::forge_tls_client::ForgeClientConfig;
use tokio::net::UdpSocket;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

/// Builds one valid IPv4 service reservation with caller-selected MAC, address, and fixture
/// identity so packet tests can isolate lookup and addressing behavior.
fn reservation(mac_address: &str, address: &str, internal_uuid: &str) -> ServiceReservation {
    let address: Ipv4Addr = address.parse().unwrap();
    let gateway = Ipv4Addr::from(u32::from(address) - 1);
    ServiceReservation {
        attachment_id: internal_uuid.parse().unwrap(),
        interface_ordinal: 0,
        internal_uuid: internal_uuid.parse().unwrap(),
        mac_address: mac_address.parse().unwrap(),
        link: ServiceLink::Ipv4(ServiceLinkV4 {
            address,
            prefix: ipnetwork::Ipv4Network::new(gateway, 31).unwrap(),
            gateway,
        }),
    }
}

/// Builds immutable DPU configuration without host data so service replies prove reservations are
/// independently operational.
fn service_config(groups: Vec<ServiceInterfaceReservations>) -> Config {
    Config::new(
        base_dhcp_config(None),
        None,
        67,
        ForgeClientConfig::new(String::new(), None),
    )
    .with_service_config(ServiceConfig::try_new(groups).unwrap())
}

/// Builds immutable DPU configuration with valid host fallback so fail-closed tests detect
/// accidental tenant-host routing.
fn service_config_with_host_fallback(groups: Vec<ServiceInterfaceReservations>) -> Config {
    // Mirror every service interface into host configuration so erroneous fallback would produce an observable host response.
    let host_ip_addresses = groups
        .iter()
        .map(|group| {
            (
                group.receiving_interface.clone(),
                InterfaceInfo {
                    address: Some("203.0.113.20".parse().unwrap()),
                    gateway: Some("203.0.113.1".parse().unwrap()),
                    prefix: Some("203.0.113.0/24".to_string()),
                    fqdn: "tenant-host.example.com".to_string(),
                    booturl: Some("http://192.0.2.2/ipxe".to_string()),
                    mtu: Some(9000),
                    ipv6: None,
                },
            )
        })
        .collect::<BTreeMap<_, _>>();

    // Install the validated full replacement beside the fallback fixture used only to detect routing mistakes.
    Config::new(
        base_dhcp_config(None),
        Some(HostConfig {
            host_interface_id: "44444444-4444-4444-4444-444444444444".parse().unwrap(),
            host_ip_addresses,
        }),
        67,
        ForgeClientConfig::new(String::new(), None),
    )
    .with_service_config(ServiceConfig::try_new(groups).unwrap())
}

/// Encodes one DHCPv4 client packet with optional address, client identity, and vendor inputs so
/// tests can isolate selection and response-option behavior.
fn request(
    message_type: MessageType,
    mac_address: [u8; 6],
    ciaddr: Ipv4Addr,
    requested_address: Option<Ipv4Addr>,
    client_identifier: Option<Vec<u8>>,
    vendor_class: Option<&[u8]>,
) -> Vec<u8> {
    let mut message = Message::new(
        ciaddr,
        Ipv4Addr::UNSPECIFIED,
        Ipv4Addr::UNSPECIFIED,
        Ipv4Addr::UNSPECIFIED,
        &mac_address,
    );
    message
        .opts_mut()
        .insert(DhcpOption::MessageType(message_type));
    if let Some(address) = requested_address {
        message
            .opts_mut()
            .insert(DhcpOption::RequestedIpAddress(address));
    }
    if let Some(client_identifier) = client_identifier {
        message
            .opts_mut()
            .insert(DhcpOption::ClientIdentifier(client_identifier));
    }
    if let Some(vendor_class) = vendor_class {
        message
            .opts_mut()
            .insert(DhcpOption::ClassIdentifier(vendor_class.to_vec()));
    }
    let mut encoded = Vec::new();
    message.encode(&mut Encoder::new(&mut encoded)).unwrap();
    encoded
}

/// Processes one service packet through the production DPU handler so assertions cover lookup,
/// response construction, and encoding together.
async fn process(
    config: &Config,
    interface: &str,
    packet: &[u8],
) -> Result<packet_handler::Packet, carbide_dhcp_server::errors::DhcpError> {
    let mut cache = machine_cache();
    process_with_cache(config, interface, packet, &mut cache).await
}

/// Processes a service packet with a caller-supplied cache so lifecycle tests reuse one
/// packet-processing context across operations.
async fn process_with_cache(
    config: &Config,
    interface: &str,
    packet: &[u8],
    cache: &mut Arc<Mutex<LruCache<String, CacheEntry>>>,
) -> Result<packet_handler::Packet, carbide_dhcp_server::errors::DhcpError> {
    packet_handler::process_packet(
        packet,
        SocketAddr::from(([192, 0, 2, 100], 68)),
        config,
        interface,
        &Dpu {},
        cache,
    )
    .await
}

/// Decodes one emitted service packet so assertions prove its complete DHCP option contract rather
/// than internal response state.
fn decode(packet: &packet_handler::Packet) -> Message {
    Message::decode(&mut Decoder::new(packet.encoded_packet())).unwrap()
}

/// Verifies two clients sharing a service interface receive only their chaddr reservation and omit
/// tenant-host options, preventing leakage across service bindings.
#[tokio::test]
async fn service_clients_receive_exact_minimal_responses() {
    // Configure adjacent /31 reservations on one receiving interface to expose any lookup collision.
    let first = reservation(
        "02:00:00:00:00:01",
        "192.0.2.11",
        "22222222-2222-2222-2222-222222222222",
    );
    let second = reservation(
        "02:00:00:00:00:02",
        "192.0.2.13",
        "33333333-3333-3333-3333-333333333333",
    );
    let config = service_config(vec![ServiceInterfaceReservations {
        receiving_interface: "pf0sf1".to_string(),
        reservations: vec![first, second],
    }]);

    let cases = [
        // A conflicting option 61 must not redirect the first chaddr to another reservation.
        (
            [0x02, 0, 0, 0, 0, 1],
            Ipv4Addr::new(192, 0, 2, 11),
            vec![1, 0x02, 0, 0, 0, 0, 2],
            Some(b"PXEClient".as_slice()),
        ),
        // The adjacent client on the same receiving interface must retain its distinct /31.
        (
            [0x02, 0, 0, 0, 0, 2],
            Ipv4Addr::new(192, 0, 2, 13),
            vec![1, 0x02, 0, 0, 0, 0, 1],
            None,
        ),
    ];

    // Process each DISCOVER through the same service slot with deliberately misleading option-61 identity.
    for (mac_address, expected_address, conflicting_identifier, vendor_class) in cases {
        let response = process(
            &config,
            "pf0sf1",
            &request(
                MessageType::Discover,
                mac_address,
                Ipv4Addr::UNSPECIFIED,
                None,
                Some(conflicting_identifier.clone()),
                vendor_class,
            ),
        )
        .await
        .unwrap();
        // Service response classification must bypass tenant host activity accounting.
        assert!(!response.records_host_activity());
        let response = decode(&response);
        let expected_gateway = Ipv4Addr::from(u32::from(expected_address) - 1);

        // The reply contains the exact point-to-point address, mask, and HBN gateway without copying overlay routes.
        assert_eq!(
            response.opts().get(OptionCode::MessageType),
            Some(&DhcpOption::MessageType(MessageType::Offer))
        );
        assert_eq!(response.yiaddr(), expected_address);
        assert_eq!(response.siaddr(), Ipv4Addr::UNSPECIFIED);
        assert_eq!(
            response.opts().get(OptionCode::SubnetMask),
            Some(&DhcpOption::SubnetMask(Ipv4Addr::new(255, 255, 255, 254)))
        );
        assert_eq!(
            response.opts().get(OptionCode::Router),
            Some(&DhcpOption::Router(vec![expected_gateway]))
        );
        assert!(
            response
                .opts()
                .get(OptionCode::ClasslessStaticRoute)
                .is_none()
        );
        assert_eq!(
            response.opts().get(OptionCode::ClientIdentifier),
            Some(&DhcpOption::ClientIdentifier(conflicting_identifier))
        );
        assert_eq!(
            response.opts().get(OptionCode::AddressLeaseTime),
            Some(&DhcpOption::AddressLeaseTime(604800))
        );
        assert_eq!(
            response.opts().get(OptionCode::Renewal),
            Some(&DhcpOption::Renewal(3600))
        );
        assert_eq!(
            response.opts().get(OptionCode::Rebinding),
            Some(&DhcpOption::Rebinding(432000))
        );
        assert_eq!(
            response.opts().get(OptionCode::ServerIdentifier),
            Some(&DhcpOption::ServerIdentifier(Ipv4Addr::LOCALHOST))
        );

        // Host naming, DNS, PXE, MTU, and machine-interface identity options must not enter the service VRF response.
        for option in [
            OptionCode::BroadcastAddr,
            OptionCode::DomainName,
            OptionCode::DomainNameServer,
            OptionCode::NameServer,
            OptionCode::Hostname,
            OptionCode::BootfileName,
            OptionCode::ClassIdentifier,
            OptionCode::InterfaceMtu,
            OptionCode::NtpServers,
            OptionCode::VendorExtensions,
        ] {
            assert!(
                response.opts().get(option).is_none(),
                "unexpected {option:?}"
            );
        }
    }
}

/// Verifies a successful service ACK produces one datagram and one sent-reply observation because
/// accounting follows transport success.
#[tokio::test]
async fn sent_service_response_records_transport_success() {
    // Prepare an address-matched REQUEST for a service-only configuration.
    let service = reservation(
        "02:00:00:00:00:01",
        "192.0.2.11",
        "22222222-2222-2222-2222-222222222222",
    );
    let config = service_config(vec![ServiceInterfaceReservations {
        receiving_interface: "pf0sf1".to_string(),
        reservations: vec![service.clone()],
    }]);
    let response = process(
        &config,
        "pf0sf1",
        &request(
            MessageType::Request,
            service.mac_address.bytes(),
            service_address(&service),
            None,
            None,
            None,
        ),
    )
    .await
    .unwrap();
    let sender = Arc::new(UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap());
    let receiver = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let SocketAddr::V4(destination) = receiver.local_addr().unwrap() else {
        panic!("test receiver must bind an IPv4 address");
    };

    // Send through the guarded production transport while capturing its generated Event.
    let metrics = MetricsCapture::start();
    let (status, logs) = capture_logs_async(response.send(destination, sender)).await;

    // Successful transport produces the ACK datagram, log, and bounded metric exactly once.
    assert_eq!(status.unwrap(), ReplyStatus::Sent);
    let mut buffer = [0; 512];
    let received = tokio::time::timeout(Duration::from_secs(1), receiver.recv_from(&mut buffer))
        .await
        .unwrap()
        .unwrap();
    assert!(received.0 > 0);
    assert!(logs.iter().any(|entry| {
        entry.metadata_name == "dhcp_server_reply_sent"
            && entry.field("message_type") == Some("ack")
    }));
    assert_eq!(
        metrics.counter_delta(
            "carbide_dhcp_replies_sent_total",
            &[("message_type", "ack")]
        ),
        1.0
    );
}

/// Verifies configured service slots fail closed for unknown MACs and wrong interfaces instead of
/// falling back to host DHCP.
#[tokio::test]
async fn service_slots_reject_unknown_or_cross_slot_clients() {
    // Pair one populated slot with one explicit empty slot while valid host fallback exists on both interfaces.
    let config = service_config_with_host_fallback(vec![
        ServiceInterfaceReservations {
            receiving_interface: "pf0sf1".to_string(),
            reservations: vec![reservation(
                "02:00:00:00:00:01",
                "192.0.2.11",
                "22222222-2222-2222-2222-222222222222",
            )],
        },
        ServiceInterfaceReservations {
            receiving_interface: "pf0sf2".to_string(),
            reservations: vec![],
        },
    ]);
    let discover = request(
        MessageType::Discover,
        [0x02, 0, 0, 0, 0, 1],
        Ipv4Addr::UNSPECIFIED,
        None,
        None,
        None,
    );
    let unknown = request(
        MessageType::Discover,
        [0x02, 0, 0, 0, 0, 9],
        Ipv4Addr::UNSPECIFIED,
        None,
        None,
        None,
    );

    // Exercise a known MAC on an explicitly empty slot and an unknown MAC on the populated slot.
    let cross_slot = process(&config, "pf0sf2", &discover).await;
    let unknown_client = process(&config, "pf0sf1", &unknown).await;

    // Both requests stop at service lookup, proving neither can reach host discovery.
    assert!(cross_slot.is_err());
    assert!(unknown_client.is_err());
}

/// Verifies DHCPREQUEST uses ciaddr before option 50 and broadcasts direct NAKs because the
/// rejected address is not a safe destination.
#[tokio::test]
async fn service_request_address_precedence_controls_ack_and_nak() {
    struct Case {
        name: &'static str,
        ciaddr: Ipv4Addr,
        requested_address: Option<Ipv4Addr>,
        client_identifier: Option<Vec<u8>>,
        expected_type: MessageType,
        expected_destination: SocketAddrV4,
    }

    // Keep one fixed reservation so only the client's two address signals vary between cases.
    let service = reservation(
        "02:00:00:00:00:01",
        "192.0.2.11",
        "22222222-2222-2222-2222-222222222222",
    );
    let config = service_config(vec![ServiceInterfaceReservations {
        receiving_interface: "pf0sf1".to_string(),
        reservations: vec![service.clone()],
    }]);
    let echoed_identifier = vec![0, 0xde, 0xad, 0xbe, 0xef];
    let cases = [
        // A bound client renews ciaddr, so a conflicting option 50 cannot override the active address.
        Case {
            name: "ciaddr match overrides option 50 mismatch",
            ciaddr: service_address(&service),
            requested_address: Some("192.0.2.99".parse().unwrap()),
            client_identifier: None,
            expected_type: MessageType::Ack,
            expected_destination: SocketAddrV4::new(service_address(&service), 68),
        },
        // A nonzero but incorrect ciaddr is rejected even when option 50 names the reservation.
        Case {
            name: "ciaddr mismatch overrides option 50 match",
            ciaddr: "192.0.2.99".parse().unwrap(),
            requested_address: Some(service_address(&service)),
            client_identifier: Some(echoed_identifier.clone()),
            expected_type: MessageType::Nak,
            expected_destination: SocketAddrV4::new(Ipv4Addr::BROADCAST, 68),
        },
        // A REQUEST without either address signal cannot prove ownership of the static reservation.
        Case {
            name: "missing ciaddr and option 50",
            ciaddr: Ipv4Addr::UNSPECIFIED,
            requested_address: None,
            client_identifier: Some(echoed_identifier.clone()),
            expected_type: MessageType::Nak,
            expected_destination: SocketAddrV4::new(Ipv4Addr::BROADCAST, 68),
        },
    ];

    // Process each request through the exact service lookup and response-destination path.
    for case in cases {
        let response = process(
            &config,
            "pf0sf1",
            &request(
                MessageType::Request,
                service.mac_address.bytes(),
                case.ciaddr,
                case.requested_address,
                case.client_identifier.clone(),
                None,
            ),
        )
        .await
        .unwrap();
        let destination = Dpu {}.get_destination_address(&response);
        let decoded = decode(&response);

        // The selected response and destination prove ciaddr precedence without trusting a rejected unicast address.
        assert_eq!(
            decoded.opts().get(OptionCode::MessageType),
            Some(&DhcpOption::MessageType(case.expected_type)),
            "{}",
            case.name
        );
        assert_eq!(destination, case.expected_destination, "{}", case.name);
        assert_eq!(
            decoded.opts().get(OptionCode::ClientIdentifier),
            case.client_identifier
                .map(DhcpOption::ClientIdentifier)
                .as_ref(),
            "{}",
            case.name
        );
        if case.expected_type == MessageType::Nak {
            assert_eq!(decoded.ciaddr(), Ipv4Addr::UNSPECIFIED, "{}", case.name);
            assert_eq!(decoded.yiaddr(), Ipv4Addr::UNSPECIFIED, "{}", case.name);
            assert_eq!(decoded.siaddr(), Ipv4Addr::UNSPECIFIED, "{}", case.name);
            assert_eq!(
                decoded.opts().get(OptionCode::ServerIdentifier),
                Some(&DhcpOption::ServerIdentifier(Ipv4Addr::LOCALHOST)),
                "{}",
                case.name
            );
            for option in [
                OptionCode::SubnetMask,
                OptionCode::Router,
                OptionCode::ClasslessStaticRoute,
                OptionCode::AddressLeaseTime,
                OptionCode::Renewal,
                OptionCode::Rebinding,
                OptionCode::DomainName,
                OptionCode::DomainNameServer,
                OptionCode::BootfileName,
                OptionCode::ClassIdentifier,
                OptionCode::InterfaceMtu,
                OptionCode::NtpServers,
                OptionCode::VendorExtensions,
            ] {
                assert!(
                    decoded.opts().get(option).is_none(),
                    "{} unexpectedly returned {option:?}",
                    case.name
                );
            }
        }
    }
}

/// Verifies DHCPv4 rejects an exact IPv6 service binding without consulting absent host fallback
/// data.
#[tokio::test]
async fn ipv4_rejects_ipv6_service_reservation_without_host_fallback() {
    // Select one valid IPv6 reservation by its exact interface and chaddr key.
    let reservation = ServiceReservation {
        attachment_id: "11111111-1111-1111-1111-111111111111".parse().unwrap(),
        interface_ordinal: 0,
        internal_uuid: "22222222-2222-2222-2222-222222222222".parse().unwrap(),
        mac_address: "02:00:00:00:00:01".parse().unwrap(),
        link: ServiceLink::Ipv6(ServiceLinkV6 {
            address: "2001:db8::11".parse().unwrap(),
            prefix: "2001:db8::10/127".parse().unwrap(),
            hbn_peer: "2001:db8::10".parse().unwrap(),
        }),
    };
    let config = service_config(vec![ServiceInterfaceReservations {
        receiving_interface: "pf0sf1".to_string(),
        reservations: vec![reservation.clone()],
    }]);

    // Send DHCPv4 for the selected IPv6 binding.
    let result = process(
        &config,
        "pf0sf1",
        &request(
            MessageType::Discover,
            reservation.mac_address.bytes(),
            Ipv4Addr::UNSPECIFIED,
            None,
            None,
            None,
        ),
    )
    .await;

    // Family mismatch fails locally instead of falling through to tenant-host discovery.
    let error = match result {
        Ok(_) => panic!("IPv6 reservation unexpectedly produced a DHCPv4 response"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("cannot answer DHCPv4"));
}

/// Verifies replacement snapshots immediately control service behavior and RELEASE leaves static
/// reservations intact, preventing prior packet history from affecting ownership.
#[tokio::test]
async fn service_replacement_and_lifecycle_are_stateless() {
    // Remove one of two original bindings while preserving the second reservation byte for byte.
    let first = reservation(
        "02:00:00:00:00:01",
        "192.0.2.11",
        "22222222-2222-2222-2222-222222222222",
    );
    let survivor = reservation(
        "02:00:00:00:00:02",
        "192.0.2.13",
        "33333333-3333-3333-3333-333333333333",
    );
    let original = service_config(vec![ServiceInterfaceReservations {
        receiving_interface: "pf0sf1".to_string(),
        reservations: vec![first.clone(), survivor.clone()],
    }]);
    let replacement = service_config(vec![ServiceInterfaceReservations {
        receiving_interface: "pf0sf1".to_string(),
        reservations: vec![survivor.clone()],
    }]);
    let mut cache = machine_cache();

    // Renewal and INIT-REBOOT are DHCPREQUEST exchanges whose ciaddr or option 50 must match the static reservation.
    let renew = process_with_cache(
        &original,
        "pf0sf1",
        &request(
            MessageType::Request,
            first.mac_address.bytes(),
            "192.0.2.11".parse().unwrap(),
            None,
            None,
            None,
        ),
        &mut cache,
    )
    .await
    .unwrap();

    // The service packet path does not populate the shared Core-response cache.
    assert!(cache.lock().await.is_empty());

    // Exercise INIT-REBOOT against the replacement after the original renewal.
    let init_reboot = process_with_cache(
        &replacement,
        "pf0sf1",
        &request(
            MessageType::Request,
            survivor.mac_address.bytes(),
            Ipv4Addr::UNSPECIFIED,
            Some("192.0.2.13".parse().unwrap()),
            None,
            None,
        ),
        &mut cache,
    )
    .await
    .unwrap();

    // Both request forms acknowledge the configured address without mutating a lease allocation.
    assert_eq!(
        decode(&renew).opts().get(OptionCode::MessageType),
        Some(&DhcpOption::MessageType(MessageType::Ack))
    );
    assert_eq!(
        decode(&init_reboot).opts().get(OptionCode::MessageType),
        Some(&DhcpOption::MessageType(MessageType::Ack))
    );

    // Request an address outside the reservation to exercise rejection without adopting client-controlled state.
    let mismatch = process_with_cache(
        &replacement,
        "pf0sf1",
        &request(
            MessageType::Request,
            survivor.mac_address.bytes(),
            Ipv4Addr::UNSPECIFIED,
            Some("192.0.2.99".parse().unwrap()),
            None,
            None,
        ),
        &mut cache,
    )
    .await
    .unwrap();

    // The mismatch is rejected with NAK rather than becoming allocation state.
    assert_eq!(
        decode(&mismatch).opts().get(OptionCode::MessageType),
        Some(&DhcpOption::MessageType(MessageType::Nak))
    );

    // Probe the removed client's DISCOVER against the one-client replacement.
    assert!(
        process_with_cache(
            &replacement,
            "pf0sf1",
            &request(
                MessageType::Discover,
                first.mac_address.bytes(),
                Ipv4Addr::UNSPECIFIED,
                None,
                None,
                None,
            ),
            &mut cache,
        )
        .await
        .is_err()
    );

    // Probe the removed client's INIT-REBOOT against the same replacement.
    let removed_init_reboot = process_with_cache(
        &replacement,
        "pf0sf1",
        &request(
            MessageType::Request,
            first.mac_address.bytes(),
            Ipv4Addr::UNSPECIFIED,
            Some("192.0.2.11".parse().unwrap()),
            None,
            None,
        ),
        &mut cache,
    )
    .await;

    // Both probes reject the removed key while the survivor retains its exact address.
    assert!(removed_init_reboot.is_err());
    assert_eq!(decode(&init_reboot).yiaddr(), service_address(&survivor));

    // Reassign the surviving MAC to a new attachment identity and /31.
    let rebound = reservation(
        "02:00:00:00:00:02",
        "192.0.2.15",
        "55555555-5555-5555-5555-555555555555",
    );
    let rebound_config = service_config(vec![ServiceInterfaceReservations {
        receiving_interface: "pf0sf1".to_string(),
        reservations: vec![rebound.clone()],
    }]);

    // Process a DISCOVER through the reassigned snapshot.
    let rebound_offer = process_with_cache(
        &rebound_config,
        "pf0sf1",
        &request(
            MessageType::Discover,
            rebound.mac_address.bytes(),
            Ipv4Addr::UNSPECIFIED,
            None,
            None,
            None,
        ),
        &mut cache,
    )
    .await
    .unwrap();

    // The replacement offers only the reassigned address.
    assert_eq!(decode(&rebound_offer).yiaddr(), service_address(&rebound));

    // RELEASE one of two clients while both remain present on the shared service slot.
    let release = process_with_cache(
        &original,
        "pf0sf1",
        &request(
            MessageType::Release,
            first.mac_address.bytes(),
            service_address(&first),
            None,
            None,
            None,
        ),
        &mut cache,
    )
    .await
    .unwrap();
    assert!(release.encoded_packet().is_empty());
    assert!(!release.records_host_activity());

    // Send through the production transport path to prove protocol silence has no datagram or reply accounting.
    let sender = Arc::new(UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap());
    let receiver = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let SocketAddr::V4(destination) = receiver.local_addr().unwrap() else {
        panic!("test receiver must bind an IPv4 address");
    };
    let metrics = MetricsCapture::start();
    let (status, logs) = capture_logs_async(release.send(destination, sender)).await;
    assert_eq!(status.unwrap(), ReplyStatus::NoReply);
    assert!(
        logs.iter()
            .all(|entry| entry.metadata_name != "dhcp_server_reply_sent")
    );
    for message_type in ["offer", "ack", "nak"] {
        assert_eq!(
            metrics.counter_delta(
                "carbide_dhcp_replies_sent_total",
                &[("message_type", message_type)]
            ),
            0.0
        );
    }
    let mut buffer = [0; 512];
    assert!(
        tokio::time::timeout(Duration::from_millis(10), receiver.recv_from(&mut buffer))
            .await
            .is_err()
    );

    // Probe both clients after RELEASE while both remain on the shared service slot.
    let released_client_after_release = process_with_cache(
        &original,
        "pf0sf1",
        &request(
            MessageType::Discover,
            first.mac_address.bytes(),
            Ipv4Addr::UNSPECIFIED,
            None,
            None,
            None,
        ),
        &mut cache,
    )
    .await
    .unwrap();
    let neighbor_after_release = process_with_cache(
        &original,
        "pf0sf1",
        &request(
            MessageType::Discover,
            survivor.mac_address.bytes(),
            Ipv4Addr::UNSPECIFIED,
            None,
            None,
            None,
        ),
        &mut cache,
    )
    .await
    .unwrap();

    // RELEASE leaves both static reservations independently usable.
    assert_eq!(
        decode(&released_client_after_release).yiaddr(),
        service_address(&first)
    );
    assert_eq!(
        decode(&neighbor_after_release).yiaddr(),
        service_address(&survivor)
    );

    // No lifecycle operation directly populated the shared Core-response cache.
    assert!(cache.lock().await.is_empty());
}

/// Verifies a prepared service response cannot reach the socket after its immutable listener
/// generation is retired.
#[tokio::test]
async fn retired_service_response_is_suppressed_at_send() {
    // Tie one exact service reservation to a controllable listener-generation token.
    let cancellation = CancellationToken::new();
    let config = service_config(vec![ServiceInterfaceReservations {
        receiving_interface: "pf0sf1".to_string(),
        reservations: vec![reservation(
            "02:00:00:00:00:01",
            "192.0.2.11",
            "22222222-2222-2222-2222-222222222222",
        )],
    }])
    .with_reply_retirement(cancellation.clone());

    // Prepare the service OFFER while the original generation is active, then retire it before transport.
    let response = process(
        &config,
        "pf0sf1",
        &request(
            MessageType::Discover,
            [0x02, 0, 0, 0, 0, 1],
            Ipv4Addr::UNSPECIFIED,
            None,
            None,
            None,
        ),
    )
    .await
    .unwrap();
    let sender = Arc::new(UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap());
    let receiver = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let SocketAddr::V4(destination) = receiver.local_addr().unwrap() else {
        panic!("test receiver must bind an IPv4 address");
    };
    // Retire the generation after preparing its OFFER so final transport must suppress it.
    cancellation.cancel();

    // The final send reports retirement without a datagram, sent-reply log, or metric increment.
    let metrics = MetricsCapture::start();
    let (status, logs) = capture_logs_async(response.send(destination, sender)).await;
    assert_eq!(status.unwrap(), ReplyStatus::Retired);
    assert!(
        logs.iter()
            .all(|entry| entry.metadata_name != "dhcp_server_reply_sent")
    );
    assert_eq!(
        metrics.counter_delta(
            "carbide_dhcp_replies_sent_total",
            &[("message_type", "offer")]
        ),
        0.0
    );
    let mut buffer = [0; 512];
    assert!(
        tokio::time::timeout(Duration::from_millis(10), receiver.recv_from(&mut buffer))
            .await
            .is_err()
    );
}

/// Extracts the IPv4 address from a validated fixture so lifecycle assertions remain explicit about
/// the selected family.
fn service_address(reservation: &ServiceReservation) -> Ipv4Addr {
    match &reservation.link {
        ServiceLink::Ipv4(link) => link.address,
        ServiceLink::Ipv6(_) => panic!("test fixture must be IPv4"),
    }
}

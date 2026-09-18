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
use std::net::{Ipv4Addr, Ipv6Addr};

use carbide_dhcp_server::errors::DhcpError;
use carbide_dhcp_server::modes::controller::Controller;
use carbide_dhcp_server::modes::dpu::Dpu;
use carbide_dhcp_server::packet_handler_v6::process_packet;
use carbide_rpc_utils::dhcp::{InterfaceInfo, InterfaceInfoV6};
use dhcproto::v6::{DhcpOption, DhcpOptions, IANA, IAPD, IATA, MessageType, OptionCode, Status};

mod common;

use common::{
    DUID_UUID, SERVER_IDENTIFIER, client_message, controller_config, decode_message,
    decode_response, dpu_config_with_bindings, encode, machine_cache, relay_forward, relay_option,
    response_ia_na, response_status,
};

const OPTION79: &[u8] = &[0, 1, 2, 0xaa, 0xbb, 0xcc, 0xdd, 0xee];

/// Provide stateful DPU configuration so identity policy is the only rejection under test.
fn config() -> carbide_dhcp_server::Config {
    // Give the receiving interface a complete production-shaped binding.
    let ingress = InterfaceInfo {
        address: Some(Ipv4Addr::new(192, 0, 2, 20)),
        gateway: Some(Ipv4Addr::new(192, 0, 2, 1)),
        prefix: Some("192.0.2.0/24".to_string()),
        fqdn: "host.example.com".to_string(),
        booturl: None,
        mtu: Some(9000),
        ipv6: Some(InterfaceInfoV6 {
            address: Some("2001:db8::20".parse().unwrap()),
            prefix: "2001:db8::/64".to_string(),
        }),
    };

    // Vary only IPv6 so a foreign binding selection is visible in the reply.
    let mut foreign = ingress.clone();
    foreign.ipv6 = Some(InterfaceInfoV6 {
        address: Some("2001:db8:1::20".parse().unwrap()),
        prefix: "2001:db8:1::/64".to_string(),
    });

    dpu_config_with_bindings(BTreeMap::from([
        ("eth0".to_string(), ingress),
        ("eth1".to_string(), foreign),
    ]))
}

/// Verifies DPU mode accepts opaque client identity because ingress interface selects the binding.
#[tokio::test]
async fn direct_non_mac_duid_uses_ingress_interface() {
    let config = config();
    let request = encode(&client_message(MessageType::Solicit, DUID_UUID, None, None));
    let mut cache = machine_cache();

    // DPU lookup is keyed by ingress interface, so a non-MAC DUID must not block service.
    let packet = process_packet(
        &request,
        "fe80::20".parse().unwrap(),
        &config,
        "eth0",
        &Dpu {},
        &mut cache,
    )
    .await
    .expect("non-MAC DUID request is valid")
    .expect("configured ingress interface serves a non-MAC DUID");

    // An Advertise alone would not prove which configured interface supplied the binding.
    let response = decode_response(&packet);
    assert_eq!(response.msg_type(), MessageType::Advertise);
    match response_ia_na(&response).opts.get(OptionCode::IAAddr) {
        Some(DhcpOption::IAAddr(address)) => {
            assert_eq!(address.addr, "2001:db8::20".parse::<Ipv6Addr>().unwrap());
        }
        other => panic!("expected eth0 IAAddr, got {other:?}"),
    }
}

/// Verifies DPU ingress selection does not bypass generic DUID validation.
#[tokio::test]
async fn direct_malformed_duid_is_rejected_before_ingress_lookup() {
    let config = config();
    let request = encode(&client_message(MessageType::Solicit, &[0], None, None));
    let mut cache = machine_cache();

    let error = process_packet(
        &request,
        "fe80::20".parse().unwrap(),
        &config,
        "eth0",
        &Dpu {},
        &mut cache,
    )
    .await
    .expect_err("malformed DUID must fail before DPU binding selection");
    assert!(matches!(error, DhcpError::MalformedDuid));
}

/// Verifies DPU mode rejects Relay-Forward before relay metadata can influence local service.
#[tokio::test]
async fn dpu_rejects_relay_forward() {
    let config = config();
    let inner = encode(&client_message(MessageType::Solicit, DUID_UUID, None, None));
    let request = relay_forward(&inner, b"eth1", &[]);
    let mut cache = machine_cache();

    let error = process_packet(
        &request,
        "fe80::20".parse().unwrap(),
        &config,
        "eth0",
        &Dpu {},
        &mut cache,
    )
    .await
    .expect_err("DPU mode must reject relayed DHCPv6");
    assert!(matches!(
        error,
        DhcpError::InvalidInput(message)
            if message == "DPU mode requires a direct DHCPv6 packet"
    ));
}

/// Verifies controller-local lease-end replies require a relay-selected MAC because they skip
/// authoritative discovery before returning a protocol-local success.
#[tokio::test]
async fn controller_lease_end_messages_require_selected_mac_identity() {
    let config = controller_config("http://[::1]:1");
    let server_id = SERVER_IDENTIFIER.to_vec();

    for message_type in [MessageType::Release, MessageType::Decline] {
        // Include the selected server and IA_NA so identity is the only rejection.
        let inner = encode(&client_message(
            message_type,
            DUID_UUID,
            Some("2001:db8::20".parse().unwrap()),
            Some(server_id.clone()),
        ));
        let request_without_identity = relay_forward(&inner, b"swp1", &[]);
        let mut cache = machine_cache();

        // Controller mode must not acknowledge a lease-end message without a MAC source.
        let error = process_packet(
            &request_without_identity,
            "fe80::20".parse().unwrap(),
            &config,
            "eth0",
            &Controller {},
            &mut cache,
        )
        .await
        .expect_err("controller lease-end identity requires a usable MAC source");
        assert!(matches!(error, DhcpError::NoMacNoOption79));

        let request_with_identity = relay_forward(&inner, b"swp1", OPTION79);
        let mut cache = machine_cache();

        // A trusted relay MAC satisfies the identity gate without invoking discovery.
        let packet = process_packet(
            &request_with_identity,
            "fe80::20".parse().unwrap(),
            &config,
            "eth0",
            &Controller {},
            &mut cache,
        )
        .await
        .expect("relay option 79 provides controller lease-end identity")
        .expect("controller returns a local lease-end reply");
        let response = decode_message(relay_option(packet.encoded_packet(), OptionCode::RelayMsg));
        assert_eq!(response.msg_type(), MessageType::Reply);
        assert_eq!(response_status(&response), Status::Success);
    }
}

/// Verifies DPU lease-end replies remain ingress-scoped because local serving does not need
/// a MAC-bearing DUID or relay-supplied option 79.
#[tokio::test]
async fn dpu_lease_end_messages_keep_ingress_identity_policy() {
    let config = config();
    let server_id = SERVER_IDENTIFIER.to_vec();

    for message_type in [MessageType::Release, MessageType::Decline] {
        // Include the selected server and IA_NA to exercise the complete local reply path.
        let request = encode(&client_message(
            message_type,
            DUID_UUID,
            Some("2001:db8::20".parse().unwrap()),
            Some(server_id.clone()),
        ));
        let mut cache = machine_cache();

        // DPU mode keys identity by ingress interface, so the non-MAC DUID remains valid.
        let packet = process_packet(
            &request,
            "fe80::20".parse().unwrap(),
            &config,
            "eth0",
            &Dpu {},
            &mut cache,
        )
        .await
        .expect("DPU lease-end identity is ingress-scoped")
        .expect("DPU returns a local lease-end reply");
        let response = decode_response(&packet);
        assert_eq!(response.msg_type(), MessageType::Reply);
        assert_eq!(response_status(&response), Status::Success);
    }
}

/// RELEASE and DECLINE identify the address whose lease-end notification is
/// being acknowledged even though the standalone server does not verify or
/// mutate authoritative binding state.
#[tokio::test]
async fn lease_end_messages_require_an_identifiable_address() {
    #[derive(Clone, Copy)]
    enum Association {
        Absent,
        Empty,
    }

    let config = config();
    for (scenario, message_type, association, expected_option) in [
        // RELEASE without IA_NA does not identify even an IAID.
        (
            "RELEASE without IA_NA",
            MessageType::Release,
            Association::Absent,
            OptionCode::IANA,
        ),
        // RELEASE with an empty IA_NA does not identify a lease to release.
        (
            "RELEASE without IAADDR",
            MessageType::Release,
            Association::Empty,
            OptionCode::IAAddr,
        ),
        // DECLINE without IA_NA does not identify even an IAID.
        (
            "DECLINE without IA_NA",
            MessageType::Decline,
            Association::Absent,
            OptionCode::IANA,
        ),
        // DECLINE with an empty IA_NA does not identify a suspect address.
        (
            "DECLINE without IAADDR",
            MessageType::Decline,
            Association::Empty,
            OptionCode::IAAddr,
        ),
    ] {
        let mut message = client_message(
            message_type,
            DUID_UUID,
            None,
            Some(SERVER_IDENTIFIER.to_vec()),
        );
        if matches!(association, Association::Empty) {
            message.opts_mut().insert(DhcpOption::IANA(IANA {
                id: 7,
                t1: 0,
                t2: 0,
                opts: DhcpOptions::new(),
            }));
        }
        let mut cache = machine_cache();

        let error = process_packet(
            &encode(&message),
            "fe80::20".parse().unwrap(),
            &config,
            "eth0",
            &Dpu {},
            &mut cache,
        )
        .await
        .expect_err(scenario);
        assert!(
            matches!(error, DhcpError::MissingOptionV6(option) if option == expected_option),
            "{scenario}: {error:?}"
        );
    }
}

/// Unsupported address-association types are rejected before protocol-local replies.
#[tokio::test]
async fn local_replies_do_not_acknowledge_temporary_or_prefix_associations() {
    #[derive(Clone, Copy)]
    enum Association {
        Temporary,
        PrefixDelegation,
    }

    let config = config();
    for (scenario, message_type, association) in [
        // CONFIRM cannot validate temporary-address state this server does not own.
        (
            "CONFIRM with IA_TA",
            MessageType::Confirm,
            Association::Temporary,
        ),
        // RELEASE must not claim that an unsupported delegated prefix was released.
        (
            "RELEASE with IA_PD",
            MessageType::Release,
            Association::PrefixDelegation,
        ),
        // DECLINE must not claim that an unsupported temporary address was quarantined.
        (
            "DECLINE with IA_TA",
            MessageType::Decline,
            Association::Temporary,
        ),
    ] {
        let server_id = matches!(message_type, MessageType::Release | MessageType::Decline)
            .then(|| SERVER_IDENTIFIER.to_vec());
        let mut message = client_message(
            message_type,
            DUID_UUID,
            Some("2001:db8::20".parse().unwrap()),
            server_id,
        );
        let expected_option = match association {
            Association::Temporary => {
                message.opts_mut().insert(DhcpOption::IATA(IATA {
                    id: 7,
                    opts: DhcpOptions::new(),
                }));
                OptionCode::IATA
            }
            Association::PrefixDelegation => {
                message.opts_mut().insert(DhcpOption::IAPD(IAPD {
                    id: 7,
                    t1: 0,
                    t2: 0,
                    opts: DhcpOptions::new(),
                }));
                OptionCode::IAPD
            }
        };
        let request = encode(&message);
        let mut cache = machine_cache();

        let error = process_packet(
            &request,
            "fe80::20".parse().unwrap(),
            &config,
            "eth0",
            &Dpu {},
            &mut cache,
        )
        .await
        .expect_err("unsupported association must not receive a local success reply");
        assert!(
            matches!(
                &error,
                DhcpError::UnsupportedAddressAssociationV6(actual)
                    if *actual == expected_option
            ),
            "{scenario} returned {error:?}"
        );
    }
}

/// Verifies controller identity requires relay option 79 for valid DUIDs without Ethernet MACs.
#[tokio::test]
async fn controller_non_mac_duids_require_relay_identity() {
    let config = controller_config("http://[::1]:1");
    let request = relay_forward(
        &encode(&client_message(MessageType::Solicit, DUID_UUID, None, None)),
        b"swp1",
        &[],
    );
    let mut cache = machine_cache();

    // A valid non-MAC DUID still cannot select an authoritative API row.
    let error = process_packet(
        &request,
        "fe80::20".parse().unwrap(),
        &config,
        "eth0",
        &Controller {},
        &mut cache,
    )
    .await
    .expect_err("controller identity requires a usable MAC source");
    assert!(matches!(error, DhcpError::NoMacNoOption79));

    let unknown = relay_forward(
        &encode(&client_message(
            MessageType::Solicit,
            &[0, 99, 0, 1, 2, 3],
            None,
            None,
        )),
        b"swp1",
        &[],
    );

    // Unknown DUID types are valid opaque identities and use the same option-79 fallback.
    let error = process_packet(
        &unknown,
        "fe80::20".parse().unwrap(),
        &config,
        "eth0",
        &Controller {},
        &mut cache,
    )
    .await
    .expect_err("controller identity requires a usable MAC source");
    assert!(matches!(error, DhcpError::NoMacNoOption79));
}

/// Verifies nested and multi-hop relay shapes are rejected before segment selection.
#[tokio::test]
async fn unsupported_relay_depth_is_rejected() {
    let config = config();
    let inner = encode(&client_message(MessageType::Solicit, DUID_UUID, None, None));
    let nested = relay_forward(&relay_forward(&inner, b"swp1", OPTION79), b"swp2", OPTION79);
    let mut cache = machine_cache();

    // Nested relay precedence is intentionally undefined in this one-hop milestone.
    let error = process_packet(
        &nested,
        "fe80::20".parse().unwrap(),
        &config,
        "eth0",
        &Dpu {},
        &mut cache,
    )
    .await
    .expect_err("nested relay must be rejected");
    assert!(matches!(error, DhcpError::NestedRelayV6));

    // A nonzero count requires another relay envelope, which this decoder rejects.
    let mut nonzero_hop = relay_forward(&inner, b"swp1", OPTION79);
    nonzero_hop[1] = 1;
    let error = process_packet(
        &nonzero_hop,
        "fe80::20".parse().unwrap(),
        &config,
        "eth0",
        &Dpu {},
        &mut cache,
    )
    .await
    .expect_err("nonzero relay hop count must be rejected");
    assert!(matches!(error, DhcpError::RelayHopCountExceededV6(1)));
}

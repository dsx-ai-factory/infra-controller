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

use std::net::Ipv6Addr;

use carbide_dhcp_server::errors::DhcpError;
use carbide_dhcp_server::modes::controller::Controller;
use carbide_dhcp_server::packet_handler_v6::process_packet;
use carbide_dhcpv6::RELAY_REPLY;
use dhcproto::v6::{DhcpOption, Message, MessageType, OptionCode};
use dhcproto::{Decodable, Decoder};
use rpc::forge::AddressFamily;

mod common;

use common::{
    DUID_UUID, MockDiscoverDhcpApi, client_message, controller_config,
    controller_config_with_lifetimes, encode, machine_cache, relay_forward, relay_option,
    response_ia_na,
};

const OPTION79: &[u8] = &[0, 1, 2, 0xaa, 0xbb, 0xcc, 0xdd, 0xee];

/// Verifies controller mode forwards selected v6 identity and restores the relay envelope.
#[tokio::test]
async fn relayed_solicit_round_trips_through_controller_api() {
    let api = MockDiscoverDhcpApi::start().await;
    let config = controller_config(api.url());
    let inner = encode(&client_message(MessageType::Solicit, DUID_UUID, None, None));
    let request = relay_forward(&inner, b"swp1", OPTION79);
    let mut cache = machine_cache();

    // A non-MAC DUID is valid here because the trusted relay supplies option 79.
    let packet = process_packet(
        &request,
        "fe80::100".parse().unwrap(),
        &config,
        "eth0",
        &Controller {},
        &mut cache,
    )
    .await;
    let discoveries = api.shutdown().await;
    let packet = packet
        .expect("relayed controller SOLICIT is valid")
        .expect("relayed controller SOLICIT is served");

    // The response preserves relay routing metadata and wraps an ADVERTISE.
    assert_eq!(packet.encoded_packet()[0], RELAY_REPLY);
    assert_eq!(
        relay_option(packet.encoded_packet(), OptionCode::InterfaceId),
        b"swp1"
    );
    let response = Message::decode(&mut Decoder::new(relay_option(
        packet.encoded_packet(),
        OptionCode::RelayMsg,
    )))
    .expect("inner ADVERTISE decodes");
    assert_eq!(response.msg_type(), MessageType::Advertise);
    match response_ia_na(&response).opts.get(OptionCode::IAAddr) {
        Some(DhcpOption::IAAddr(address)) => {
            assert_eq!(address.addr, "2001:db8::ee".parse::<Ipv6Addr>().unwrap());
        }
        other => panic!("expected controller IAADDR, got {other:?}"),
    }

    // The API observes the transport-selected MAC and complete family-aware contract.
    assert_eq!(discoveries.len(), 1);
    let discovery = &discoveries[0];
    assert_eq!(discovery.address_family, Some(AddressFamily::V6 as i32));
    assert_eq!(discovery.duid.as_deref(), Some(DUID_UUID));
    assert_eq!(discovery.mac_address, "02:aa:bb:cc:dd:ee");
    assert_eq!(discovery.circuit_id.as_deref(), Some("73777031"));
    assert_eq!(discovery.link_address.as_deref(), Some("2001:db8::1"));
}

/// Verifies controller CONFIRM without optional Interface-ID remains silent.
#[tokio::test]
async fn relayed_confirm_without_link_knowledge_is_ignored() {
    let config = controller_config("http://[::1]:1");
    let inner = encode(&client_message(
        MessageType::Confirm,
        DUID_UUID,
        Some("2001:db8::20".parse().unwrap()),
        None,
    ));
    let request = relay_forward(&inner, &[], OPTION79);
    let mut cache = machine_cache();

    // CONFIRM bypasses the API, so omitting relay Interface-ID must preserve silence.
    let response = process_packet(
        &request,
        "fe80::100".parse().unwrap(),
        &config,
        "eth0",
        &Controller {},
        &mut cache,
    )
    .await
    .expect("relayed CONFIRM is valid");
    assert!(response.is_none());
}

/// Invalid stateful lifetimes fail locally before controller discovery can allocate.
#[tokio::test]
async fn invalid_stateful_lifetimes_fail_before_controller_discovery() {
    // An unreachable API makes any accidental discovery call observable as a transport error.
    let config = controller_config_with_lifetimes("http://[::1]:1", 0, 7200);
    let inner = encode(&client_message(MessageType::Solicit, DUID_UUID, None, None));
    let request = relay_forward(&inner, b"swp1", OPTION79);
    let mut cache = machine_cache();

    let result = process_packet(
        &request,
        "fe80::100".parse().unwrap(),
        &config,
        "eth0",
        &Controller {},
        &mut cache,
    )
    .await;

    assert!(matches!(
        result,
        Err(DhcpError::InvalidDhcpV6Lifetimes {
            preferred_lifetime_secs: 0,
            valid_lifetime_secs: 7200,
        })
    ));
}

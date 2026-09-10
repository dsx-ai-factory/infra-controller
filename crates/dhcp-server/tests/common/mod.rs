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
// Each integration-test binary compiles these helpers independently and uses
// only the subset relevant to that binary.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::sync::Arc;

use carbide_dhcp_server::cache::{self, CacheEntry};
use carbide_dhcp_server::{Config, packet_handler_v6};
use carbide_dhcpv6::RELAY_FORWARD;
use carbide_rpc_utils::dhcp::{DhcpConfig, HostConfig, InterfaceInfo, InterfaceInfoV6};
use dhcproto::v6::{
    DhcpOption, DhcpOptions, IAAddr, IANA, Message, MessageType, OptionCode, Status,
};
use dhcproto::{Decodable, Decoder, Encodable, Encoder};
use lru::LruCache;
use rpc::forge_tls_client::ForgeClientConfig;
use tokio::sync::Mutex;

pub(super) const DUID_LL: &[u8] = &[0, 3, 0, 1, 2, 0xaa, 0xbb, 0xcc, 0xdd, 0xee];
pub(super) const DUID_UUID: &[u8] = &[0, 4, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];
pub(super) const IAID: u32 = 0x0102_0304;
/// Expected DUID-EN from the shared test configuration's loopback server identity.
///
/// Keeping this as an independent wire fixture verifies production derivation
/// without exposing the internal helper solely to integration tests.
pub(super) const SERVER_IDENTIFIER: &[u8] = &[0, 2, 0, 0, 0x16, 0x47, 127, 0, 0, 1];

/// Provide an isolated machine cache matching the production capacity contract.
pub(super) fn machine_cache() -> Arc<Mutex<LruCache<String, CacheEntry>>> {
    Arc::new(Mutex::new(LruCache::new(
        std::num::NonZeroUsize::new(cache::MACHINE_CACHE_SIZE)
            .expect("production cache size is nonzero"),
    )))
}

/// Build deterministic DPU configuration whose IPv6 block drives packet outcomes.
pub(super) fn dpu_config(interface: &str, ipv6: InterfaceInfoV6) -> Config {
    dpu_config_with_bindings(BTreeMap::from([(
        interface.to_string(),
        InterfaceInfo {
            address: Some(Ipv4Addr::new(192, 0, 2, 20)),
            gateway: Some(Ipv4Addr::new(192, 0, 2, 1)),
            prefix: Some("192.0.2.0/24".to_string()),
            fqdn: "host.example.com".to_string(),
            booturl: None,
            mtu: Some(9000),
            ipv6: Some(ipv6),
        },
    )]))
}

/// Build DPU configuration with caller-selected bindings so identity tests can distinguish
/// authoritative ingress from untrusted relay metadata.
pub(super) fn dpu_config_with_bindings(
    host_ip_addresses: BTreeMap<String, InterfaceInfo>,
) -> Config {
    // Keep server-wide identity and options fixed so callers vary only binding selection.
    let host_config = HostConfig {
        host_interface_id: "0fd6e9a3-06fc-4a22-ad29-aca299677b00"
            .parse()
            .expect("test host interface id is valid"),
        host_ip_addresses,
    };

    Config::new(
        base_dhcp_config(None),
        Some(host_config),
        67,
        forge_config(),
    )
}

/// Build controller configuration pointing at the supplied mock Forge API.
pub(super) fn controller_config(api_url: &str) -> Config {
    controller_config_with_lifetimes(api_url, 3600, 7200)
}

/// Build controller configuration with explicit DHCPv6 stateful lifetimes.
pub(super) fn controller_config_with_lifetimes(
    api_url: &str,
    preferred_lifetime: u32,
    valid_lifetime: u32,
) -> Config {
    let mut dhcp_config = base_dhcp_config(Some(api_url.to_string()));
    dhcp_config.dhcpv6_preferred_lifetime_secs = preferred_lifetime;
    dhcp_config.dhcpv6_valid_lifetime_secs = valid_lifetime;
    Config::new(dhcp_config, None, 67, forge_config())
}

/// Build the dual-stack option and lifetime settings shared by v6 packet tests.
fn base_dhcp_config(api_url: Option<String>) -> DhcpConfig {
    DhcpConfig {
        carbide_api_url: api_url,
        carbide_nameservers_v6: vec!["2001:db8::53".parse().unwrap()],
        carbide_ntpservers_v6: vec!["2001:db8::123".parse().unwrap()],
        dhcpv6_preferred_lifetime_secs: 3600,
        dhcpv6_valid_lifetime_secs: 7200,
        ..Default::default()
    }
}

/// Disable TLS for the loopback mock API used by controller-mode tests.
fn forge_config() -> ForgeClientConfig {
    ForgeClientConfig::new(String::new(), None)
}

/// Build a client DHCPv6 message with optional IA_NA and server selection.
///
/// IA_NA is included when `address` is present or the message is SOLICIT or
/// REQUEST. It is omitted for RENEW and REBIND when `address` is absent.
pub(super) fn client_message(
    message_type: MessageType,
    duid: &[u8],
    address: Option<Ipv6Addr>,
    server_id: Option<Vec<u8>>,
) -> Message {
    let mut message = Message::new_with_id(message_type, [0xaa, 0xbb, 0xcc]);
    message
        .opts_mut()
        .insert(DhcpOption::ClientId(duid.to_vec()));
    if let Some(server_id) = server_id {
        message.opts_mut().insert(DhcpOption::ServerId(server_id));
    }
    if address.is_some() || matches!(message_type, MessageType::Solicit | MessageType::Request) {
        let mut options = DhcpOptions::new();
        if let Some(address) = address {
            options.insert(DhcpOption::IAAddr(IAAddr {
                addr: address,
                preferred_life: 300,
                valid_life: 600,
                opts: DhcpOptions::new(),
            }));
        }
        message.opts_mut().insert(DhcpOption::IANA(IANA {
            id: IAID,
            t1: 0,
            t2: 0,
            opts: options,
        }));
    }
    message
}

/// Encode a typed DHCPv6 client message into its wire representation.
pub(super) fn encode(message: &Message) -> Vec<u8> {
    let mut packet = Vec::new();
    message
        .encode(&mut Encoder::new(&mut packet))
        .expect("test DHCPv6 message encodes");
    packet
}

/// Decode a direct packet response produced by the server packet handler.
pub(super) fn decode_response(packet: &packet_handler_v6::PacketV6) -> Message {
    decode_message(packet.encoded_packet())
}

/// Decode DHCPv6 wire bytes so socket tests verify the received datagram.
pub(super) fn decode_message(packet: &[u8]) -> Message {
    Message::decode(&mut Decoder::new(packet)).expect("server DHCPv6 response decodes")
}

/// Return the single IA_NA carried by a stateful response.
pub(super) fn response_ia_na(response: &Message) -> &IANA {
    match response.opts().get(OptionCode::IANA) {
        Some(DhcpOption::IANA(association)) => association,
        other => panic!("expected response IA_NA, got {other:?}"),
    }
}

/// Assert an IA-specific failure retains IAID and contains no fabricated IAADDR.
pub(super) fn assert_ia_na_failure(response: &Message, expected_status: Status) {
    assert!(response.opts().get(OptionCode::StatusCode).is_none());
    let association = response_ia_na(response);
    assert_eq!(association.id, IAID);
    assert!(association.opts.get(OptionCode::IAAddr).is_none());
    match association.opts.get(OptionCode::StatusCode) {
        Some(DhcpOption::StatusCode(status)) => {
            assert_eq!(status.status, expected_status);
        }
        other => panic!("expected nested IA_NA status, got {other:?}"),
    }
}

/// Return the top-level status carried by a protocol-local response.
pub(super) fn response_status(response: &Message) -> dhcproto::v6::Status {
    match response.opts().get(OptionCode::StatusCode) {
        Some(DhcpOption::StatusCode(status)) => status.status,
        other => panic!("expected response status, got {other:?}"),
    }
}

/// Encode one raw relay option for envelope construction and inspection.
pub(super) fn raw_option(code: OptionCode, payload: &[u8]) -> Vec<u8> {
    let mut option = Vec::new();
    option.extend_from_slice(&u16::from(code).to_be_bytes());
    option.extend_from_slice(
        &u16::try_from(payload.len())
            .expect("test relay option fits on the wire")
            .to_be_bytes(),
    );
    option.extend_from_slice(payload);
    option
}

/// Wrap a direct client message in the supported one-hop Relay-Forward shape,
/// omitting Interface-ID or option 79 when its corresponding payload is empty.
pub(super) fn relay_forward(inner: &[u8], interface_id: &[u8], option79: &[u8]) -> Vec<u8> {
    // A relay forwarding a client message initializes the hop count to zero.
    let mut relay = vec![RELAY_FORWARD, 0];
    relay.extend_from_slice(&"2001:db8::1".parse::<Ipv6Addr>().unwrap().octets());
    relay.extend_from_slice(&"fe80::1".parse::<Ipv6Addr>().unwrap().octets());
    // Empty metadata represents omission. Callers can use raw_option when
    // they need a distinct zero-length option on the wire.
    if !interface_id.is_empty() {
        relay.extend_from_slice(&raw_option(OptionCode::InterfaceId, interface_id));
    }
    if !option79.is_empty() {
        relay.extend_from_slice(&raw_option(OptionCode::ClientLinklayerAddr, option79));
    }
    relay.extend_from_slice(&raw_option(OptionCode::RelayMsg, inner));
    relay
}

/// Parse one uniquely occurring option from a raw relay envelope.
pub(super) fn relay_option(packet: &[u8], code: OptionCode) -> &[u8] {
    let mut options = packet.get(34..).unwrap_or_else(|| {
        panic!(
            "malformed relay packet: {} bytes is shorter than the 34-byte header",
            packet.len()
        )
    });
    while !options.is_empty() {
        assert!(
            options.len() >= 4,
            "malformed relay option header: only {} bytes remain",
            options.len()
        );
        let current = u16::from_be_bytes([options[0], options[1]]);
        let length = u16::from_be_bytes([options[2], options[3]]) as usize;
        let remaining = &options[4..];
        assert!(
            length <= remaining.len(),
            "malformed relay option length {length}: only {} payload bytes remain",
            remaining.len()
        );
        let (payload, rest) = remaining.split_at(length);
        if current == u16::from(code) {
            return payload;
        }
        options = rest;
    }
    panic!("relay option {code:?} is absent")
}

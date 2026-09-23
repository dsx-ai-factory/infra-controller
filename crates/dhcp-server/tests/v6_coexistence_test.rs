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

use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};
use std::sync::Arc;
use std::time::Duration;

use carbide_dhcp_server::modes::dpu::Dpu;
use carbide_dhcp_server::util::{get_socket, get_socket_v6};
use carbide_dhcp_server::{packet_handler, packet_handler_v6};
use carbide_rpc_utils::dhcp::InterfaceInfoV6;
use dhcproto::v4::{
    DhcpOption as DhcpOptionV4, Message as MessageV4, MessageType as MessageTypeV4,
    OptionCode as OptionCodeV4,
};
use dhcproto::v6::{Message as MessageV6, MessageType as MessageTypeV6};
use dhcproto::{Decodable, Decoder, Encodable, Encoder};
use tokio::net::UdpSocket;
use tokio::time::timeout;

mod common;

use common::{DUID_LL, client_message, dpu_config, encode, machine_cache};

/// Verifies the per-interface v4 and v6 sockets independently serve both families.
#[tokio::test]
async fn v4_and_v6_listeners_serve_concurrent_requests() {
    // Loopback keeps the two production socket paths isolated from external
    // interfaces while retaining bind-to-device behavior.
    let config = dpu_config(
        "lo",
        InterfaceInfoV6 {
            address: Some("2001:db8::20".parse().unwrap()),
            prefix: "2001:db8::/64".to_string(),
        },
    );
    let v4_listener = Arc::new(
        get_socket(
            SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0),
            "lo".to_string(),
        )
        .await,
    );
    let v6_listener = Arc::new(
        get_socket_v6(
            SocketAddrV6::new(Ipv6Addr::UNSPECIFIED, 0, 0, 0),
            "lo",
            false,
        )
        .await
        .expect("DHCPv6 listener binds"),
    );
    let v4_client = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("DHCPv4 client binds");
    let v6_client = UdpSocket::bind(SocketAddrV6::new(Ipv6Addr::LOCALHOST, 0, 0, 0))
        .await
        .expect("DHCPv6 client binds");

    // Build one valid request for each family.
    let mut discover = MessageV4::new(
        Ipv4Addr::UNSPECIFIED,
        Ipv4Addr::UNSPECIFIED,
        Ipv4Addr::UNSPECIFIED,
        Ipv4Addr::UNSPECIFIED,
        &[0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee],
    );
    discover
        .opts_mut()
        .insert(DhcpOptionV4::MessageType(MessageTypeV4::Discover));
    let mut discover_wire = Vec::new();
    discover
        .encode(&mut Encoder::new(&mut discover_wire))
        .expect("DHCPv4 DISCOVER encodes");
    let solicit_wire = encode(&client_message(MessageTypeV6::Solicit, DUID_LL, None, None));

    // Send both requests before receiving either, exercising simultaneous listeners.
    v4_client
        .send_to(
            &discover_wire,
            SocketAddrV4::new(
                Ipv4Addr::LOCALHOST,
                v4_listener
                    .local_addr()
                    .expect("v4 listener address")
                    .port(),
            ),
        )
        .await
        .expect("client sends DHCPv4 DISCOVER");
    v6_client
        .send_to(
            &solicit_wire,
            SocketAddrV6::new(
                Ipv6Addr::LOCALHOST,
                v6_listener
                    .local_addr()
                    .expect("v6 listener address")
                    .port(),
                0,
                0,
            ),
        )
        .await
        .expect("client sends DHCPv6 SOLICIT");

    let mut v4_request = vec![0; 2048];
    let mut v6_request = vec![0; 2048];
    let (v4_received, v6_received) = tokio::join!(
        timeout(
            Duration::from_secs(5),
            v4_listener.recv_from(&mut v4_request)
        ),
        timeout(
            Duration::from_secs(5),
            v6_listener.recv_from(&mut v6_request)
        ),
    );
    let (v4_length, v4_source) = v4_received
        .expect("v4 listener receives before timeout")
        .expect("v4 listener receives DISCOVER");
    let (v6_length, v6_source) = v6_received
        .expect("v6 listener receives before timeout")
        .expect("v6 listener receives SOLICIT");
    let SocketAddr::V4(v4_source) = v4_source else {
        panic!("DHCPv4 listener received a non-IPv4 source");
    };
    let SocketAddr::V6(v6_source) = v6_source else {
        panic!("DHCPv6 listener received a non-IPv6 source");
    };

    // Process and return each response through its corresponding socket.
    let mut v4_cache = machine_cache();
    let v4_response = packet_handler::process_packet(
        &v4_request[..v4_length],
        SocketAddr::V4(v4_source),
        &config,
        "lo",
        &Dpu {},
        &mut v4_cache,
    )
    .await
    .expect("DHCPv4 DISCOVER is served");
    v4_response
        .send(v4_source, v4_listener)
        .await
        .expect("listener sends DHCPv4 OFFER");

    let mut v6_cache = machine_cache();
    let v6_response = packet_handler_v6::process_packet(
        &v6_request[..v6_length],
        *v6_source.ip(),
        &config,
        "lo",
        &Dpu {},
        &mut v6_cache,
    )
    .await
    .expect("DHCPv6 SOLICIT is valid")
    .expect("DHCPv6 SOLICIT is served");
    v6_listener
        .send_to(v6_response.encoded_packet(), v6_source)
        .await
        .expect("listener sends DHCPv6 ADVERTISE");

    // Both clients must receive the correct family-specific response.
    let mut offer_wire = vec![0; 2048];
    let mut advertise_wire = vec![0; 2048];
    let (offer, advertise) = tokio::join!(
        timeout(Duration::from_secs(5), v4_client.recv_from(&mut offer_wire)),
        timeout(
            Duration::from_secs(5),
            v6_client.recv_from(&mut advertise_wire)
        ),
    );
    let (offer_length, _) = offer
        .expect("v4 client receives before timeout")
        .expect("v4 client receives OFFER");
    let (advertise_length, _) = advertise
        .expect("v6 client receives before timeout")
        .expect("v6 client receives ADVERTISE");
    let offer = MessageV4::decode(&mut Decoder::new(&offer_wire[..offer_length]))
        .expect("DHCPv4 OFFER decodes");
    let advertise = MessageV6::decode(&mut Decoder::new(&advertise_wire[..advertise_length]))
        .expect("DHCPv6 ADVERTISE decodes");

    assert_eq!(
        offer.opts().get(OptionCodeV4::MessageType),
        Some(&DhcpOptionV4::MessageType(MessageTypeV4::Offer))
    );
    assert_eq!(advertise.msg_type(), MessageTypeV6::Advertise);
}

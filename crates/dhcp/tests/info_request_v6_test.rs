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
use std::fs;
use std::net::UdpSocket;
use std::path::PathBuf;
use std::time::Duration;

use dhcp::mock_api_server::{self, DHCP_RESPONSE_FQDN};
use dhcproto::v6::{self, DhcpOption, OptionCode};
use rpc::forge as rpc;

mod common;

use common::{
    DHCPv6Factory, Kea6, assert_api_v6_domain_search, assert_hook_v6_dns_servers,
    assert_hook_v6_ntp_servers, send_and_recv_v6,
};

const READ_TIMEOUT: Duration = Duration::from_millis(500);

struct Harness {
    _rt: tokio::runtime::Runtime,
    api_server: mock_api_server::MockAPIServer,
    _kea: Kea6,
    socket: UdpSocket,
    _lease_dir: tempfile::TempDir,
    lease_file_path: PathBuf,
}

impl Harness {
    fn new() -> Self {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let api_server = rt.block_on(mock_api_server::MockAPIServer::start());
        let lease_dir = tempfile::tempdir().unwrap();
        let lease_file_path = lease_dir.path().join("kea-leases6.csv");

        let (kea, socket) =
            Kea6::start(api_server.local_http_addr(), Some(&lease_file_path)).unwrap();
        socket.set_read_timeout(Some(READ_TIMEOUT)).unwrap();

        Harness {
            _rt: rt,
            api_server,
            _kea: kea,
            socket,
            _lease_dir: lease_dir,
            lease_file_path,
        }
    }

    fn assert_no_lease_written(&self) {
        let contents = fs::read_to_string(&self.lease_file_path).unwrap_or_default();
        let active_na_leases = contents
            .lines()
            .filter(|line| {
                let columns = line.split(',').collect::<Vec<_>>();
                if columns.len() < 14 {
                    return false;
                }

                // Kea memfile rows encode active IA_NA leases as type 0 with
                // a positive lifetime and normal state, independent of prefix.
                let valid_lifetime = columns[2].parse::<u32>().ok();
                let lease_type = columns[6].parse::<u32>().ok();
                let state = columns[13].parse::<u32>().ok();

                valid_lifetime.is_some_and(|lifetime| lifetime > 0)
                    && lease_type == Some(0)
                    && state == Some(0)
            })
            .collect::<Vec<_>>();

        assert!(
            active_na_leases.is_empty(),
            "stateless DHCPv6 exchange should not persist active IA_NA leases: {active_na_leases:?}\nmemfile:\n{contents}"
        );
    }
}

fn client_fqdn_payload(response: &v6::Message) -> Option<&[u8]> {
    match response.opts().get(OptionCode::ClientFqdn) {
        Some(DhcpOption::Unknown(option)) => Some(option.data()),
        _ => None,
    }
}

/// Return an RFC 4704 option 39 payload for a dotted FQDN.
fn fqdn_payload(flags: u8, fqdn: &str) -> Vec<u8> {
    // RFC 4704 carries flags followed by DNS label-length encoding.
    let mut payload = vec![flags];
    for label in fqdn.split('.') {
        payload.push(label.len() as u8);
        payload.extend_from_slice(label.as_bytes());
    }
    payload.push(0);
    payload
}

fn assert_stateless_response(h: &Harness, response: &v6::Message, expected_type: v6::MessageType) {
    // Stateless flows should call the API as observation and not carry IA_NA.
    assert_eq!(response.msg_type(), expected_type);
    assert_eq!(DHCPv6Factory::ia_addr(response), None);
    let discovery = h.api_server.discoveries().last().unwrap().clone();
    assert_eq!(
        discovery.message_kind,
        Some(rpc::MessageKind::V6InfoRequest as i32)
    );

    // The hook should still render service options on the response.
    assert_hook_v6_dns_servers(response);
    assert_api_v6_domain_search(response);
    assert_hook_v6_ntp_servers(response);
}

#[test]
fn information_request_is_observed_without_allocating_a_lease() -> Result<(), eyre::Report> {
    let h = Harness::new();

    // INFORMATION-REQUEST should be observed as V6InfoRequest.
    let response = send_and_recv_v6(
        &h.socket,
        DHCPv6Factory::information_request_with_client_fqdn(0x30),
    )?
    .expect("kea did not respond to INFORMATION-REQUEST");
    assert_stateless_response(&h, &response, v6::MessageType::Reply);
    let requested_flags = DHCPv6Factory::client_fqdn_payload()[0];
    let expected_client_fqdn = fqdn_payload(requested_flags, DHCP_RESPONSE_FQDN);
    assert_eq!(
        client_fqdn_payload(&response),
        Some(expected_client_fqdn.as_slice())
    );
    h.assert_no_lease_written();

    Ok(())
}

#[test]
fn information_request_with_empty_api_fqdn_does_not_echo_client_fqdn() -> Result<(), eyre::Report> {
    let idx = 0x33;
    let h = Harness::new();
    h.api_server
        .set_fqdn_override(&DHCPv6Factory::mac_string(idx), "");

    // Empty FQDN models anonymous reserved-segment options-only records.
    let response = send_and_recv_v6(
        &h.socket,
        DHCPv6Factory::information_request_with_client_fqdn(idx),
    )?
    .expect("kea did not respond to INFORMATION-REQUEST with empty API FQDN");
    assert_eq!(response.msg_type(), v6::MessageType::Reply);
    assert_eq!(DHCPv6Factory::ia_addr(&response), None);

    // Client FQDN is untrusted; an empty API FQDN removes option 39 instead of echoing.
    assert!(response.opts().get(OptionCode::ClientFqdn).is_none());
    assert_hook_v6_ntp_servers(&response);
    h.assert_no_lease_written();

    Ok(())
}

#[test]
fn stateless_solicit_is_observed_without_allocating_a_lease() -> Result<(), eyre::Report> {
    let h = Harness::new();

    // SOLICIT without IA_NA follows the same information-only path.
    let response = send_and_recv_v6(&h.socket, DHCPv6Factory::stateless_solicit(0x31))?
        .expect("kea did not respond to stateless SOLICIT");
    assert_stateless_response(&h, &response, v6::MessageType::Advertise);
    assert!(response.opts().get(OptionCode::ClientFqdn).is_none());
    h.assert_no_lease_written();

    Ok(())
}

#[test]
fn options_only_and_stateful_v6_requests_do_not_share_cache() -> Result<(), eyre::Report> {
    let idx = 0x32;
    let h = Harness::new();

    // Options-only observation returns no address and must not poison the
    // following stateful allocation cache entry for the same identity.
    let info_response = send_and_recv_v6(&h.socket, DHCPv6Factory::information_request(idx))?
        .expect("kea did not respond to INFORMATION-REQUEST");
    assert_stateless_response(&h, &info_response, v6::MessageType::Reply);
    assert_eq!(
        h.api_server
            .calls_for(mock_api_server::ENDPOINT_DISCOVER_DHCP),
        1
    );

    let solicit_response = send_and_recv_v6(&h.socket, DHCPv6Factory::solicit(idx))?
        .expect("kea did not respond to stateful SOLICIT after options-only request");
    assert_eq!(solicit_response.msg_type(), v6::MessageType::Advertise);
    assert_eq!(
        DHCPv6Factory::ia_addr(&solicit_response),
        Some(DHCPv6Factory::mock_addr(idx))
    );
    assert_eq!(
        h.api_server
            .calls_for(mock_api_server::ENDPOINT_DISCOVER_DHCP),
        2
    );

    Ok(())
}

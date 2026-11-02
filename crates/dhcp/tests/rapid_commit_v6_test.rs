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
use std::path::Path;
use std::time::{Duration, Instant};

use dhcp::mock_api_server::{self, ENDPOINT_DISCOVER_DHCP};
use dhcproto::v6::{DhcpOption, MessageType, OptionCode};
use rpc::forge as rpc;

mod common;

use common::{DHCPv6Factory, Kea6, Kea6Config, send_and_recv_v6};

const READ_TIMEOUT: Duration = Duration::from_millis(500);
const MEMFILE_TIMEOUT: Duration = Duration::from_secs(2);

/// Report whether Kea persisted the expected active non-temporary address lease.
///
/// Rapid commit is only complete when the one-exchange REPLY commits durable
/// lease state, while an ADVERTISE must remain a fake allocation.
fn active_lease_exists(path: &Path, expected_addr: &str, expected_duid: &str) -> bool {
    let Ok(contents) = fs::read_to_string(path) else {
        return false;
    };
    let expected_duid = normalize_hex(expected_duid);

    contents.lines().skip(1).any(|line| {
        let columns = line.split(',').collect::<Vec<_>>();
        columns.len() >= 14
            && columns[0] == expected_addr
            && normalize_hex(columns[1]) == expected_duid
            && columns[2].parse::<u32>().is_ok_and(|lifetime| lifetime > 0)
            && columns[6] == "0"
            && columns[13] == "0"
    })
}

/// Normalize Kea's accepted hexadecimal separators and case for DUID comparison.
///
/// The assertion concerns client identity rather than memfile presentation.
fn normalize_hex(value: &str) -> String {
    value
        .chars()
        .filter(|ch| ch.is_ascii_hexdigit())
        .map(|ch| ch.to_ascii_lowercase())
        .collect()
}

/// Wait for the active lease created by rapid commit to reach Kea's memfile.
///
/// The bounded poll distinguishes a wire-level REPLY from successful durable
/// allocation without hiding malformed or missing lease state.
fn wait_for_active_lease(path: &Path, expected_addr: &str, expected_duid: &str) {
    let deadline = Instant::now() + MEMFILE_TIMEOUT;
    while !active_lease_exists(path, expected_addr, expected_duid) {
        assert!(
            Instant::now() < deadline,
            "Kea did not persist rapid-commit lease {expected_addr} for DUID {expected_duid}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Verify the hook rejects configurations where Kea cannot complete Rapid Commit.
///
/// Core classifies an eligible SOLICIT as a committed request before Kea
/// selects its response, so accepting this mismatch would split API and lease
/// semantics.
#[test]
fn rapid_commit_rejects_disabled_kea_native_support() -> Result<(), eyre::Report> {
    // Start the API required by the generated Kea configuration.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let api_server = runtime.block_on(mock_api_server::MockAPIServer::start());

    // Enable the hook while explicitly disabling Kea's native subnet policy.
    let start_result = Kea6::start_with_config(
        api_server.local_http_addr(),
        None,
        Kea6Config {
            rapid_commit_v6: true,
            kea_rapid_commit_v6: false,
            ..Kea6Config::default()
        },
    );

    // Kea must reject the configuration rather than serve inconsistent replies.
    assert!(
        start_result.is_err(),
        "Kea accepted incompatible Rapid Commit configuration"
    );

    Ok(())
}

/// Verify that server policy and client opt-in jointly select DHCPv6 rapid commit.
///
/// This protects the real Kea hook boundary: only the eligible exchange may
/// emit one option 14, use API request semantics, and persist on SOLICIT.
#[test]
fn rapid_commit_requires_server_gate_and_client_opt_in() -> Result<(), eyre::Report> {
    struct Case {
        name: &'static str,
        rapid_commit_enabled: bool,
        rapid_commit_option_count: usize,
        expected_message_type: MessageType,
        expected_message_kind: rpc::MessageKind,
        lease_persisted: bool,
    }

    let cases = [
        // A client option cannot bypass the default-disabled server gate.
        Case {
            name: "disabled gate suppresses client opt-in",
            rapid_commit_enabled: false,
            rapid_commit_option_count: 1,
            expected_message_type: MessageType::Advertise,
            expected_message_kind: rpc::MessageKind::V6Solicit,
            lease_persisted: false,
        },
        // Duplicate client options must not bypass the disabled server gate.
        Case {
            name: "disabled gate removes duplicate client opt-in",
            rapid_commit_enabled: false,
            rapid_commit_option_count: 2,
            expected_message_type: MessageType::Advertise,
            expected_message_kind: rpc::MessageKind::V6Solicit,
            lease_persisted: false,
        },
        // The default configuration preserves the ordinary four-message flow.
        Case {
            name: "disabled gate preserves ordinary solicit",
            rapid_commit_enabled: false,
            rapid_commit_option_count: 0,
            expected_message_type: MessageType::Advertise,
            expected_message_kind: rpc::MessageKind::V6Solicit,
            lease_persisted: false,
        },
        // Enabling the server still preserves the normal flow for clients
        // that did not explicitly request the two-message exchange.
        Case {
            name: "enabled gate still requires client opt-in",
            rapid_commit_enabled: true,
            rapid_commit_option_count: 0,
            expected_message_type: MessageType::Advertise,
            expected_message_kind: rpc::MessageKind::V6Solicit,
            lease_persisted: false,
        },
        // Joint opt-in is the only path that allocates in the SOLICIT exchange.
        Case {
            name: "joint opt-in commits lease in one exchange",
            rapid_commit_enabled: true,
            rapid_commit_option_count: 1,
            expected_message_type: MessageType::Reply,
            expected_message_kind: rpc::MessageKind::V6Request,
            lease_persisted: true,
        },
    ];

    for (case_index, case) in cases.into_iter().enumerate() {
        // Each case gets an isolated Kea process, hook config, API, and memfile.
        let client_index = 0x80 + case_index as u8;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?;
        let api_server = runtime.block_on(mock_api_server::MockAPIServer::start());
        let lease_dir = tempfile::tempdir()?;
        let lease_path = lease_dir.path().join("kea-leases6.csv");
        let config = Kea6Config {
            rapid_commit_v6: case.rapid_commit_enabled,
            ..Kea6Config::default()
        };
        let (_kea, socket) =
            Kea6::start_with_config(api_server.local_http_addr(), Some(&lease_path), config)?;
        socket.set_read_timeout(Some(READ_TIMEOUT))?;

        let packet = DHCPv6Factory::solicit_with_rapid_commit_options(
            client_index,
            case.rapid_commit_option_count,
        );
        let response = send_and_recv_v6(&socket, packet)?
            .unwrap_or_else(|| panic!("Kea did not respond for {}", case.name));

        // The response form and option cardinality expose Kea's chosen flow.
        assert_eq!(
            response.msg_type(),
            case.expected_message_type,
            "{}",
            case.name
        );
        let rapid_commit_options = response
            .opts()
            .get_all(OptionCode::RapidCommit)
            .into_iter()
            .flatten()
            .filter(|option| matches!(option, DhcpOption::RapidCommit))
            .count();
        assert_eq!(
            rapid_commit_options,
            usize::from(case.lease_persisted),
            "{}",
            case.name
        );

        // The API sees request semantics only for an allocation completed by
        // this SOLICIT; every case issues exactly one family-aware call.
        assert_eq!(
            api_server.calls_for(ENDPOINT_DISCOVER_DHCP),
            1,
            "{}",
            case.name
        );
        let discoveries = api_server.discoveries();
        assert_eq!(
            discoveries[0].message_kind,
            Some(case.expected_message_kind as i32),
            "{}",
            case.name
        );
        assert_eq!(
            discoveries[0].address_family,
            Some(rpc::AddressFamily::V6 as i32),
            "{}",
            case.name
        );

        let expected_addr = DHCPv6Factory::mock_addr(client_index).to_string();
        let expected_duid = DHCPv6Factory::duid_ll_hex(client_index);
        if case.lease_persisted {
            // A durable one-exchange reply must expose the address it committed.
            assert_eq!(
                DHCPv6Factory::ia_addr(&response),
                Some(DHCPv6Factory::mock_addr(client_index)),
                "{}",
                case.name
            );
            wait_for_active_lease(&lease_path, &expected_addr, &expected_duid);
        } else {
            assert!(
                !active_lease_exists(&lease_path, &expected_addr, &expected_duid),
                "{} must not persist its advertised address",
                case.name
            );
        }
    }

    Ok(())
}

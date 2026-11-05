/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

#[path = "support/nic_firmware.rs"]
mod support;

use prost::Message as _;
use rpc::forge::{
    AddNetworkSegmentPrefixRequest, NetworkPrefix, NetworkSegment, NetworkSegmentConfig,
};
use support::{Exchange, assert_success, run_command};

/// The real command must send the selected prefix/relay/reservation and render
/// the server's two stored prefixes, including populated and absent relay cells.
#[tokio::test]
async fn add_prefix_sends_request_and_displays_stored_prefixes() {
    let id = "12345678-1234-5678-90ab-cdef01234567";
    let expected: Vec<_> = [
        ("Prefix", "192.0.2.0/24"),
        ("DHCPv6 Link", ""),
        ("Reserve First", "0"),
        ("Free IP Count", "Unknown"),
        ("Prefix", "2001:db8::/64"),
        ("DHCPv6 Link", "2001:db8:ffff::1"),
        ("Reserve First", "9"),
        ("Free IP Count", "Unknown"),
    ]
    .map(|(label, value)| (label.to_string(), value.to_string()))
    .to_vec();
    let cases = [
        // Exercise the IPv6 example, including explicit reservation and relay values.
        carbide_test_support::Case {
            scenario: "IPv6 with relay",
            input: (
                vec![
                    "--prefix",
                    "2001:db8::/64",
                    "--reserve-first",
                    "5",
                    "--dhcpv6-link-address",
                    "2001:db8:ffff::1",
                ],
                AddNetworkSegmentPrefixRequest {
                    network_segment_id: Some(id.parse().expect("segment UUID")),
                    prefix: "2001:db8::/64".to_string(),
                    gateway: None,
                    reserve_first: 5,
                    dhcpv6_link_address: Some("2001:db8:ffff::1".to_string()),
                },
            ),
            expect: carbide_test_support::Outcome::Yields(expected.clone()),
        },
        // The opposite direction must send the gateway and use omitted defaults.
        carbide_test_support::Case {
            scenario: "IPv4 with gateway",
            input: (
                vec!["--prefix", "192.0.2.0/24", "--gateway", "192.0.2.1"],
                AddNetworkSegmentPrefixRequest {
                    network_segment_id: Some(id.parse().expect("segment UUID")),
                    prefix: "192.0.2.0/24".to_string(),
                    gateway: Some("192.0.2.1".to_string()),
                    ..Default::default()
                },
            ),
            expect: carbide_test_support::Outcome::Yields(expected),
        },
    ];
    carbide_test_support::check_cases_async(cases, |(suffix, request)| async move {
        let mut args = vec![
            "--cloud-unsafe-op=admin",
            "network-segment",
            "add-prefix",
            "--id",
            id,
        ];
        args.extend(suffix);
        let output = run_command(
            &args,
            None,
            vec![Exchange {
                method: "/forge.Forge/AddNetworkSegmentPrefix",
                request: request.encode_to_vec(),
                response: Ok(NetworkSegment {
                    id: Some(id.parse().expect("segment UUID")),
                    config: Some(NetworkSegmentConfig {
                        prefixes: vec![
                            // The old prefix remains visible without a relay value.
                            NetworkPrefix {
                                prefix: "192.0.2.0/24".to_string(),
                                gateway: Some("192.0.2.1".to_string()),
                                ..Default::default()
                            },
                            // A server-returned reservation different from either request
                            // prevents request-based success output passing this assertion.
                            NetworkPrefix {
                                prefix: "2001:db8::/64".to_string(),
                                reserve_first: 9,
                                dhcpv6_link_address: Some("2001:db8:ffff::1".to_string()),
                                ..Default::default()
                            },
                        ],
                        ..Default::default()
                    }),
                    ..Default::default()
                }
                .encode_to_vec()),
            }],
        )
        .await;
        assert_success(&output);
        let output = String::from_utf8(output.stdout).expect("UTF-8 output");
        Ok::<_, std::convert::Infallible>(
            output
                .lines()
                .filter_map(|line| {
                    let (label, value) = line.trim().split_once(':')?;
                    matches!(
                        label.trim(),
                        "Prefix" | "DHCPv6 Link" | "Reserve First" | "Free IP Count"
                    )
                    .then_some((label.trim().to_string(), value.trim().to_string()))
                })
                .collect::<Vec<_>>(),
        )
    })
    .await;
}

/// The acknowledgement guard must reject before issuing a mutation RPC.
#[tokio::test]
async fn add_prefix_requires_cloud_unsafe_acknowledgement() {
    let output = run_command(
        &[
            "network-segment",
            "add-prefix",
            "--id",
            "12345678-1234-5678-90ab-cdef01234567",
            "--prefix",
            "2001:db8::/64",
        ],
        None,
        vec![],
    )
    .await;
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("operation not allowed"));
}

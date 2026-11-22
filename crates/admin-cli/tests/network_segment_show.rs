/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

#[path = "support/nic_firmware.rs"]
mod support;

use prost::Message as _;
use rpc::forge::{
    NetworkPrefix, NetworkSegment, NetworkSegmentConfig, NetworkSegmentList,
    NetworkSegmentStateHistoriesRequest, NetworkSegmentsByIdsRequest, StateHistories,
    StateHistoryRecord, StateHistoryRecords,
};
use support::{Exchange, assert_success, run_command};

/// An omitted legacy scalar decodes to zero on both prefixes. Only the
/// prefix counted by Core should display that zero.
#[tokio::test]
async fn show_distinguishes_legacy_zero_from_an_uncounted_prefix() {
    let id = "12345678-1234-5678-90ab-cdef01234567";
    let segment_id = id.parse().expect("segment UUID");
    // Put IPv6 first so assuming that only IPv4 was counted fails here.
    let prefixes = ["2001:db8::/126", "192.0.2.0/24"]
        .into_iter()
        .map(|cidr| NetworkPrefix {
            prefix: cidr.to_string(),
            ..Default::default()
        })
        .collect();
    let output = run_command(
        &["network-segment", "show", id],
        None,
        vec![
            Exchange {
                method: "/forge.Forge/FindNetworkSegmentsByIds",
                request: NetworkSegmentsByIdsRequest {
                    network_segments_ids: vec![segment_id],
                    include_num_free_ips: true,
                    include_history: false,
                }
                .encode_to_vec(),
                response: Ok(NetworkSegmentList {
                    network_segments: vec![NetworkSegment {
                        id: Some(segment_id),
                        config: Some(NetworkSegmentConfig {
                            prefixes,
                            ..Default::default()
                        }),
                        ..Default::default()
                    }],
                }
                .encode_to_vec()),
            },
            Exchange {
                method: "/forge.Forge/FindNetworkSegmentStateHistories",
                request: NetworkSegmentStateHistoriesRequest {
                    network_segment_ids: vec![segment_id],
                }
                .encode_to_vec(),
                response: Ok(StateHistories {
                    histories: [(
                        id.to_string(),
                        StateHistoryRecords {
                            records: vec![StateHistoryRecord {
                                state: r#"{"state":"Ready"}"#.to_string(),
                                version: "42".to_string(),
                                ..Default::default()
                            }],
                        },
                    )]
                    .into(),
                }
                .encode_to_vec()),
            },
        ],
    )
    .await;
    assert_success(&output);
    let stdout = String::from_utf8(output.stdout).expect("UTF-8 output");
    let (_, history) = stdout
        .split_once("STATE HISTORY: (Latest 5 only)")
        .expect("detailed show should render the history returned by Core");
    assert!(history.lines().any(|line| {
        let mut columns = line.split_whitespace();
        columns.next() == Some("Ready") && columns.next() == Some("42")
    }));
    let fields: Vec<_> = stdout
        .lines()
        .filter_map(|line| {
            let (label, value) = line.trim().split_once(':')?;
            let label = label.trim();
            matches!(label, "Prefix" | "Free IP Count").then_some((label, value.trim()))
        })
        .collect();
    assert_eq!(
        fields,
        [
            ("Prefix", "2001:db8::/126"),
            ("Free IP Count", "0"),
            ("Prefix", "192.0.2.0/24"),
            ("Free IP Count", "Unknown"),
        ]
    );
}

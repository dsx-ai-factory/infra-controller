/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

#[path = "support/nic_firmware.rs"]
mod support;

use carbide_uuid::operating_system::OperatingSystemId;
use prost::Message as _;
use rpc::forge::{
    IpxeTemplateArtifact, IpxeTemplateParameter, OperatingSystem, OperatingSystemIdList,
    OperatingSystemList, OperatingSystemSearchFilter, OperatingSystemsByIdsRequest,
};
use support::{Exchange, assert_success, run_command};

#[tokio::test]
async fn operating_system_list_preserves_multiline_values_and_empty_defaults() {
    let ids: [OperatingSystemId; 2] = [
        "abcdef01-2345-6789-abcd-ef0123456789".parse().unwrap(),
        "12345678-1234-5678-90ab-cdef01234567".parse().unwrap(),
    ];
    let output = run_command(
        &["operating-system", "show"],
        None,
        vec![
            Exchange {
                method: rpc::service_path!("FindOperatingSystemIds"),
                request: OperatingSystemSearchFilter::default().encode_to_vec(),
                response: Ok(OperatingSystemIdList { ids: ids.to_vec() }.encode_to_vec()),
            },
            Exchange {
                method: rpc::service_path!("FindOperatingSystemsByIds"),
                request: OperatingSystemsByIdsRequest { ids: ids.to_vec() }.encode_to_vec(),
                response: Ok(OperatingSystemList {
                    operating_systems: vec![
                        OperatingSystem {
                            id: Some(ids[0]),
                            name: "populated-os".to_string(),
                            ipxe_template_parameters: vec![
                                IpxeTemplateParameter {
                                    name: "z-last".to_string(),
                                    value: "κ=\"quoted\"".to_string(),
                                },
                                IpxeTemplateParameter {
                                    name: "a-first".to_string(),
                                    value: String::new(),
                                },
                            ],
                            ipxe_template_artifacts: vec![
                                IpxeTemplateArtifact {
                                    name: "z-initrd".to_string(),
                                    url: "https://example.test/initrd".to_string(),
                                    ..Default::default()
                                },
                                IpxeTemplateArtifact {
                                    name: "a-kernel".to_string(),
                                    url: "https://example.test/kernel".to_string(),
                                    ..Default::default()
                                },
                            ],
                            ..Default::default()
                        },
                        OperatingSystem {
                            id: Some(ids[1]),
                            name: "empty-os".to_string(),
                            ..Default::default()
                        },
                    ],
                }
                .encode_to_vec()),
            },
        ],
    )
    .await;
    assert_success(&output);
    let output = String::from_utf8(output.stdout).expect("UTF-8 CLI output");
    let rows: Vec<Vec<&str>> = output
        .lines()
        .filter_map(|line| line.trim().strip_prefix('|')?.strip_suffix('|'))
        .map(|line| line.split('|').map(str::trim).collect())
        .collect();
    assert_eq!(
        rows[0],
        [
            "ID",
            "Name",
            "Org",
            "Type",
            "Template",
            "Parameters",
            "Artifacts",
            "Active",
        ]
    );
    let values: Vec<_> = rows[1..]
        .iter()
        .map(|row| (row[1], row[5], row[6]))
        .collect();
    assert_eq!(
        values,
        [
            (
                "populated-os",
                "z-last=κ=\"quoted\"",
                "z-initrd: https://example.test/initrd",
            ),
            ("", "a-first=", "a-kernel: https://example.test/kernel"),
            ("empty-os", "-", "-"),
        ],
        "{output}"
    );
}

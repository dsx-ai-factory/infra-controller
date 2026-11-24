/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

#[path = "support/nic_firmware.rs"]
mod support;

use prost::Message as _;
use rpc::forge::{self, ExpectedInterface, ExpectedInterfaceRole, ExpectedMachine};
use support::{Exchange, assert_success, run_command};

/// List output exposes reservations for each DPU without displaying OS addresses
/// as loopbacks, and leaves the cell empty for machines without reservations.
#[tokio::test]
async fn loopback_show_displays_mac_and_reserved_addresses() {
    let interfaces = [
        (
            ExpectedInterfaceRole::DpuOs,
            "02:00:00:00:20:01",
            "192.0.2.10",
        ),
        (
            ExpectedInterfaceRole::DpuLoopback,
            "02:00:00:00:20:01",
            "198.51.100.20",
        ),
        (
            ExpectedInterfaceRole::DpuLoopback,
            "02:00:00:00:20:01",
            "2001:db8::20",
        ),
        (
            ExpectedInterfaceRole::DpuLoopback,
            "02:00:00:00:20:02",
            "198.51.100.21",
        ),
    ]
    .into_iter()
    .map(|(role, mac, ip)| ExpectedInterface {
        mac_address: Some(mac.to_string()),
        role: Some(role as i32),
        fixed_ip: Some(ip.to_string()),
        ..Default::default()
    })
    .collect();
    let output = run_command(
        &["em", "show"],
        None,
        vec![
            Exchange {
                method: "/forge.Forge/GetAllExpectedMachines",
                request: vec![],
                response: Ok(forge::ExpectedMachineList {
                    expected_machines: vec![
                        ExpectedMachine {
                            bmc_mac_address: "00:11:22:33:44:55".to_string(),
                            chassis_serial_number: "with-reservations".to_string(),
                            host_nics: interfaces,
                            ..Default::default()
                        },
                        ExpectedMachine {
                            bmc_mac_address: "00:11:22:33:44:66".to_string(),
                            chassis_serial_number: "without-reservations".to_string(),
                            ..Default::default()
                        },
                    ],
                }
                .encode_to_vec()),
            },
            Exchange {
                method: "/forge.Forge/FindInterfaces",
                request: forge::InterfaceSearchQuery::default().encode_to_vec(),
                response: Ok(forge::InterfaceList::default().encode_to_vec()),
            },
            Exchange {
                method: "/forge.Forge/FindMachineIdsByBmcIps",
                request: forge::BmcIpList::default().encode_to_vec(),
                response: Ok(forge::MachineIdBmcIpPairs::default().encode_to_vec()),
            },
        ],
    )
    .await;
    assert_success(&output);
    let output = String::from_utf8(output.stdout).expect("UTF-8 CLI output");
    let mut rows = output
        .lines()
        .filter_map(|line| line.trim().strip_prefix('|')?.strip_suffix('|'))
        .map(|line| line.split('|').map(str::trim).collect::<Vec<_>>());
    let headers = rows.next().expect("table header");
    let column = headers
        .iter()
        .position(|header| *header == "DPU Loopback Reservations")
        .expect("loopback reservation column");
    let reservations: Vec<_> = rows.map(|row| (row[0], row[column])).collect();
    assert_eq!(
        reservations,
        [
            ("with-reservations", "02:00:00:00:20:01: 198.51.100.20"),
            ("", "02:00:00:00:20:01: 2001:db8::20"),
            ("", "02:00:00:00:20:02: 198.51.100.21"),
            ("without-reservations", ""),
        ],
        "{output}"
    );
}

/// JSON output exposes the unified interface list for scripts.
#[tokio::test]
async fn loopback_show_json_uses_interface_entries() {
    let output = run_command(
        &["--format", "json", "expected-machine", "show"],
        None,
        vec![Exchange {
            method: "/forge.Forge/GetAllExpectedMachines",
            request: vec![],
            response: Ok(forge::ExpectedMachineList {
                expected_machines: vec![ExpectedMachine {
                    host_nics: vec![ExpectedInterface {
                        mac_address: Some("02:00:00:00:20:01".to_string()),
                        role: Some(ExpectedInterfaceRole::DpuLoopback as i32),
                        fixed_ip: Some("198.51.100.20".to_string()),
                        ..Default::default()
                    }],
                    ..Default::default()
                }],
            }
            .encode_to_vec()),
        }],
    )
    .await;
    assert_success(&output);
    let output: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("JSON CLI output");
    let interfaces = output["expected_machines"][0]["interfaces"]
        .as_array()
        .expect("interface list");
    assert_eq!(interfaces.len(), 1);
    assert_eq!(interfaces[0]["role"], "dpu_loopback");
    assert_eq!(interfaces[0]["mac_address"], "02:00:00:00:20:01");
    assert_eq!(interfaces[0]["fixed_ip"], "198.51.100.20");
}

/// The public command carries explicit replacement intent only when the user
/// supplies interfaces. In particular, an empty array must clear reservations.
#[tokio::test]
async fn loopback_patch_examples_send_replacement_intent() {
    let mac = "00:11:22:33:44:55";
    let id = rpc::common::Uuid {
        value: "12345678-1234-5678-90ab-cdef01234567".to_string(),
    };
    let loopback = ExpectedInterface {
        mac_address: Some("02:00:00:00:20:01".to_string()),
        role: Some(ExpectedInterfaceRole::DpuLoopback as i32),
        fixed_ip: Some("198.51.100.20".to_string()),
        ..Default::default()
    };
    for (flag, value, interfaces, replace, selected, sku) in [
        (
            "--interfaces",
            r#"[{"mac_address":"02:00:00:00:20:01","role":"dpu_loopback","fixed_ip":"198.51.100.20"}]"#,
            vec![loopback],
            true,
            "host_nics",
            None,
        ),
        ("--interfaces", "[]", vec![], true, "host_nics", None),
        (
            "--sku-id",
            "DGX-H100-640GB",
            vec![],
            false,
            "sku_id",
            Some("DGX-H100-640GB"),
        ),
    ] {
        let mut patch = forge::PatchExpectedMachineRequest {
            expected_machine: Some(ExpectedMachine {
                id: Some(id.clone()),
                host_nics: interfaces,
                replace_dpu_loopbacks: replace,
                sku_id: sku.map(str::to_string),
                ..Default::default()
            }),
            ..Default::default()
        };
        patch.update_mask.get_or_insert_default().paths = vec![selected.to_string()];
        let output = run_command(
            &[
                "expected-machine",
                "patch",
                "--bmc-mac-address",
                mac,
                flag,
                value,
            ],
            None,
            vec![
                Exchange {
                    method: "/forge.Forge/GetExpectedMachine",
                    request: forge::ExpectedMachineRequest {
                        bmc_mac_address: mac.to_string(),
                        id: None,
                    }
                    .encode_to_vec(),
                    response: Ok(ExpectedMachine {
                        id: Some(id.clone()),
                        ..Default::default()
                    }
                    .encode_to_vec()),
                },
                Exchange {
                    method: "/forge.Forge/PatchExpectedMachine",
                    request: patch.encode_to_vec(),
                    response: Ok(vec![]),
                },
            ],
        )
        .await;
        assert_success(&output);
    }
}

/// Exercise the shared-MAC example through the public command and RPC boundary.
#[tokio::test]
async fn loopback_add_example_keeps_os_and_loopback_roles_distinct() {
    let interfaces = vec![
        ExpectedInterface {
            mac_address: Some("02:00:00:00:20:01".to_string()),
            role: Some(ExpectedInterfaceRole::DpuOs as i32),
            ip_allocation: Some(forge::ExpectedInterfaceIpAllocation::Fixed as i32),
            fixed_ip: Some("192.0.2.10".to_string()),
            ..Default::default()
        },
        ExpectedInterface {
            mac_address: Some("02:00:00:00:20:01".to_string()),
            role: Some(ExpectedInterfaceRole::DpuLoopback as i32),
            fixed_ip: Some("198.51.100.20".to_string()),
            ..Default::default()
        },
    ];
    let output = run_command(
        &[
            "expected-machine", "add", "--bmc-mac-address", "00:11:22:33:44:55",
            "--bmc-username", "admin", "--bmc-password", "mypassword",
            "--chassis-serial-number", "sample_serial-1", "--interfaces",
            r#"[{"mac_address":"02:00:00:00:20:01","role":"dpu_os","ip_allocation":"fixed","fixed_ip":"192.0.2.10"},{"mac_address":"02:00:00:00:20:01","role":"dpu_loopback","fixed_ip":"198.51.100.20"}]"#,
        ],
        None,
        vec![Exchange {
            method: "/forge.Forge/AddExpectedMachine",
            request: ExpectedMachine {
                bmc_mac_address: "00:11:22:33:44:55".to_string(),
                bmc_username: "admin".to_string(),
                bmc_password: "mypassword".to_string(),
                chassis_serial_number: "sample_serial-1".to_string(),
                metadata: Some(forge::Metadata::default()),
                host_nics: interfaces,
                #[allow(deprecated)]
                dpf_enabled: true,
                ..Default::default()
            }.encode_to_vec(),
            response: Ok(vec![]),
        }],
    ).await;
    assert_success(&output);
}

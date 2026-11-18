/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

#[path = "support/nic_firmware.rs"]
mod support;

use prost::Message as _;
use rpc::forge::nic_firmware_device_target::Result as TargetResult;
use rpc::forge::{
    GetDesiredNicFirmwareRequest, GetDesiredNicFirmwareResponse, NicFirmwareApproach,
    NicFirmwareArtifact, NicFirmwareDeviceTarget, NicFirmwareProfileEntry, NicFirmwareResolution,
    NicFirmwareSelectionScope,
};
use rpc::protos::mlx_device::{FirmwareSpec, MlxDeviceIdentity, MlxDeviceInfo};
use serde_json::{Value, json};
use support::{Exchange, assert_success, run_command};

const HOST_ID: &str = "fm100hseddco33hvlofuqvg543p6p9aj60g76q5cq491g9m9tgtf2dk0530";
const DPU_ID: &str = "fm100dskla0ihp0pn4tv7v1js2k2mo37sl0jjr8141okqg8pjpdpfihaa80";
const PROFILE_VERSION: &str = "V7-T1789080000000000";
const SITE_VERSION: &str = "V2-T1789080000000001";
const ERROR_SITE_VERSION: &str = "V3-T1789080000000002";
const OBSERVED_AT: &str = "2026-10-14T12:00:00+00:00";
const WARNINGS: [&str; 2] = [
    "saved card BaseGuid(\"0011223344556677\") has no match, selection retained",
    "saved card BaseMac(00:11:22:33:44:99) has no match",
];
const HEADERS: [&str; 13] = [
    "Device",
    "Base MAC",
    "Base GUID",
    "Part Number",
    "PSID",
    "Observed Firmware",
    "Target Firmware",
    "Status",
    "Scope",
    "Profile ID",
    "Profile Version",
    "Site Selection Version",
    "Error",
];
const MIXED_ROWS: [[&str; 13]; 7] = [
    [
        "0000:83:00.0",
        "00:11:22:33:44:55",
        "0011223344556655",
        "MCX75310AAS-NEAT",
        "MT_0000000838",
        "28.43.1014",
        "28.42.1000",
        "Resolved",
        "Card",
        "connectx-pinned",
        PROFILE_VERSION,
        SITE_VERSION,
        "",
    ],
    [
        "0000:03:00.0",
        "",
        "",
        "MCX755106AS-HEAT",
        "MT_0000000840",
        "",
        "",
        "Unmanaged",
        "",
        "",
        "",
        "",
        "",
    ],
    [
        "0000:53:00.0",
        "00:11:22:33:44:77",
        "",
        "900-9D3B4-00CV-TA0",
        "MT_0000000884",
        "32.43.1014",
        "",
        "Error",
        "",
        "",
        "",
        "",
        "device is associated with a managed DPU",
    ],
    [
        "0000:93:00.0",
        "00:11:22:33:44:61",
        "0011223344556661",
        "MCX75310AAS-NEAT",
        "MT_0000000838",
        "28.43.1014",
        "28.42.1000",
        "Resolved",
        "Site",
        "connectx-site",
        PROFILE_VERSION,
        SITE_VERSION,
        "",
    ],
    [
        "0000:23:00.0",
        "00:11:22:33:44:62",
        "0011223344556662",
        "MCX75310AAS-NEAT",
        "MT_0000000838",
        "28.43.1014",
        "28.42.1000",
        "Resolved",
        "Allocation",
        "connectx-allocation",
        PROFILE_VERSION,
        SITE_VERSION,
        "",
    ],
    [
        "0000:73:00.0",
        "00:11:22:33:44:63",
        "0011223344556663",
        "MCX75310AAS-NEAT",
        "MT_0000000838",
        "28.43.1014",
        "28.42.1000",
        "Resolved",
        "Machine",
        "connectx-machine",
        PROFILE_VERSION,
        SITE_VERSION,
        "",
    ],
    [
        "0000:43:00.0",
        "00:11:22:33:44:88",
        "",
        "MCX75310AAS-NEAT",
        "MT_0000000838",
        "28.43.1014",
        "",
        "Error",
        "",
        "",
        "",
        ERROR_SITE_VERSION,
        "selected profile does not support this hardware",
    ],
];

/// `desired_reply` keeps report order distinct from PCI order and includes each
/// result kind and scope without asking the CLI to repeat Core's selection logic.
fn desired_reply() -> GetDesiredNicFirmwareResponse {
    let mut response = GetDesiredNicFirmwareResponse {
        observed_at: Some(
            OBSERVED_AT
                .parse::<chrono::DateTime<chrono::Utc>>()
                .expect("fixture observation time")
                .into(),
        ),
        targets: vec![
            // The exact target is older than the observed version. It must not
            // disappear as an apparent no-op or lose its independent site token.
            NicFirmwareDeviceTarget {
                identity: Some(MlxDeviceIdentity {
                    device_info: Some(MlxDeviceInfo {
                        pci_name: "0000:83:00.0".into(),
                        device_type: "ConnectX-7".into(),
                        part_number: "MCX75310AAS-NEAT".into(),
                        psid: "MT_0000000838".into(),
                        fw_version_current: "28.43.1014".into(),
                        base_mac: "00:11:22:33:44:55".into(),
                        base_guid: Some("0011223344556655".into()),
                        ..Default::default()
                    }),
                    managed_dpu_machine_ids: Vec::new(),
                }),
                result: Some(TargetResult::Firmware(NicFirmwareResolution {
                    scope: NicFirmwareSelectionScope::Card.into(),
                    profile_id: "connectx-pinned".into(),
                    profile_version: PROFILE_VERSION.into(),
                    entry: Some(NicFirmwareProfileEntry {
                        firmware: Some(FirmwareSpec {
                            part_number: "MCX75310AAS-NEAT".into(),
                            psid: "MT_0000000838".into(),
                            version: "28.42.1000".into(),
                        }),
                        image: Some(NicFirmwareArtifact {
                            url: "https://firmware.invalid/image.bin".into(),
                            sha256: "ab".repeat(32),
                        }),
                        device_config: Some(NicFirmwareArtifact {
                            url: "https://firmware.invalid/config.bin".into(),
                            sha256: "cd".repeat(32),
                        }),
                        approach: NicFirmwareApproach::Scout.into(),
                    }),
                })),
                site_selection_version: Some(SITE_VERSION.into()),
            },
            // An absent result is unmanaged, not a missing row or a failure.
            NicFirmwareDeviceTarget {
                identity: Some(MlxDeviceIdentity {
                    device_info: Some(MlxDeviceInfo {
                        pci_name: "0000:03:00.0".into(),
                        part_number: "MCX755106AS-HEAT".into(),
                        psid: "MT_0000000840".into(),
                        ..Default::default()
                    }),
                    managed_dpu_machine_ids: Vec::new(),
                }),
                result: None,
                site_selection_version: None,
            },
            // Preserve the excluded device's identity and diagnostic without
            // inventing a profile or site selection for it.
            NicFirmwareDeviceTarget {
                identity: Some(MlxDeviceIdentity {
                    device_info: Some(MlxDeviceInfo {
                        pci_name: "0000:53:00.0".into(),
                        part_number: "900-9D3B4-00CV-TA0".into(),
                        psid: "MT_0000000884".into(),
                        fw_version_current: "32.43.1014".into(),
                        base_mac: "00:11:22:33:44:77".into(),
                        ..Default::default()
                    }),
                    managed_dpu_machine_ids: vec![DPU_ID.parse().expect("fixture DPU ID")],
                }),
                result: Some(TargetResult::Error(
                    "device is associated with a managed DPU".into(),
                )),
                site_selection_version: None,
            },
        ],
        warnings: WARNINGS.into_iter().map(str::to_owned).collect(),
    };
    // The remaining scopes exercise presentation only: Core has already chosen
    // each result, and optional device-config artifacts need not be present.
    for (pci_name, base_mac, base_guid, scope, profile_id) in [
        (
            "0000:93:00.0",
            "00:11:22:33:44:61",
            "0011223344556661",
            NicFirmwareSelectionScope::Site,
            "connectx-site",
        ),
        (
            "0000:23:00.0",
            "00:11:22:33:44:62",
            "0011223344556662",
            NicFirmwareSelectionScope::Allocation,
            "connectx-allocation",
        ),
        (
            "0000:73:00.0",
            "00:11:22:33:44:63",
            "0011223344556663",
            NicFirmwareSelectionScope::Machine,
            "connectx-machine",
        ),
    ] {
        let mut target = response.targets[0].clone();
        let device = target
            .identity
            .as_mut()
            .expect("fixture target identity")
            .device_info
            .as_mut()
            .expect("fixture device facts");
        device.pci_name = pci_name.into();
        device.base_mac = base_mac.into();
        device.base_guid = Some(base_guid.into());
        let Some(TargetResult::Firmware(firmware)) = target.result.as_mut() else {
            panic!("first fixture target must have firmware");
        };
        firmware.scope = scope.into();
        firmware.profile_id = profile_id.into();
        firmware
            .entry
            .as_mut()
            .expect("fixture firmware entry")
            .device_config = None;
        response.targets.push(target);
    }
    // A resolution error may still retain a site token; only the managed-DPU
    // exclusion above lacks one. Neither error may hide successful devices.
    response.targets.push(NicFirmwareDeviceTarget {
        identity: Some(MlxDeviceIdentity {
            device_info: Some(MlxDeviceInfo {
                pci_name: "0000:43:00.0".into(),
                part_number: "MCX75310AAS-NEAT".into(),
                psid: "MT_0000000838".into(),
                fw_version_current: "28.43.1014".into(),
                base_mac: "00:11:22:33:44:88".into(),
                ..Default::default()
            }),
            managed_dpu_machine_ids: Vec::new(),
        }),
        result: Some(TargetResult::Error(
            "selected profile does not support this hardware".into(),
        )),
        site_selection_version: Some(ERROR_SITE_VERSION.into()),
    });
    response
}

/// `desired_exchange` requires exactly one desired read, including the optional
/// preview profile; the shared harness rejects extra requests or mutations.
fn desired_exchange(
    response: GetDesiredNicFirmwareResponse,
    allocation_profile_id: Option<&str>,
) -> Exchange {
    Exchange {
        method: "/forge.Forge/GetDesiredNicFirmware",
        request: GetDesiredNicFirmwareRequest {
            machine_id: Some(HOST_ID.parse().expect("fixture host ID")),
            allocation_profile_id: allocation_profile_id.map(str::to_owned),
        }
        .encode_to_vec(),
        response: Ok(response.encode_to_vec()),
    }
}

/// The public table preserves every scope and both card-selector identities,
/// keeps errors beside successful devices, and retains metadata without devices.
#[tokio::test]
async fn desired_table_preserves_results_order_and_empty_report_metadata() {
    struct Case {
        scenario: &'static str,
        response: GetDesiredNicFirmwareResponse,
        observed_at: &'static str,
        rows: &'static [[&'static str; 13]],
    }
    for case in [
        // Resolved, unmanaged and excluded devices all remain visible together.
        Case {
            scenario: "all scopes, unmanaged devices and errors retain report order",
            response: desired_reply(),
            observed_at: OBSERVED_AT,
            rows: &MIXED_ROWS,
        },
        // No observation cannot erase warnings for stored unmatched selections.
        Case {
            scenario: "no Scout observation",
            response: GetDesiredNicFirmwareResponse {
                warnings: WARNINGS.into_iter().map(str::to_owned).collect(),
                ..Default::default()
            },
            observed_at: "Not observed",
            rows: &[],
        },
    ] {
        let output = run_command(
            &["firmware", "nic", "desired", HOST_ID],
            None,
            vec![desired_exchange(case.response, None)],
        )
        .await;
        assert_success(&output);
        let text = String::from_utf8(output.stdout).expect("desired table is UTF-8");
        let mut lines = text.lines();
        assert_eq!(
            lines.next(),
            Some(format!("Scout observed at: {}", case.observed_at).as_str()),
            "{}",
            case.scenario,
        );
        for warning in WARNINGS {
            assert_eq!(
                lines.next(),
                Some(format!("Warning: {warning}").as_str()),
                "{}",
                case.scenario,
            );
        }
        let rows: Vec<Vec<_>> = lines
            .filter_map(|line| line.trim().strip_prefix('|')?.strip_suffix('|'))
            .map(|line| line.split('|').map(str::trim).collect())
            .collect();
        let expected: Vec<Vec<_>> = std::iter::once(HEADERS.as_slice())
            .chain(case.rows.iter().map(|row| row.as_slice()))
            .map(|row| row.to_vec())
            .collect();
        assert_eq!(rows, expected, "{}: {text}", case.scenario);
    }
}

/// Structured output preserves scopes, identities and independent site tokens,
/// including on errors; absent observations and optional artifacts remain null.
/// Only an explicitly requested allocation preview is passed to Core.
#[tokio::test]
async fn desired_structured_output_preserves_full_reports_and_preview_requests() {
    let response = desired_reply();
    let identities: Vec<_> = response
        .targets
        .iter()
        .map(|target| serde_json::to_value(&target.identity).expect("fixture identity serializes"))
        .collect();
    let mut expected = json!({
        "observed_at": "2026-10-14T12:00:00Z",
        "targets": [
            {
                "identity": identities[0],
                "result": {"Firmware": {
                    "scope": "Card",
                    "profile_id": "connectx-pinned",
                    "profile_version": PROFILE_VERSION,
                    "entry": {
                        "firmware": {
                            "part_number": "MCX75310AAS-NEAT",
                            "psid": "MT_0000000838",
                            "version": "28.42.1000"
                        },
                        "image": {
                            "url": "https://firmware.invalid/image.bin",
                            "sha256": "ab".repeat(32)
                        },
                        "device_config": {
                            "url": "https://firmware.invalid/config.bin",
                            "sha256": "cd".repeat(32)
                        },
                        "approach": "Scout"
                    }
                }},
                "site_selection_version": SITE_VERSION
            },
            {
                "identity": identities[1],
                "result": null,
                "site_selection_version": null
            },
            {
                "identity": identities[2],
                "result": {"Error": "device is associated with a managed DPU"},
                "site_selection_version": null
            }
        ],
        "warnings": WARNINGS
    });
    for (index, scope, profile_id) in [
        (3, "Site", "connectx-site"),
        (4, "Allocation", "connectx-allocation"),
        (5, "Machine", "connectx-machine"),
    ] {
        let mut target = expected["targets"][0].clone();
        target["identity"] = identities[index].clone();
        target["result"]["Firmware"]["scope"] = scope.into();
        target["result"]["Firmware"]["profile_id"] = profile_id.into();
        target["result"]["Firmware"]["entry"]["device_config"] = Value::Null;
        expected["targets"]
            .as_array_mut()
            .expect("expected target array")
            .push(target);
    }
    expected["targets"]
        .as_array_mut()
        .expect("expected target array")
        .push(json!({
            "identity": identities[6],
            "result": {"Error": "selected profile does not support this hardware"},
            "site_selection_version": ERROR_SITE_VERSION
        }));
    let directory =
        std::env::temp_dir().join(format!("nic-desired-output-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&directory).expect("create desired output directory");
    let output_path = directory.join("desired.json");
    struct Case {
        scenario: &'static str,
        format: &'static str,
        output_file: bool,
        allocation_profile: Option<&'static str>,
        response: GetDesiredNicFirmwareResponse,
        expected: Value,
    }
    for case in [
        // A saved JSON report must use --output without inventing a preview.
        Case {
            scenario: "complete JSON file without allocation preview",
            format: "json",
            output_file: true,
            allocation_profile: None,
            response: response.clone(),
            expected: expected.clone(),
        },
        // YAML uses tagged result variants and forwards the explicit preview.
        Case {
            scenario: "complete YAML report with allocation preview",
            format: "yaml",
            output_file: false,
            allocation_profile: Some("allocation-preview"),
            response,
            expected,
        },
        // Absence has a structured null timestamp, not a fabricated epoch or
        // the human-readable table's "Not observed" placeholder.
        Case {
            scenario: "JSON without a Scout observation retains warnings",
            format: "json",
            output_file: false,
            allocation_profile: None,
            response: GetDesiredNicFirmwareResponse {
                warnings: WARNINGS.into_iter().map(str::to_owned).collect(),
                ..Default::default()
            },
            expected: json!({
                "observed_at": null,
                "targets": [],
                "warnings": WARNINGS
            }),
        },
    ] {
        let mut args = vec!["--format", case.format];
        if case.output_file {
            args.extend(["--output", output_path.to_str().expect("UTF-8 output path")]);
        }
        args.extend(["firmware", "nic", "desired", HOST_ID]);
        if let Some(profile_id) = case.allocation_profile {
            args.extend(["--allocation-profile", profile_id]);
        }
        let output = run_command(
            &args,
            None,
            vec![desired_exchange(case.response, case.allocation_profile)],
        )
        .await;
        assert_success(&output);
        let bytes = if case.output_file {
            assert!(
                output.stdout.is_empty(),
                "{}: --output must leave stdout empty",
                case.scenario,
            );
            std::fs::read(&output_path).expect("read desired JSON output")
        } else {
            output.stdout
        };
        if case.format == "json" {
            let report: Value = serde_json::from_slice(&bytes).expect("desired JSON parses");
            assert_eq!(report, case.expected, "{}", case.scenario);
        } else {
            let report: serde_yaml::Value =
                serde_yaml::from_slice(&bytes).expect("desired YAML parses");
            let mut expected_yaml =
                serde_yaml::to_value(&case.expected).expect("expected YAML value");
            // YAML represents enum variants as tags instead of JSON's one-key
            // objects. Assert those tags while sharing every report field.
            for (index, variant) in [
                (0, "Firmware"),
                (2, "Error"),
                (3, "Firmware"),
                (4, "Firmware"),
                (5, "Firmware"),
                (6, "Error"),
            ] {
                expected_yaml["targets"][index]["result"] =
                    serde_yaml::Value::Tagged(Box::new(serde_yaml::value::TaggedValue {
                        tag: serde_yaml::value::Tag::new(variant),
                        value: serde_yaml::to_value(
                            &case.expected["targets"][index]["result"][variant],
                        )
                        .expect("expected result payload"),
                    }));
            }
            assert_eq!(report, expected_yaml, "{}", case.scenario);
        }
    }
    std::fs::remove_dir_all(directory).expect("remove desired output directory");
}

/// CSV repeats report metadata for device rows and uses one explicit metadata
/// row when there are no devices, preserving quoted and multiline warnings.
#[tokio::test]
async fn desired_csv_retains_metadata_with_and_without_device_rows() {
    for (scenario, response, expected_rows, observed_at) in [
        // Every device row needs metadata when processed independently by scripts.
        (
            "populated report",
            desired_reply(),
            MIXED_ROWS.to_vec(),
            OBSERVED_AT,
        ),
        // An empty report must not drop warnings merely because CSV has no preface.
        (
            "metadata without devices",
            GetDesiredNicFirmwareResponse {
                warnings: WARNINGS.into_iter().map(str::to_owned).collect(),
                ..Default::default()
            },
            vec![["", "", "", "", "", "", "", "No devices", "", "", "", "", ""]],
            "Not observed",
        ),
    ] {
        let output = run_command(
            &["--format", "csv", "firmware", "nic", "desired", HOST_ID],
            None,
            vec![desired_exchange(response, None)],
        )
        .await;
        assert_success(&output);
        let rows: Vec<Vec<String>> = csv::ReaderBuilder::new()
            .has_headers(false)
            .from_reader(output.stdout.as_slice())
            .records()
            .map(|row| {
                row.expect("valid desired CSV")
                    .iter()
                    .map(str::to_owned)
                    .collect()
            })
            .collect();
        let headers: Vec<_> = HEADERS
            .into_iter()
            .chain(["Observed At", "Warnings"])
            .map(str::to_owned)
            .collect();
        let warnings = WARNINGS.join("\n");
        let expected: Vec<Vec<_>> = std::iter::once(headers)
            .chain(expected_rows.into_iter().map(|row| {
                row.into_iter()
                    .chain([observed_at, warnings.as_str()])
                    .map(str::to_owned)
                    .collect()
            }))
            .collect();
        assert_eq!(rows, expected, "{scenario}");
    }
}

/// An unknown wire scope is a failed read, not a site selection or unmanaged
/// device; even earlier valid rows must not be printed as a partial report.
#[tokio::test]
async fn desired_rejects_unknown_scope_without_partial_output() {
    let mut response = desired_reply();
    // Put the malformed result after a valid one to expose streaming output
    // that starts before the complete response has been decoded.
    let Some(TargetResult::Firmware(mut resolution)) = response.targets[0].result.clone() else {
        panic!("first fixture target must have firmware");
    };
    resolution.scope = i32::MAX;
    response.targets[1].result = Some(TargetResult::Firmware(resolution));
    let output = run_command(
        &["firmware", "nic", "desired", HOST_ID],
        None,
        vec![desired_exchange(response, None)],
    )
    .await;
    assert_eq!(output.status.code(), Some(1));
    assert!(
        output.stdout.is_empty(),
        "failed read must not print a report"
    );
    let error = String::from_utf8(output.stderr).expect("diagnostic is UTF-8");
    assert!(
        error.contains("while attempting to decode NIC target 1"),
        "{error}"
    );
    assert!(
        error.contains("unsupported NIC firmware selection scope"),
        "{error}"
    );
}

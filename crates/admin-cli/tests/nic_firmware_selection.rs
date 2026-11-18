/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

#[path = "support/nic_firmware.rs"]
mod support;

use prost::Message as _;
use rpc::forge::nic_firmware_card_selector::Identity;
use rpc::forge::set_machine_nic_firmware_config_request::Action as MachineAction;
use rpc::forge::set_nic_firmware_site_selection_request::Action as SiteAction;
use rpc::forge::{
    FindNicFirmwareProfileIdsRequest, FindNicFirmwareProfileIdsResponse,
    FindNicFirmwareSiteSelectionIdsResponse, FindNicFirmwareSiteSelectionsByIdsRequest,
    FindNicFirmwareSiteSelectionsByIdsResponse, Machine, MachineConfig, MachineList,
    MachinesByIdsRequest, NicFirmwareCardSelection, NicFirmwareCardSelections,
    NicFirmwareCardSelector, NicFirmwareHardware, NicFirmwareSiteSelection,
    NicFirmwareSiteSelectionResponse, NicFirmwareSiteSelectionSearchFilter,
    SetMachineNicFirmwareConfigRequest, SetNicFirmwareSiteSelectionRequest,
};
use serde_json::{Value, json};
use support::{Exchange, assert_success, run_command};
use tonic::{Code, Status};

const HOST_ID: &str = "fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg";
const PROFILE_ID: &str = "connectx-production";
const PART_NUMBER: &str = "MCX75310AAS-NEAT";
const PSID: &str = "MT_0000000838";
const ORIGINAL_VERSION: &str = "V7-T1789080000000000";
const UPDATED_VERSION: &str = "V8-T1789080000000001";
const CARDS_JSON: &str = r#"[
  {"selector":{"BaseMac":"02:AA:BB:CC:DD:EE"},"profile_id":"connectx-card"},
  {"selector":{"BaseGuid":"0011223344556677"},"profile_id":"connectx-guid"}
]"#;

/// `hardware` supplies one exact pair for request and response comparisons.
fn hardware() -> NicFirmwareHardware {
    NicFirmwareHardware {
        part_number: PART_NUMBER.into(),
        psid: PSID.into(),
    }
}

/// `cards` keeps both selector variants visible in the atomic replacement.
fn cards() -> Vec<NicFirmwareCardSelection> {
    vec![
        NicFirmwareCardSelection {
            selector: Some(NicFirmwareCardSelector {
                identity: Some(Identity::BaseMac("02:AA:BB:CC:DD:EE".into())),
            }),
            profile_id: "connectx-card".into(),
        },
        NicFirmwareCardSelection {
            selector: Some(NicFirmwareCardSelector {
                identity: Some(Identity::BaseGuid("0011223344556677".into())),
            }),
            profile_id: "connectx-guid".into(),
        },
    ]
}

/// `table_rows` compares cells without depending on prettytable's border widths.
fn table_rows(bytes: &[u8]) -> Vec<Vec<String>> {
    std::str::from_utf8(bytes)
        .expect("table is UTF-8")
        .lines()
        .filter_map(|line| line.trim().strip_prefix('|')?.strip_suffix('|'))
        .map(|line| line.split('|').map(|cell| cell.trim().to_owned()).collect())
        .collect()
}

/// `csv_rows` reads actual CSV cells, including empty values and quoted text.
fn csv_rows(bytes: &[u8]) -> Vec<Vec<String>> {
    csv::ReaderBuilder::new()
        .has_headers(false)
        .from_reader(bytes)
        .records()
        .map(|row| {
            row.expect("valid selection CSV")
                .iter()
                .map(str::to_owned)
                .collect()
        })
        .collect()
}

/// Compatible-profile listing forwards the complete hardware pair to Core.
#[tokio::test]
async fn profile_list_filters_by_the_requested_hardware() {
    let output = run_command(
        &[
            "--format",
            "json",
            "firmware",
            "nic",
            "profile",
            "list",
            "--part-number",
            PART_NUMBER,
            "--psid",
            PSID,
        ],
        None,
        vec![Exchange {
            method: "/forge.Forge/FindNicFirmwareProfileIds",
            request: FindNicFirmwareProfileIdsRequest {
                hardware: Some(hardware()),
            }
            .encode_to_vec(),
            // An empty match must not become an unfiltered detail request.
            response: Ok(FindNicFirmwareProfileIdsResponse {
                profile_ids: Vec::new(),
            }
            .encode_to_vec()),
        }],
    )
    .await;
    assert_success(&output);
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap(),
        json!([])
    );
}

/// Site edits preserve caller tokens and display the stored assignment revision.
#[tokio::test]
async fn site_changes_forward_creation_replacement_and_clear_without_prereads() {
    struct Case {
        scenario: &'static str,
        command: &'static str,
        format: &'static str,
        version: Option<&'static str>,
        action: SiteAction,
        selection: Option<NicFirmwareSiteSelection>,
    }
    for case in [
        // Omission is create-only; no token lookup may silently turn it into set.
        // YAML must retain the same complete saved assignment as JSON.
        Case {
            scenario: "create without a token",
            command: "set",
            format: "yaml",
            version: None,
            action: SiteAction::ProfileId(PROFILE_ID.into()),
            selection: Some(NicFirmwareSiteSelection {
                hardware: Some(hardware()),
                profile_id: PROFILE_ID.into(),
                version: ORIGINAL_VERSION.into(),
            }),
        },
        // Only Core can supply the new selection revision after replacement.
        Case {
            scenario: "replace at the observed token",
            command: "set",
            format: "json",
            version: Some(ORIGINAL_VERSION),
            action: SiteAction::ProfileId(PROFILE_ID.into()),
            selection: Some(NicFirmwareSiteSelection {
                hardware: Some(hardware()),
                profile_id: PROFILE_ID.into(),
                version: UPDATED_VERSION.into(),
            }),
        },
        // Clearing removes the assignment, so there is no result document.
        Case {
            scenario: "clear at the observed token",
            command: "clear",
            format: "json",
            version: Some(UPDATED_VERSION),
            action: SiteAction::Clear(()),
            selection: None,
        },
    ] {
        let mut args = vec![
            "--format",
            case.format,
            "firmware",
            "nic",
            "site",
            case.command,
            "--part-number",
            PART_NUMBER,
            "--psid",
            PSID,
        ];
        if case.command == "set" {
            args.extend(["--profile", PROFILE_ID]);
        }
        if let Some(version) = case.version {
            args.extend(["--if-version-match", version]);
        }
        let output = run_command(
            &args,
            None,
            vec![Exchange {
                method: "/forge.Forge/SetNicFirmwareSiteSelection",
                request: SetNicFirmwareSiteSelectionRequest {
                    hardware: Some(hardware()),
                    action: Some(case.action),
                    if_version_match: case.version.map(str::to_owned),
                }
                .encode_to_vec(),
                response: Ok(NicFirmwareSiteSelectionResponse {
                    site_selection: case.selection.clone(),
                }
                .encode_to_vec()),
            }],
        )
        .await;
        assert_success(&output);
        if let Some(selection) = case.selection {
            let document: Value = if case.format == "yaml" {
                serde_yaml::from_slice(&output.stdout).expect("site selection YAML")
            } else {
                serde_json::from_slice(&output.stdout).expect("site selection JSON")
            };
            assert_eq!(
                document,
                json!([{
                    "hardware": {"part_number": PART_NUMBER, "psid": PSID},
                    "profile_id": PROFILE_ID, "version": selection.version,
                }]),
                "{}",
                case.scenario
            );
        } else {
            assert!(output.stdout.is_empty(), "{}", case.scenario);
        }
    }
}

/// Site listing bounds detail requests and omits keys cleared between reads.
#[tokio::test]
async fn site_list_renders_exact_survivors_and_empty_headers() {
    let directory = std::env::temp_dir().join(format!("nic-site-output-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&directory).expect("create site output directory");
    let output_path = directory.join("selections.json");
    // The first key disappears after enumeration; later survivors retain their order.
    let missing = NicFirmwareHardware {
        part_number: "A-part".into(),
        psid: "A-psid".into(),
    };
    let last = NicFirmwareHardware {
        part_number: "Z-part".into(),
        psid: "Z-psid".into(),
    };
    struct Case {
        scenario: &'static str,
        format: &'static str,
        populated: bool,
        profile: Option<&'static str>,
    }
    for case in [
        // Missing details must not hide a later selection or fabricate a row.
        Case {
            scenario: "cleared first key leaves both survivors in order",
            format: "ascii-table",
            populated: true,
            profile: Some(PROFILE_ID),
        },
        // Empty enumeration still exposes the columns without a detail RPC.
        Case {
            scenario: "empty enumeration prints only headers",
            format: "ascii-table",
            populated: false,
            profile: Some(PROFILE_ID),
        },
        // Without a profile flag, CSV must enumerate all selections, not filter.
        Case {
            scenario: "unfiltered CSV retains all columns",
            format: "csv",
            populated: true,
            profile: None,
        },
        // The documented export writes the complete array to its requested file.
        Case {
            scenario: "unfiltered JSON export leaves stdout empty",
            format: "json",
            populated: true,
            profile: None,
        },
    ] {
        let mut exchanges = vec![Exchange {
            method: "/forge.Forge/FindNicFirmwareSiteSelectionIds",
            request: NicFirmwareSiteSelectionSearchFilter {
                profile_id: case.profile.map(str::to_owned),
            }
            .encode_to_vec(),
            response: Ok(FindNicFirmwareSiteSelectionIdsResponse {
                hardware: if case.populated {
                    vec![missing.clone(), hardware(), last.clone()]
                } else {
                    Vec::new()
                },
            }
            .encode_to_vec()),
        }];
        if case.populated {
            // The CLI requests pages of two, but the mock Core advertises one.
            for (key, site_selections) in [
                (missing.clone(), Vec::new()),
                (
                    hardware(),
                    vec![NicFirmwareSiteSelection {
                        hardware: Some(hardware()),
                        profile_id: PROFILE_ID.into(),
                        version: ORIGINAL_VERSION.into(),
                    }],
                ),
                (
                    last.clone(),
                    vec![NicFirmwareSiteSelection {
                        hardware: Some(last.clone()),
                        profile_id: PROFILE_ID.into(),
                        version: UPDATED_VERSION.into(),
                    }],
                ),
            ] {
                exchanges.push(Exchange {
                    method: "/forge.Forge/FindNicFirmwareSiteSelectionsByIds",
                    request: FindNicFirmwareSiteSelectionsByIdsRequest {
                        hardware: vec![key],
                    }
                    .encode_to_vec(),
                    response: Ok(
                        FindNicFirmwareSiteSelectionsByIdsResponse { site_selections }
                            .encode_to_vec(),
                    ),
                });
            }
        }
        let mut args = vec!["--internal-page-size", "2", "--format", case.format];
        if case.format == "json" {
            args.extend(["--output", output_path.to_str().unwrap()]);
        }
        args.extend(["firmware", "nic", "site", "list"]);
        if let Some(profile) = case.profile {
            args.extend(["--profile", profile]);
        }
        let output = run_command(&args, None, exchanges).await;
        assert_success(&output);
        if case.format == "json" {
            assert!(output.stdout.is_empty(), "{}", case.scenario);
            let document: Value = serde_json::from_slice(
                &std::fs::read(&output_path).expect("read exported site selections"),
            )
            .expect("site selections JSON");
            assert_eq!(
                document,
                json!([{
                    "hardware": {"part_number": PART_NUMBER, "psid": PSID},
                    "profile_id": PROFILE_ID, "version": ORIGINAL_VERSION,
                }, {
                    "hardware": {"part_number": "Z-part", "psid": "Z-psid"},
                    "profile_id": PROFILE_ID, "version": UPDATED_VERSION,
                }]),
                "{}",
                case.scenario
            );
            continue;
        }
        let mut expected = vec![vec![
            "Part Number",
            "PSID",
            "Profile ID",
            "Selection Version",
        ]];
        if case.populated {
            expected.push(vec![PART_NUMBER, PSID, PROFILE_ID, ORIGINAL_VERSION]);
            expected.push(vec!["Z-part", "Z-psid", PROFILE_ID, UPDATED_VERSION]);
        }
        let rows = if case.format == "csv" {
            csv_rows(&output.stdout)
        } else {
            table_rows(&output.stdout)
        };
        assert_eq!(rows, expected, "{}", case.scenario);
    }
    std::fs::remove_dir_all(directory).expect("remove site output directory");
}

/// A later site fetch or decode failure must not publish earlier selections.
#[tokio::test]
async fn site_list_later_failures_leave_stdout_empty() {
    let last = NicFirmwareHardware {
        part_number: "Z-part".into(),
        psid: "Z-psid".into(),
    };
    struct Case {
        scenario: &'static str,
        response: Result<Vec<u8>, Status>,
        diagnostic: &'static str,
    }
    for case in [
        // Fetching the first selection does not make a partial list safe to publish.
        Case {
            scenario: "later site detail RPC fails",
            response: Err(Status::unavailable("later site batch unavailable")),
            diagnostic: "later site batch unavailable",
        },
        // A response without its key cannot be mistaken for a cleared selection.
        Case {
            scenario: "later site selection has no hardware",
            response: Ok(FindNicFirmwareSiteSelectionsByIdsResponse {
                site_selections: vec![NicFirmwareSiteSelection {
                    hardware: None,
                    profile_id: PROFILE_ID.into(),
                    version: UPDATED_VERSION.into(),
                }],
            }
            .encode_to_vec()),
            diagnostic: "without hardware",
        },
    ] {
        let output = run_command(
            &["firmware", "nic", "site", "list"],
            None,
            vec![
                Exchange {
                    method: "/forge.Forge/FindNicFirmwareSiteSelectionIds",
                    request: NicFirmwareSiteSelectionSearchFilter::default().encode_to_vec(),
                    response: Ok(FindNicFirmwareSiteSelectionIdsResponse {
                        hardware: vec![hardware(), last.clone()],
                    }
                    .encode_to_vec()),
                },
                Exchange {
                    method: "/forge.Forge/FindNicFirmwareSiteSelectionsByIds",
                    request: FindNicFirmwareSiteSelectionsByIdsRequest {
                        hardware: vec![hardware()],
                    }
                    .encode_to_vec(),
                    response: Ok(FindNicFirmwareSiteSelectionsByIdsResponse {
                        site_selections: vec![NicFirmwareSiteSelection {
                            hardware: Some(hardware()),
                            profile_id: PROFILE_ID.into(),
                            version: ORIGINAL_VERSION.into(),
                        }],
                    }
                    .encode_to_vec()),
                },
                Exchange {
                    method: "/forge.Forge/FindNicFirmwareSiteSelectionsByIds",
                    request: FindNicFirmwareSiteSelectionsByIdsRequest {
                        hardware: vec![last.clone()],
                    }
                    .encode_to_vec(),
                    response: case.response,
                },
            ],
        )
        .await;
        assert_eq!(output.status.code(), Some(1), "{}", case.scenario);
        assert!(output.stdout.is_empty(), "{}", case.scenario);
        let error = String::from_utf8(output.stderr).unwrap();
        assert!(
            error.contains(case.diagnostic),
            "{}: {error}",
            case.scenario
        );
        assert!(
            !error.contains("core accepted"),
            "{}: {error}",
            case.scenario
        );
    }
}

/// Machine edits distinguish omitted cards, explicit clear and atomic replacement.
#[tokio::test]
async fn machine_changes_preserve_card_presence_and_do_not_fetch_a_new_version() {
    let directory = std::env::temp_dir().join(format!("nic-selections-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&directory).expect("create card input directory");
    let path = directory.join("cards.json");
    std::fs::write(&path, CARDS_JSON).expect("write card selections");
    struct Case<'a> {
        scenario: &'static str,
        machine_args: &'static [&'static str],
        action: MachineAction,
        cards_file: Option<&'a str>,
        stdin: Option<&'static [u8]>,
        selections: Option<Vec<NicFirmwareCardSelection>>,
    }
    for case in [
        // A machine-only clear must leave the cards wrapper absent.
        Case {
            scenario: "omitted cards preserve saved intent",
            machine_args: &["--clear-profile"],
            action: MachineAction::Clear(()),
            cards_file: None,
            stdin: None,
            selections: None,
        },
        // The documented machine-profile-only edit also leaves cards untouched.
        Case {
            scenario: "machine profile selection preserves saved cards",
            machine_args: &["--profile", PROFILE_ID],
            action: MachineAction::ProfileId(PROFILE_ID.into()),
            cards_file: None,
            stdin: None,
            selections: None,
        },
        // Explicit empty input is a clear, not preservation or no-op.
        Case {
            scenario: "empty standard input clears cards",
            machine_args: &["--clear-profile"],
            action: MachineAction::Clear(()),
            cards_file: Some("-"),
            stdin: Some(b"[]"),
            selections: Some(Vec::new()),
        },
        // Both identity variants and the machine choice must share one request/token.
        Case {
            scenario: "a file replaces all cards alongside the machine selection",
            machine_args: &["--profile", PROFILE_ID],
            action: MachineAction::ProfileId(PROFILE_ID.into()),
            cards_file: Some(path.to_str().unwrap()),
            stdin: None,
            selections: Some(cards()),
        },
    ] {
        let mut args = vec![
            "firmware",
            "nic",
            "machine",
            "set",
            HOST_ID,
            "--if-version-match",
            ORIGINAL_VERSION,
        ];
        args.extend(case.machine_args);
        if let Some(path) = case.cards_file {
            args.extend(["--cards-file", path]);
        }
        let output = run_command(
            &args,
            case.stdin,
            vec![Exchange {
                method: "/forge.Forge/SetMachineNicFirmwareConfig",
                request: SetMachineNicFirmwareConfigRequest {
                    machine_id: Some(HOST_ID.parse().unwrap()),
                    if_version_match: Some(ORIGINAL_VERSION.into()),
                    action: Some(case.action),
                    card_selections: case
                        .selections
                        .map(|selections| NicFirmwareCardSelections { selections }),
                }
                .encode_to_vec(),
                response: Ok(Vec::new()),
            }],
        )
        .await;
        assert_success(&output);
        assert!(output.stdout.is_empty(), "{}", case.scenario);
    }
    std::fs::remove_dir_all(directory).expect("remove card input directory");
}

/// Stored machine configuration exposes the edit token and both selection scopes.
#[tokio::test]
async fn machine_show_renders_stored_configuration_and_inheritance() {
    let directory =
        std::env::temp_dir().join(format!("nic-machine-output-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&directory).expect("create machine output directory");
    let output_path = directory.join("machine.json");
    struct Case {
        scenario: &'static str,
        format: &'static str,
        populated: bool,
    }
    for case in [
        // A machine row and two cards must keep their distinct identities visible.
        Case {
            scenario: "populated machine and card table",
            format: "ascii-table",
            populated: true,
        },
        // Absence still needs a machine row with the token required for the next edit.
        Case {
            scenario: "inherited machine table retains its token",
            format: "ascii-table",
            populated: false,
        },
        // A JSON export retains selector tags for a subsequent edit without stdout.
        Case {
            scenario: "JSON export preserves selectors without stdout",
            format: "json",
            populated: true,
        },
        // CSV must preserve all scope/selector columns through its own writer.
        Case {
            scenario: "CSV renders both card identity variants",
            format: "csv",
            populated: true,
        },
        // YAML represents populated selectors as tags, not JSON mappings.
        Case {
            scenario: "YAML preserves BaseMac and BaseGuid tags",
            format: "yaml",
            populated: true,
        },
        // Inheritance must remain explicit null/empty data in structured output.
        Case {
            scenario: "YAML preserves absent machine and card selections",
            format: "yaml",
            populated: false,
        },
    ] {
        let mut args = vec!["--format", case.format];
        if case.format == "json" {
            args.extend(["--output", output_path.to_str().unwrap()]);
        }
        args.extend(["firmware", "nic", "machine", "show", HOST_ID]);
        let output = run_command(
            &args,
            None,
            vec![Exchange {
                method: "/forge.Forge/FindMachinesByIds",
                request: MachinesByIdsRequest {
                    machine_ids: vec![HOST_ID.parse().unwrap()],
                    include_history: false,
                }
                .encode_to_vec(),
                response: Ok(MachineList {
                    machines: vec![Machine {
                        id: Some(HOST_ID.parse().unwrap()),
                        version: ORIGINAL_VERSION.into(),
                        config: Some(MachineConfig {
                            nic_firmware_profile_id: case.populated.then(|| PROFILE_ID.into()),
                            nic_firmware_card_selections: if case.populated {
                                cards()
                            } else {
                                Vec::new()
                            },
                            ..Default::default()
                        }),
                        ..Default::default()
                    }],
                }
                .encode_to_vec()),
            }],
        )
        .await;
        assert_success(&output);
        if case.format == "json" {
            assert!(output.stdout.is_empty(), "{}", case.scenario);
            let document: Value = serde_json::from_slice(
                &std::fs::read(&output_path).expect("read exported machine configuration"),
            )
            .expect("machine config JSON");
            assert_eq!(
                document,
                json!({
                    "machine_id": HOST_ID, "version": ORIGINAL_VERSION,
                    "profile_id": case.populated.then_some(PROFILE_ID),
                    "card_selections": if case.populated { serde_json::from_str::<Value>(CARDS_JSON).unwrap() } else { json!([]) },
                }),
                "{}",
                case.scenario,
            );
        } else if case.format == "yaml" {
            let document: serde_yaml::Value =
                serde_yaml::from_slice(&output.stdout).expect("machine config YAML");
            let expected: serde_yaml::Value = if case.populated {
                serde_yaml::from_str(&format!(
                    r#"machine_id: {HOST_ID}
version: {ORIGINAL_VERSION}
profile_id: {PROFILE_ID}
card_selections:
- selector: !BaseMac '02:AA:BB:CC:DD:EE'
  profile_id: connectx-card
- selector: !BaseGuid '0011223344556677'
  profile_id: connectx-guid
"#
                ))
                .expect("expected tagged YAML")
            } else {
                serde_yaml::to_value(json!({
                    "machine_id": HOST_ID,
                    "version": ORIGINAL_VERSION,
                    "profile_id": null,
                    "card_selections": [],
                }))
                .expect("expected inherited YAML")
            };
            assert_eq!(document, expected, "{}", case.scenario);
        } else {
            let mut expected = vec![
                vec![
                    "Machine ID",
                    "Machine Version",
                    "Scope",
                    "Selector",
                    "Profile ID",
                ],
                vec![
                    HOST_ID,
                    ORIGINAL_VERSION,
                    "Machine",
                    "",
                    if case.populated { PROFILE_ID } else { "" },
                ],
            ];
            if case.populated {
                expected.extend([
                    vec![
                        HOST_ID,
                        ORIGINAL_VERSION,
                        "Card",
                        "base MAC 02:AA:BB:CC:DD:EE",
                        "connectx-card",
                    ],
                    vec![
                        HOST_ID,
                        ORIGINAL_VERSION,
                        "Card",
                        "base GUID 0011223344556677",
                        "connectx-guid",
                    ],
                ]);
            }
            let rows = if case.format == "csv" {
                csv_rows(&output.stdout)
            } else {
                table_rows(&output.stdout)
            };
            assert_eq!(rows, expected, "{}", case.scenario);
        }
    }
    std::fs::remove_dir_all(directory).expect("remove machine output directory");
}

/// Missing tokens and conflicting choices remain native Clap errors, before data RPCs.
#[tokio::test]
async fn selection_arguments_require_explicit_edit_intent() {
    struct Case {
        scenario: &'static str,
        args: Vec<&'static str>,
        stdin: Option<&'static [u8]>,
        diagnostic: &'static str,
    }
    for case in [
        // A hardware filter cannot silently become a broader partial selector.
        Case {
            scenario: "part-number-only filter is rejected",
            args: vec!["profile", "list", "--part-number", PART_NUMBER],
            stdin: None,
            diagnostic: "--psid",
        },
        // Each flag owns its reciprocal Clap requirement.
        Case {
            scenario: "PSID-only filter is rejected",
            args: vec!["profile", "list", "--psid", PSID],
            stdin: None,
            diagnostic: "--part-number",
        },
        // A valid DPU ID must not reach the host-only read through a broader parser.
        Case {
            scenario: "machine configuration requires a host ID",
            args: vec![
                "machine",
                "show",
                "fm100dskla0ihp0pn4tv7v1js2k2mo37sl0jjr8141okqg8pjpdpfihaa80",
            ],
            stdin: None,
            diagnostic: "stable or predicted host ID",
        },
        // A site clear must not invent or reread the operator's assignment token.
        Case {
            scenario: "site clear requires an observed token",
            args: vec![
                "site",
                "clear",
                "--part-number",
                PART_NUMBER,
                "--psid",
                PSID,
            ],
            stdin: None,
            diagnostic: "--if-version-match",
        },
        // Machine changes always need the complete shared configuration token.
        Case {
            scenario: "machine clear requires an observed token",
            args: vec!["machine", "set", HOST_ID, "--clear-profile"],
            stdin: None,
            diagnostic: "--if-version-match",
        },
        // Card input does not implicitly choose what happens to the machine profile.
        Case {
            scenario: "card replacement still requires a machine-profile action",
            args: vec![
                "machine",
                "set",
                HOST_ID,
                "--if-version-match",
                ORIGINAL_VERSION,
                "--cards-file",
                "-",
            ],
            stdin: Some(b"[]"),
            diagnostic: "--profile",
        },
        // Set and clear are alternative actions, not an order-sensitive pair.
        Case {
            scenario: "machine profile set and clear cannot coexist",
            args: vec![
                "machine",
                "set",
                HOST_ID,
                "--if-version-match",
                ORIGINAL_VERSION,
                "--profile",
                PROFILE_ID,
                "--clear-profile",
            ],
            stdin: None,
            diagnostic: "cannot be used",
        },
    ] {
        let args: Vec<_> = ["firmware", "nic"].into_iter().chain(case.args).collect();
        let output = run_command(&args, case.stdin, Vec::new()).await;
        assert_eq!(output.status.code(), Some(2), "{}", case.scenario);
        assert!(output.stdout.is_empty(), "{}", case.scenario);
        let error = String::from_utf8(output.stderr).unwrap();
        // Clap omits Usage for typed-value errors; when present, it must not
        // satisfy the expected diagnostic just by listing the flag's name.
        let diagnostic = error
            .split_once("Usage:")
            .map_or(error.as_str(), |(diagnostic, _)| diagnostic);
        assert!(
            diagnostic.starts_with("error:"),
            "{}: {error}",
            case.scenario
        );
        assert!(
            diagnostic.contains(case.diagnostic),
            "{}: {error}",
            case.scenario,
        );
    }
}

/// Unreadable or invalid card input fails locally without changing saved intent.
#[tokio::test]
async fn machine_cards_reject_invalid_input_before_mutation() {
    let missing_path =
        std::env::temp_dir().join(format!("missing-nic-cards-{}.json", uuid::Uuid::new_v4()));
    struct Case<'a> {
        scenario: &'static str,
        path: &'a str,
        input: Option<&'static [u8]>,
        diagnostics: &'static [&'static str],
    }
    for case in [
        // The MAC library accepts a plus-prefixed octet, but wire validation does
        // not. Deserializing into the model first would lose the raw spelling.
        Case {
            scenario: "strict MAC syntax",
            path: "-",
            input: Some(br#"[{"selector":{"BaseMac":"+2:AA:BB:CC:DD:EE"},"profile_id":"connectx-card"}]"#),
            diagnostics: &["card_selections[0]", "base MAC"],
        },
        // JSON null must not be interpreted as omitted cards or an empty clear.
        Case {
            scenario: "null is not preservation or clear",
            path: "-",
            input: Some(b"null"),
            diagnostics: &["parse NIC firmware card selections JSON", "invalid type: null"],
        },
        // All required values remain valid so only ignoring an unknown field
        // could make this unintended input reach Core.
        Case {
            scenario: "unknown entry field is rejected",
            path: "-",
            input: Some(br#"[{"selector":{"BaseMac":"02:AA:BB:CC:DD:EE"},"profile_id":"connectx-card","unexpected":true}]"#),
            diagnostics: &["parse NIC firmware card selections JSON", "unknown field", "unexpected"],
        },
        // A missing file is not omission: it must name the source and send no mutation.
        Case {
            scenario: "missing card file identifies the source",
            path: missing_path.to_str().unwrap(),
            input: None,
            diagnostics: &["while attempting to read NIC firmware card selections from"],
        },
    ] {
        let output = run_command(
            &[
                "firmware", "nic", "machine", "set", HOST_ID, "--clear-profile",
                "--if-version-match", ORIGINAL_VERSION, "--cards-file", case.path,
            ],
            case.input,
            Vec::new(),
        ).await;
        assert_eq!(output.status.code(), Some(1), "{}", case.scenario);
        assert!(output.stdout.is_empty(), "{}", case.scenario);
        let error = String::from_utf8(output.stderr).unwrap();
        for diagnostic in case.diagnostics {
            assert!(error.contains(*diagnostic), "{}: {error}", case.scenario);
        }
        if case.path != "-" {
            assert!(error.contains(case.path), "{}: {error}", case.scenario);
        }
    }
}

/// Rejected selection writes preserve Core's diagnostic without retry or token refresh.
#[tokio::test]
async fn selection_mutation_failures_preserve_status_and_original_request() {
    struct Case {
        scenario: &'static str,
        args: Vec<&'static str>,
        exchange: Exchange,
        code: Code,
        diagnostic: &'static str,
    }
    for case in [
        // A stale site assignment cannot be repaired by silently reading a new token.
        Case {
            scenario: "stale site token remains an operator-visible rejection",
            args: vec![
                "site",
                "set",
                "--part-number",
                PART_NUMBER,
                "--psid",
                PSID,
                "--profile",
                PROFILE_ID,
                "--if-version-match",
                ORIGINAL_VERSION,
            ],
            exchange: Exchange {
                method: "/forge.Forge/SetNicFirmwareSiteSelection",
                request: SetNicFirmwareSiteSelectionRequest {
                    hardware: Some(hardware()),
                    action: Some(SiteAction::ProfileId(PROFILE_ID.into())),
                    if_version_match: Some(ORIGINAL_VERSION.into()),
                }
                .encode_to_vec(),
                response: Err(Status::failed_precondition(
                    "site selection version is stale",
                )),
            },
            code: Code::FailedPrecondition,
            diagnostic: "site selection version is stale",
        },
        // An unavailable reply cannot establish whether the atomic mutation committed.
        Case {
            scenario: "unavailable machine mutation is not retried",
            args: vec![
                "machine",
                "set",
                HOST_ID,
                "--clear-profile",
                "--if-version-match",
                ORIGINAL_VERSION,
            ],
            exchange: Exchange {
                method: "/forge.Forge/SetMachineNicFirmwareConfig",
                request: SetMachineNicFirmwareConfigRequest {
                    machine_id: Some(HOST_ID.parse().unwrap()),
                    action: Some(MachineAction::Clear(())),
                    if_version_match: Some(ORIGINAL_VERSION.into()),
                    card_selections: None,
                }
                .encode_to_vec(),
                response: Err(Status::unavailable("machine config response unavailable")),
            },
            code: Code::Unavailable,
            diagnostic: "machine config response unavailable",
        },
        // The shared admission helper retries ResourceExhausted; site set must not.
        Case {
            scenario: "site set admission rejection is not retried",
            args: vec![
                "site",
                "set",
                "--part-number",
                PART_NUMBER,
                "--psid",
                PSID,
                "--profile",
                PROFILE_ID,
                "--if-version-match",
                ORIGINAL_VERSION,
            ],
            exchange: Exchange {
                method: "/forge.Forge/SetNicFirmwareSiteSelection",
                request: SetNicFirmwareSiteSelectionRequest {
                    hardware: Some(hardware()),
                    action: Some(SiteAction::ProfileId(PROFILE_ID.into())),
                    if_version_match: Some(ORIGINAL_VERSION.into()),
                }
                .encode_to_vec(),
                response: Err(Status::resource_exhausted("site set admission exhausted")),
            },
            code: Code::ResourceExhausted,
            diagnostic: "site set admission exhausted",
        },
        // Site clear has a separate command path and must keep its original token.
        Case {
            scenario: "site clear admission rejection is not retried",
            args: vec![
                "site",
                "clear",
                "--part-number",
                PART_NUMBER,
                "--psid",
                PSID,
                "--if-version-match",
                ORIGINAL_VERSION,
            ],
            exchange: Exchange {
                method: "/forge.Forge/SetNicFirmwareSiteSelection",
                request: SetNicFirmwareSiteSelectionRequest {
                    hardware: Some(hardware()),
                    action: Some(SiteAction::Clear(())),
                    if_version_match: Some(ORIGINAL_VERSION.into()),
                }
                .encode_to_vec(),
                response: Err(Status::resource_exhausted("site clear admission exhausted")),
            },
            code: Code::ResourceExhausted,
            diagnostic: "site clear admission exhausted",
        },
        // Machine and card changes share one mutation, without an admission retry.
        Case {
            scenario: "machine set admission rejection is not retried",
            args: vec![
                "machine",
                "set",
                HOST_ID,
                "--profile",
                PROFILE_ID,
                "--if-version-match",
                ORIGINAL_VERSION,
            ],
            exchange: Exchange {
                method: "/forge.Forge/SetMachineNicFirmwareConfig",
                request: SetMachineNicFirmwareConfigRequest {
                    machine_id: Some(HOST_ID.parse().unwrap()),
                    action: Some(MachineAction::ProfileId(PROFILE_ID.into())),
                    if_version_match: Some(ORIGINAL_VERSION.into()),
                    card_selections: None,
                }
                .encode_to_vec(),
                response: Err(Status::resource_exhausted(
                    "machine set admission exhausted",
                )),
            },
            code: Code::ResourceExhausted,
            diagnostic: "machine set admission exhausted",
        },
    ] {
        let args: Vec<_> = ["firmware", "nic"].into_iter().chain(case.args).collect();
        let output = run_command(&args, None, vec![case.exchange]).await;
        assert_eq!(output.status.code(), Some(1), "{}", case.scenario);
        assert!(output.stdout.is_empty(), "{}", case.scenario);
        let error = String::from_utf8(output.stderr).unwrap();
        assert!(
            error.contains(case.diagnostic),
            "{}: {error}",
            case.scenario
        );
        assert!(
            error.contains(&format!("code: '{}'", case.code)),
            "{}: {error}",
            case.scenario,
        );
        assert!(
            !error.contains("core accepted"),
            "{}: {error}",
            case.scenario
        );
        if case.code == Code::Unavailable {
            // A lost response cannot tell the operator whether Core committed.
            assert!(
                error.contains("after a connection error, inspect firmware nic machine show before another write"),
                "{}: {error}",
                case.scenario,
            );
        }
    }
}

/// An incomplete accepted site response must not invite a blind write retry.
#[tokio::test]
async fn site_set_incomplete_responses_explain_that_the_change_was_accepted() {
    for (scenario, site_selection, diagnostic) in [
        // The mutation succeeded but Core omitted its required saved assignment.
        ("missing assignment", None, "no site NIC firmware selection"),
        // A returned assignment without its identity cannot be displayed safely.
        (
            "missing hardware",
            Some(NicFirmwareSiteSelection {
                hardware: None,
                profile_id: PROFILE_ID.into(),
                version: ORIGINAL_VERSION.into(),
            }),
            "without hardware",
        ),
    ] {
        let output = run_command(
            &[
                "firmware",
                "nic",
                "site",
                "set",
                "--part-number",
                PART_NUMBER,
                "--psid",
                PSID,
                "--profile",
                PROFILE_ID,
            ],
            None,
            vec![Exchange {
                method: "/forge.Forge/SetNicFirmwareSiteSelection",
                request: SetNicFirmwareSiteSelectionRequest {
                    hardware: Some(hardware()),
                    action: Some(SiteAction::ProfileId(PROFILE_ID.into())),
                    if_version_match: None,
                }
                .encode_to_vec(),
                response: Ok(NicFirmwareSiteSelectionResponse { site_selection }.encode_to_vec()),
            }],
        )
        .await;
        assert_eq!(output.status.code(), Some(1), "{scenario}");
        assert!(output.stdout.is_empty(), "{scenario}");
        let error = String::from_utf8(output.stderr).unwrap();
        for message in [
            "core accepted",
            "firmware nic site list",
            "before another write",
            diagnostic,
        ] {
            assert!(error.contains(message), "{scenario}: {error}");
        }
    }
}

/// Missing machines and incomplete configuration reads fail before rendering rows.
#[tokio::test]
async fn machine_show_does_not_turn_missing_data_into_inherited_configuration() {
    for (scenario, machines, diagnostic) in [
        // An empty lookup means the host is absent, not a host without selections.
        ("missing host", Vec::new(), "not found"),
        // Missing configuration is not the same as an explicit empty configuration.
        (
            "missing config",
            vec![Machine {
                id: Some(HOST_ID.parse().unwrap()),
                version: ORIGINAL_VERSION.into(),
                config: None,
                ..Default::default()
            }],
            "without a config",
        ),
    ] {
        let output = run_command(
            &["firmware", "nic", "machine", "show", HOST_ID],
            None,
            vec![Exchange {
                method: "/forge.Forge/FindMachinesByIds",
                request: MachinesByIdsRequest {
                    machine_ids: vec![HOST_ID.parse().unwrap()],
                    include_history: false,
                }
                .encode_to_vec(),
                response: Ok(MachineList { machines }.encode_to_vec()),
            }],
        )
        .await;
        assert_eq!(output.status.code(), Some(1), "{scenario}");
        assert!(output.stdout.is_empty(), "{scenario}");
        let error = String::from_utf8(output.stderr).unwrap();
        assert!(error.contains(HOST_ID), "{scenario}: {error}");
        assert!(error.contains(diagnostic), "{scenario}: {error}");
    }
}

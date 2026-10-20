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

#[path = "support/nic_firmware.rs"]
mod support;

use prost::Message as _;
use rpc::forge::{
    CreateNicFirmwareProfileRequest, DeleteNicFirmwareProfileRequest,
    FindNicFirmwareProfileIdsRequest, FindNicFirmwareProfileIdsResponse,
    FindNicFirmwareProfilesByIdsRequest, FindNicFirmwareProfilesByIdsResponse, NicFirmwareApproach,
    NicFirmwareArtifact, NicFirmwareHardware, NicFirmwareProfile, NicFirmwareProfileConfig,
    NicFirmwareProfileEntry, NicFirmwareProfileResponse, UpdateNicFirmwareProfileRequest,
};
use rpc::protos::mlx_device::FirmwareSpec;
use serde_json::{Value, json};
use support::{Exchange, assert_success, run_command};
use tonic::{Code, Status};

const PROFILE_ID: &str = "connectx-production";
const ORIGINAL_VERSION: &str = "V7-T1789080000000000";
const UPDATED_VERSION: &str = "V8-T1789080000000001";
const CONFIG_JSON: &str = r#"{
  "entries": [{
    "firmware": {
      "part_number": "MCX75310AAS-NEAT",
      "psid": "MT_0000000838",
      "version": "28.43.1014"
    },
    "image": {
      "url": "https://firmware.invalid/image.bin",
      "sha256": "abababababababababababababababababababababababababababababababab"
    },
    "device_config": {
      "url": "https://firmware.invalid/config.bin",
      "sha256": "cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd"
    },
    "approach": "Scout"
  }, {
    "firmware": {
      "part_number": "900-9D3B4-00CV-TA0",
      "psid": "MT_0000000884",
      "version": "32.43.1014"
    },
    "image": {
      "url": "https://firmware.invalid/second-image.bin",
      "sha256": "efefefefefefefefefefefefefefefefefefefefefefefefefefefefefefefef"
    },
    "device_config": null,
    "approach": "Scout"
  }]
}"#;

/// `profile_config` keeps two hardware targets in the operator's unsorted order.
fn profile_config() -> NicFirmwareProfileConfig {
    NicFirmwareProfileConfig {
        entries: vec![
            // The first target has a config artifact and sorts after the second.
            NicFirmwareProfileEntry {
                firmware: Some(FirmwareSpec {
                    part_number: "MCX75310AAS-NEAT".into(),
                    psid: "MT_0000000838".into(),
                    version: "28.43.1014".into(),
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
            },
            // The second target has its own image and no optional config.
            NicFirmwareProfileEntry {
                firmware: Some(FirmwareSpec {
                    part_number: "900-9D3B4-00CV-TA0".into(),
                    psid: "MT_0000000884".into(),
                    version: "32.43.1014".into(),
                }),
                image: Some(NicFirmwareArtifact {
                    url: "https://firmware.invalid/second-image.bin".into(),
                    sha256: "ef".repeat(32),
                }),
                device_config: None,
                approach: NicFirmwareApproach::Scout.into(),
            },
        ],
    }
}

/// Public CRUD commands preserve complete configs and use Core's returned revisions.
#[tokio::test]
async fn profile_crud_preserves_requests_and_reports_server_revisions() {
    let directory = std::env::temp_dir().join(format!("nic-profile-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&directory).expect("create profile test directory");
    let output_path = directory.join("created.json");
    let input_path = directory.join("replacement.json");
    let config_json: Value = serde_json::from_str(CONFIG_JSON).expect("fixture JSON parses");
    // A distinct returned digest makes echoing the submitted config observable.
    let mut stored_config = profile_config();
    stored_config.entries[0].image.as_mut().unwrap().sha256 = "12".repeat(32);
    let mut stored_json = config_json.clone();
    stored_json["entries"][0]["image"]["sha256"] = json!("12".repeat(32));
    let original = NicFirmwareProfile {
        id: PROFILE_ID.into(),
        config: Some(stored_config),
        version: ORIGINAL_VERSION.into(),
    };

    // Omitted `--config-file` reads stdin; `--output` must receive the
    // returned profile, including the revision only Core can supply.
    let output = run_command(
        &[
            "--format",
            "json",
            "--output",
            output_path.to_str().unwrap(),
            "firmware",
            "nic",
            "profile",
            "create",
            PROFILE_ID,
        ],
        Some(CONFIG_JSON.as_bytes()),
        vec![Exchange {
            method: "/forge.Forge/CreateNicFirmwareProfile",
            request: CreateNicFirmwareProfileRequest {
                id: PROFILE_ID.into(),
                config: Some(profile_config()),
            }
            .encode_to_vec(),
            response: Ok(NicFirmwareProfileResponse {
                profile: Some(original.clone()),
            }
            .encode_to_vec()),
        }],
    )
    .await;
    assert_success(&output);
    assert!(
        output.stdout.is_empty(),
        "--output should leave stdout empty"
    );
    let expected_original = json!([{
        "id": PROFILE_ID, "config": stored_json, "version": ORIGINAL_VERSION,
    }]);
    assert_eq!(
        serde_json::from_slice::<Value>(&std::fs::read(&output_path).unwrap()).unwrap(),
        expected_original,
    );

    // A read uses the requested ID and retains the optional artifact in YAML.
    let output = run_command(
        &[
            "--format", "yaml", "firmware", "nic", "profile", "show", PROFILE_ID,
        ],
        None,
        vec![Exchange {
            method: "/forge.Forge/FindNicFirmwareProfilesByIds",
            request: FindNicFirmwareProfilesByIdsRequest {
                profile_ids: vec![PROFILE_ID.into()],
            }
            .encode_to_vec(),
            response: Ok(FindNicFirmwareProfilesByIdsResponse {
                profiles: vec![original],
            }
            .encode_to_vec()),
        }],
    )
    .await;
    assert_success(&output);
    assert_eq!(
        serde_yaml::from_slice::<Value>(&output.stdout).unwrap(),
        expected_original
    );

    // Replacement removes the first artifact and forwards an older target unchanged.
    // The untouched second entry must remain in place beside the original token.
    let mut replacement_json = config_json;
    replacement_json["entries"][0]["firmware"]["version"] = json!("28.42.1000");
    replacement_json["entries"][0]["device_config"] = Value::Null;
    std::fs::write(&input_path, serde_json::to_vec(&replacement_json).unwrap()).unwrap();
    let mut replacement = profile_config();
    replacement.entries[0].firmware.as_mut().unwrap().version = "28.42.1000".into();
    replacement.entries[0].device_config = None;
    let output = run_command(
        &[
            "--format",
            "json",
            "firmware",
            "nic",
            "profile",
            "update",
            PROFILE_ID,
            "--if-version-match",
            ORIGINAL_VERSION,
            "--config-file",
            input_path.to_str().unwrap(),
        ],
        None,
        vec![Exchange {
            method: "/forge.Forge/UpdateNicFirmwareProfile",
            request: UpdateNicFirmwareProfileRequest {
                id: PROFILE_ID.into(),
                config: Some(replacement.clone()),
                if_version_match: Some(ORIGINAL_VERSION.into()),
            }
            .encode_to_vec(),
            response: Ok(NicFirmwareProfileResponse {
                profile: Some(NicFirmwareProfile {
                    id: PROFILE_ID.into(),
                    config: Some(replacement),
                    version: UPDATED_VERSION.into(),
                }),
            }
            .encode_to_vec()),
        }],
    )
    .await;
    assert_success(&output);
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap(),
        json!([{
            "id": PROFILE_ID, "config": replacement_json, "version": UPDATED_VERSION,
        }])
    );

    // Delete uses the supplied revision and leaves a requested JSON output file empty.
    let deletion_output_path = directory.join("deleted.json");
    let output = run_command(
        &[
            "--format",
            "json",
            "--output",
            deletion_output_path.to_str().unwrap(),
            "firmware",
            "nic",
            "profile",
            "delete",
            PROFILE_ID,
            "--if-version-match",
            UPDATED_VERSION,
        ],
        None,
        vec![Exchange {
            method: "/forge.Forge/DeleteNicFirmwareProfile",
            request: DeleteNicFirmwareProfileRequest {
                id: PROFILE_ID.into(),
                if_version_match: Some(UPDATED_VERSION.into()),
            }
            .encode_to_vec(),
            response: Ok(Vec::new()),
        }],
    )
    .await;
    assert_success(&output);
    assert!(output.stdout.is_empty(), "delete has no result document");
    assert!(
        std::fs::read(deletion_output_path)
            .expect("delete creates the requested output file")
            .is_empty()
    );
    std::fs::remove_dir_all(directory).expect("remove profile test files");
}

/// Listing preserves entry order, bounds batches, and skips profiles deleted between reads.
#[tokio::test]
async fn profile_list_bounds_fetches_and_renders_the_public_table() {
    let headers = [
        "Profile ID",
        "Profile Version",
        "Part Number",
        "PSID",
        "Firmware Version",
        "Approach",
    ];
    /// Each case protects a distinct rendering or enumeration boundary.
    struct Case {
        scenario: &'static str,
        format: &'static str,
        ids: &'static [&'static str],
        disappeared: Option<&'static str>,
    }
    for case in [
        // Multiple detail batches must retain each profile's unsorted entries.
        Case {
            scenario: "ASCII rows preserve profile and entry order across batches",
            format: "ascii-table",
            ids: &["a-profile", "z-profile"],
            disappeared: None,
        },
        // An empty catalog still prints the operator's table headers.
        Case {
            scenario: "empty ASCII table has only headers",
            format: "ascii-table",
            ids: &[],
            disappeared: None,
        },
        // CSV has its own writer and must retain every entry as a distinct row.
        Case {
            scenario: "CSV preserves complete rows and entry order",
            format: "csv",
            ids: &["a-profile"],
            disappeared: None,
        },
        // Scripts need the same CSV columns when no profiles are registered.
        Case {
            scenario: "empty CSV has only headers",
            format: "csv",
            ids: &[],
            disappeared: None,
        },
        // An empty first detail response must not stop the remaining batches.
        Case {
            scenario: "a deleted first profile does not hide the later profile",
            format: "ascii-table",
            ids: &["a-profile", "z-profile"],
            disappeared: Some("a-profile"),
        },
    ] {
        // Core advertises a one-ID cap while the CLI requests pages of two.
        // An empty catalog must never send an empty detail request.
        let mut exchanges = vec![Exchange {
            method: "/forge.Forge/FindNicFirmwareProfileIds",
            request: FindNicFirmwareProfileIdsRequest::default().encode_to_vec(),
            response: Ok(FindNicFirmwareProfileIdsResponse {
                profile_ids: case.ids.iter().map(|id| (*id).into()).collect(),
            }
            .encode_to_vec()),
        }];
        exchanges.extend(case.ids.iter().map(|id| {
            Exchange {
                method: "/forge.Forge/FindNicFirmwareProfilesByIds",
                request: FindNicFirmwareProfilesByIdsRequest {
                    profile_ids: vec![(*id).into()],
                }
                .encode_to_vec(),
                response: Ok(FindNicFirmwareProfilesByIdsResponse {
                    profiles: if Some(*id) == case.disappeared {
                        Vec::new()
                    } else {
                        vec![NicFirmwareProfile {
                            id: (*id).into(),
                            config: Some(profile_config()),
                            version: ORIGINAL_VERSION.into(),
                        }]
                    },
                }
                .encode_to_vec()),
            }
        }));
        let output = run_command(
            &[
                "--internal-page-size",
                "2",
                "--format",
                case.format,
                "firmware",
                "nic",
                "profile",
                "list",
            ],
            None,
            exchanges,
        )
        .await;
        assert_success(&output);
        let text = String::from_utf8(output.stdout).expect("table is UTF-8");
        let rows: Vec<Vec<String>> = if case.format == "csv" {
            csv::ReaderBuilder::new()
                .has_headers(false)
                .from_reader(text.as_bytes())
                .records()
                .map(|row| {
                    row.expect("valid CSV row")
                        .iter()
                        .map(str::to_owned)
                        .collect()
                })
                .collect()
        } else {
            text.lines()
                .filter_map(|line| line.trim().strip_prefix('|')?.strip_suffix('|'))
                .map(|line| line.split('|').map(|cell| cell.trim().to_owned()).collect())
                .collect()
        };
        let mut rows = rows.into_iter();
        assert_eq!(
            rows.next(),
            Some(headers.iter().map(|header| (*header).to_owned()).collect()),
            "{}: {text}",
            case.scenario,
        );
        let expected: Vec<Vec<String>> = case
            .ids
            .iter()
            .filter(|id| Some(**id) != case.disappeared)
            .flat_map(|id| {
                [
                    [
                        *id,
                        ORIGINAL_VERSION,
                        "MCX75310AAS-NEAT",
                        "MT_0000000838",
                        "28.43.1014",
                        "Scout",
                    ],
                    [
                        *id,
                        ORIGINAL_VERSION,
                        "900-9D3B4-00CV-TA0",
                        "MT_0000000884",
                        "32.43.1014",
                        "Scout",
                    ],
                ]
                .into_iter()
                .map(|row| row.into_iter().map(str::to_owned).collect())
            })
            .collect();
        assert_eq!(
            rows.collect::<Vec<_>>(),
            expected,
            "{}: {text}",
            case.scenario
        );
    }
}

/// A filtered YAML export retains the complete profile and leaves stdout empty.
#[tokio::test]
async fn profile_list_exports_filtered_definitions_as_yaml() {
    let directory =
        std::env::temp_dir().join(format!("nic-profile-export-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&directory).expect("create profile export directory");
    let output_path = directory.join("nic-profiles.yaml");
    let output = run_command(
        &[
            "--format",
            "yaml",
            "--output",
            output_path.to_str().unwrap(),
            "firmware",
            "nic",
            "profile",
            "list",
            "--part-number",
            "MCX75310AAS-NEAT",
            "--psid",
            "MT_0000000838",
        ],
        None,
        vec![
            Exchange {
                method: "/forge.Forge/FindNicFirmwareProfileIds",
                request: FindNicFirmwareProfileIdsRequest {
                    hardware: Some(NicFirmwareHardware {
                        part_number: "MCX75310AAS-NEAT".into(),
                        psid: "MT_0000000838".into(),
                    }),
                }
                .encode_to_vec(),
                response: Ok(FindNicFirmwareProfileIdsResponse {
                    profile_ids: vec![PROFILE_ID.into()],
                }
                .encode_to_vec()),
            },
            Exchange {
                method: "/forge.Forge/FindNicFirmwareProfilesByIds",
                request: FindNicFirmwareProfilesByIdsRequest {
                    profile_ids: vec![PROFILE_ID.into()],
                }
                .encode_to_vec(),
                // Filtering selects whole profiles, not just their matching entries.
                response: Ok(FindNicFirmwareProfilesByIdsResponse {
                    profiles: vec![NicFirmwareProfile {
                        id: PROFILE_ID.into(),
                        config: Some(profile_config()),
                        version: ORIGINAL_VERSION.into(),
                    }],
                }
                .encode_to_vec()),
            },
        ],
    )
    .await;
    assert_success(&output);
    assert!(
        output.stdout.is_empty(),
        "YAML export must leave stdout empty"
    );
    let document: Value = serde_yaml::from_slice(
        &std::fs::read(&output_path).expect("read exported NIC firmware profiles"),
    )
    .expect("exported profiles are YAML");
    assert_eq!(
        document,
        json!([{
            "id": PROFILE_ID,
            "config": serde_json::from_str::<Value>(CONFIG_JSON).expect("fixture config JSON"),
            "version": ORIGINAL_VERSION,
        }]),
    );
    std::fs::remove_dir_all(directory).expect("remove profile export directory");
}

/// A later fetch or decode failure must not publish earlier profile results.
#[tokio::test]
async fn profile_list_later_failures_leave_stdout_empty() {
    let mut malformed_config = profile_config();
    malformed_config.entries[0].approach = i32::MAX;
    /// Each case fails after a valid batch at a different boundary.
    struct Case {
        scenario: &'static str,
        response: Result<Vec<u8>, Status>,
        context: &'static str,
        diagnostic: &'static str,
    }
    for case in [
        // A failed later RPC must not expose the successfully fetched profile.
        Case {
            scenario: "later detail RPC fails",
            response: Err(Status::unavailable("later profile batch unavailable")),
            context: "while attempting to read a batch of NIC firmware profiles",
            diagnostic: "later profile batch unavailable",
        },
        // A malformed later definition must fail before any profile is rendered.
        Case {
            scenario: "later profile definition cannot be decoded",
            response: Ok(FindNicFirmwareProfilesByIdsResponse {
                profiles: vec![NicFirmwareProfile {
                    id: "z-profile".into(),
                    config: Some(malformed_config),
                    version: ORIGINAL_VERSION.into(),
                }],
            }
            .encode_to_vec()),
            context: "while attempting to decode NIC firmware profile z-profile",
            diagnostic: "entries[0].approach is unsupported",
        },
    ] {
        let output = run_command(
            &["firmware", "nic", "profile", "list"],
            None,
            vec![
                Exchange {
                    method: "/forge.Forge/FindNicFirmwareProfileIds",
                    request: FindNicFirmwareProfileIdsRequest::default().encode_to_vec(),
                    response: Ok(FindNicFirmwareProfileIdsResponse {
                        profile_ids: vec!["a-profile".into(), "z-profile".into()],
                    }
                    .encode_to_vec()),
                },
                Exchange {
                    method: "/forge.Forge/FindNicFirmwareProfilesByIds",
                    request: FindNicFirmwareProfilesByIdsRequest {
                        profile_ids: vec!["a-profile".into()],
                    }
                    .encode_to_vec(),
                    response: Ok(FindNicFirmwareProfilesByIdsResponse {
                        profiles: vec![NicFirmwareProfile {
                            id: "a-profile".into(),
                            config: Some(profile_config()),
                            version: ORIGINAL_VERSION.into(),
                        }],
                    }
                    .encode_to_vec()),
                },
                Exchange {
                    method: "/forge.Forge/FindNicFirmwareProfilesByIds",
                    request: FindNicFirmwareProfilesByIdsRequest {
                        profile_ids: vec!["z-profile".into()],
                    }
                    .encode_to_vec(),
                    response: case.response,
                },
            ],
        )
        .await;
        assert_eq!(output.status.code(), Some(1), "{}", case.scenario);
        assert!(
            output.stdout.is_empty(),
            "{}: partial list printed",
            case.scenario
        );
        let error = String::from_utf8(output.stderr).expect("diagnostic is UTF-8");
        assert!(error.contains(case.context), "{}: {error}", case.scenario);
        assert!(
            error.contains(case.diagnostic),
            "{}: {error}",
            case.scenario
        );
    }
}

/// Rejected mutations preserve Core's diagnostic and fail the actual command.
#[tokio::test]
async fn profile_mutation_failures_are_visible_without_retrying() {
    /// Each failure checks the command's request, status identity, and diagnostic.
    struct Case {
        scenario: &'static str,
        args: &'static [&'static str],
        stdin: Option<&'static [u8]>,
        exchange: Exchange,
        code: Code,
        diagnostic: &'static str,
    }
    for case in [
        // A stale edit must be returned to the operator, without a token refresh.
        Case {
            scenario: "stale update preserves the original token and status",
            args: &["update", PROFILE_ID, "--if-version-match", ORIGINAL_VERSION],
            stdin: Some(CONFIG_JSON.as_bytes()),
            exchange: Exchange {
                method: "/forge.Forge/UpdateNicFirmwareProfile",
                request: UpdateNicFirmwareProfileRequest {
                    id: PROFILE_ID.into(),
                    config: Some(profile_config()),
                    if_version_match: Some(ORIGINAL_VERSION.into()),
                }
                .encode_to_vec(),
                response: Err(Status::failed_precondition("profile edit version is stale")),
            },
            code: Code::FailedPrecondition,
            diagnostic: "profile edit version is stale",
        },
        // A saved selection prevents deletion; the CLI must not claim success.
        Case {
            scenario: "referenced delete preserves the rejection",
            args: &["delete", PROFILE_ID, "--if-version-match", ORIGINAL_VERSION],
            stdin: None,
            exchange: Exchange {
                method: "/forge.Forge/DeleteNicFirmwareProfile",
                request: DeleteNicFirmwareProfileRequest {
                    id: PROFILE_ID.into(),
                    if_version_match: Some(ORIGINAL_VERSION.into()),
                }
                .encode_to_vec(),
                response: Err(Status::failed_precondition(
                    "profile is selected by a machine",
                )),
            },
            code: Code::FailedPrecondition,
            diagnostic: "profile is selected by a machine",
        },
        // Creation has its own RPC and must not turn an existing ID into an update.
        Case {
            scenario: "duplicate create reports AlreadyExists without replacing",
            args: &["create", PROFILE_ID],
            stdin: Some(CONFIG_JSON.as_bytes()),
            exchange: Exchange {
                method: "/forge.Forge/CreateNicFirmwareProfile",
                request: CreateNicFirmwareProfileRequest {
                    id: PROFILE_ID.into(),
                    config: Some(profile_config()),
                }
                .encode_to_vec(),
                response: Err(Status::already_exists("profile name is already registered")),
            },
            code: Code::AlreadyExists,
            diagnostic: "profile name is already registered",
        },
        // An unavailable reply cannot establish whether a mutation committed.
        Case {
            scenario: "unavailable update is not retried or refreshed",
            args: &["update", PROFILE_ID, "--if-version-match", ORIGINAL_VERSION],
            stdin: Some(CONFIG_JSON.as_bytes()),
            exchange: Exchange {
                method: "/forge.Forge/UpdateNicFirmwareProfile",
                request: UpdateNicFirmwareProfileRequest {
                    id: PROFILE_ID.into(),
                    config: Some(profile_config()),
                    if_version_match: Some(ORIGINAL_VERSION.into()),
                }
                .encode_to_vec(),
                response: Err(Status::unavailable("catalog mutation response unavailable")),
            },
            code: Code::Unavailable,
            diagnostic: "catalog mutation response unavailable",
        },
        // The shared admission retry helper retries this code; mutations must not.
        Case {
            scenario: "admission rejection does not retry deletion",
            args: &["delete", PROFILE_ID, "--if-version-match", ORIGINAL_VERSION],
            stdin: None,
            exchange: Exchange {
                method: "/forge.Forge/DeleteNicFirmwareProfile",
                request: DeleteNicFirmwareProfileRequest {
                    id: PROFILE_ID.into(),
                    if_version_match: Some(ORIGINAL_VERSION.into()),
                }
                .encode_to_vec(),
                response: Err(Status::resource_exhausted(
                    "catalog mutation admission exhausted",
                )),
            },
            code: Code::ResourceExhausted,
            diagnostic: "catalog mutation admission exhausted",
        },
        Case {
            scenario: "admission rejection does not retry creation",
            args: &["create", PROFILE_ID],
            stdin: Some(CONFIG_JSON.as_bytes()),
            exchange: Exchange {
                method: "/forge.Forge/CreateNicFirmwareProfile",
                request: CreateNicFirmwareProfileRequest {
                    id: PROFILE_ID.into(),
                    config: Some(profile_config()),
                }
                .encode_to_vec(),
                response: Err(Status::resource_exhausted(
                    "catalog creation admission exhausted",
                )),
            },
            code: Code::ResourceExhausted,
            diagnostic: "catalog creation admission exhausted",
        },
    ] {
        let args: Vec<_> = ["firmware", "nic", "profile"]
            .into_iter()
            .chain(case.args.iter().copied())
            .collect();
        let output = run_command(&args, case.stdin, vec![case.exchange]).await;
        assert_eq!(output.status.code(), Some(1), "{}", case.scenario);
        assert!(
            output.stdout.is_empty(),
            "{}: failed mutation printed success output",
            case.scenario,
        );
        let error = String::from_utf8(output.stderr).expect("diagnostic is UTF-8");
        assert!(
            error.contains(case.diagnostic),
            "{}: {error}",
            case.scenario
        );
        assert!(
            error.contains(&format!("code: '{}'", case.code)),
            "{}: status identity missing from {error}",
            case.scenario,
        );
    }
}

/// Missing edit tokens remain Clap errors and never send a mutation to Core.
#[tokio::test]
async fn profile_mutations_require_an_explicit_revision() {
    // Update and delete declare their required tokens on separate argument types.
    for command in ["update", "delete"] {
        let output = run_command(
            &["firmware", "nic", "profile", command, PROFILE_ID],
            None,
            Vec::new(),
        )
        .await;
        assert_eq!(output.status.code(), Some(2), "{command}");
        let error = String::from_utf8(output.stderr).expect("Clap diagnostic is UTF-8");
        assert!(error.contains("--if-version-match"), "{error}");
        assert!(error.contains("Usage:"), "{error}");
    }
}

/// A successful lookup with no matching profile still fails the operator's show command.
#[tokio::test]
async fn profile_show_reports_a_missing_catalog_entry() {
    let output = run_command(
        &["firmware", "nic", "profile", "show", PROFILE_ID],
        None,
        vec![Exchange {
            method: "/forge.Forge/FindNicFirmwareProfilesByIds",
            request: FindNicFirmwareProfilesByIdsRequest {
                profile_ids: vec![PROFILE_ID.into()],
            }
            .encode_to_vec(),
            response: Ok(FindNicFirmwareProfilesByIdsResponse {
                profiles: Vec::new(),
            }
            .encode_to_vec()),
        }],
    )
    .await;
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty(), "missing profile printed a result");
    let error = String::from_utf8(output.stderr).expect("diagnostic is UTF-8");
    assert!(error.contains(PROFILE_ID), "{error}");
    assert!(error.contains("not found"), "{error}");
}

/// Local input failures identify the broken source before sending a catalog request.
#[tokio::test]
async fn profile_create_rejects_unreadable_or_malformed_config_before_rpc() {
    let missing_path =
        std::env::temp_dir().join(format!("missing-nic-profile-{}.json", uuid::Uuid::new_v4()));
    /// File acquisition and JSON decoding fail at different local boundaries.
    struct Case<'a> {
        scenario: &'static str,
        config_file: Option<&'a str>,
        stdin: Option<&'static [u8]>,
        diagnostic: &'static str,
    }
    for case in [
        // Malformed JSON must fail locally, rather than send a partial definition.
        Case {
            scenario: "malformed standard input",
            config_file: None,
            stdin: Some(b"{"),
            diagnostic: "while attempting to parse NIC firmware profile config JSON",
        },
        // A missing file must name the requested path so the operator can fix it.
        Case {
            scenario: "missing config file",
            config_file: Some(missing_path.to_str().unwrap()),
            stdin: None,
            diagnostic: "while attempting to read NIC firmware profile config from",
        },
    ] {
        let mut args = vec!["firmware", "nic", "profile", "create", PROFILE_ID];
        if let Some(path) = case.config_file {
            args.extend(["--config-file", path]);
        }
        let output = run_command(&args, case.stdin, Vec::new()).await;
        assert_eq!(output.status.code(), Some(1), "{}", case.scenario);
        assert!(
            output.stdout.is_empty(),
            "{}: unexpected output",
            case.scenario
        );
        let error = String::from_utf8(output.stderr).expect("diagnostic is UTF-8");
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
        if let Some(path) = case.config_file {
            assert!(error.contains(path), "{}: {error}", case.scenario);
        }
    }
}

/// Incomplete successful mutation responses fail with guidance to inspect the accepted change.
#[tokio::test]
async fn profile_mutations_report_incomplete_successful_responses() {
    /// The envelope and profile config are separate required response fields.
    struct Case {
        scenario: &'static str,
        id: &'static str,
        args: &'static [&'static str],
        exchange: Exchange,
        diagnostic: &'static str,
    }
    for case in [
        // Create may have committed even though the response omits its profile.
        Case {
            scenario: "create response has no profile",
            id: "connectx production",
            args: &["create", "connectx production"],
            exchange: Exchange {
                method: "/forge.Forge/CreateNicFirmwareProfile",
                request: CreateNicFirmwareProfileRequest {
                    id: "connectx production".into(),
                    config: Some(profile_config()),
                }
                .encode_to_vec(),
                response: Ok(NicFirmwareProfileResponse { profile: None }.encode_to_vec()),
            },
            diagnostic: "returned no NIC firmware profile",
        },
        // A revision without its accepted config is not a complete update result.
        Case {
            scenario: "update response has no config",
            id: "-connectx-production",
            args: &[
                "update",
                "--if-version-match",
                ORIGINAL_VERSION,
                "--",
                "-connectx-production",
            ],
            exchange: Exchange {
                method: "/forge.Forge/UpdateNicFirmwareProfile",
                request: UpdateNicFirmwareProfileRequest {
                    id: "-connectx-production".into(),
                    config: Some(profile_config()),
                    if_version_match: Some(ORIGINAL_VERSION.into()),
                }
                .encode_to_vec(),
                response: Ok(NicFirmwareProfileResponse {
                    profile: Some(NicFirmwareProfile {
                        id: "-connectx-production".into(),
                        config: None,
                        version: UPDATED_VERSION.into(),
                    }),
                }
                .encode_to_vec()),
            },
            diagnostic: "without a config",
        },
    ] {
        let args: Vec<_> = ["--format", "json", "firmware", "nic", "profile"]
            .into_iter()
            .chain(case.args.iter().copied())
            .collect();
        let output = run_command(&args, Some(CONFIG_JSON.as_bytes()), vec![case.exchange]).await;
        assert_eq!(output.status.code(), Some(1), "{}", case.scenario);
        assert!(
            output.stdout.is_empty(),
            "{}: partial result printed",
            case.scenario
        );
        let error = String::from_utf8(output.stderr).expect("diagnostic is UTF-8");
        assert!(
            error.contains(case.diagnostic),
            "{}: {error}",
            case.scenario
        );
        assert!(
            error.contains(&format!(
                "core accepted the change to NIC firmware profile {}",
                case.id
            )),
            "{}: {error}",
            case.scenario
        );
        assert!(
            error.contains(
                "use the firmware nic profile show command with the same profile ID to inspect the definition before another write"
            ),
            "{}: missing recovery command in {error}",
            case.scenario,
        );
    }
}

/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

//! The fixture runs the public CLI and handles `Version` calls separately.
//! Each script lists every other expected RPC, so an extra token lookup or
//! mutation retry fails even if the command eventually prints the expected result.

#[path = "support/nic_firmware.rs"]
mod support;

use prost::Message as _;
use rpc::forge::set_tenant_nic_firmware_config_request::Action;
use rpc::forge::{
    GetTenantNicFirmwareConfigRequest, GetTenantNicFirmwareConfigResponse,
    SetTenantNicFirmwareConfigRequest, SetTenantNicFirmwareConfigResponse, TenantNicFirmwareConfig,
};
use serde_json::{Value, json};
use support::{Exchange, assert_success, run_command};
use tonic::{Code, Status};

const TENANT_ORG_ID: &str = "fds34511233a";
const PROFILE_ID: &str = "connectx-production";
const ORIGINAL_VERSION: &str = "V7-T1789080000000000";
const UPDATED_VERSION: &str = "V8-T1789080000000001";

/// Compare cells, including empty profiles, without depending on border widths.
fn table_rows(bytes: &[u8]) -> Vec<Vec<String>> {
    std::str::from_utf8(bytes)
        .expect("table is UTF-8")
        .lines()
        .filter_map(|line| line.trim().strip_prefix('|')?.strip_suffix('|'))
        .map(|line| line.split('|').map(|cell| cell.trim().to_owned()).collect())
        .collect()
}

/// Parse CSV cells so an unset profile must retain its column, not just its header.
fn csv_rows(bytes: &[u8]) -> Vec<Vec<String>> {
    csv::ReaderBuilder::new()
        .has_headers(false)
        .from_reader(bytes)
        .records()
        .map(|row| {
            row.expect("valid tenant configuration CSV")
                .iter()
                .map(str::to_owned)
                .collect()
        })
        .collect()
}

/// Exercise public command rendering: absence of a selection must not hide the
/// tenant or the token that an operator needs for the next write.
#[tokio::test]
async fn tenant_show_keeps_the_tenant_and_version_when_the_profile_is_unset() {
    struct Case {
        scenario: &'static str,
        format: &'static str,
        profile_id: Option<&'static str>,
    }
    for case in [
        // A populated row establishes the column order and profile value.
        Case {
            scenario: "populated table",
            format: "ascii-table",
            profile_id: Some(PROFILE_ID),
        },
        // Unset is a real configuration row, not an empty search result.
        Case {
            scenario: "unset table retains the row",
            format: "ascii-table",
            profile_id: None,
        },
        // CSV must preserve the empty trailing cell for downstream consumers.
        Case {
            scenario: "CSV preserves the empty profile cell",
            format: "csv",
            profile_id: None,
        },
    ] {
        let output = run_command(
            &[
                "--format",
                case.format,
                "firmware",
                "nic",
                "tenant",
                "show",
                "--tenant-org-id",
                TENANT_ORG_ID,
            ],
            None,
            vec![Exchange {
                method: "/forge.Forge/GetTenantNicFirmwareConfig",
                request: GetTenantNicFirmwareConfigRequest {
                    tenant_organization_id: TENANT_ORG_ID.into(),
                }
                .encode_to_vec(),
                response: Ok(GetTenantNicFirmwareConfigResponse {
                    firmware_config: Some(TenantNicFirmwareConfig {
                        tenant_organization_id: TENANT_ORG_ID.into(),
                        profile_id: case.profile_id.map(str::to_owned),
                        config_version: ORIGINAL_VERSION.into(),
                    }),
                }
                .encode_to_vec()),
            }],
        )
        .await;
        assert_success(&output);
        let rows = if case.format == "csv" {
            csv_rows(&output.stdout)
        } else {
            table_rows(&output.stdout)
        };
        assert_eq!(
            rows,
            vec![
                vec!["Tenant Org ID", "Tenant Version", "Profile ID"],
                vec![
                    TENANT_ORG_ID,
                    ORIGINAL_VERSION,
                    case.profile_id.unwrap_or_default(),
                ],
            ],
            "{}",
            case.scenario
        );
    }
}

/// Set and clear must send the caller's token once and display Core's new token;
/// exported JSON and stdout YAML also cover the public structured-output paths.
#[tokio::test]
async fn tenant_mutations_send_the_observed_version_and_display_the_returned_version() {
    let output_path =
        std::env::temp_dir().join(format!("nic-tenant-output-{}.json", uuid::Uuid::new_v4()));
    struct Case {
        scenario: &'static str,
        format: &'static str,
        args: Vec<&'static str>,
        action: Action,
        profile_id: Option<&'static str>,
    }
    for case in [
        // Set must encode the profile action and honor the global output-file option.
        Case {
            scenario: "set returns the new version in a JSON object",
            format: "json",
            args: vec!["set", "--profile", PROFILE_ID],
            action: Action::ProfileId(PROFILE_ID.into()),
            profile_id: Some(PROFILE_ID),
        },
        // Clear uses a distinct action and represents absence as null, not a missing key.
        Case {
            scenario: "clear returns null and the new version in a YAML mapping",
            format: "yaml",
            args: vec!["clear"],
            action: Action::Clear(()),
            profile_id: None,
        },
    ] {
        let mut args = vec!["--format", case.format];
        if case.format == "json" {
            args.extend(["--output", output_path.to_str().expect("UTF-8 output path")]);
        }
        args.extend(["firmware", "nic", "tenant"]);
        args.extend(case.args);
        args.extend([
            "--tenant-org-id",
            TENANT_ORG_ID,
            "--if-version-match",
            ORIGINAL_VERSION,
        ]);
        let output = run_command(
            &args,
            None,
            vec![Exchange {
                method: "/forge.Forge/SetTenantNicFirmwareConfig",
                request: SetTenantNicFirmwareConfigRequest {
                    tenant_organization_id: TENANT_ORG_ID.into(),
                    if_version_match: ORIGINAL_VERSION.into(),
                    action: Some(case.action),
                }
                .encode_to_vec(),
                response: Ok(SetTenantNicFirmwareConfigResponse {
                    firmware_config: Some(TenantNicFirmwareConfig {
                        tenant_organization_id: TENANT_ORG_ID.into(),
                        profile_id: case.profile_id.map(str::to_owned),
                        config_version: UPDATED_VERSION.into(),
                    }),
                }
                .encode_to_vec()),
            }],
        )
        .await;
        assert_success(&output);
        let document: Value = if case.format == "json" {
            assert!(output.stdout.is_empty(), "{}", case.scenario);
            serde_json::from_slice(
                &std::fs::read(&output_path).expect("read exported tenant configuration"),
            )
            .expect("JSON object")
        } else {
            serde_yaml::from_slice(&output.stdout).expect("YAML mapping")
        };
        assert_eq!(
            document,
            json!({
                "tenant_org_id": TENANT_ORG_ID,
                "profile_id": case.profile_id,
                "config_version": UPDATED_VERSION,
            }),
            "{}",
            case.scenario
        );
    }
    std::fs::remove_file(output_path).expect("remove exported tenant configuration");
}

/// A missing configuration is an invalid response, not an unset profile.
#[tokio::test]
async fn tenant_show_rejects_a_response_without_a_configuration() {
    let output = run_command(
        &[
            "firmware",
            "nic",
            "tenant",
            "show",
            "--tenant-org-id",
            TENANT_ORG_ID,
        ],
        None,
        vec![Exchange {
            method: "/forge.Forge/GetTenantNicFirmwareConfig",
            request: GetTenantNicFirmwareConfigRequest {
                tenant_organization_id: TENANT_ORG_ID.into(),
            }
            .encode_to_vec(),
            response: Ok(GetTenantNicFirmwareConfigResponse {
                firmware_config: None,
            }
            .encode_to_vec()),
        }],
    )
    .await;
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let error = String::from_utf8(output.stderr).expect("UTF-8 diagnostic");
    assert!(
        error.contains("core response omitted NIC firmware configuration for tenant fds34511233a"),
        "{error}"
    );
    assert!(!error.contains("core accepted"), "{error}");
}

/// A missing configuration does not prove an accepted write was rolled back;
/// both commands must retain the acceptance context and require a reread.
#[tokio::test]
async fn tenant_mutations_without_a_configuration_report_acceptance_without_a_result() {
    struct Case {
        scenario: &'static str,
        args: Vec<&'static str>,
        action: Action,
    }
    for case in [
        // Set can commit before an incomplete response reaches the CLI.
        Case {
            scenario: "set rejects a missing configuration",
            args: vec!["set", "--profile", PROFILE_ID],
            action: Action::ProfileId(PROFILE_ID.into()),
        },
        // A clear still returns the entity; only `profile_id` may be absent.
        Case {
            scenario: "clear rejects a missing configuration",
            args: vec!["clear"],
            action: Action::Clear(()),
        },
    ] {
        let args: Vec<_> = ["firmware", "nic", "tenant"]
            .into_iter()
            .chain(case.args)
            .chain([
                "--tenant-org-id",
                TENANT_ORG_ID,
                "--if-version-match",
                ORIGINAL_VERSION,
            ])
            .collect();
        let output = run_command(
            &args,
            None,
            vec![Exchange {
                method: "/forge.Forge/SetTenantNicFirmwareConfig",
                request: SetTenantNicFirmwareConfigRequest {
                    tenant_organization_id: TENANT_ORG_ID.into(),
                    if_version_match: ORIGINAL_VERSION.into(),
                    action: Some(case.action),
                }
                .encode_to_vec(),
                response: Ok(SetTenantNicFirmwareConfigResponse {
                    firmware_config: None,
                }
                .encode_to_vec()),
            }],
        )
        .await;
        assert_eq!(output.status.code(), Some(1), "{}", case.scenario);
        assert!(output.stdout.is_empty(), "{}", case.scenario);
        let error = String::from_utf8(output.stderr).expect("UTF-8 diagnostic");
        for expected in [
            "core accepted the NIC firmware selection change",
            "firmware nic tenant show --tenant-org-id fds34511233a before another write",
            "core response omitted NIC firmware configuration for tenant fds34511233a",
        ] {
            assert!(error.contains(expected), "{}: {error}", case.scenario);
        }
    }
}

/// Both mutation leaves require the observed token in Clap before any tenant
/// configuration RPC, so neither command can silently fetch a replacement token.
#[tokio::test]
async fn tenant_mutations_require_an_observed_version_before_tenant_configuration_rpcs() {
    for (scenario, command) in [
        // Supplying a profile must not make the set token optional.
        ("set", vec!["set", "--profile", PROFILE_ID]),
        // Clear has its own argument declaration and must enforce the same guard.
        ("clear", vec!["clear"]),
    ] {
        let args: Vec<_> = ["firmware", "nic", "tenant"]
            .into_iter()
            .chain(command)
            .chain(["--tenant-org-id", TENANT_ORG_ID])
            .collect();
        let output = run_command(&args, None, Vec::new()).await;
        assert_eq!(output.status.code(), Some(2), "{scenario}");
        assert!(output.stdout.is_empty(), "{scenario}");
        let error = String::from_utf8(output.stderr).expect("UTF-8 diagnostic");
        // Usage also names the token; only the diagnostic proves why Clap failed.
        let (diagnostic, _) = error.split_once("Usage:").expect("Clap usage");
        assert!(diagnostic.starts_with("error:"), "{scenario}: {error}");
        assert!(
            diagnostic.contains("--if-version-match"),
            "{scenario}: {error}"
        );
    }
}

/// Rejected and uncertain writes must retain their original cause and token;
/// the strict RPC script rejects hidden retries or a replacement-token lookup.
#[tokio::test]
async fn tenant_mutation_failures_preserve_the_cause_without_retry_or_token_refresh() {
    struct Case {
        scenario: &'static str,
        args: Vec<&'static str>,
        action: Action,
        failure: Status,
    }
    for case in [
        // A stale token requires an operator decision, not an automatic refresh.
        Case {
            scenario: "stale set token remains a rejection",
            args: vec!["set", "--profile", PROFILE_ID],
            action: Action::ProfileId(PROFILE_ID.into()),
            failure: Status::failed_precondition("tenant version is stale"),
        },
        // Admission retries elsewhere in the client must not repeat this write.
        Case {
            scenario: "set admission failure is not retried",
            args: vec!["set", "--profile", PROFILE_ID],
            action: Action::ProfileId(PROFILE_ID.into()),
            failure: Status::resource_exhausted("tenant set admission exhausted"),
        },
        // A lost response cannot establish whether clear committed.
        Case {
            scenario: "an unavailable clear response has an uncertain commit",
            args: vec!["clear"],
            action: Action::Clear(()),
            failure: Status::unavailable("tenant clear response unavailable"),
        },
    ] {
        let args: Vec<_> = ["firmware", "nic", "tenant"]
            .into_iter()
            .chain(case.args)
            .chain([
                "--tenant-org-id",
                TENANT_ORG_ID,
                "--if-version-match",
                ORIGINAL_VERSION,
            ])
            .collect();
        let code = case.failure.code();
        let diagnostic = case.failure.message().to_owned();
        let output = run_command(
            &args,
            None,
            vec![Exchange {
                method: "/forge.Forge/SetTenantNicFirmwareConfig",
                request: SetTenantNicFirmwareConfigRequest {
                    tenant_organization_id: TENANT_ORG_ID.into(),
                    if_version_match: ORIGINAL_VERSION.into(),
                    action: Some(case.action),
                }
                .encode_to_vec(),
                response: Err(case.failure),
            }],
        )
        .await;
        assert_eq!(output.status.code(), Some(1), "{}", case.scenario);
        assert!(output.stdout.is_empty(), "{}", case.scenario);
        let error = String::from_utf8(output.stderr).expect("UTF-8 diagnostic");
        for expected in [
            TENANT_ORG_ID,
            ORIGINAL_VERSION,
            &diagnostic,
            &format!("code: '{code}'"),
        ] {
            assert!(error.contains(expected), "{}: {error}", case.scenario);
        }
        assert!(
            !error.contains("core accepted"),
            "{}: {error}",
            case.scenario
        );
        if code == Code::Unavailable {
            assert!(
                error.contains("after a connection error, inspect firmware nic tenant show before another write"),
                "{}: {error}",
                case.scenario
            );
        }
    }
}

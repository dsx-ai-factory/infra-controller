// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use std::cell::RefCell;

use carbide_test_support::Outcome::*;
use carbide_test_support::scenarios;
use carbide_uuid::machine::{MachineIdSource, MachineType};
use clap::error::ErrorKind;
use rpc::forge::SpdmMachineAttestationStatus;

use super::*;
use crate::async_write::CapturedOutput;
use crate::attestation::Cmd as AttestationCmd;
use crate::attestation::spdm::Cmd as SpdmCmd;
use crate::attestation::spdm::machines::Cmd as MachinesCmd;
use crate::cfg::cli_options::{CliCommand, CliOptions};

fn parse(args: &[&str]) -> Result<Args, clap::Error> {
    // Exercise the public Clap tree rather than parsing a detached argument struct.
    let options =
        CliOptions::try_parse_from(std::iter::once("nico-admin-cli").chain(args.iter().copied()))?;
    let Some(CliCommand::Attestation(AttestationCmd::Spdm(SpdmCmd::Machines(MachinesCmd::List(
        args,
    ))))) = options.commands
    else {
        panic!("expected the public SPDM machines list command");
    };
    Ok(args)
}

#[test]
fn machine_list_selectors() {
    scenarios!(
        run = |args: &[&str]| parse(args).map(|args| (args.passed, args.failed, args.cancelled, args.in_progress)).map_err(|error| error.kind());
        "all machines by default" {
            &["attestation", "spdm", "machines", "list"][..] => Yields((false, false, false, false)),
        }
        "passed selector through att alias" {
            &["att", "spdm", "machines", "list", "--passed"][..] => Yields((true, false, false, false)),
        }
        "failed selector" {
            &["att", "spdm", "machines", "list", "--failed"][..] => Yields((false, true, false, false)),
        }
        "cancelled selector" {
            &["att", "spdm", "machines", "list", "--cancelled"][..] => Yields((false, false, true, false)),
        }
        "in-progress selector" {
            &["att", "spdm", "machines", "list", "--in-progress"][..] => Yields((false, false, false, true)),
        }
        "single machine has no fleet selector" {
            &["att", "spdm", "machines", "list", "--machine-id", "fm100hsag07peffp850l14kvmhrqjf9h6jslilfahaknhvb6sq786c0g3jg"][..] => Yields((false, false, false, false)),
        }
        "timeout needs a machine" {
            &["att", "spdm", "machines", "list", "--redfish-timeout-secs", "60"][..] => FailsWith(ErrorKind::MissingRequiredArgument),
        }
        "zero timeout is invalid" {
            &["att", "spdm", "machines", "list", "--machine-id", "fm100hsag07peffp850l14kvmhrqjf9h6jslilfahaknhvb6sq786c0g3jg", "--redfish-timeout-secs", "0"][..] => FailsWith(ErrorKind::ValueValidation),
        }
        "machine mode excludes fleet filters" {
            &["att", "spdm", "machines", "list", "--machine-id", "fm100hsag07peffp850l14kvmhrqjf9h6jslilfahaknhvb6sq786c0g3jg", "--passed"][..] => FailsWith(ErrorKind::ArgumentConflict),
        }
        "contradictory status selectors" {
            &["att", "spdm", "machines", "list", "--passed", "--failed"][..] => FailsWith(ErrorKind::ArgumentConflict),
            &["att", "spdm", "machines", "list", "--passed", "--cancelled"][..] => FailsWith(ErrorKind::ArgumentConflict),
            &["att", "spdm", "machines", "list", "--failed", "--in-progress"][..] => FailsWith(ErrorKind::ArgumentConflict),
        }
    );
}

struct FakeClient {
    statuses: Vec<SpdmMachineAttestationStatus>,
    selector: Option<i32>,
    requests: RefCell<Vec<ListMachineAttestationStatusesRequest>>,
}

impl MachineAttestationClient for FakeClient {
    async fn list(
        &self,
        request: ListMachineAttestationStatusesRequest,
    ) -> Result<ListMachineAttestationStatusesResponse, Status> {
        let start = match request.target.as_ref().unwrap() {
            Target::Fleet(fleet) => {
                assert_eq!(fleet.attestation_status, self.selector);
                assert_eq!(fleet.page_size, 2);
                // Use an opaque fixture token; the CLI must return it unchanged.
                if self.requests.borrow().is_empty() {
                    assert!(fleet.page_token.is_empty());
                    0
                } else {
                    assert_eq!(fleet.page_token, "opaque-next-page");
                    2
                }
            }
            Target::Machine(machine) => {
                assert!(
                    self.requests.borrow().is_empty(),
                    "single-machine assessments must not paginate"
                );
                assert_eq!(machine.machine_id, self.statuses[0].machine_id);
                assert_eq!(machine.redfish_timeout_secs, Some(60));
                0
            }
        };
        self.requests.borrow_mut().push(request);
        let statuses = self.statuses.iter().skip(start).take(2).cloned().collect();
        Ok(ListMachineAttestationStatusesResponse {
            statuses,
            next_page_token: if self.statuses.len() > start + 2 {
                "opaque-next-page".to_string()
            } else {
                String::new()
            },
        })
    }
}

fn machine(seed: u8) -> MachineId {
    MachineId::new(
        MachineIdSource::ProductBoardChassisSerial,
        [seed; 32],
        MachineType::Host,
    )
}

async fn execute(client: &FakeClient, format: OutputFormat, flags: &[&str]) -> (String, String) {
    let mut output = CapturedOutput::new();
    let mut warnings = CapturedOutput::new();
    let args = ["attestation", "spdm", "machines", "list"]
        .into_iter()
        .chain(flags.iter().copied())
        .collect::<Vec<_>>();
    parse(&args)
        .unwrap()
        .execute(client, format, 2, output.writer(), warnings.writer())
        .await
        .unwrap();
    (
        String::from_utf8(output.into_bytes().await).unwrap(),
        String::from_utf8(warnings.into_bytes().await).unwrap(),
    )
}

#[tokio::test]
async fn machine_table_follows_pages_and_prints_populated_and_empty_cells() {
    let mut ids = vec![machine(1), machine(2), machine(3)];
    ids.sort();
    let statuses = ids
        .iter()
        .enumerate()
        .map(|(index, id)| SpdmMachineAttestationStatus {
            machine_id: Some(*id),
            attester_selection_status: None,
            attestation_status: if index == 0 {
                Some(SpdmAttestationStatus::SpdmAttPassed.into())
            } else {
                None
            },
        })
        .collect();
    let client = FakeClient {
        statuses,
        selector: None,
        requests: RefCell::default(),
    };
    let (output, warning) = execute(&client, OutputFormat::AsciiTable, &[]).await;
    let rows = output
        .lines()
        .filter(|line| line.starts_with('|'))
        .map(|line| {
            line.trim_matches('|')
                .split('|')
                .map(|cell| cell.trim().to_string())
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        rows,
        vec![
            vec!["MACHINE ID".to_string(), "ATTESTATION STATUS".to_string()],
            vec![ids[0].to_string(), "SPDM_ATT_PASSED".to_string()],
            vec![ids[1].to_string(), "unknown".to_string()],
            vec![ids[2].to_string(), "unknown".to_string()],
        ]
    );
    assert_eq!(client.requests.borrow().len(), 2);
    assert_eq!(
        warning,
        "Predicted attestation status, for definitive attestation status use \"attestation spdm machines list --machine-id <your machine id>\"\n"
    );
    let id = ids[0].to_string();
    for (scenario, attester_selection_status, status, expected_status, expected_warning) in [
        (
            "complete selection",
            SpdmAttesterSelectionStatus::Selected,
            Some(SpdmAttestationStatus::SpdmAttPassed),
            "SPDM_ATT_PASSED",
            "",
        ),
        (
            "empty selection",
            SpdmAttesterSelectionStatus::NoProfile,
            None,
            "not assessed",
            "",
        ),
        (
            "partial selection",
            SpdmAttesterSelectionStatus::PartiallySatisfied,
            Some(SpdmAttestationStatus::SpdmAttPassed),
            "SPDM_ATT_PASSED",
            "Warning: attestation status covers only the selected devices; the profile is partially satisfied\n",
        ),
    ] {
        let client = FakeClient {
            statuses: vec![SpdmMachineAttestationStatus {
                machine_id: Some(ids[0]),
                attestation_status: status.map(Into::into),
                attester_selection_status: Some(attester_selection_status.into()),
            }],
            selector: None,
            requests: RefCell::default(),
        };
        let (output, warning) = execute(
            &client,
            OutputFormat::AsciiTable,
            &["--machine-id", &id, "--redfish-timeout-secs", "60"],
        )
        .await;
        let rows = output
            .lines()
            .filter(|line| line.starts_with('|'))
            .map(|line| {
                line.trim_matches('|')
                    .split('|')
                    .map(|cell| cell.trim().to_string())
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        assert_eq!(
            rows,
            vec![
                vec![
                    "MACHINE ID",
                    "ATTESTATION STATUS",
                    "ATTESTER SELECTION STATUS"
                ]
                .into_iter()
                .map(str::to_string)
                .collect::<Vec<_>>(),
                vec![
                    id.clone(),
                    expected_status.to_string(),
                    attester_selection_status.as_str_name().to_string()
                ],
            ],
            "{scenario}"
        );
        assert_eq!(warning, expected_warning, "{scenario}");
        assert_eq!(client.requests.borrow().len(), 1, "{scenario}");
    }
}

#[tokio::test]
async fn machine_json_keeps_unknown_fields_and_warning_separate() {
    let id = machine(1);
    let client = FakeClient {
        statuses: vec![SpdmMachineAttestationStatus {
            machine_id: Some(id),
            attester_selection_status: None,
            attestation_status: None,
        }],
        selector: None,
        requests: RefCell::default(),
    };
    let (output, warning) = execute(&client, OutputFormat::Json, &[]).await;
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&output).unwrap(),
        serde_json::json!([
            { "machine_id": id, "attestation_status": null }
        ])
    );
    assert_eq!(warning, format!("{PREDICTION_WARNING}\n"));
    let client = FakeClient {
        statuses: vec![SpdmMachineAttestationStatus {
            machine_id: Some(id),
            attestation_status: None,
            attester_selection_status: Some(SpdmAttesterSelectionStatus::NoProfile.into()),
        }],
        selector: None,
        requests: RefCell::default(),
    };
    let id_arg = id.to_string();
    let (output, warning) = execute(
        &client,
        OutputFormat::Json,
        &["--machine-id", &id_arg, "--redfish-timeout-secs", "60"],
    )
    .await;
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&output).unwrap(),
        serde_json::json!([
            {"machine_id": id, "attestation_status": null,
             "attester_selection_status": "SPDM_ATTESTER_SELECTION_STATUS_NO_PROFILE"}
        ])
    );
    assert!(warning.is_empty());
}

#[tokio::test]
async fn status_filters_reach_rpc_and_empty_results_still_warn() {
    for (flag, status) in [
        ("--passed", SpdmAttestationStatus::SpdmAttPassed),
        ("--failed", SpdmAttestationStatus::SpdmAttFailed),
        ("--cancelled", SpdmAttestationStatus::SpdmAttCancelled),
        ("--in-progress", SpdmAttestationStatus::SpdmAttInProgress),
    ] {
        let client = FakeClient {
            statuses: Vec::new(),
            selector: Some(status.into()),
            requests: RefCell::default(),
        };
        let (output, warning) = execute(&client, OutputFormat::Json, &[flag]).await;
        assert_eq!(output.trim(), "[]", "{flag}");
        assert_eq!(warning, format!("{PREDICTION_WARNING}\n"), "{flag}");
        assert_eq!(client.requests.borrow().len(), 1, "{flag}");
    }
}

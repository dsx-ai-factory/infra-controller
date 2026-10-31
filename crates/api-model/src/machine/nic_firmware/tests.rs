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

use carbide_libmlx_model::firmware::result::FirmwareFlashReport;
use carbide_test_support::{Case, Check, Outcome, check_cases, check_values};

use super::*;
use crate::test_support::nic_firmware::{operation, target};

/// The fixture workflow's ID, which every card's report repeats as both its
/// operation and phase.
const WORKFLOW: &str = "nic-workflow";

/// The fixture card's PCI address, which names its supervisor journal.
const CARD: &str = "0000:01:00.0";

/// Burn evidence for the fixture's exact version and caller-selected lifecycle
/// state; optional phase evidence stays absent unless the case requests it.
/// Under the fixture's host-reboot activation, a `Complete` receipt is complete.
fn receipt(state: FirmwareOperationState) -> FirmwareOperation {
    FirmwareOperation {
        operation_id: WORKFLOW.to_string(),
        state,
        phase: None,
        report: FirmwareFlashReport {
            flashed: true,
            expected_version: Some("1.2.3".into()),
            ..Default::default()
        },
        diagnostic: None,
    }
}

/// A second valid card with its own identity and supervisor journal.
fn second_card() -> NicFirmwareTarget {
    NicFirmwareTarget {
        selector: NicFirmwareCardSelector::Mac("02:00:00:00:00:02".parse().expect("fixture MAC")),
        pci_name: "0000:02:00.0".into(),
        ..target()
    }
}

/// The message `validate` returns for one rejected rule.
fn invalid(message: &str) -> Outcome<(), String> {
    Outcome::FailsWith(format!("invalid value: {message}"))
}

#[test]
fn missing_receipt_freezes_helper_identity_without_regrant() {
    let mut operation = operation();
    assert!(operation.accept_missing_receipt(WORKFLOW, WORKFLOW, CARD, "helper-a".into()));

    // A process replacement must not reopen the admission gate, even while the
    // controller is still persisting its initial observation phase.
    assert!(operation.accept_missing_receipt(WORKFLOW, WORKFLOW, CARD, "helper-b".into()));
    operation.phase = NicFirmwarePhase::UpdatingFirmware { target_index: 0 };
    let before = operation.clone();
    assert!(!operation.accept_missing_receipt(WORKFLOW, WORKFLOW, CARD, "helper-b".into()));
    assert_eq!(operation, before);
    assert_eq!(
        operation.targets[0].supervisor_instance_id.as_deref(),
        Some("helper-a")
    );
    assert!(
        operation.targets[0]
            .diagnostic
            .as_deref()
            .expect("replacement diagnostic")
            .contains("not renewed")
    );
}

#[test]
fn missing_receipt_grants_a_helper_only_during_a_clean_first_observation() {
    // Only the first answer, during initial observation and before any receipt,
    // may freeze the helper that later submissions name. Every other missing
    // read records uncertainty without granting submission, and a repeat of
    // the same answer changes nothing.
    struct Answer {
        scenario: &'static str,
        phase: NicFirmwarePhase,
        previous: Option<FirmwareOperationState>,
        frozen: Option<&'static str>,
        instance: &'static str,
        changed: bool,
        diagnostic: Option<&'static str>,
    }
    let answers = [
        Answer {
            scenario: "journal lost after admission",
            phase: NicFirmwarePhase::UpdatingFirmware { target_index: 0 },
            previous: Some(FirmwareOperationState::Running),
            frozen: Some("helper-a"),
            instance: "helper-b",
            changed: true,
            diagnostic: Some("without resubmitting"),
        },
        Answer {
            scenario: "running receipt found during first observation",
            phase: NicFirmwarePhase::ObservingSupervisor { target_index: 0 },
            previous: Some(FirmwareOperationState::Running),
            frozen: None,
            instance: "helper-a",
            changed: true,
            diagnostic: Some("without resubmitting"),
        },
        Answer {
            scenario: "first answer after observation ended",
            phase: NicFirmwarePhase::UpdatingFirmware { target_index: 0 },
            previous: None,
            frozen: None,
            instance: "helper-a",
            changed: true,
            diagnostic: Some("not renewed"),
        },
        Answer {
            scenario: "malformed helper identity",
            phase: NicFirmwarePhase::ObservingSupervisor { target_index: 0 },
            previous: None,
            frozen: None,
            instance: "has space",
            changed: false,
            diagnostic: None,
        },
    ];
    for answer in answers {
        let mut operation = operation();
        operation.phase = answer.phase;
        operation.targets[0].receipt = answer.previous.map(receipt);
        operation.targets[0].supervisor_instance_id = answer.frozen.map(String::from);
        let before = operation.clone();

        let changed =
            operation.accept_missing_receipt(WORKFLOW, WORKFLOW, CARD, answer.instance.into());

        let scenario = answer.scenario;
        assert_eq!(changed, answer.changed, "{scenario}");
        let card = &operation.targets[0];
        assert_eq!(card.receipt, before.targets[0].receipt, "{scenario}");
        assert_eq!(
            card.supervisor_instance_id, before.targets[0].supervisor_instance_id,
            "{scenario}: only a clean first observation grants a helper"
        );
        match answer.diagnostic {
            Some(expected) => assert!(
                card.diagnostic
                    .as_deref()
                    .is_some_and(|diagnostic| diagnostic.contains(expected)),
                "{scenario}: {:?}",
                card.diagnostic
            ),
            None => assert_eq!(operation, before, "{scenario}"),
        }
        let settled = operation.clone();
        assert!(
            !operation.accept_missing_receipt(WORKFLOW, WORKFLOW, CARD, answer.instance.into()),
            "{scenario}: repeat"
        );
        assert_eq!(operation, settled, "{scenario}: repeat");
    }
}

#[test]
fn receipt_intake_matches_the_whole_request_and_preserves_terminal_evidence() {
    /// The phase that receives one execution report, the identifiers the
    /// report repeats, its receipt, and the card's previously accepted receipt.
    struct Report {
        phase: NicFirmwarePhase,
        operation_id: &'static str,
        phase_id: &'static str,
        pci_name: &'static str,
        receipt_operation_id: &'static str,
        state: FirmwareOperationState,
        previous: Option<FirmwareOperationState>,
    }
    const MATCHING: Report = Report {
        phase: NicFirmwarePhase::UpdatingFirmware { target_index: 0 },
        operation_id: WORKFLOW,
        phase_id: WORKFLOW,
        pci_name: CARD,
        receipt_operation_id: WORKFLOW,
        state: FirmwareOperationState::Running,
        previous: None,
    };

    check_values(
        [
            Check {
                scenario: "different operation",
                input: Report {
                    operation_id: "other",
                    ..MATCHING
                },
                expect: false,
            },
            Check {
                scenario: "different workflow phase",
                input: Report {
                    phase_id: "other",
                    ..MATCHING
                },
                expect: false,
            },
            Check {
                scenario: "different journal target",
                input: Report {
                    pci_name: "0000:02:00.0",
                    ..MATCHING
                },
                expect: false,
            },
            Check {
                scenario: "receipt for another operation",
                input: Report {
                    receipt_operation_id: "other",
                    ..MATCHING
                },
                expect: false,
            },
            Check {
                scenario: "report outside execution phases",
                input: Report {
                    phase: NicFirmwarePhase::Unlocking { target_index: 0 },
                    ..MATCHING
                },
                expect: false,
            },
            Check {
                scenario: "first exact receipt",
                input: MATCHING,
                expect: true,
            },
            Check {
                scenario: "newer progress replaces a running receipt",
                input: Report {
                    state: FirmwareOperationState::Complete,
                    previous: Some(FirmwareOperationState::Running),
                    ..MATCHING
                },
                expect: true,
            },
            Check {
                scenario: "repeated running receipt is unchanged without terminal protection",
                input: Report {
                    previous: Some(FirmwareOperationState::Running),
                    ..MATCHING
                },
                expect: false,
            },
            Check {
                scenario: "late progress after completion",
                input: Report {
                    previous: Some(FirmwareOperationState::Complete),
                    ..MATCHING
                },
                expect: false,
            },
            Check {
                scenario: "late progress after failure",
                input: Report {
                    previous: Some(FirmwareOperationState::Failed),
                    ..MATCHING
                },
                expect: false,
            },
        ],
        |report| {
            let mut operation = operation();
            operation.phase = report.phase;
            operation.targets[0].receipt = report.previous.map(receipt);
            operation.targets[0].diagnostic = Some("earlier delivery error".into());
            let before = operation.clone();
            let mut incoming = receipt(report.state);
            incoming.operation_id = report.receipt_operation_id.to_string();

            let accepted = operation.accept_receipt(
                report.operation_id,
                report.phase_id,
                report.pci_name,
                incoming.clone(),
            );

            if accepted {
                assert_eq!(operation.targets[0].receipt, Some(incoming));
                assert_eq!(operation.targets[0].diagnostic, None);
            } else {
                assert_eq!(
                    operation, before,
                    "ignored evidence must not modify the operation"
                );
            }
            accepted
        },
    );
}

#[test]
fn delivery_errors_are_kept_only_for_the_active_unsettled_request() {
    // Delivery errors have their own intake path. A correlated error is kept
    // until execution settles; a repeat, a late error, an unrelated request or
    // a malformed diagnostic leaves the stored evidence alone.
    struct Delivery {
        operation_id: &'static str,
        diagnostic: String,
        previous: FirmwareOperationState,
        stored: Option<&'static str>,
    }
    let correlated = || Delivery {
        operation_id: WORKFLOW,
        diagnostic: "delivery failed".into(),
        previous: FirmwareOperationState::Running,
        stored: None,
    };
    check_values(
        [
            Check {
                scenario: "correlated error is kept",
                input: correlated(),
                expect: true,
            },
            Check {
                scenario: "repeated error",
                input: Delivery {
                    stored: Some("delivery failed"),
                    ..correlated()
                },
                expect: false,
            },
            Check {
                scenario: "error after a terminal receipt",
                input: Delivery {
                    previous: FirmwareOperationState::Complete,
                    ..correlated()
                },
                expect: false,
            },
            Check {
                scenario: "unrelated workflow",
                input: Delivery {
                    operation_id: "another-workflow",
                    ..correlated()
                },
                expect: false,
            },
            Check {
                scenario: "empty diagnostic",
                input: Delivery {
                    diagnostic: String::new(),
                    ..correlated()
                },
                expect: false,
            },
            Check {
                scenario: "diagnostic over the wire limit",
                input: Delivery {
                    diagnostic: "x".repeat(2049),
                    ..correlated()
                },
                expect: false,
            },
        ],
        |delivery| {
            let mut operation = operation();
            operation.phase = NicFirmwarePhase::UpdatingFirmware { target_index: 0 };
            operation.targets[0].receipt = Some(receipt(delivery.previous));
            operation.targets[0].supervisor_instance_id = Some("helper-a".into());
            operation.targets[0].diagnostic = delivery.stored.map(String::from);
            let before = operation.clone();

            let kept = operation.accept_error(
                delivery.operation_id,
                WORKFLOW,
                CARD,
                delivery.diagnostic.clone(),
            );

            if kept {
                let mut expected = before;
                expected.targets[0].diagnostic = Some(delivery.diagnostic);
                assert_eq!(operation, expected);
            } else {
                assert_eq!(operation, before);
            }
            kept
        },
    );
}

#[test]
fn initial_observation_accepts_existing_receipt_without_granting_submission() {
    let mut operation = operation();
    let completed = receipt(FirmwareOperationState::Complete);
    assert!(operation.accept_receipt(WORKFLOW, WORKFLOW, CARD, completed.clone()));
    assert_eq!(operation.targets[0].receipt, Some(completed));
    assert_eq!(operation.targets[0].supervisor_instance_id, None);
    assert_eq!(
        operation.phase,
        NicFirmwarePhase::ObservingSupervisor { target_index: 0 }
    );

    // Observation is not new admission: neither a later missing read nor a late
    // progress receipt can reopen submission or replace this terminal evidence.
    let observed = operation.clone();
    assert!(!operation.accept_missing_receipt(
        WORKFLOW,
        WORKFLOW,
        CARD,
        "replacement-helper".into(),
    ));
    assert!(!operation.accept_receipt(
        WORKFLOW,
        WORKFLOW,
        CARD,
        receipt(FirmwareOperationState::Running),
    ));
    assert_eq!(operation, observed);
}

#[test]
fn completion_requires_the_frozen_activation() {
    // Reset evidence must match the activation exactly, including its absence
    // when a host reboot activates the cards instead.
    let card_reset = NicFirmwareActivation::CardReset {
        reset_level: 3,
        post_reset_verify_retry_interval_seconds: None,
        post_reset_verify_timeout_seconds: None,
    };
    check_values(
        [
            Check {
                scenario: "host reboot without reset evidence",
                input: (NicFirmwareActivation::HostReboot, None),
                expect: true,
            },
            Check {
                scenario: "card reset proven",
                input: (card_reset.clone(), Some(true)),
                expect: true,
            },
            Check {
                scenario: "card reset evidence omitted",
                input: (card_reset, None),
                expect: false,
            },
            Check {
                scenario: "reset evidence despite a host reboot",
                input: (NicFirmwareActivation::HostReboot, Some(true)),
                expect: false,
            },
        ],
        |(activation, reset_result)| {
            let mut operation = operation();
            operation.options.activation = activation;
            let mut completed = receipt(FirmwareOperationState::Complete);
            completed.report.reset = reset_result;
            operation.targets[0].receipt = Some(completed);
            operation.targets[0].is_complete(WORKFLOW, &operation.options)
        },
    );
}

#[test]
fn completion_rejects_missing_or_contradictory_execution_evidence() {
    // Each row isolates a different request/report field. Validating only the
    // terminal label must not erase a requested phase or a mismatched version.
    type Mutation = fn(&mut NicFirmwareExecutionOptions, &mut FirmwareOperation);
    check_values(
        [
            Check {
                scenario: "receipt belongs to another workflow",
                input: (|_, receipt: &mut FirmwareOperation| {
                    receipt.operation_id = "another-workflow".into();
                }) as Mutation,
                expect: false,
            },
            Check {
                scenario: "running receipt is not completion",
                input: |_, receipt| receipt.state = FirmwareOperationState::Running,
                expect: false,
            },
            Check {
                scenario: "completed no-work receipt is not a burn",
                input: |_, receipt| receipt.report.flashed = false,
                expect: false,
            },
            Check {
                scenario: "reported target version is missing",
                input: |_, receipt| receipt.report.expected_version = None,
                expect: false,
            },
            Check {
                scenario: "reported target version is different",
                input: |_, receipt| receipt.report.expected_version = Some("other-version".into()),
                expect: false,
            },
            Check {
                scenario: "required image check is missing",
                input: |options, _| options.verify_image = true,
                expect: false,
            },
            Check {
                scenario: "unrequested image check is present",
                input: |_, receipt| receipt.report.verified_image = Some(true),
                expect: false,
            },
            Check {
                scenario: "required image check succeeded",
                input: |options, receipt| {
                    options.verify_image = true;
                    receipt.report.verified_image = Some(true);
                },
                expect: true,
            },
            Check {
                scenario: "required image check failed",
                input: |options, receipt| {
                    options.verify_image = true;
                    receipt.report.verified_image = Some(false);
                },
                expect: false,
            },
            Check {
                scenario: "required version check is missing",
                input: |options, _| options.verify_version = true,
                expect: false,
            },
            Check {
                scenario: "unrequested version check is present",
                input: |_, receipt| receipt.report.verified_version = Some(true),
                expect: false,
            },
            Check {
                scenario: "successful version flag has no version",
                input: |options, receipt| {
                    options.verify_version = true;
                    receipt.report.verified_version = Some(true);
                },
                expect: false,
            },
            Check {
                scenario: "successful version flag contradicts observed version",
                input: |options, receipt| {
                    options.verify_version = true;
                    receipt.report.verified_version = Some(true);
                    receipt.report.observed_version = Some("old-version".into());
                },
                expect: false,
            },
            Check {
                scenario: "required exact version was observed",
                input: |options, receipt| {
                    options.verify_version = true;
                    receipt.report.verified_version = Some(true);
                    receipt.report.observed_version = Some("1.2.3".into());
                },
                expect: true,
            },
        ],
        |mutate| {
            let mut operation = operation();
            let mut completed = receipt(FirmwareOperationState::Complete);
            mutate(&mut operation.options, &mut completed);
            operation.targets[0].receipt = Some(completed);
            operation.targets[0].is_complete(WORKFLOW, &operation.options)
        },
    );
}

#[test]
fn lockdown_restoration_requires_evidence_that_mutation_has_settled_safely() {
    // Missing completion checkpoints can leave effects unknown even when
    // `flashed` is false, so the recorded phase must also come before any
    // mutation. A reported burn rules restoration out at any phase.
    /// The stored receipt's operation, execution state, last recorded phase,
    /// and whether it reports a burn.
    struct Settlement {
        operation_id: &'static str,
        state: FirmwareOperationState,
        phase: Option<FirmwareOperationPhase>,
        flashed: bool,
    }
    const FAILED_PREPARATION: Settlement = Settlement {
        operation_id: WORKFLOW,
        state: FirmwareOperationState::Failed,
        phase: Some(FirmwareOperationPhase::Prepare),
        flashed: false,
    };
    check_values(
        [
            Check {
                scenario: "completed executor released its work",
                input: Settlement {
                    state: FirmwareOperationState::Complete,
                    phase: Some(FirmwareOperationPhase::VerifyVersion),
                    flashed: true,
                    ..FAILED_PREPARATION
                },
                expect: true,
            },
            Check {
                scenario: "failed preparation cannot have mutated the card",
                input: FAILED_PREPARATION,
                expect: true,
            },
            Check {
                scenario: "cancel before phase selection is safe",
                input: Settlement {
                    state: FirmwareOperationState::Canceled,
                    phase: None,
                    ..FAILED_PREPARATION
                },
                expect: true,
            },
            Check {
                scenario: "receipt for another operation",
                input: Settlement {
                    operation_id: "another-workflow",
                    ..FAILED_PREPARATION
                },
                expect: false,
            },
            Check {
                scenario: "a reported burn rules out restoration",
                input: Settlement {
                    flashed: true,
                    ..FAILED_PREPARATION
                },
                expect: false,
            },
            Check {
                scenario: "configuration failure may have changed the card",
                input: Settlement {
                    phase: Some(FirmwareOperationPhase::Configure),
                    ..FAILED_PREPARATION
                },
                expect: false,
            },
            Check {
                scenario: "burn intent without a checkpoint is uncertain",
                input: Settlement {
                    phase: Some(FirmwareOperationPhase::Burn),
                    ..FAILED_PREPARATION
                },
                expect: false,
            },
            Check {
                scenario: "cancel after burn does not authorize reset or restoration",
                input: Settlement {
                    state: FirmwareOperationState::Canceled,
                    phase: Some(FirmwareOperationPhase::Activate),
                    flashed: true,
                    ..FAILED_PREPARATION
                },
                expect: false,
            },
            Check {
                scenario: "interruption is not safe completion",
                input: Settlement {
                    state: FirmwareOperationState::Interrupted,
                    ..FAILED_PREPARATION
                },
                expect: false,
            },
            Check {
                scenario: "manual intervention remains explicit even before burn",
                input: Settlement {
                    state: FirmwareOperationState::ManualIntervention,
                    phase: Some(FirmwareOperationPhase::Inspect),
                    ..FAILED_PREPARATION
                },
                expect: false,
            },
        ],
        |settlement| {
            let mut target = target();
            target.receipt = Some(FirmwareOperation {
                operation_id: settlement.operation_id.to_string(),
                state: settlement.state,
                phase: settlement.phase,
                report: FirmwareFlashReport {
                    flashed: settlement.flashed,
                    ..Default::default()
                },
                diagnostic: None,
            });
            target.can_restore_lockdown(WORKFLOW)
        },
    );
}

#[test]
fn validation_rejects_unsafe_requests() {
    // Reject malformed persisted requests before a caller can authorize device
    // effects. Each mutation isolates one rule and expects that rule's message.
    type Mutation = fn(&mut NicFirmwareOperation);
    let mutations: [(&str, Mutation, &str); 18] = [
        (
            "no cards",
            |operation| operation.targets.clear(),
            "NIC firmware operation requires a valid identity and at least one target",
        ),
        (
            "reserved legacy identity",
            |operation| operation.operation_id = "dpa-firmware:old".into(),
            "NIC firmware operation requires a valid identity and at least one target",
        ),
        (
            "short PCI locator",
            |operation| operation.targets[0].pci_name = "01:00.0".into(),
            "NIC firmware target requires a full lowercase PCI BDF",
        ),
        (
            "PCI locator with a leading space",
            |operation| operation.targets[0].pci_name = " 000:01:00.0".into(),
            "NIC firmware target requires a full lowercase PCI BDF",
        ),
        (
            "uppercase image digest",
            |operation| operation.targets[0].resolution.entry.image.sha256 = "AB".repeat(32),
            "NIC firmware target requires a normalized firmware entry",
        ),
        (
            "malformed supervisor instance",
            |operation| operation.targets[0].supervisor_instance_id = Some("has space".into()),
            "NIC firmware target has an invalid supervisor instance",
        ),
        (
            "lockdown key context for another card",
            |operation| {
                operation.targets[0].original_lockdown = NicFirmwareLockdownPolicy::Locked {
                    ikm_version: 1,
                    mac_address: "02:00:00:00:00:02".parse().expect("fixture MAC"),
                }
            },
            "NIC firmware lockdown context does not match the selected card",
        ),
        (
            "phase names a missing card",
            |operation| operation.phase = NicFirmwarePhase::UpdatingFirmware { target_index: 1 },
            "NIC firmware phase references a missing target",
        ),
        (
            "zero retry interval",
            |operation| {
                operation.options.activation = NicFirmwareActivation::CardReset {
                    reset_level: 3,
                    post_reset_verify_retry_interval_seconds: Some(0),
                    post_reset_verify_timeout_seconds: None,
                }
            },
            "NIC firmware post-reset verification timing must be in 1..=1800 seconds",
        ),
        (
            "excessive verification budget",
            |operation| {
                operation.options.activation = NicFirmwareActivation::CardReset {
                    reset_level: 3,
                    post_reset_verify_retry_interval_seconds: None,
                    post_reset_verify_timeout_seconds: Some(1801),
                }
            },
            "NIC firmware post-reset verification timing must be in 1..=1800 seconds",
        ),
        (
            "submission without a frozen helper or receipt",
            |operation| operation.phase = NicFirmwarePhase::UpdatingFirmware { target_index: 0 },
            "NIC firmware submission requires a frozen supervisor instance or a receipt",
        ),
        (
            "next card while an earlier card is incomplete",
            |operation| {
                operation.targets.push(second_card());
                operation.phase = NicFirmwarePhase::ObservingSupervisor { target_index: 1 };
            },
            "NIC firmware card work requires every earlier card to complete",
        ),
        (
            "host reboot under card reset activation",
            |operation| {
                operation.options.activation = NicFirmwareActivation::CardReset {
                    reset_level: 3,
                    post_reset_verify_retry_interval_seconds: None,
                    post_reset_verify_timeout_seconds: None,
                };
                operation.phase = NicFirmwarePhase::Rebooting {
                    started_at: "2026-10-27T00:00:00Z".parse().expect("fixture time"),
                    last_attempt_at: None,
                };
            },
            "NIC firmware host reboot requires host reboot activation",
        ),
        (
            "persisted activation lacks completed receipts",
            |operation| {
                operation.phase = NicFirmwarePhase::Rebooting {
                    started_at: "2026-10-27T00:00:00Z".parse().expect("fixture time"),
                    last_attempt_at: None,
                };
            },
            "NIC firmware activation and success require evidence for every requested phase",
        ),
        (
            "persisted reobservation lacks completed receipts",
            |operation| {
                operation.phase = NicFirmwarePhase::Reobserving {
                    not_before: "2026-10-27T00:00:00Z".parse().expect("fixture time"),
                };
            },
            "NIC firmware activation and success require evidence for every requested phase",
        ),
        (
            "successful restoration lacks completed receipts",
            |operation| {
                operation.phase = NicFirmwarePhase::RestoringLockdown {
                    target_index: 0,
                    failure: None,
                };
            },
            "NIC firmware activation and success require evidence for every requested phase",
        ),
        (
            "restoration with uncertain execution",
            |operation| {
                operation.phase = NicFirmwarePhase::RestoringLockdown {
                    target_index: 0,
                    failure: Some("interrupted burn".into()),
                };
                operation.targets[0].receipt = Some(receipt(FirmwareOperationState::Interrupted));
            },
            "NIC firmware lockdown cannot be restored while execution effects remain uncertain",
        ),
        (
            "failure recorded with uncertain execution",
            |operation| {
                operation.phase = NicFirmwarePhase::Failed {
                    reason: "interrupted burn".into(),
                };
                operation.targets[0].receipt = Some(receipt(FirmwareOperationState::Interrupted));
            },
            "NIC firmware lockdown cannot be restored while execution effects remain uncertain",
        ),
    ];
    check_cases(
        mutations.map(|(scenario, input, message)| Case {
            scenario,
            input,
            expect: invalid(message),
        }),
        |mutate| {
            let mut operation = operation();
            mutate(&mut operation);
            operation.validate().map_err(|error| error.to_string())
        },
    );
}

#[test]
fn validation_accepts_consistent_requests() {
    // Positive controls for the rules above: each request sits next to a
    // rejected case and must still validate.
    type Mutation = fn(&mut NicFirmwareOperation);
    let requests: [(&str, Mutation); 5] = [
        ("card reset timing at both bounds", |operation| {
            // The supervisor accepts both ends of 1..=1800 seconds, so Core
            // must not reject either one before delivery.
            operation.options.activation = NicFirmwareActivation::CardReset {
                reset_level: 3,
                post_reset_verify_retry_interval_seconds: Some(1),
                post_reset_verify_timeout_seconds: Some(1800),
            }
        }),
        ("locked card restored under its own MAC", |operation| {
            operation.targets[0].original_lockdown = NicFirmwareLockdownPolicy::Locked {
                ikm_version: 1,
                mac_address: "02:00:00:00:00:01".parse().expect("fixture MAC"),
            }
        }),
        ("submission under a frozen helper", |operation| {
            operation.phase = NicFirmwarePhase::UpdatingFirmware { target_index: 0 };
            operation.targets[0].supervisor_instance_id = Some("helper-a".into());
        }),
        ("host reboot after every card completed", |operation| {
            operation.phase = NicFirmwarePhase::Rebooting {
                started_at: "2026-10-27T00:00:00Z".parse().expect("fixture time"),
                last_attempt_at: None,
            };
            operation.targets[0].receipt = Some(receipt(FirmwareOperationState::Complete));
        }),
        ("failure after a stop before mutation", |operation| {
            operation.phase = NicFirmwarePhase::Failed {
                reason: "preparation failed".into(),
            };
            let mut failed = receipt(FirmwareOperationState::Failed);
            failed.phase = Some(FirmwareOperationPhase::Prepare);
            failed.report.flashed = false;
            operation.targets[0].receipt = Some(failed);
        }),
    ];
    check_cases(
        requests.map(|(scenario, input)| Case {
            scenario,
            input,
            expect: Outcome::Yields(()),
        }),
        |mutate| {
            let mut operation = operation();
            mutate(&mut operation);
            operation.validate().map_err(|error| error.to_string())
        },
    );
}

#[test]
fn target_uniqueness_rejects_each_identity_independently() {
    // Two otherwise distinct cards isolate each uniqueness rule; duplicating
    // the entire first target would let either guard hide the other.
    type Mutation = fn(&mut NicFirmwareTarget, &NicFirmwareTarget);
    check_values(
        [
            Check {
                scenario: "different physical cards and journals",
                input: (|_, _| {}) as Mutation,
                expect: true,
            },
            Check {
                scenario: "same physical selector",
                input: |second, first| second.selector = first.selector.clone(),
                expect: false,
            },
            Check {
                scenario: "same PCI locator",
                input: |second, first| second.pci_name = first.pci_name.clone(),
                expect: false,
            },
        ],
        |mutate| {
            let mut operation = operation();
            let mut second = second_card();
            mutate(&mut second, &operation.targets[0]);
            operation.targets.push(second);
            operation.validate().is_ok()
        },
    );
}

#[test]
fn populated_operation_round_trips_through_json() {
    // Every optional or non-default field holds a value, so a serde attribute
    // that drops or renames one shows up as a mismatch.
    let mut operation = operation();
    operation.purpose = NicFirmwarePurpose::RestoreBaseline {
        instance_id: uuid::Uuid::nil().into(),
        cleanup_context: CleanupContext::Reset,
    };
    operation.options = NicFirmwareExecutionOptions {
        verify_image: true,
        verify_version: true,
        activation: NicFirmwareActivation::CardReset {
            reset_level: 3,
            post_reset_verify_retry_interval_seconds: Some(10),
            post_reset_verify_timeout_seconds: Some(600),
        },
    };
    operation.phase = NicFirmwarePhase::RestoringLockdown {
        target_index: 0,
        failure: Some("verification failed".into()),
    };
    operation.lockdown = NicFirmwareLockdownProgress {
        step: NicFirmwareLockdownStep::Change,
        result: Some(NicFirmwareLockdownResult::Error {
            diagnostic: "lock failed".into(),
        }),
    };
    let card = &mut operation.targets[0];
    card.original_lockdown = NicFirmwareLockdownPolicy::Locked {
        ikm_version: 2,
        mac_address: "02:00:00:00:00:01".parse().expect("fixture MAC"),
    };
    card.supervisor_instance_id = Some("helper-a".into());
    card.receipt = Some(receipt(FirmwareOperationState::Complete));
    card.diagnostic = Some("receipt replayed".into());

    let encoded = serde_json::to_string(&operation).expect("encode operation");
    let decoded: NicFirmwareOperation = serde_json::from_str(&encoded).expect("decode operation");

    assert_eq!(decoded, operation);
}

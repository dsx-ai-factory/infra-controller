// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! `MlxDeviceLockdown` reads or changes the lockdown state of one NIC.
//!
//! Each command asks for one step: read the lock status, lock, or unlock.
//! Scout checks that the card at the target PCI address is still the one the
//! request names, then runs the step under the same host claim as that check.
//! A card that reports a different identity is refused. The claim keeps other
//! Scout hardware work out until the step finishes; it cannot stop someone
//! physically swapping the card after the check. Results go out as
//! `lockdown_result`, never the legacy `lock_status` field that DPA state
//! reads.

use std::future::Future;
use std::time::Duration;

use carbide_libmlx_model::device::identity::MlxDeviceSelector;
use carbide_libmlx_model::device::info::MlxDeviceInfo;
use libmlx::device::discovery::{
    DeviceDiscoveryError, discover_device_bounded_with_executor, normalize_pci_address,
};
use libmlx::firmware::operation::SupervisorConfig;
use libmlx::lockdown::error::{MlxError, MlxResult};
use libmlx::lockdown::lockdown::{LockdownManager, StatusReport};
use libmlx::lockdown::runner::FlintRunner;
use libmlx::runner::FirmwareToolExecutor;
use rpc::forge_agent_control_response::MlxDeviceLockdown;
use rpc::forge_agent_control_response::mlx_device_lockdown::Request;
use rpc::protos::mlx_device as pb;
use rpc::protos::mlx_device::mlx_device_lockdown_result::{Changed, Result as StepResult};

use crate::mlx_access::{DirectMlxAccess, DirectMlxAccessError};
use crate::mlx_device::is_firmware_identifier;

// Each bounded tool attempt gets this long, matching bounded inventory; the
// executor adds up to five seconds for cleanup and five more for the result.
// It bounds one step, not how long the controller keeps reconciling.
const LOCKDOWN_TIMEOUT: Duration = Duration::from_secs(30);

/// `handle` answers one lockdown step. `None` means Scout did not take the
/// host claim: other hardware work holds it, its marker is unsafe or
/// unreadable, or acquiring it failed. Nothing touches the card, and the same
/// command can run again on a later poll.
pub(super) async fn handle(
    device: &str,
    task: &MlxDeviceLockdown,
    config: &SupervisorConfig,
) -> Option<pb::MlxObservation> {
    let executor = FirmwareToolExecutor::default();
    // Automatic lockdown steps run `flint` from PATH, the way bounded inventory runs
    // `mlxfwmanager`. Naming the tool skips the version probe in `new()`.
    let manager = LockdownManager::with_runner(FlintRunner::with_path("flint"));
    handle_with(
        device,
        task,
        config,
        || discover_device_bounded_with_executor(device, &executor),
        |step| run_step(step, device, &manager, &executor),
    )
    .await
}

/// Check the request, then discover the card and run the step under one claim.
/// Tests supply `discover` and `run` to watch that ordering.
async fn handle_with<Discover, Run>(
    device: &str,
    task: &MlxDeviceLockdown,
    config: &SupervisorConfig,
    discover: impl FnOnce() -> Discover,
    run: impl FnOnce(Request) -> Run,
) -> Option<pb::MlxObservation>
where
    Discover: Future<Output = Result<MlxDeviceInfo, DeviceDiscoveryError>>,
    Run: Future<Output = MlxResult<StepResult>>,
{
    let (expected_device, step) = match checked_request(device, task) {
        Ok(checked) => checked,
        Err(diagnostic) => return Some(lockdown_error_observation(device, task, diagnostic)),
    };
    let mut claim = DirectMlxAccess::acquire_or_defer(config, device)?;

    let step_result = match discover().await {
        Err(error) => {
            claim.observe_error(&error);
            tracing::warn!(pci_name = device, %error, "NIC lockdown discovery failed");
            Err("NIC discovery failed before the lockdown step")
        }
        Ok(device_info) => match unexpected_card(device, &expected_device, &device_info) {
            Some(diagnostic) => {
                tracing::warn!(
                    pci_name = device,
                    expected = %expected_device,
                    observed_pci_name = %device_info.pci_name,
                    observed_base_mac = ?device_info.base_mac,
                    observed_base_guid = ?device_info.base_guid,
                    diagnostic,
                    "NIC lockdown refused for an unexpected card"
                );
                Err(diagnostic)
            }
            None => match run(step).await {
                Ok(result) => Ok((device_info, result)),
                Err(error) => {
                    // Settlement is recorded before the error becomes a fixed
                    // diagnostic. Bounded `flint` errors already drop their
                    // arguments and tool output, so logging one cannot repeat
                    // the key.
                    claim.observe_error(&error);
                    tracing::warn!(pci_name = device, %error, "NIC lockdown step failed");
                    Err("NIC lockdown step failed")
                }
            },
        },
    };

    match claim.finish() {
        Ok(()) => {}
        // The tool has stopped, so its answer stands. Release removes the
        // marker before syncing its directory, so a failure may or may not
        // leave the marker behind; one that remains blocks later hardware work
        // until an operator reconciles it.
        Err(DirectMlxAccessError::Release(error)) => {
            tracing::warn!(pci_name = device, %error, "NIC lockdown finished but its claim release failed");
        }
        Err(error) => {
            tracing::warn!(pci_name = device, %error, "NIC lockdown process settlement is unknown");
            return Some(lockdown_error_observation(
                device,
                task,
                "NIC lockdown process settlement is unknown; hardware access stays blocked",
            ));
        }
    }

    Some(match step_result {
        Ok((device_info, result)) => pb::MlxObservation {
            device_info: Some(device_info.into()),
            lockdown_result: Some(lockdown_result(device, task, result)),
            ..Default::default()
        },
        Err(diagnostic) => lockdown_error_observation(device, task, diagnostic),
    })
}

/// Validate before claiming hardware. The step targets the outer PCI address,
/// and the selector names the physical card that has to be there.
fn checked_request(
    device: &str,
    task: &MlxDeviceLockdown,
) -> Result<(MlxDeviceSelector, Request), &'static str> {
    if !is_firmware_identifier(&task.controller_state_version) {
        return Err("NIC lockdown request has an invalid controller state version");
    }
    if normalize_pci_address(device).as_deref() != Some(device) {
        return Err("NIC lockdown request needs a full lowercase PCI address");
    }
    let expected_device = task
        .expected_device
        .clone()
        .ok_or("NIC lockdown request has no expected device")?
        .try_into()
        .map_err(|_| "NIC lockdown request has an invalid expected device")?;
    let step = task
        .request
        .clone()
        .ok_or("NIC lockdown request has no step")?;
    Ok((expected_device, step))
}

/// `unexpected_card` compares discovery with the request the way the
/// supervisor does before an update mutates a card: the exact PCI address,
/// then the base MAC or GUID. Part number and PSID are not identity, since a
/// replacement card can share them. A card that reports no value for the
/// selected field gets its own diagnostic, since it may not be a different
/// card at all.
fn unexpected_card(
    device: &str,
    expected: &MlxDeviceSelector,
    device_info: &MlxDeviceInfo,
) -> Option<&'static str> {
    if normalize_pci_address(&device_info.pci_name).as_deref() != Some(device) {
        return Some("NIC discovery reported a different PCI address");
    }
    if expected.matches(device_info) {
        return None;
    }
    let reported = match expected {
        MlxDeviceSelector::Mac(_) => device_info.base_mac.is_some(),
        MlxDeviceSelector::Guid(_) => device_info.base_guid.is_some(),
    };
    Some(if reported {
        "NIC does not match the expected device"
    } else {
        "NIC did not report the identity the request expects"
    })
}

/// Run one step with the bounded lockdown calls. A card already in the
/// requested state counts as changed, whichever key locked it; the controller
/// reads the status separately before relying on it.
async fn run_step(
    step: Request,
    device: &str,
    manager: &LockdownManager,
    executor: &FirmwareToolExecutor,
) -> MlxResult<StepResult> {
    match step {
        Request::Status(_) => {
            let status = manager
                .get_status_bounded(device, executor, LOCKDOWN_TIMEOUT)
                .await?;
            Ok(StepResult::StatusReport(
                StatusReport::new(device.to_string(), status).into(),
            ))
        }
        Request::Lock(lock) => match manager
            .lock_device_bounded(device, &lock.key, executor, LOCKDOWN_TIMEOUT)
            .await
        {
            Ok(_) | Err(MlxError::AlreadyLocked) => Ok(StepResult::Changed(Changed {})),
            Err(error) => Err(error),
        },
        Request::Unlock(unlock) => match manager
            .unlock_device_bounded(device, &unlock.key, executor, LOCKDOWN_TIMEOUT)
            .await
        {
            Ok(_) | Err(MlxError::AlreadyUnlocked) => Ok(StepResult::Changed(Changed {})),
            Err(error) => Err(error),
        },
    }
}

fn lockdown_result(
    device: &str,
    task: &MlxDeviceLockdown,
    result: StepResult,
) -> pb::MlxDeviceLockdownResult {
    pb::MlxDeviceLockdownResult {
        controller_state_version: task.controller_state_version.clone(),
        pci_name: device.to_string(),
        expected_device: task.expected_device.clone(),
        result: Some(result),
    }
}

/// Report a correlated failure with a fixed diagnostic. It never includes the
/// key or facts from a card that failed the identity check.
pub(super) fn lockdown_error_observation(
    device: &str,
    task: &MlxDeviceLockdown,
    diagnostic: &'static str,
) -> pb::MlxObservation {
    pb::MlxObservation {
        lockdown_result: Some(lockdown_result(
            device,
            task,
            StepResult::Error(diagnostic.to_string()),
        )),
        ..Default::default()
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use std::os::unix::process::ExitStatusExt;
    use std::path::Path;

    use carbide_test_support::{Case, Outcome, check_cases};
    use libmlx::firmware::error::{AttemptError, AttemptFailure};
    use libmlx::firmware::operation::has_unresolved_firmware_claim;
    use rpc::forge_agent_control_response as fac;

    use super::*;

    const DEVICE: &str = "0000:03:00.0";
    const KEY: &str = "0123456789abcdef";
    const EXPECTED_MAC: &str = "02:00:00:00:00:01";
    const EXPECTED_GUID: &str = "0200000300000001";

    /// A lockdown command for `DEVICE` that expects the card with `EXPECTED_MAC`.
    fn task(step: Request) -> MlxDeviceLockdown {
        task_expecting(MlxDeviceSelector::Mac(EXPECTED_MAC.parse().unwrap()), step)
    }

    /// A lockdown command for `DEVICE` that expects the card `expected` names.
    fn task_expecting(expected: MlxDeviceSelector, step: Request) -> MlxDeviceLockdown {
        MlxDeviceLockdown {
            controller_state_version: "state-version-1".into(),
            expected_device: Some(expected.into()),
            request: Some(step),
        }
    }

    /// A lock step with the fixture key.
    fn lock() -> Request {
        Request::Lock(fac::MlxDeviceLock { key: KEY.into() })
    }

    /// Discovery output for a card at `pci_name` that reports `base_mac` and no
    /// GUID.
    fn card(pci_name: &str, base_mac: Option<&str>) -> MlxDeviceInfo {
        MlxDeviceInfo {
            pci_name: pci_name.into(),
            device_type: "ConnectX7".into(),
            psid: Some("psid".into()),
            device_description: None,
            part_number: Some("part".into()),
            fw_version_current: None,
            pxe_version_current: None,
            uefi_version_current: None,
            uefi_version_virtio_blk_current: None,
            uefi_version_virtio_net_current: None,
            base_mac: base_mac.map(|mac| mac.parse().unwrap()),
            base_guid: None,
            status: None,
        }
    }

    /// Discovery output for the card `task` expects.
    fn expected_card() -> MlxDeviceInfo {
        card(DEVICE, Some(EXPECTED_MAC))
    }

    /// The expected card at `DEVICE`, reporting `base_guid` as its GUID.
    fn card_with_guid(base_guid: Option<&str>) -> MlxDeviceInfo {
        MlxDeviceInfo {
            base_guid: base_guid.map(str::to_string),
            ..expected_card()
        }
    }

    /// The claim marker's file identity, which changes if the claim is released
    /// and taken again.
    fn marker_identity(marker: &Path) -> (u64, u64, i64, i64) {
        use std::os::unix::fs::MetadataExt;

        let metadata = std::fs::metadata(marker).unwrap();
        (
            metadata.dev(),
            metadata.ino(),
            metadata.ctime(),
            metadata.ctime_nsec(),
        )
    }

    /// Supervisor state in a private directory, using the libmlx test root
    /// when CI provides one so socket and claim paths stay short.
    fn state_directory() -> tempfile::TempDir {
        use std::os::unix::fs::PermissionsExt;

        let directory = match std::env::var_os("LIBMLX_TEST_STATE_DIRECTORY") {
            Some(root) => tempfile::tempdir_in(root),
            None => tempfile::tempdir(),
        }
        .unwrap();
        // Supervisor state rejects an ancestor other users could write.
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        directory
    }

    /// Supervisor configuration whose claim marker lives under `state`.
    fn config(state: &Path) -> SupervisorConfig {
        SupervisorConfig {
            state_directory: state.join("state"),
            ..Default::default()
        }
    }

    /// A tool attempt that was killed at its deadline. Cleanup errors mean
    /// its descendants may still be running.
    fn killed_attempt(command: &str, cleanup_errors: Vec<std::io::Error>) -> AttemptError {
        AttemptError::Started {
            command: command.into(),
            status: Some(std::process::ExitStatus::from_raw(9)),
            stdout: Vec::new(),
            stderr: Vec::new(),
            reason: AttemptFailure::Deadline(LOCKDOWN_TIMEOUT),
            cleanup_errors,
        }
    }

    /// The controller state version, the exact address, the physical card, and
    /// the step are separate inputs; none can be inferred from the others.
    #[test]
    fn lockdown_requests_need_version_address_card_and_step() {
        let valid = task(Request::Status(fac::MlxDeviceLockStatus {}));
        let mut no_version = valid.clone();
        no_version.controller_state_version.clear();
        let mut no_card = valid.clone();
        no_card.expected_device = None;
        let mut placeholder_card = valid.clone();
        placeholder_card.expected_device = Some(pb::MlxDeviceSelector {
            identity: Some(pb::mlx_device_selector::Identity::BaseGuid("--".into())),
        });
        let mut no_step = valid.clone();
        no_step.request = None;

        check_cases(
            [
                Case {
                    scenario: "a result cannot bind to an absent version",
                    input: (DEVICE, no_version),
                    expect: Outcome::FailsWith(
                        "NIC lockdown request has an invalid controller state version",
                    ),
                },
                Case {
                    scenario: "an address alone does not identify the card",
                    input: (DEVICE, no_card),
                    expect: Outcome::FailsWith("NIC lockdown request has no expected device"),
                },
                Case {
                    scenario: "an absence placeholder is not a card identity",
                    input: (DEVICE, placeholder_card),
                    expect: Outcome::FailsWith(
                        "NIC lockdown request has an invalid expected device",
                    ),
                },
                Case {
                    scenario: "omitting the step is not a status read",
                    input: (DEVICE, no_step),
                    expect: Outcome::FailsWith("NIC lockdown request has no step"),
                },
                Case {
                    scenario: "a short address cannot echo the exact target",
                    input: ("03:00.0", valid),
                    expect: Outcome::FailsWith(
                        "NIC lockdown request needs a full lowercase PCI address",
                    ),
                },
            ],
            |(device, task)| checked_request(device, &task).map(|_| ()),
        );
    }

    /// Only the card at the requested address that reports the expected base
    /// MAC or GUID gets the step, and it runs under the same claim that
    /// discovery held, never one released and taken again. A card that reports
    /// no value for the selected field is refused with its own diagnostic.
    #[tokio::test]
    async fn lockdown_steps_run_only_on_the_expected_card() {
        struct CardCase {
            scenario: &'static str,
            expected: MlxDeviceSelector,
            discovered: MlxDeviceInfo,
            refusal: Option<&'static str>,
        }

        let state = state_directory();
        let config = config(state.path());
        let claim = config.state_directory.join("mutation.json");
        let by_mac = MlxDeviceSelector::Mac(EXPECTED_MAC.parse().unwrap());
        let by_guid = MlxDeviceSelector::Guid(EXPECTED_GUID.into());
        for case in [
            CardCase {
                scenario: "same part and PSID, different card",
                expected: by_mac.clone(),
                discovered: card(DEVICE, Some("02:00:00:00:00:02")),
                refusal: Some("NIC does not match the expected device"),
            },
            CardCase {
                scenario: "card reports no base MAC",
                expected: by_mac.clone(),
                discovered: card(DEVICE, None),
                refusal: Some("NIC did not report the identity the request expects"),
            },
            CardCase {
                scenario: "discovery answered for another function",
                expected: by_mac.clone(),
                discovered: card("0000:03:00.1", Some(EXPECTED_MAC)),
                refusal: Some("NIC discovery reported a different PCI address"),
            },
            CardCase {
                scenario: "expected card",
                expected: by_mac.clone(),
                discovered: expected_card(),
                refusal: None,
            },
            CardCase {
                scenario: "different base GUID",
                expected: by_guid.clone(),
                discovered: card_with_guid(Some("0200000300000002")),
                refusal: Some("NIC does not match the expected device"),
            },
            CardCase {
                scenario: "card reports no base GUID",
                expected: by_guid.clone(),
                discovered: card_with_guid(None),
                refusal: Some("NIC did not report the identity the request expects"),
            },
            CardCase {
                scenario: "expected card by GUID",
                expected: by_guid.clone(),
                discovered: card_with_guid(Some(EXPECTED_GUID)),
                refusal: None,
            },
        ] {
            let task = task_expecting(case.expected.clone(), lock());
            let discovery_claim = std::cell::Cell::new(None);
            let mut ran = false;

            let observation = handle_with(
                DEVICE,
                &task,
                &config,
                || async {
                    discovery_claim.set(Some(marker_identity(&claim)));
                    Ok(case.discovered.clone())
                },
                |step| {
                    assert_eq!(step, lock(), "{}", case.scenario);
                    assert_eq!(
                        Some(marker_identity(&claim)),
                        discovery_claim.get(),
                        "{}: the step holds the claim discovery held",
                        case.scenario
                    );
                    ran = true;
                    std::future::ready(Ok(StepResult::Changed(Changed {})))
                },
            )
            .await
            .expect("a free claim never defers");

            // A refused card's facts stay local; only the step result for the
            // expected card includes `device_info`.
            let expected = match case.refusal {
                Some(diagnostic) => lockdown_error_observation(DEVICE, &task, diagnostic),
                None => pb::MlxObservation {
                    device_info: Some(case.discovered.into()),
                    lockdown_result: Some(lockdown_result(
                        DEVICE,
                        &task,
                        StepResult::Changed(Changed {}),
                    )),
                    ..Default::default()
                },
            };
            assert_eq!(ran, case.refusal.is_none(), "{}", case.scenario);
            assert_eq!(observation, expected, "{}", case.scenario);
            assert!(!claim.exists(), "{}: the claim is released", case.scenario);
        }
    }

    /// A settled failure in discovery or in the step releases the host. A lost
    /// process result in either keeps the claim, and the next request waits
    /// instead of discovering or acting.
    #[tokio::test]
    async fn lockdown_failures_keep_the_claim_only_when_settlement_is_unknown() {
        struct SettlementCase {
            scenario: &'static str,
            fails_in_discovery: bool,
            cleanup_errors: Vec<std::io::Error>,
            diagnostic: &'static str,
            keeps_claim: bool,
        }

        let state = state_directory();
        for (index, case) in [
            SettlementCase {
                scenario: "settled discovery failure",
                fails_in_discovery: true,
                cleanup_errors: Vec::new(),
                diagnostic: "NIC discovery failed before the lockdown step",
                keeps_claim: false,
            },
            SettlementCase {
                scenario: "discovery cleanup unknown",
                fails_in_discovery: true,
                cleanup_errors: vec![std::io::Error::other("child cleanup failed")],
                diagnostic: "NIC lockdown process settlement is unknown; hardware access stays blocked",
                keeps_claim: true,
            },
            SettlementCase {
                scenario: "settled step failure",
                fails_in_discovery: false,
                cleanup_errors: Vec::new(),
                diagnostic: "NIC lockdown step failed",
                keeps_claim: false,
            },
            SettlementCase {
                scenario: "step cleanup unknown",
                fails_in_discovery: false,
                cleanup_errors: vec![std::io::Error::other("child cleanup failed")],
                diagnostic: "NIC lockdown process settlement is unknown; hardware access stays blocked",
                keeps_claim: true,
            },
        ]
        .into_iter()
        .enumerate()
        {
            let config = SupervisorConfig {
                state_directory: state.path().join(format!("case-{index}")),
                ..Default::default()
            };
            let task = task(lock());
            let (discovery_failure, step_failure) = if case.fails_in_discovery {
                let attempt = killed_attempt("mlxfwmanager", case.cleanup_errors);
                (Some(DeviceDiscoveryError::QueryAttempt(attempt)), None)
            } else {
                let attempt = killed_attempt("flint", case.cleanup_errors);
                (None, Some(MlxError::CommandAttempt(attempt.into())))
            };

            let observation = handle_with(
                DEVICE,
                &task,
                &config,
                || async move { discovery_failure.map_or_else(|| Ok(expected_card()), Err) },
                |_| async move { Err(step_failure.expect("discovery failed before the step")) },
            )
            .await
            .expect("a free claim never defers");

            assert_eq!(
                observation,
                lockdown_error_observation(DEVICE, &task, case.diagnostic),
                "{}",
                case.scenario
            );
            assert_eq!(
                has_unresolved_firmware_claim(&config).unwrap(),
                case.keeps_claim,
                "{}",
                case.scenario
            );
            if case.keeps_claim {
                let next = handle_with(
                    DEVICE,
                    &task,
                    &config,
                    || async { panic!("a held claim must block discovery") },
                    |_| async { panic!("a held claim must block the step") },
                )
                .await;
                assert_eq!(next, None, "{}", case.scenario);
            }
        }
    }

    /// Dispatch one lockdown command through Scout's production dispatcher with a
    /// valid configuration, returning the observations it published, if any.
    async fn dispatch(
        config: &SupervisorConfig,
        task: MlxDeviceLockdown,
    ) -> Option<Vec<pb::MlxObservation>> {
        let machine_id = "fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg"
            .parse()
            .unwrap();
        let mut published = None;
        crate::collect_and_publish_mlx_observations(
            &machine_id,
            vec![fac::MlxDeviceAction {
                pci_name: DEVICE.into(),
                command: Some(fac::mlx_device_action::Command::Lockdown(task)),
            }],
            Ok(config),
            |_| panic!("lockdown commands never use the legacy discovery"),
            |request| {
                published = Some(request);
                std::future::ready(Ok(pb::PublishMlxObservationReportResponse::default()))
            },
        )
        .await;
        published.map(|request| request.report.unwrap().observations)
    }

    /// The dispatcher routes the lockdown command to this module: an invalid
    /// request gets a correlated error without a claim or discovery, even while
    /// other work holds the claim, and a held claim defers a valid command
    /// without publishing anything.
    #[tokio::test]
    async fn dispatcher_answers_or_defers_lockdown_commands() {
        let state = state_directory();
        let config = config(state.path());

        let mut no_card = task(lock());
        no_card.expected_device = None;
        assert_eq!(
            dispatch(&config, no_card.clone()).await,
            Some(vec![lockdown_error_observation(
                DEVICE,
                &no_card,
                "NIC lockdown request has no expected device",
            )])
        );
        assert!(!config.state_directory.join("mutation.json").exists());

        let held = DirectMlxAccess::acquire(&config, "other-work").unwrap();
        // Validation runs before the claim, so busy hardware never delays the
        // answer to a request that could not run anyway.
        assert_eq!(
            dispatch(&config, no_card.clone()).await,
            Some(vec![lockdown_error_observation(
                DEVICE,
                &no_card,
                "NIC lockdown request has no expected device",
            )])
        );
        assert_eq!(dispatch(&config, task(lock())).await, None);
        held.finish().unwrap();
    }

    /// A completed step keeps its result when releasing the claim fails
    /// afterward, so Core never sees an error for a change that already
    /// happened and asks for it again. Removing the marker during the step
    /// makes the release fail.
    #[tokio::test]
    async fn completed_step_keeps_its_result_when_claim_release_fails() {
        let state = state_directory();
        let config = config(state.path());
        let claim = config.state_directory.join("mutation.json");
        let task = task(lock());

        let observation = handle_with(
            DEVICE,
            &task,
            &config,
            || async { Ok(expected_card()) },
            |_| {
                std::fs::remove_file(&claim).unwrap();
                std::future::ready(Ok(StepResult::Changed(Changed {})))
            },
        )
        .await
        .expect("a free claim never defers");

        assert_eq!(
            observation,
            pb::MlxObservation {
                device_info: Some(expected_card().into()),
                lockdown_result: Some(lockdown_result(
                    DEVICE,
                    &task,
                    StepResult::Changed(Changed {}),
                )),
                ..Default::default()
            }
        );
    }

    /// Status is an independent query: after a lock or unlock, including one
    /// the card already had, the read reports what `flint` says. The fake tool
    /// runs through the real bounded helper and logs each call. Discovery keeps
    /// returning the card's MAC while it is locked, as SuperNICs do; a card
    /// that stops reporting it is refused before the step instead.
    #[tokio::test]
    async fn lockdown_status_reads_back_through_bounded_tools() {
        let state = state_directory();
        let tool = state.path().join("flint");
        let script = r#"#!/bin/sh
set -eu
[ "$1" = -d ] && [ "$2" = 0000:03:00.0 ] || exit 40
case "$3" in
    hw_access)
        [ "$#" -eq 5 ] && [ "$5" = 0123456789abcdef ] || exit 41
        printf '%s\n' "$4" >> "$0.calls"
        if [ "$4" = disable ]; then
            if [ -f "$0.locked" ]; then printf 'already disabled\n'; exit 1; fi
            printf 'locked' > "$0.locked"
        elif [ "$4" = enable ]; then
            if [ ! -f "$0.locked" ]; then printf 'already enabled\n'; exit 1; fi
            /bin/rm "$0.locked"
        else exit 42; fi
        ;;
    q)
        [ "$#" -eq 3 ] || exit 43
        printf 'query\n' >> "$0.calls"
        if [ -f "$0.locked" ]; then printf 'HW access is disabled\n'; exit 1; fi
        ;;
    *) exit 44 ;;
esac
"#;
        // A child writes the executable so another test's concurrent fork
        // cannot inherit a writable descriptor and fail it with ETXTBSY.
        let writer = tokio::process::Command::new("/bin/sh")
            .args([
                "-c",
                "set -eu; umask 077; printf '%s' \"$2\" > \"$1\"; chmod 700 \"$1\"",
                "fixture-writer",
            ])
            .arg(&tool)
            .arg(script)
            .kill_on_drop(true)
            .status();
        assert!(
            tokio::time::timeout(Duration::from_secs(5), writer)
                .await
                .unwrap()
                .unwrap()
                .success()
        );
        // The normal test prerequisite builds this cross-package binary.
        let helper = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("mlx-firmware-supervisor");
        assert!(
            helper.is_file(),
            "run cargo make build-nic-firmware-test-helper before Scout unit tests"
        );
        let executor = FirmwareToolExecutor::with_supervisor_path(helper);
        let manager = LockdownManager::with_runner(FlintRunner::with_path(tool.to_string_lossy()));
        let config = config(state.path());
        let status = || Request::Status(fac::MlxDeviceLockStatus {});
        let unlock = || Request::Unlock(fac::MlxDeviceUnlock { key: KEY.into() });

        tokio::time::timeout(Duration::from_secs(20), async {
            for (step, request, expect_status) in [
                ("lock", lock(), None),
                ("already locked", lock(), None),
                ("read locked", status(), Some(pb::LockStatus::Locked)),
                ("unlock", unlock(), None),
                ("already unlocked", unlock(), None),
                ("read unlocked", status(), Some(pb::LockStatus::Unlocked)),
            ] {
                let task = task(request);

                let observation = handle_with(
                    DEVICE,
                    &task,
                    &config,
                    || async { Ok(expected_card()) },
                    |step| run_step(step, DEVICE, &manager, &executor),
                )
                .await
                .expect("a free claim never defers");

                let result = observation.lockdown_result.clone().unwrap();
                assert_eq!(
                    result.controller_state_version, task.controller_state_version,
                    "{step}"
                );
                assert_eq!(result.pci_name, DEVICE, "{step}");
                assert_eq!(result.expected_device, task.expected_device, "{step}");
                assert_eq!(
                    observation,
                    pb::MlxObservation {
                        device_info: Some(expected_card().into()),
                        lockdown_result: Some(result.clone()),
                        ..Default::default()
                    },
                    "{step}"
                );
                match (expect_status, result.result.unwrap()) {
                    (None, StepResult::Changed(_)) => {}
                    (Some(expected), StepResult::StatusReport(report)) => {
                        assert_eq!(report.device_id, DEVICE, "{step}");
                        assert_eq!(report.status, expected as i32, "{step}");
                        assert!(
                            chrono::DateTime::parse_from_rfc3339(&report.timestamp).is_ok(),
                            "{step}"
                        );
                    }
                    unexpected => panic!("{step}: unexpected result {unexpected:?}"),
                }
            }
        })
        .await
        .expect("bounded lockdown steps finished");
        assert_eq!(
            std::fs::read_to_string(tool.with_extension("calls")).unwrap(),
            "disable\ndisable\nquery\nenable\nenable\nquery\n"
        );
    }
}

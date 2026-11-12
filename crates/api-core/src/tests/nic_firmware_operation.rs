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

//! Exercise admitted NIC work through authenticated reporting, Scout delivery,
//! and persisted controller iterations. Admission itself remains disabled.

use carbide_libmlx_model::firmware::FirmwareDeliveryMode;
use carbide_libmlx_model::firmware::operation::{
    FirmwareOperation, FirmwareOperationPhase, FirmwareOperationState,
};
use carbide_libmlx_model::firmware::result::FirmwareFlashReport;
use carbide_machine_controller::metrics::MachineMetrics;
use carbide_redfish::libredfish::test_support::RedfishSimAction;
use carbide_secrets::credentials::{CredentialKey, CredentialWriter, Credentials, NicLockdownIkm};
use carbide_uuid::machine::{HostMachineId, MachineId};
use carbide_uuid::network::NetworkSegmentId;
use chrono::Utc;
use libredfish::SystemPowerControl;
use model::controller_outcome::PersistentStateHandlerOutcome;
use model::dpa_interface::{DpaInterfaceType, NewDpaInterface};
use model::machine::machine_search_config::MachineSearchConfig;
use model::machine::nic_firmware::{
    NicFirmwareActivation, NicFirmwareBaselineContinuation, NicFirmwareDeliveryErrorCode,
    NicFirmwareLockdownPolicy, NicFirmwareLockdownResult, NicFirmwareLockdownStatus,
    NicFirmwareLockdownStep, NicFirmwareOperation, NicFirmwarePhase, NicFirmwarePurpose,
    NicFirmwareTarget,
};
use model::machine::{
    CleanupContext, CleanupState, HostMachine, InstanceState, MachineLastRebootRequested,
    MachineLastRebootRequestedMode, MachineState, ManagedHostState, ResetRequest,
};
use model::nic_firmware::{NicFirmwareArtifact, NicFirmwareCardSelector};
use model::test_support::nic_firmware as fixtures;
use rpc::forge::ForgeAgentControlRequest;
use rpc::forge::forge_server::Forge;
use rpc::forge_agent_control_response as fac;
use rpc::forge_agent_control_response::mlx_device_lockdown::Request as LockdownRequest;
use rpc::protos::mlx_device as pb;
use rpc::protos::mlx_device::mlx_device_lockdown_result::Result as LockdownResult;
use rpc::protos::mlx_device::mlx_device_update_firmware_result::Result as FirmwareResult;
use state_controller::db_write_batch::DbWriteBatch;
use state_controller::state_handler::{
    StateHandler, StateHandlerContext, StateHandlerError, StateHandlerOutcome,
};
use tonic::{Code, Request};

use crate::auth::AuthContext;
use crate::tests::common::api_fixtures::instance::single_interface_network_config;
use crate::tests::common::api_fixtures::rpc_instance::RpcInstance;
use crate::tests::common::api_fixtures::{
    TestEnv, TestEnvOverrides, TestManagedHost, create_managed_host,
    create_test_env_with_overrides, get_config, remove_health_report_entry,
    send_health_report_entry,
};
use crate::tests::common::postgres::wait_for_blocked_query;
use crate::tests::common::rpc_builder::{
    InstanceAllocationRequest, InstanceConfig, InstanceConfigExt,
};

fn authenticated<T>(message: T, machine_id: MachineId) -> Request<T> {
    let mut request = Request::new(message);
    let mut auth = AuthContext::default();
    auth.principals.push(
        carbide_authn::middleware::Principal::SpiffeMachineIdentifier(machine_id.to_string()),
    );
    request.extensions_mut().insert(auth);
    request
}

// Generic delivery and persistence must operate with legacy SVPC disabled.
async fn environment(pool: sqlx::PgPool) -> TestEnv {
    let mut config = get_config();
    config.ewethers_config = None;
    create_test_env_with_overrides(
        pool,
        TestEnvOverrides {
            config: Some(config),
            ..Default::default()
        },
    )
    .await
}

// Persist a real allocation owner without advancing the unrelated DPA-enabled
// provisioning fixture; these stories enter NIC recovery with SVPC disabled.
async fn allocate_owner(
    env: &TestEnv,
    host: &TestManagedHost,
    segment: NetworkSegmentId,
) -> RpcInstance {
    let config =
        InstanceConfig::default_tenant_and_os().network(single_interface_network_config(segment));
    let instance = env
        .api
        .allocate_instance(
            InstanceAllocationRequest::builder(false)
                .machine_id(host.host().id)
                .config(config)
                .tonic_request(),
        )
        .await
        .expect("allocate lifecycle owner")
        .into_inner();
    env.one_instance(instance.id.expect("allocated owner has an ID"))
        .await
}

async fn machine(env: &TestEnv, host_id: HostMachineId) -> HostMachine {
    db::machine::find_one(&env.pool, &host_id, Default::default())
        .await
        .expect("read host")
        .expect("host exists")
}

// Read the actual work queue so unchanged state alone cannot hide a redundant
// controller wakeup from an ignored observation.
async fn queued(env: &TestEnv, host_id: HostMachineId) -> bool {
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM machine_state_controller_queued_objects WHERE object_id = $1)",
    )
    .bind(host_id.to_string())
    .fetch_one(&env.pool)
    .await
    .expect("read machine queue")
}

async fn operation(env: &TestEnv, host_id: HostMachineId) -> NicFirmwareOperation {
    let ManagedHostState::NicFirmwareUpdating { operation } =
        machine(env, host_id).await.state.value
    else {
        panic!("expected active NIC operation")
    };
    operation
}

// Tests seed the same durable state that admission will eventually create.
// The real controller and RPC handlers perform every subsequent transition.
async fn seed(env: &TestEnv, host_id: HostMachineId, operation: NicFirmwareOperation) {
    operation.validate().expect("valid admitted fixture");
    let mut txn = env.pool.begin().await.expect("begin fixture transaction");
    db::machine::update_state(
        &mut txn,
        &host_id,
        &ManagedHostState::NicFirmwareUpdating { operation },
    )
    .await
    .expect("seed admitted operation");
    txn.commit().await.expect("commit admitted operation");
}

fn observation(
    operation: &NicFirmwareOperation,
    target: &NicFirmwareTarget,
    result: FirmwareResult,
) -> pb::MlxObservation {
    pb::MlxObservation {
        firmware_update_result: Some(pb::MlxDeviceUpdateFirmwareResult {
            operation_id: operation.operation_id.clone(),
            phase_id: operation.operation_id.clone(),
            pci_name: target.pci_name.clone(),
            result: Some(result),
        }),
        ..Default::default()
    }
}

// Only requested phases have success evidence; an omitted optional phase must
// remain absent instead of asserting that an unrequested hardware step ran.
fn complete(operation: &NicFirmwareOperation, target: &NicFirmwareTarget) -> FirmwareOperation {
    FirmwareOperation {
        operation_id: operation.operation_id.clone(),
        state: FirmwareOperationState::Complete,
        phase: None,
        report: FirmwareFlashReport {
            flashed: true,
            reset: matches!(
                operation.options.activation,
                NicFirmwareActivation::CardReset { .. }
            )
            .then_some(true),
            verified_image: operation.options.verify_image.then_some(true),
            verified_version: operation.options.verify_version.then_some(true),
            expected_version: Some(target.resolution.entry.firmware.version.clone()),
            observed_version: operation
                .options
                .verify_version
                .then(|| target.resolution.entry.firmware.version.clone()),
        },
        diagnostic: None,
    }
}

fn publication(
    host_id: HostMachineId,
    observation: pb::MlxObservation,
) -> pb::PublishMlxObservationReportRequest {
    pb::PublishMlxObservationReportRequest {
        report: Some(pb::MlxObservationReport {
            machine_id: Some(host_id.into()),
            observations: vec![observation],
            ..Default::default()
        }),
    }
}

async fn publish(env: &TestEnv, host_id: HostMachineId, observation: pb::MlxObservation) {
    env.api
        .publish_mlx_observation_report(authenticated(
            publication(host_id, observation),
            host_id.into(),
        ))
        .await
        .expect("publish correlated firmware observation");
}

async fn record_cleanup_failure(env: &TestEnv, machine_id: MachineId, message: &str) {
    env.api
        .cleanup_machine_completed(Request::new(rpc::forge::MachineCleanupInfo {
            machine_id: Some(machine_id),
            nvme: Some(rpc::forge::machine_cleanup_info::CleanupStepResult {
                result: rpc::forge::machine_cleanup_info::CleanupResult::Error.into(),
                message: message.into(),
            }),
            ..Default::default()
        }))
        .await
        .expect("record the real cleanup failure");
}

async fn action(env: &TestEnv, host_id: HostMachineId) -> fac::Action {
    action_with_site_cache_support(env, host_id, false).await
}

async fn action_with_site_cache_support(
    env: &TestEnv,
    host_id: HostMachineId,
    site_cache_supported: bool,
) -> fac::Action {
    env.api
        .forge_agent_control(authenticated(
            ForgeAgentControlRequest {
                machine_id: Some(host_id.into()),
                nic_firmware_site_cache_supported: site_cache_supported,
            },
            host_id.into(),
        ))
        .await
        .expect("poll firmware action")
        .into_inner()
        .action
        .expect("action")
}

fn command(action: fac::Action) -> fac::mlx_device_action::Command {
    let fac::Action::MlxAction(mut action) = action else {
        panic!("expected MLX action")
    };
    assert_eq!(action.device_actions.len(), 1);
    action
        .device_actions
        .remove(0)
        .command
        .expect("device command")
}

fn missing(
    operation: &NicFirmwareOperation,
    target: &NicFirmwareTarget,
    instance: &str,
) -> pb::MlxObservation {
    observation(
        operation,
        target,
        FirmwareResult::MissingReceipt(pb::FirmwareOperationMissingReceipt {
            supervisor_instance_id: instance.into(),
        }),
    )
}

// Ordinary inventory confirms card identity and version without an operation
// identifier. Correlation remains a separate execution-receipt contract.
fn device(target: &NicFirmwareTarget) -> pb::MlxDeviceInfo {
    let NicFirmwareCardSelector::Mac(mac) = target.selector else {
        panic!("fixture uses MAC")
    };
    pb::MlxDeviceInfo {
        pci_name: target.pci_name.clone(),
        base_mac: mac.to_string(),
        part_number: target.resolution.entry.firmware.part_number.clone(),
        psid: target.resolution.entry.firmware.psid.clone(),
        fw_version_running: Some(target.resolution.entry.firmware.version.clone()),
        fw_version_current: target.resolution.entry.firmware.version.clone(),
        device_type: "ConnectX".into(),
        ..Default::default()
    }
}

// Echo the committed request revision exactly as Scout does. A later controller
// step must not consume a delayed reply from an earlier read or mutation.
fn lockdown_observation(
    request: &fac::MlxDeviceLockdown,
    target: &NicFirmwareTarget,
    result: LockdownResult,
) -> pb::MlxObservation {
    pb::MlxObservation {
        device_info: Some(device(target)),
        lockdown_result: Some(pb::MlxDeviceLockdownResult {
            controller_state_version: request.controller_state_version.clone(),
            pci_name: target.pci_name.clone(),
            expected_device: request.expected_device.clone(),
            result: Some(result),
        }),
        ..Default::default()
    }
}

fn status(target: &NicFirmwareTarget, locked: bool) -> LockdownResult {
    LockdownResult::StatusReport(pb::StatusReport {
        device_id: target.pci_name.clone(),
        status: if locked {
            pb::LockStatus::Locked
        } else {
            pb::LockStatus::Unlocked
        }
        .into(),
        timestamp: Utc::now().to_rfc3339(),
    })
}

async fn lockdown_action(env: &TestEnv, host_id: HostMachineId) -> fac::MlxDeviceLockdown {
    let fac::mlx_device_action::Command::Lockdown(request) = command(action(env, host_id).await)
    else {
        panic!("expected lockdown reconciliation")
    };
    request
}

// Poll and publication may use a different Core replica from the controller;
// the database, not a registered process-local stream, connects those steps.
async fn report_lockdown(
    env: &TestEnv,
    host_id: HostMachineId,
    target: &NicFirmwareTarget,
    result: LockdownResult,
) -> fac::MlxDeviceLockdown {
    let request = lockdown_action(env, host_id).await;
    publish(env, host_id, lockdown_observation(&request, target, result)).await;
    request
}

async fn inventory(
    env: &TestEnv,
    host_id: HostMachineId,
    devices: Vec<pb::MlxDeviceInfo>,
) -> chrono::DateTime<Utc> {
    let observed_at = Utc::now();
    env.api
        .publish_mlx_device_report(authenticated(
            pb::PublishMlxDeviceReportRequest {
                report: Some(pb::MlxDeviceReport {
                    machine_id: Some(host_id.into()),
                    timestamp: Some(observed_at.into()),
                    devices,
                    ..Default::default()
                }),
            },
            host_id.into(),
        ))
        .await
        .expect("publish ordinary inventory");
    observed_at
}

#[crate::sqlx_test]
async fn site_cache_delivery_requires_support_and_preserves_recovery(pool: sqlx::PgPool) {
    let env = environment(pool).await;
    let host = create_managed_host(&env).await;
    let host_id = *host.id;
    let mut admitted = fixtures::operation();
    admitted.delivery_mode = FirmwareDeliveryMode::SiteCache;

    for phase in [
        NicFirmwarePhase::ObservingSupervisor { target_index: 0 },
        NicFirmwarePhase::Unlocking { target_index: 0 },
        NicFirmwarePhase::UpdatingFirmware { target_index: 0 },
    ] {
        admitted.phase = phase;
        admitted.targets[0].supervisor_instance_id =
            (!matches!(admitted.phase, NicFirmwarePhase::ObservingSupervisor { .. }))
                .then(|| "helper".into());
        seed(&env, host_id, admitted.clone()).await;
        let error = env
            .api
            .forge_agent_control(authenticated(
                ForgeAgentControlRequest {
                    machine_id: Some(host_id.into()),
                    nic_firmware_site_cache_supported: false,
                },
                host_id.into(),
            ))
            .await
            .expect_err("unsupported Scout cannot receive SiteCache work");
        assert_eq!(error.code(), Code::FailedPrecondition);
        assert!(error.message().contains("SiteCache"));
        assert_eq!(operation(&env, host_id).await, admitted);

        let resumed = command(action_with_site_cache_support(&env, host_id, true).await);
        assert_eq!(operation(&env, host_id).await, admitted);
        match (admitted.phase.clone(), resumed) {
            (
                NicFirmwarePhase::ObservingSupervisor { .. },
                fac::mlx_device_action::Command::ObserveFirmwareUpdate(observe),
            ) => {
                assert_eq!(observe.operation_id, admitted.operation_id);
                assert_eq!(
                    observe.delivery_mode,
                    i32::from(pb::FirmwareDeliveryMode::SiteCache)
                );
                // No firmware was submitted by the rejected poll. A capable
                // Scout can still establish the helper and authorize unlocking.
                publish(
                    &env,
                    host_id,
                    missing(&admitted, &admitted.targets[0], "helper"),
                )
                .await;
                env.run_machine_state_controller_iteration_without_requeue()
                    .await;
                let mut expected = admitted.clone();
                expected.targets[0].supervisor_instance_id = Some("helper".into());
                expected.phase = NicFirmwarePhase::Unlocking { target_index: 0 };
                assert_eq!(operation(&env, host_id).await, expected);
            }
            (
                NicFirmwarePhase::Unlocking { .. },
                fac::mlx_device_action::Command::Lockdown(lockdown),
            ) => {
                assert_eq!(
                    lockdown.expected_device,
                    Some(admitted.targets[0].selector.clone().into())
                );
                assert!(matches!(lockdown.request, Some(LockdownRequest::Status(_))));
            }
            (
                NicFirmwarePhase::UpdatingFirmware { .. },
                fac::mlx_device_action::Command::UpdateFirmware(update),
            ) => {
                assert_eq!(update.operation_id, admitted.operation_id);
                assert_eq!(
                    update.supervisor_instance_id,
                    admitted.targets[0].supervisor_instance_id
                );
                assert_eq!(
                    update.delivery_mode,
                    i32::from(pb::FirmwareDeliveryMode::SiteCache)
                );
            }
            (phase, command) => panic!("unexpected resumed command for {phase:?}: {command:?}"),
        }
    }

    admitted.targets[0].receipt = Some(complete(&admitted, &admitted.targets[0]));
    admitted.phase = NicFirmwarePhase::RestoringLockdown {
        target_index: 0,
        failure: None,
    };
    seed(&env, host_id, admitted.clone()).await;
    assert!(matches!(
        command(action(&env, host_id).await),
        fac::mlx_device_action::Command::Lockdown(_)
    ));
    assert_eq!(operation(&env, host_id).await, admitted);

    admitted.phase = NicFirmwarePhase::Reobserving {
        not_before: Utc::now(),
    };
    seed(&env, host_id, admitted.clone()).await;
    assert!(matches!(
        action(&env, host_id).await,
        fac::Action::MlxDeviceInfoReport(_)
    ));
    assert_eq!(operation(&env, host_id).await, admitted);
}

#[crate::sqlx_test]
async fn site_cache_tasks_keep_the_saved_mode_and_original_sources(pool: sqlx::PgPool) {
    let env = environment(pool).await;
    let host = create_managed_host(&env).await;
    let host_id = *host.id;
    let mut admitted = fixtures::operation();
    admitted.delivery_mode = FirmwareDeliveryMode::SiteCache;
    admitted.targets[0].supervisor_instance_id = Some("helper".into());
    admitted.targets[0].resolution.entry.device_config = Some(NicFirmwareArtifact {
        url: "https://firmware.example.test/device.conf".parse().unwrap(),
        sha256: "cd".repeat(32),
    });
    seed(&env, host_id, admitted.clone()).await;
    let fac::mlx_device_action::Command::ObserveFirmwareUpdate(observe) =
        command(action_with_site_cache_support(&env, host_id, true).await)
    else {
        panic!("supported Scout receives checked SiteCache observation")
    };
    admitted.phase = NicFirmwarePhase::UpdatingFirmware { target_index: 0 };
    seed(&env, host_id, admitted.clone()).await;
    let fac::mlx_device_action::Command::UpdateFirmware(update) =
        command(action_with_site_cache_support(&env, host_id, true).await)
    else {
        panic!("supported Scout receives frozen SiteCache update")
    };
    assert_eq!(
        update.delivery_mode,
        i32::from(pb::FirmwareDeliveryMode::SiteCache)
    );
    assert_eq!(observe.delivery_mode, update.delivery_mode);
    assert_eq!(observe.profile, update.profile);
    assert_eq!(observe.expected_digests, update.expected_digests);
    let flash = update.profile.unwrap().flash_spec.unwrap();
    assert_eq!(
        flash.firmware_url,
        admitted.targets[0].resolution.entry.image.url.as_str()
    );
    assert_eq!(
        flash.device_conf_url.as_deref(),
        Some("https://firmware.example.test/device.conf")
    );
    assert_eq!(operation(&env, host_id).await, admitted);
}

// Drive authenticated reports into actual control delivery with DPA disabled.
// Submission permission must come from saved evidence and survive helper loss.
#[crate::sqlx_test]
async fn generic_delivery_freezes_helper_before_submission_and_never_replaces_it(
    pool: sqlx::PgPool,
) {
    let env = environment(pool).await;
    let host = create_managed_host(&env).await;
    let host_id = *host.id;
    let mut initial = fixtures::operation();
    initial.options.verify_image = true;
    initial.options.verify_version = true;
    initial.options.activation = NicFirmwareActivation::CardReset {
        reset_level: 4,
        post_reset_verify_retry_interval_seconds: Some(17),
        post_reset_verify_timeout_seconds: Some(241),
    };
    initial.targets[0].resolution.entry.device_config = Some(NicFirmwareArtifact {
        url: "https://firmware.example.test/device.conf".parse().unwrap(),
        sha256: "cd".repeat(32),
    });
    let target = &initial.targets[0];
    seed(&env, host_id, initial.clone()).await;
    assert!(!env.api.runtime_config.is_svpc_enabled());

    // A permanent delivery failure owns this boundary before either the
    // no-helper wait or a saved-helper transition could authorize unlocking.
    for saved_helper in [None, Some("first-helper")] {
        let mut blocked = initial.clone();
        blocked.targets[0].supervisor_instance_id = saved_helper.map(String::from);
        seed(&env, host_id, blocked.clone()).await;
        publish(
            &env,
            host_id,
            observation(
                &blocked,
                &blocked.targets[0],
                FirmwareResult::Error(pb::MlxDeviceUpdateFirmwareError {
                    code: pb::MlxDeviceUpdateFirmwareErrorCode::Unsupported.into(),
                    diagnostic: "the frozen workflow is unsupported".into(),
                }),
            ),
        )
        .await;
        env.run_machine_state_controller_iteration_without_requeue()
            .await;
        let retained = operation(&env, host_id).await;
        assert_eq!(retained.phase, initial.phase);
        assert_eq!(
            retained.targets[0].supervisor_instance_id.as_deref(),
            saved_helper
        );
        assert_eq!(
            retained.targets[0].delivery_error_code,
            Some(NicFirmwareDeliveryErrorCode::Unsupported)
        );
        assert!(matches!(
            machine(&env, host_id).await.controller_state_outcome,
            Some(PersistentStateHandlerOutcome::Error { .. })
        ));
        assert!(matches!(
            command(action(&env, host_id).await),
            fac::mlx_device_action::Command::ObserveFirmwareUpdate(_)
        ));
    }
    seed(&env, host_id, initial.clone()).await;

    let fac::mlx_device_action::Command::ObserveFirmwareUpdate(observe) =
        command(action(&env, host_id).await)
    else {
        panic!("expected checked observation")
    };

    sqlx::query("DELETE FROM machine_state_controller_queued_objects WHERE object_id = $1")
        .bind(host_id.to_string())
        .execute(&env.pool)
        .await
        .unwrap();
    assert!(!queued(&env, host_id).await);
    publish(&env, host_id, missing(&initial, target, "first-helper")).await;
    assert!(queued(&env, host_id).await);
    let observed = operation(&env, host_id).await;
    assert_eq!(
        observed.targets[0].supervisor_instance_id.as_deref(),
        Some("first-helper")
    );
    assert_eq!(observed.phase, initial.phase);
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    assert_eq!(
        operation(&env, host_id).await.phase,
        NicFirmwarePhase::Unlocking { target_index: 0 }
    );

    report_lockdown(&env, host_id, target, status(target, false)).await;
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    assert_eq!(
        operation(&env, host_id).await.phase,
        NicFirmwarePhase::UpdatingFirmware { target_index: 0 }
    );
    let submitted_action = action(&env, host_id).await;
    let fac::Action::MlxAction(ref mlx) = submitted_action else {
        panic!("expected MLX delivery")
    };
    assert_eq!(mlx.device_actions[0].pci_name, "0000:01:00.0");
    let fac::mlx_device_action::Command::UpdateFirmware(submit) = command(submitted_action) else {
        panic!("expected guarded update")
    };
    // Assert the complete public payload, including nondefault options. This
    // catches a dropped URL, digest, timing choice or selector at Core delivery.
    let expected_profile = pb::FirmwareFlasherProfile {
        firmware_spec: Some(pb::FirmwareSpec {
            part_number: "test-card".into(),
            psid: "test-psid".into(),
            version: "1.2.3".into(),
        }),
        flash_spec: Some(pb::FlashSpec {
            firmware_url: "https://firmware.example.test/image.bin".into(),
            device_conf_url: Some("https://firmware.example.test/device.conf".into()),
            ..Default::default()
        }),
        flash_options: Some(pb::FlashOptions {
            reset: true,
            verify_image: true,
            verify_version: true,
            reset_level: 4,
            post_reset_verify_retry_interval_seconds: Some(17),
            post_reset_verify_timeout_seconds: Some(241),
        }),
    };
    assert_eq!(
        submit,
        fac::MlxDeviceUpdateFirmware {
            operation_id: initial.operation_id.clone(),
            phase_id: initial.operation_id.clone(),
            supervisor_instance_id: Some("first-helper".into()),
            profile: Some(expected_profile),
            expected_digests: Some(pb::FirmwareArtifactDigests {
                image_sha256: "ab".repeat(32),
                configuration_sha256: Some("cd".repeat(32)),
            }),
            delivery_mode: pb::FirmwareDeliveryMode::Direct.into(),
            expected_device: Some(pb::MlxDeviceSelector {
                identity: Some(pb::mlx_device_selector::Identity::BaseMac(
                    "02:00:00:00:00:01".into()
                )),
            }),
        }
    );
    assert_eq!(observe.operation_id, submit.operation_id);
    assert_eq!(observe.phase_id, submit.phase_id);
    assert_eq!(observe.profile, submit.profile);
    assert_eq!(observe.expected_digests, submit.expected_digests);
    assert_eq!(observe.expected_device, submit.expected_device);
    assert_eq!(observe.delivery_mode, submit.delivery_mode);

    // Delivery failures stay visible independently of receipt availability.
    // Unavailable remains retryable under the same frozen helper and recipe.
    let delivery_error = observation(
        &initial,
        target,
        FirmwareResult::Error(pb::MlxDeviceUpdateFirmwareError {
            code: pb::MlxDeviceUpdateFirmwareErrorCode::Unavailable.into(),
            diagnostic: "helper connection interrupted".into(),
        }),
    );
    publish(&env, host_id, delivery_error).await;
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    let failed_delivery = operation(&env, host_id).await;
    assert_eq!(
        failed_delivery.phase,
        NicFirmwarePhase::UpdatingFirmware { target_index: 0 }
    );
    assert!(failed_delivery.targets[0].receipt.is_none());
    assert_eq!(
        failed_delivery.targets[0].delivery_error_code,
        Some(NicFirmwareDeliveryErrorCode::Unavailable)
    );
    assert!(
        matches!(machine(&env, host_id).await.controller_state_outcome,
        Some(PersistentStateHandlerOutcome::Wait { reason, .. }) if reason.contains("helper connection interrupted"))
    );
    assert_eq!(
        failed_delivery.targets[0].supervisor_instance_id.as_deref(),
        Some("first-helper")
    );
    assert!(
        failed_delivery.targets[0]
            .diagnostic
            .as_deref()
            .unwrap()
            .contains("helper connection interrupted")
    );

    let fac::mlx_device_action::Command::UpdateFirmware(retry) =
        command(action(&env, host_id).await)
    else {
        panic!("unavailable delivery must permit retry of the same request")
    };
    assert_eq!(retry, submit);

    // A replacement cannot settle already-authorized work. Preserve the
    // original helper, expose intervention and observe without another submit.
    publish(
        &env,
        host_id,
        missing(&initial, target, "replacement-helper"),
    )
    .await;
    let uncertain = operation(&env, host_id).await;
    assert_eq!(
        uncertain.targets[0].supervisor_instance_id.as_deref(),
        Some("first-helper")
    );
    assert!(
        uncertain.targets[0]
            .diagnostic
            .as_deref()
            .is_some_and(|diagnostic| diagnostic.contains("instance changed"))
    );
    assert_eq!(
        uncertain.targets[0].delivery_error_code,
        Some(NicFirmwareDeliveryErrorCode::Conflict)
    );
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    assert!(matches!(
        machine(&env, host_id).await.controller_state_outcome,
        Some(PersistentStateHandlerOutcome::Error { .. })
    ));
    assert!(matches!(
        command(action(&env, host_id).await),
        fac::mlx_device_action::Command::ObserveFirmwareUpdate(_)
    ));
    // An explicit conflict keeps submission excluded under that same helper.
    publish(
        &env,
        host_id,
        observation(
            &initial,
            target,
            FirmwareResult::Error(pb::MlxDeviceUpdateFirmwareError {
                code: pb::MlxDeviceUpdateFirmwareErrorCode::Conflict.into(),
                diagnostic: "helper connection interrupted".into(),
            }),
        ),
    )
    .await;
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    let intervention = operation(&env, host_id).await;
    assert_eq!(
        intervention.phase,
        NicFirmwarePhase::UpdatingFirmware { target_index: 0 }
    );
    assert_eq!(
        intervention.targets[0].delivery_error_code,
        Some(NicFirmwareDeliveryErrorCode::Conflict)
    );
    assert_eq!(
        intervention.targets[0].supervisor_instance_id.as_deref(),
        Some("first-helper")
    );
    assert!(intervention.targets[0].receipt.is_none());
    assert!(
        matches!(machine(&env, host_id).await.controller_state_outcome,
        Some(PersistentStateHandlerOutcome::Error { err, .. }) if err.contains("helper connection interrupted"))
    );
    assert!(matches!(
        command(action(&env, host_id).await),
        fac::mlx_device_action::Command::ObserveFirmwareUpdate(_)
    ));

    let before_replacement = machine(&env, host_id).await;
    publish(
        &env,
        host_id,
        observation(
            &initial,
            target,
            FirmwareResult::Error(pb::MlxDeviceUpdateFirmwareError {
                code: pb::MlxDeviceUpdateFirmwareErrorCode::Unavailable.into(),
                diagnostic: "a later lookup is unavailable".into(),
            }),
        ),
    )
    .await;
    publish(
        &env,
        host_id,
        missing(&initial, target, "replacement-helper"),
    )
    .await;
    let after_replacement = machine(&env, host_id).await;
    assert_eq!(
        after_replacement.state.value,
        before_replacement.state.value
    );
    assert_eq!(
        after_replacement.state.version,
        before_replacement.state.version
    );
    assert!(matches!(
        command(action(&env, host_id).await),
        fac::mlx_device_action::Command::ObserveFirmwareUpdate(_)
    ));

    // An uncertain terminal receipt found on the first read is a visible
    // intervention boundary, without first committing an execution transition.
    seed(&env, host_id, initial.clone()).await;
    let mut unresolved = complete(&initial, target);
    unresolved.state = FirmwareOperationState::ManualIntervention;
    unresolved.report = FirmwareFlashReport::default();
    unresolved.diagnostic = Some("execution effects are unresolved".into());
    publish(
        &env,
        host_id,
        observation(
            &initial,
            target,
            FirmwareResult::Receipt(unresolved.clone().into()),
        ),
    )
    .await;
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    let retained = operation(&env, host_id).await;
    assert_eq!(retained.phase, initial.phase);
    assert_eq!(retained.targets[0].receipt, Some(unresolved));
    assert!(matches!(
        machine(&env, host_id).await.controller_state_outcome,
        Some(PersistentStateHandlerOutcome::Error { .. })
    ));
    assert!(matches!(
        command(action(&env, host_id).await),
        fac::mlx_device_action::Command::ObserveFirmwareUpdate(_)
    ));

    // Core can encounter a receipt on its first observation, before it saved a
    // helper token. That running operation may hold the card claim, so recovery
    // must skip lockdown changes and continue observing without submission.
    seed(&env, host_id, initial.clone()).await;
    let mut existing = complete(&initial, target);
    existing.state = FirmwareOperationState::Running;
    existing.phase = Some(FirmwareOperationPhase::Prepare);
    existing.report = FirmwareFlashReport::default();
    publish(
        &env,
        host_id,
        observation(
            &initial,
            target,
            FirmwareResult::Receipt(existing.clone().into()),
        ),
    )
    .await;
    assert_eq!(operation(&env, host_id).await.phase, initial.phase);
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    let resumed = operation(&env, host_id).await;
    assert_eq!(
        resumed.phase,
        NicFirmwarePhase::UpdatingFirmware { target_index: 0 }
    );
    assert_eq!(resumed.targets[0].receipt.as_ref(), Some(&existing));
    assert!(resumed.targets[0].supervisor_instance_id.is_none());
    assert!(matches!(
        command(action(&env, host_id).await),
        fac::mlx_device_action::Command::ObserveFirmwareUpdate(_)
    ));
    publish(
        &env,
        host_id,
        missing(&initial, target, "replacement-without-journal"),
    )
    .await;
    let missing_after_receipt = operation(&env, host_id).await;
    assert_eq!(
        missing_after_receipt.targets[0].receipt.as_ref(),
        Some(&existing)
    );
    assert!(
        missing_after_receipt.targets[0]
            .supervisor_instance_id
            .is_none()
    );
    assert!(matches!(
        command(action(&env, host_id).await),
        fac::mlx_device_action::Command::ObserveFirmwareUpdate(_)
    ));

    // A retained Running receipt must not hide the missing-journal diagnostic
    // from the persisted controller outcome that operators inspect.
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    assert_eq!(operation(&env, host_id).await, missing_after_receipt);
    let diagnostic = missing_after_receipt.targets[0]
        .diagnostic
        .as_deref()
        .expect("missing receipt diagnostic");
    assert!(
        matches!(machine(&env, host_id).await.controller_state_outcome,
        Some(PersistentStateHandlerOutcome::Wait { reason, .. })
            if reason.contains("Running") && reason.contains(diagnostic))
    );
}

// A report that loses its state CAS must be retried from fresh state. The retry
// must commit its receipt before waking a controller that will read that state.
#[crate::sqlx_test]
async fn retried_firmware_report_commits_before_queueing(pool: sqlx::PgPool) {
    let env = environment(pool).await;
    let host = create_managed_host(&env).await;
    let host_id = *host.id;
    let mut admitted = fixtures::operation();
    admitted.phase = NicFirmwarePhase::UpdatingFirmware { target_index: 0 };
    admitted.targets[0].supervisor_instance_id = Some("observed-helper".into());
    seed(&env, host_id, admitted.clone()).await;
    sqlx::query("DELETE FROM machine_state_controller_queued_objects WHERE object_id = $1")
        .bind(host_id.to_string())
        .execute(&env.pool)
        .await
        .expect("clear this host's queued work");
    let before = machine(&env, host_id).await;
    let receipt = complete(&admitted, &admitted.targets[0]);
    let report = publication(
        host_id,
        observation(
            &admitted,
            &admitted.targets[0],
            FirmwareResult::Receipt(receipt.clone().into()),
        ),
    );

    // Leave a competing revision uncommitted so ingestion reads the old one,
    // then wait until its state-history insert reaches the held lock.
    let mut newer = admitted.clone();
    newer.targets[0].diagnostic = Some("newer controller diagnostic".into());
    let winner_version = before.state.version.increment();
    let mut winner = env.pool.begin().await.unwrap();
    let winner_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(winner.as_mut())
        .await
        .unwrap();
    assert_eq!(
        db::machine::try_update_controller_state(
            &mut winner,
            &host_id,
            before.state.version,
            winner_version,
            &ManagedHostState::NicFirmwareUpdating {
                operation: newer.clone(),
            },
        )
        .await
        .unwrap(),
        db::ConditionalWrite::Applied(())
    );
    let stale_report = env
        .api
        .publish_mlx_observation_report(authenticated(report.clone(), host_id.into()));
    let commit_winner = async {
        wait_for_blocked_query(&env.pool, winner_pid, "INSERT INTO machine_state_history").await;
        assert_eq!(operation(&env, host_id).await, admitted);
        assert!(!queued(&env, host_id).await);
        winner.commit().await.unwrap();
    };
    let (rejected, ()) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::join!(stale_report, commit_winner)
    })
    .await
    .expect("competing report and state commit finish");
    let error = rejected.expect_err("stale report must be retried");
    assert_eq!(error.code(), Code::FailedPrecondition);
    assert!(error.message().contains("machine changed"));
    assert_eq!(operation(&env, host_id).await, newer);
    assert_eq!(machine(&env, host_id).await.state.version, winner_version);
    assert!(!queued(&env, host_id).await);

    // This fixture has its own database. Pause only its queue insert so an
    // independent connection can prove the retry's receipt already committed.
    let mut queue_gate = env.pool.begin().await.unwrap();
    let queue_gate_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(queue_gate.as_mut())
        .await
        .unwrap();
    sqlx::query("LOCK TABLE machine_state_controller_queued_objects IN SHARE MODE")
        .execute(queue_gate.as_mut())
        .await
        .unwrap();
    let mut accepted = newer;
    accepted.targets[0].receipt = Some(receipt);
    accepted.targets[0].diagnostic = None;
    let retry = env
        .api
        .publish_mlx_observation_report(authenticated(report, host_id.into()));
    let observe_commit = async {
        wait_for_blocked_query(
            &env.pool,
            queue_gate_pid,
            "INSERT INTO machine_state_controller_queued_objects",
        )
        .await;
        assert_eq!(operation(&env, host_id).await, accepted);
        assert_eq!(
            machine(&env, host_id).await.state.version.version_nr(),
            winner_version.version_nr() + 1
        );
        assert!(!queued(&env, host_id).await);
        queue_gate.commit().await.unwrap();
    };
    let (retried, ()) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::join!(retry, observe_commit)
    })
    .await
    .expect("committed report and queued wakeup finish");
    retried.expect("retry stores the receipt");
    assert_eq!(operation(&env, host_id).await, accepted);
    assert!(queued(&env, host_id).await);
}

// These guards must apply at authenticated ingress before changing the saved
// operation, not merely when constructing or validating local model values.
#[crate::sqlx_test]
async fn generic_reports_reject_foreign_or_contradictory_evidence_without_state_changes(
    pool: sqlx::PgPool,
) {
    let env = environment(pool).await;
    let host = create_managed_host(&env).await;
    let host_id = *host.id;
    let mut admitted = fixtures::operation();
    admitted.phase = NicFirmwarePhase::UpdatingFirmware { target_index: 0 };
    admitted.targets[0].supervisor_instance_id = Some("first-helper".into());
    seed(&env, host_id, admitted.clone()).await;
    let target = &admitted.targets[0];
    let valid = observation(
        &admitted,
        target,
        FirmwareResult::Receipt(complete(&admitted, target).into()),
    );

    struct Case {
        name: &'static str,
        change: fn(&mut Vec<pb::MlxObservation>),
        authenticated_as_host: bool,
        expected_error: Option<Code>,
    }
    let cases = [
        // A valid but delayed phase is ignored, not turned into newer evidence.
        Case {
            name: "stale phase",
            change: |reports| {
                reports[0].firmware_update_result.as_mut().unwrap().phase_id =
                    "earlier-phase".into();
            },
            authenticated_as_host: true,
            expected_error: None,
        },
        // The operation token cannot override contradictory device identity.
        Case {
            name: "wrong card",
            change: |reports| {
                // The valid first row must not commit when a later report
                // contradicts the same request's physical identity.
                let mut contradictory = reports[0].clone();
                contradictory.device_info = Some(pb::MlxDeviceInfo {
                    pci_name: "0000:01:00.0".into(),
                    base_mac: "02:00:00:00:00:ff".into(),
                    ..Default::default()
                });
                reports.push(contradictory);
            },
            authenticated_as_host: true,
            expected_error: Some(Code::InvalidArgument),
        },
        // A DPU principal cannot publish a receipt on behalf of its host.
        Case {
            name: "different authenticated machine",
            change: |_| {},
            authenticated_as_host: false,
            expected_error: Some(Code::PermissionDenied),
        },
    ];
    let before = machine(&env, host_id).await;
    sqlx::query("DELETE FROM machine_state_controller_queued_objects WHERE object_id = $1")
        .bind(host_id.to_string())
        .execute(&env.pool)
        .await
        .unwrap();
    for case in cases {
        let mut request = publication(host_id, valid.clone());
        (case.change)(&mut request.report.as_mut().unwrap().observations);
        let source = if case.authenticated_as_host {
            host_id.into()
        } else {
            host.dpu().id.into()
        };
        let result = env
            .api
            .publish_mlx_observation_report(authenticated(request, source))
            .await;
        assert_eq!(
            result.err().map(|error| error.code()),
            case.expected_error,
            "{}",
            case.name
        );
        let after = machine(&env, host_id).await;
        assert_eq!(after.state.value, before.state.value, "{}", case.name);
        assert_eq!(after.state.version, before.state.version, "{}", case.name);
        assert!(!queued(&env, host_id).await, "{}", case.name);
    }
    let error = env
        .api
        .publish_mlx_observation_report(Request::new(publication(host_id, valid)))
        .await
        .expect_err("generic evidence needs an authenticated host");
    assert_eq!(error.code(), Code::PermissionDenied);
    let after = machine(&env, host_id).await;
    assert_eq!(after.state.value, before.state.value);
    assert_eq!(after.state.version, before.state.version);

    // Polling is an admission boundary too: neither an unauthenticated caller
    // nor the attached DPU may obtain the host's guarded firmware command.
    for principal in [None, Some(host.dpu().id.into())] {
        let poll = ForgeAgentControlRequest {
            machine_id: Some(host_id.into()),
            nic_firmware_site_cache_supported: false,
        };
        let request = principal.map_or_else(
            || Request::new(poll.clone()),
            |id| authenticated(poll.clone(), id),
        );
        assert_eq!(
            env.api
                .forge_agent_control(request)
                .await
                .unwrap_err()
                .code(),
            Code::PermissionDenied
        );
        let after_poll = machine(&env, host_id).await;
        assert_eq!(after_poll.state.value, before.state.value);
        assert_eq!(after_poll.state.version, before.state.version);
    }
}

// Process cards in order, then require fresh inventory and independent
// lockdown readback before the controller can resume Ready.
#[crate::sqlx_test]
async fn cards_converge_sequentially_after_inventory_retry_and_lockdown_readback(
    pool: sqlx::PgPool,
) {
    let env = environment(pool.clone()).await;
    let replica =
        create_test_env_with_overrides(pool, TestEnvOverrides::no_network_segments()).await;
    let host = create_managed_host(&env).await;
    let host_id = *host.id;
    let mut admitted = fixtures::operation();
    admitted.options.verify_version = true;
    admitted.options.activation = NicFirmwareActivation::CardReset {
        reset_level: 3,
        post_reset_verify_retry_interval_seconds: None,
        post_reset_verify_timeout_seconds: None,
    };
    let mut second = admitted.targets[0].clone();
    second.selector = NicFirmwareCardSelector::Mac("02:00:00:00:00:02".parse().unwrap());
    second.pci_name = "0000:02:00.0".into();
    second.supervisor_instance_id = None;
    admitted.targets.push(second);
    let NicFirmwareCardSelector::Mac(mac_address) = admitted.targets[0].selector else {
        unreachable!()
    };
    admitted.targets[0].original_lockdown = NicFirmwareLockdownPolicy::Locked {
        ikm_version: 7,
        mac_address,
    };
    assert!(replica.api.runtime_config.is_svpc_enabled());
    for core in [&env, &replica] {
        core.test_credential_manager
            .set_credentials(
                &CredentialKey::NicLockdownIkm {
                    credential_type: NicLockdownIkm::SiteWide { version: 7 },
                },
                &Credentials::UsernamePassword {
                    username: String::new(),
                    password: "test-ikm".into(),
                },
            )
            .await
            .unwrap();
    }
    let lock_key = crate::dpa::lockdown::build_resolved_lockdown_key(
        host_id,
        mac_address,
        &*env.test_credential_manager,
        7,
    )
    .await
    .unwrap();
    seed(&env, host_id, admitted.clone()).await;
    let power_before = env.redfish_sim.timepoint();

    // Start with the first card, including a failed unlock. A command failure
    // may have changed hardware, so the next committed step is another read.
    // A mixed batch has two independent destinations: generic progress stays
    // on the machine, while the legacy profile observation stays on its DPA.
    let mut txn = env.pool.begin().await.unwrap();
    let legacy = db::dpa_interface::persist(
        NewDpaInterface {
            machine_id: host_id,
            mac_address: "02:00:00:00:00:99".parse().unwrap(),
            device_type: "SuperNIC".into(),
            pci_name: "0000:99:00.0".into(),
            device_description: None,
            interface_type: DpaInterfaceType::Svpc,
        },
        &mut txn,
    )
    .await
    .unwrap();
    txn.commit().await.unwrap();
    let legacy_observation = pb::MlxObservation {
        device_info: Some(pb::MlxDeviceInfo {
            base_mac: legacy.mac_address.to_string(),
            pci_name: legacy.pci_name.clone(),
            ..Default::default()
        }),
        profile_name: Some("legacy-profile".into()),
        ..Default::default()
    };
    replica
        .api
        .publish_mlx_observation_report(authenticated(
            pb::PublishMlxObservationReportRequest {
                report: Some(pb::MlxObservationReport {
                    machine_id: Some(host_id.into()),
                    observations: vec![
                        missing(&admitted, &admitted.targets[0], "helper"),
                        legacy_observation,
                    ],
                    ..Default::default()
                }),
            },
            host_id.into(),
        ))
        .await
        .unwrap();
    let cards = db::dpa_interface::find_by_machine_id(&env.pool, host_id, Default::default())
        .await
        .unwrap();
    assert_eq!(cards.len(), 1);
    assert_eq!(cards[0].id, legacy.id);
    let legacy_state = cards[0].card_state.as_ref().unwrap();
    assert_eq!(legacy_state.profile.as_deref(), Some("legacy-profile"));
    assert!(legacy_state.firmware_operation.is_none());
    assert_eq!(
        operation(&env, host_id).await.targets[0]
            .supervisor_instance_id
            .as_deref(),
        Some("helper")
    );
    env.run_machine_state_controller_iteration_without_requeue()
        .await;

    // A mutation failure may have changed hardware. Its next committed step
    // must read status again under a new controller revision.
    let stale_read = lockdown_action(&replica, host_id).await;
    assert!(matches!(
        &stale_read.request,
        Some(LockdownRequest::Status(_))
    ));
    publish(
        &replica,
        host_id,
        lockdown_observation(
            &stale_read,
            &admitted.targets[0],
            status(&admitted.targets[0], true),
        ),
    )
    .await;
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    assert_eq!(
        operation(&env, host_id).await.lockdown.step,
        NicFirmwareLockdownStep::Change
    );
    let unlock = lockdown_action(&replica, host_id).await;
    assert_eq!(
        unlock.request,
        Some(LockdownRequest::Unlock(fac::MlxDeviceUnlock {
            key: lock_key.clone(),
        }))
    );
    publish(
        &replica,
        host_id,
        lockdown_observation(
            &unlock,
            &admitted.targets[0],
            LockdownResult::Error("unlock completion uncertain".into()),
        ),
    )
    .await;
    assert_eq!(action(&replica, host_id).await, fac::Action::noop());
    assert_eq!(
        operation(&env, host_id).await.lockdown.result,
        Some(NicFirmwareLockdownResult::Error {
            diagnostic: "unlock completion uncertain".into(),
        })
    );
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    let checking_unlock = machine(&env, host_id).await;
    assert_eq!(
        operation(&env, host_id).await.lockdown.step,
        NicFirmwareLockdownStep::Observe
    );
    assert!(operation(&env, host_id).await.lockdown.result.is_none());

    // This pre-mutation status has the right device but the wrong revision.
    // Accepting it would bypass the independent read after the uncertain unlock.
    publish(
        &replica,
        host_id,
        lockdown_observation(
            &stale_read,
            &admitted.targets[0],
            status(&admitted.targets[0], false),
        ),
    )
    .await;
    let after_stale = machine(&env, host_id).await;
    assert_eq!(after_stale.state.value, checking_unlock.state.value);
    assert_eq!(after_stale.state.version, checking_unlock.state.version);
    report_lockdown(
        &replica,
        host_id,
        &admitted.targets[0],
        status(&admitted.targets[0], false),
    )
    .await;
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    assert_eq!(
        operation(&env, host_id).await.phase,
        NicFirmwarePhase::UpdatingFirmware { target_index: 0 }
    );
    assert!(matches!(
        command(action(&replica, host_id).await),
        fac::mlx_device_action::Command::UpdateFirmware(_)
    ));

    publish(
        &env,
        host_id,
        observation(
            &admitted,
            &admitted.targets[0],
            FirmwareResult::Receipt(complete(&admitted, &admitted.targets[0]).into()),
        ),
    )
    .await;
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    let next = operation(&env, host_id).await;
    assert_eq!(
        next.phase,
        NicFirmwarePhase::ObservingSupervisor { target_index: 1 }
    );
    assert!(next.targets[0].is_complete(&admitted.operation_id, &admitted.options));
    assert!(next.targets[1].receipt.is_none());

    publish(
        &replica,
        host_id,
        missing(&admitted, &admitted.targets[1], "helper"),
    )
    .await;
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    report_lockdown(
        &replica,
        host_id,
        &admitted.targets[1],
        status(&admitted.targets[1], false),
    )
    .await;
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    assert_eq!(
        operation(&env, host_id).await.phase,
        NicFirmwarePhase::UpdatingFirmware { target_index: 1 }
    );
    publish(
        &env,
        host_id,
        observation(
            &admitted,
            &admitted.targets[1],
            FirmwareResult::Receipt(complete(&admitted, &admitted.targets[1]).into()),
        ),
    )
    .await;
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    let awaiting_inventory = operation(&env, host_id).await;
    assert!(matches!(
        awaiting_inventory.phase,
        NicFirmwarePhase::Reobserving { .. }
    ));
    assert!(
        awaiting_inventory
            .targets
            .iter()
            .all(|target| target.is_complete(&admitted.operation_id, &admitted.options))
    );

    // An installed image can match while the running firmware is still old.
    // Keep polling ordinary inventory until every card runs its target version.
    assert_eq!(
        action(&replica, host_id).await,
        fac::Action::MlxDeviceInfoReport(fac::MlxDeviceInfoReport {})
    );
    let mut wrong_versions: Vec<_> = admitted.targets.iter().map(device).collect();
    wrong_versions[0].fw_version_running = Some("0.9.0".into());
    inventory(&replica, host_id, wrong_versions).await;
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    assert_eq!(operation(&env, host_id).await, awaiting_inventory);
    assert!(
        matches!(machine(&env, host_id).await.controller_state_outcome,
        Some(PersistentStateHandlerOutcome::Wait { reason, .. }) if reason.contains("firmware identity and version"))
    );
    assert!(matches!(
        action(&replica, host_id).await,
        fac::Action::MlxDeviceInfoReport(_)
    ));
    inventory(
        &replica,
        host_id,
        admitted.targets.iter().map(device).collect(),
    )
    .await;
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    let restoring = operation(&env, host_id).await;
    assert_eq!(
        restoring.phase,
        NicFirmwarePhase::RestoringLockdown {
            target_index: 0,
            failure: None,
        }
    );
    // A successful lock command is not independent readback. The Ready
    // continuation must wait for a later status request under its own revision.
    report_lockdown(
        &replica,
        host_id,
        &admitted.targets[0],
        status(&admitted.targets[0], false),
    )
    .await;
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    let lock = report_lockdown(
        &replica,
        host_id,
        &admitted.targets[0],
        LockdownResult::Changed(pb::mlx_device_lockdown_result::Changed {}),
    )
    .await;
    assert_eq!(
        lock.request,
        Some(LockdownRequest::Lock(fac::MlxDeviceLock { key: lock_key }))
    );
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    let waiting_readback = operation(&env, host_id).await;
    assert_eq!(waiting_readback.phase, restoring.phase);
    assert_eq!(
        waiting_readback.lockdown.step,
        NicFirmwareLockdownStep::Observe
    );
    assert!(waiting_readback.lockdown.result.is_none());
    report_lockdown(
        &replica,
        host_id,
        &admitted.targets[0],
        status(&admitted.targets[0], true),
    )
    .await;
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    assert_eq!(
        operation(&env, host_id).await.phase,
        NicFirmwarePhase::RestoringLockdown {
            target_index: 1,
            failure: None,
        }
    );
    report_lockdown(
        &replica,
        host_id,
        &admitted.targets[1],
        status(&admitted.targets[1], false),
    )
    .await;
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    let finished = machine(&env, host_id).await;
    assert_eq!(finished.state.value, ManagedHostState::Ready);
    assert!(
        env.redfish_sim
            .actions_since(&power_before)
            .all_hosts()
            .is_empty(),
        "supervisor reset must not add a Core reset"
    );
}

// All completed cards share one host activation. Retaining both its request
// and confirmation prevents ordinary restart verification from repeating it.
#[crate::sqlx_test]
async fn completed_burns_are_retained_before_one_planned_host_reboot(pool: sqlx::PgPool) {
    // Keep this story's snapshot and SQLx futures on the heap so it fits the
    // test runner's default thread stack, like the shared environment fixture.
    Box::pin(completed_burn_reboot_recovery(pool)).await;
}

async fn completed_burn_reboot_recovery(pool: sqlx::PgPool) {
    let env = environment(pool).await;
    let host = create_managed_host(&env).await;
    let host_id = *host.id;
    let mut admitted = fixtures::operation();
    let mut second = fixtures::target();
    second.selector = NicFirmwareCardSelector::Mac("02:00:00:00:00:02".parse().unwrap());
    second.pci_name = "0000:02:00.0".into();
    admitted.targets.push(second);
    let immutable_request = admitted.clone();
    for target in &mut admitted.targets {
        target.supervisor_instance_id = Some("pre-reboot-helper".into());
        target.receipt = Some(complete(&immutable_request, target));
    }
    admitted.phase = NicFirmwarePhase::UpdatingFirmware { target_index: 1 };
    let before = env.redfish_sim.timepoint();

    // The final card has not settled yet. Even the earlier completed burn must
    // remain on this host without triggering activation ahead of its peer.
    let mut pending = admitted.clone();
    pending.targets[1].receipt = None;
    seed(&env, host_id, pending.clone()).await;
    sqlx::query("UPDATE machines SET last_reboot_time = NULL WHERE id = $1")
        .bind(host_id)
        .execute(&env.pool)
        .await
        .expect("clear simulated reboot feedback");
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    assert_eq!(operation(&env, host_id).await, pending);
    assert!(
        env.redfish_sim
            .actions_since(&before)
            .all_hosts()
            .is_empty()
    );

    publish(
        &env,
        host_id,
        observation(
            &admitted,
            &admitted.targets[1],
            FirmwareResult::Receipt(complete(&admitted, &admitted.targets[1]).into()),
        ),
    )
    .await;
    // Fail an actual simulated Redfish request, then reload its persisted
    // attempt marker. An immediate later iteration must observe the cooldown.
    env.redfish_sim.fail_next_power_action(
        SystemPowerControl::ForceRestart,
        "BMC temporarily unavailable",
    );
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    let ready_to_reboot = operation(&env, host_id).await;
    let NicFirmwarePhase::Rebooting {
        started_at,
        last_attempt_at: None,
    } = ready_to_reboot.phase
    else {
        panic!("activation intent must commit before reboot")
    };
    assert_eq!(ready_to_reboot.targets, admitted.targets);
    assert!(
        env.redfish_sim
            .actions_since(&before)
            .all_hosts()
            .is_empty()
    );

    // A newer state version revokes this snapshot's reboot authorization even
    // when its operation is unchanged. Reject it before issuing a power action.
    let mut txn = env.db_txn().await;
    let mut stale_snapshot = host.snapshot(&mut txn).await;
    txn.commit().await.unwrap();
    let stale_state = stale_snapshot.host_snapshot.state.value.clone();
    seed(&env, host_id, ready_to_reboot.clone()).await;
    let winner = machine(&env, host_id).await;
    assert_ne!(
        winner.state.version,
        stale_snapshot.host_snapshot.state.version
    );
    let mut services = env.machine_state_handler_services();
    let mut metrics = MachineMetrics::default();
    let mut pending_db_writes = DbWriteBatch::new();
    let mut ctx = StateHandlerContext {
        services: &mut services,
        metrics: &mut metrics,
        pending_db_writes: &mut pending_db_writes,
    };
    let rejected = env
        .machine_state_handler
        .handle_object_state(&host_id, &mut stale_snapshot, &stale_state, &mut ctx)
        .await;
    assert!(matches!(
        rejected,
        Err(StateHandlerError::IterationInvalidated { .. })
    ));
    let after_rejection = machine(&env, host_id).await;
    assert_eq!(after_rejection.state.value, winner.state.value);
    assert_eq!(after_rejection.state.version, winner.state.version);
    assert_eq!(operation(&env, host_id).await, ready_to_reboot);
    assert!(
        env.redfish_sim
            .actions_since(&before)
            .all_hosts()
            .is_empty()
    );

    // An otherwise eligible activation must honor the existing health override
    // without consuming an attempt or the retained successful burn receipts.
    let mut block_reboot = health_report::HealthReport::empty("test-nic-reboot-block".into());
    block_reboot.alerts.push(health_report::HealthProbeAlert {
        id: "TestNicRebootBlock".parse().unwrap(),
        target: None,
        in_alert_since: None,
        message: "hold firmware activation".into(),
        tenant_message: None,
        classifications: vec![
            health_report::HealthAlertClassification::stop_reboot_for_automatic_recovery_from_state_machine(),
        ],
    });
    send_health_report_entry(
        &env,
        &host_id.into(),
        (block_reboot, health_report::HealthReportApplyMode::Merge),
    )
    .await;
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    let blocked = machine(&env, host_id).await;
    assert_eq!(operation(&env, host_id).await, ready_to_reboot);
    assert_eq!(blocked.state.version, after_rejection.state.version);
    assert_eq!(
        blocked.status.last_reboot_requested,
        after_rejection.status.last_reboot_requested
    );
    assert!(matches!(
        blocked.controller_state_outcome,
        Some(PersistentStateHandlerOutcome::Wait { reason, .. })
            if reason == "NIC firmware activation reboot is blocked by a health override"
    ));
    assert!(
        env.redfish_sim
            .actions_since(&before)
            .all_hosts()
            .is_empty()
    );
    remove_health_report_entry(&env, &host_id.into(), "test-nic-reboot-block".into()).await;

    // Missing DPU snapshots must not strand already-authorized firmware work.
    // Keep the association, omit its snapshot, and reach the actual reboot
    // attempt so the persisted marker proves recovery passed that guard.
    let mut txn = env.db_txn().await;
    let mut recovery_snapshot = host.snapshot(&mut txn).await;
    txn.commit().await.unwrap();
    assert!(
        !recovery_snapshot
            .host_snapshot
            .associated_dpu_machine_ids()
            .is_empty()
    );
    recovery_snapshot.dpu_snapshots.clear();
    let recovery_state = recovery_snapshot.host_snapshot.state.value.clone();
    let mut recovery_services = env.machine_state_handler_services();
    let mut recovery_metrics = MachineMetrics::default();
    let mut recovery_writes = DbWriteBatch::new();
    let mut recovery_ctx = StateHandlerContext {
        services: &mut recovery_services,
        metrics: &mut recovery_metrics,
        pending_db_writes: &mut recovery_writes,
    };
    let recovery = env
        .machine_state_handler
        .handle_object_state(
            &host_id,
            &mut recovery_snapshot,
            &recovery_state,
            &mut recovery_ctx,
        )
        .await;
    assert!(matches!(
        recovery,
        Ok(StateHandlerOutcome::Wait { reason, .. })
            if reason.contains("waiting for Scout to report")
    ));
    let mut txn = env.db_txn().await;
    recovery_writes.apply_all(&mut txn).await.unwrap();
    txn.commit().await.unwrap();
    let reboot_requested = operation(&env, host_id).await;
    assert!(matches!(
        reboot_requested.phase,
        NicFirmwarePhase::Rebooting {
            last_attempt_at: Some(_),
            ..
        }
    ));
    assert_eq!(reboot_requested.targets, admitted.targets);
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    assert_eq!(operation(&env, host_id).await, reboot_requested);
    assert_eq!(
        env.redfish_sim.actions_since(&before).all_hosts(),
        vec![RedfishSimAction::PowerFailed(
            SystemPowerControl::ForceRestart
        )]
    );

    // Advance only the persisted clock boundary, not the receipt or owner. The
    // next iteration must retry the failed request and retain every burn receipt.
    let mut retry_due = reboot_requested;
    retry_due.phase = NicFirmwarePhase::Rebooting {
        started_at,
        last_attempt_at: Some(Utc::now() - chrono::Duration::hours(1)),
    };
    seed(&env, host_id, retry_due).await;
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    let retried = operation(&env, host_id).await;
    let NicFirmwarePhase::Rebooting {
        last_attempt_at: Some(latest_attempt),
        ..
    } = retried.phase
    else {
        panic!("retry attempt must remain durable")
    };
    assert!(latest_attempt > started_at);
    assert_eq!(retried.targets, admitted.targets);
    assert_eq!(
        env.redfish_sim.actions_since(&before).all_hosts(),
        vec![
            RedfishSimAction::PowerFailed(SystemPowerControl::ForceRestart),
            RedfishSimAction::Power(SystemPowerControl::ForceRestart),
        ]
    );

    let pending_reboot = machine(&env, host_id)
        .await
        .status
        .last_reboot_requested
        .expect("activation reboot request persisted");
    assert_eq!(pending_reboot.time, latest_attempt);
    assert_eq!(pending_reboot.mode, MachineLastRebootRequestedMode::Reboot);
    assert_eq!(pending_reboot.restart_verified, Some(false));
    // Neither another host's credential nor the unbound simulator service can
    // provide this host's activation evidence. Rejection must preserve the
    // durable timestamp and request, not merely the visible controller phase.
    let before_report = machine(&env, host_id).await;
    let foreign_host: MachineId = "fm100hseddco33hvlofuqvg543p6p9aj60g76q5cq491g9m9tgtf2dk0530"
        .parse()
        .unwrap();
    assert_ne!(foreign_host, host_id.into());
    let report = rpc::forge::MachineRebootCompletedRequest {
        machine_id: Some(host_id.into()),
    };
    for principal in [
        carbide_authn::middleware::Principal::SpiffeMachineIdentifier(foreign_host.to_string()),
        carbide_authn::middleware::Principal::SpiffeServiceIdentifier("machine-a-tron".into()),
    ] {
        let mut request = Request::new(report.clone());
        let mut auth = AuthContext::default();
        auth.principals.push(principal);
        request.extensions_mut().insert(auth);
        assert_eq!(
            env.api.reboot_completed(request).await.unwrap_err().code(),
            Code::PermissionDenied
        );
        let rejected = machine(&env, host_id).await;
        assert_eq!(
            rejected.status.last_reboot_time,
            before_report.status.last_reboot_time
        );
        assert_eq!(
            rejected.status.last_reboot_requested,
            before_report.status.last_reboot_requested
        );
        assert_eq!(rejected.state.value, before_report.state.value);
        assert_eq!(rejected.state.version, before_report.state.version);
    }
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    assert_eq!(operation(&env, host_id).await, retried);

    let mut txn = env.db_txn().await;
    let mut before_feedback = host.snapshot(&mut txn).await;
    txn.commit().await.unwrap();
    assert!(
        before_feedback
            .host_snapshot
            .status
            .last_reboot_time
            .is_none()
    );
    env.api
        .reboot_completed(authenticated(report, host_id.into()))
        .await
        .unwrap();
    let reported_at = machine(&env, host_id)
        .await
        .status
        .last_reboot_time
        .unwrap();
    assert!(reported_at > started_at);
    // Feedback committed after this snapshot. The handler's locked reread must
    // consume it rather than issue another reboot from the stale snapshot.
    let stale_version = before_feedback.host_snapshot.state.version;
    let stale_state = before_feedback.host_snapshot.state.value.clone();
    let mut services = env.machine_state_handler_services();
    let mut metrics = MachineMetrics::default();
    let mut writes = DbWriteBatch::new();
    let mut ctx = StateHandlerContext {
        services: &mut services,
        metrics: &mut metrics,
        pending_db_writes: &mut writes,
    };
    let outcome = env
        .machine_state_handler
        .handle_object_state(&host_id, &mut before_feedback, &stale_state, &mut ctx)
        .await
        .expect("the locked reread must find committed reboot feedback");
    let StateHandlerOutcome::Transition {
        next_state,
        txn: Some(mut txn),
        ..
    } = outcome
    else {
        panic!("committed feedback must authorize the transactional inventory transition")
    };
    assert_eq!(
        db::machine::try_update_controller_state(
            &mut txn,
            &host_id,
            stale_version,
            stale_version.increment(),
            &next_state,
        )
        .await
        .unwrap(),
        db::ConditionalWrite::Applied(())
    );
    writes.apply_all(&mut txn).await.unwrap();
    txn.commit().await.unwrap();
    let activated = operation(&env, host_id).await;
    assert_eq!(
        activated.phase,
        NicFirmwarePhase::Reobserving {
            not_before: reported_at
        }
    );
    assert_eq!(activated.targets, admitted.targets);
    assert!(matches!(
        action(&env, host_id).await,
        fac::Action::MlxDeviceInfoReport(_)
    ));
    // Verify the exact recorded request, preserving its timestamp and mode.
    // Otherwise the generic verifier can retry it after this workflow returns.
    assert_eq!(
        machine(&env, host_id).await.status.last_reboot_requested,
        Some(MachineLastRebootRequested {
            restart_verified: Some(true),
            verification_attempts: Some(0),
            ..pending_reboot
        }),
    );
    assert_eq!(
        env.redfish_sim.actions_since(&before).all_hosts(),
        vec![
            RedfishSimAction::PowerFailed(SystemPowerControl::ForceRestart),
            RedfishSimAction::Power(SystemPowerControl::ForceRestart)
        ],
    );
}

// Uncertain terminal effects retain firmware ownership and keep a queued reset
// unstarted until execution can be reconciled safely.
#[crate::sqlx_test]
async fn uncertain_execution_keeps_lockdown_restoration_and_reset_excluded(pool: sqlx::PgPool) {
    let env = environment(pool).await;
    let host = create_managed_host(&env).await;
    let host_id = *host.id;
    let mut admitted = fixtures::operation();
    admitted.phase = NicFirmwarePhase::UpdatingFirmware { target_index: 0 };
    admitted.targets[0].supervisor_instance_id = Some("helper".into());
    admitted.options.verify_version = true;
    seed(&env, host_id, admitted.clone()).await;
    let target = &admitted.targets[0];
    let mut receipt = complete(&admitted, target);
    receipt.state = FirmwareOperationState::ManualIntervention;
    receipt.phase = Some(FirmwareOperationPhase::VerifyVersion);
    receipt.diagnostic = Some("version query timed out".into());
    receipt.report.verified_version = Some(false);
    receipt.report.observed_version = None;
    publish(
        &env,
        host_id,
        observation(
            &admitted,
            target,
            FirmwareResult::Receipt(receipt.clone().into()),
        ),
    )
    .await;

    // An operator's ordinary host-reset request must not overtake unsettled
    // firmware work. Keep it pending until recovery establishes a safe exit.
    let reset_request = ResetRequest {
        requested_at: Utc::now(),
        initiator: "test-operator".into(),
        started_at: None,
        ignore_cleanup: false,
    };
    sqlx::query("UPDATE machines SET reset_requested = $1 WHERE id = $2")
        .bind(sqlx::types::Json(&reset_request))
        .bind(host_id)
        .execute(&env.pool)
        .await
        .unwrap();
    let power_before = env.redfish_sim.timepoint();

    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    let uncertain = operation(&env, host_id).await;
    assert_eq!(uncertain.phase, admitted.phase);
    assert_eq!(uncertain.targets[0].receipt, Some(receipt));
    assert!(matches!(
        command(action(&env, host_id).await),
        fac::mlx_device_action::Command::ObserveFirmwareUpdate(_)
    ));
    assert!(
        matches!(machine(&env, host_id).await.controller_state_outcome,
        Some(PersistentStateHandlerOutcome::Error { err, .. }) if err.contains("without successful requested-phase evidence"))
    );
    let retained_reset = machine(&env, host_id).await.reset_requested.unwrap();
    assert_eq!(retained_reset.requested_at, reset_request.requested_at);
    assert!(retained_reset.started_at.is_none());
    assert!(
        env.redfish_sim
            .actions_since(&power_before)
            .all_hosts()
            .is_empty()
    );
}

// A cleanup failure must stop the next card without discarding an in-flight
// burn or the independent readback needed to restore its original lockdown.
#[crate::sqlx_test]
async fn recorded_cleanup_failure_settles_the_active_card_before_parking(pool: sqlx::PgPool) {
    // SQLx and snapshot futures in this multi-step story otherwise accumulate
    // on libtest's default thread stack. Keep the story's future on the heap.
    Box::pin(cleanup_failure_recovery(pool)).await;
}

async fn cleanup_failure_recovery(pool: sqlx::PgPool) {
    let env = environment(pool).await;
    let host = create_managed_host(&env).await;
    let host_id = *host.id;
    let mut admitted = fixtures::operation();
    admitted.options.activation = NicFirmwareActivation::CardReset {
        reset_level: 3,
        post_reset_verify_retry_interval_seconds: None,
        post_reset_verify_timeout_seconds: None,
    };
    let mut second = fixtures::target();
    second.selector = NicFirmwareCardSelector::Mac("02:00:00:00:00:02".parse().unwrap());
    second.pci_name = "0000:02:00.0".into();
    admitted.targets.push(second.clone());
    admitted.phase = NicFirmwarePhase::UpdatingFirmware { target_index: 0 };
    admitted.targets[0].supervisor_instance_id = Some("cleanup-test-helper".into());
    let NicFirmwareCardSelector::Mac(mac_address) = admitted.targets[0].selector else {
        unreachable!()
    };
    admitted.targets[0].original_lockdown = NicFirmwareLockdownPolicy::Locked {
        ikm_version: 7,
        mac_address,
    };
    env.test_credential_manager
        .set_credentials(
            &CredentialKey::NicLockdownIkm {
                credential_type: NicLockdownIkm::SiteWide { version: 7 },
            },
            &Credentials::UsernamePassword {
                username: String::new(),
                password: "test-ikm".into(),
            },
        )
        .await
        .unwrap();
    seed(&env, host_id, admitted.clone()).await;
    let power_before = env.redfish_sim.timepoint();
    assert!(matches!(
        command(action(&env, host_id).await),
        fac::mlx_device_action::Command::UpdateFirmware(update)
            if update.expected_device == Some(admitted.targets[0].selector.clone().into())
    ));

    // The real producer commits failure separately from controller state. Core
    // must revoke unsent work immediately, before the controller observes it.
    let cleanup_message = "storage cleanup failed during NIC execution";
    record_cleanup_failure(&env, host_id.into(), cleanup_message).await;
    let recorded_failure = machine(&env, host_id).await.status.failure_details;
    assert_eq!(
        recorded_failure.cause,
        model::machine::FailureCause::NVMECleanFailed {
            err: format!("NVMe cleanup failed: {cleanup_message}"),
        }
    );
    assert_eq!(
        recorded_failure.source,
        model::machine::FailureSource::Scout
    );
    let failure_category = format!(
        "{}/{}",
        recorded_failure.source,
        recorded_failure.cause.metric_label()
    );
    assert_eq!(operation(&env, host_id).await, admitted);
    let fac::Action::MlxAction(observing) = action(&env, host_id).await else {
        panic!("recorded failure must retain status-only NIC delivery")
    };
    assert_eq!(observing.device_actions.len(), 1);
    assert_eq!(
        observing.device_actions[0].pci_name,
        admitted.targets[0].pci_name
    );
    assert!(matches!(
        &observing.device_actions[0].command,
        Some(fac::mlx_device_action::Command::ObserveFirmwareUpdate(observe))
            if observe.operation_id == admitted.operation_id
    ));

    let running = FirmwareOperation {
        operation_id: admitted.operation_id.clone(),
        state: FirmwareOperationState::Running,
        phase: Some(FirmwareOperationPhase::Burn),
        report: FirmwareFlashReport::default(),
        diagnostic: None,
    };
    publish(
        &env,
        host_id,
        observation(
            &admitted,
            &admitted.targets[0],
            FirmwareResult::Receipt(running.clone().into()),
        ),
    )
    .await;
    let mut pending = admitted.clone();
    pending.targets[0].receipt = Some(running);
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    assert_eq!(operation(&env, host_id).await, pending);
    assert!(matches!(
        machine(&env, host_id).await.controller_state_outcome,
        Some(PersistentStateHandlerOutcome::Wait { reason, .. })
            if reason.contains(&failure_category)
                && reason.contains(&host_id.to_string())
    ));
    let reset_error = env
        .api
        .trigger_managed_host_reset(Request::new(rpc::forge::ManagedHostResetRequest {
            machine_id: Some(host_id.into()),
            mode: rpc::forge::managed_host_reset_request::Mode::Set.into(),
            initiator: rpc::forge::UpdateInitiator::AdminCli.into(),
            allow_reset_with_instance: false,
            ignore_cleanup: false,
        }))
        .await
        .unwrap_err();
    assert_eq!(reset_error.code(), Code::FailedPrecondition);
    assert!(reset_error.message().contains("NIC firmware recovery"));
    assert!(machine(&env, host_id).await.reset_requested.is_none());

    // Safe settlement starts restoration at card zero rather than advancing
    // to the untouched card's firmware request.
    let completed = complete(&admitted, &admitted.targets[0]);
    publish(
        &env,
        host_id,
        observation(
            &admitted,
            &admitted.targets[0],
            FirmwareResult::Receipt(completed.clone().into()),
        ),
    )
    .await;
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    let restoring = operation(&env, host_id).await;
    let NicFirmwarePhase::RestoringLockdown {
        target_index: 0,
        failure: Some(reason),
    } = &restoring.phase
    else {
        panic!("settled failure must restore the original policies")
    };
    let reason = reason.clone();
    assert!(reason.contains(&failure_category));
    assert!(reason.contains(&host_id.to_string()));
    assert_eq!(restoring.targets[0].receipt, Some(completed));
    assert_eq!(restoring.targets[1], second);
    assert_eq!(restoring.lockdown.step, NicFirmwareLockdownStep::Observe);
    assert!(restoring.lockdown.result.is_none());
    report_lockdown(
        &env,
        host_id,
        &admitted.targets[0],
        status(&admitted.targets[0], false),
    )
    .await;
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    let changing = operation(&env, host_id).await;
    assert_eq!(changing.phase, restoring.phase);
    assert_eq!(changing.lockdown.step, NicFirmwareLockdownStep::Change);
    let lock = report_lockdown(
        &env,
        host_id,
        &admitted.targets[0],
        LockdownResult::Changed(Default::default()),
    )
    .await;
    assert!(matches!(lock.request, Some(LockdownRequest::Lock(_))));
    assert_eq!(
        operation(&env, host_id).await.lockdown.result,
        Some(NicFirmwareLockdownResult::Changed)
    );
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    let awaiting_readback = operation(&env, host_id).await;
    assert_eq!(awaiting_readback.phase, restoring.phase);
    assert_eq!(
        awaiting_readback.lockdown.step,
        NicFirmwareLockdownStep::Observe
    );
    assert!(awaiting_readback.lockdown.result.is_none());

    // A successful change is insufficient: persist independent locked status
    // before moving to the second card's unchanged original unlocked policy.
    report_lockdown(
        &env,
        host_id,
        &admitted.targets[0],
        status(&admitted.targets[0], true),
    )
    .await;
    assert_eq!(
        operation(&env, host_id).await.lockdown.result,
        Some(NicFirmwareLockdownResult::Status(
            NicFirmwareLockdownStatus::Locked
        ))
    );
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    let second_restoration = operation(&env, host_id).await;
    assert_eq!(
        second_restoration.phase,
        NicFirmwarePhase::RestoringLockdown {
            target_index: 1,
            failure: Some(reason.clone()),
        }
    );
    assert_eq!(second_restoration.targets, restoring.targets);
    report_lockdown(&env, host_id, &second, status(&second, false)).await;
    assert_eq!(
        operation(&env, host_id).await.lockdown.result,
        Some(NicFirmwareLockdownResult::Status(
            NicFirmwareLockdownStatus::Unlocked
        ))
    );
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    let parked = operation(&env, host_id).await;
    assert_eq!(parked.phase, NicFirmwarePhase::Failed { reason });
    assert_eq!(parked.targets, restoring.targets);
    assert_eq!(parked.purpose, admitted.purpose);
    assert_eq!(action(&env, host_id).await, fac::Action::noop());
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    assert_eq!(operation(&env, host_id).await, parked);
    assert_eq!(
        machine(&env, host_id).await.status.failure_details,
        recorded_failure
    );
    assert!(matches!(
        machine(&env, host_id).await.controller_state_outcome,
        Some(PersistentStateHandlerOutcome::Error { err, .. })
            if err.contains(&failure_category) && err.contains(&host_id.to_string())
    ));
    assert!(
        env.redfish_sim
            .actions_since(&power_before)
            .all_hosts()
            .is_empty()
    );
}

// Attached-DPU failures must revoke an otherwise valid unlock through the real
// host poll, while retaining permanent execution barriers during recovery.
#[crate::sqlx_test]
async fn attached_dpu_failure_revokes_unlock_and_preserves_delivery_barriers(pool: sqlx::PgPool) {
    Box::pin(attached_dpu_failure_recovery(pool)).await;
}

async fn attached_dpu_failure_recovery(pool: sqlx::PgPool) {
    let env = environment(pool).await;
    let host = create_managed_host(&env).await;
    let host_id = *host.id;
    let dpu_id = host.dpu().id;
    let segment = env.create_vpc_and_tenant_segment().await;
    let owner = allocate_owner(&env, &host, segment).await;
    let mut admitted = fixtures::operation();
    admitted.purpose = NicFirmwarePurpose::Allocation {
        instance_id: owner.id(),
    };
    admitted.phase = NicFirmwarePhase::Unlocking { target_index: 0 };
    admitted.lockdown.step = NicFirmwareLockdownStep::Change;
    admitted.targets[0].supervisor_instance_id = Some("dpu-failure-helper".into());
    let NicFirmwareCardSelector::Mac(mac_address) = admitted.targets[0].selector else {
        unreachable!()
    };
    admitted.targets[0].original_lockdown = NicFirmwareLockdownPolicy::Locked {
        ikm_version: 7,
        mac_address,
    };
    env.test_credential_manager
        .set_credentials(
            &CredentialKey::NicLockdownIkm {
                credential_type: NicLockdownIkm::SiteWide { version: 7 },
            },
            &Credentials::UsernamePassword {
                username: String::new(),
                password: "test-ikm".into(),
            },
        )
        .await
        .unwrap();
    seed(&env, host_id, admitted.clone()).await;
    assert!(matches!(
        lockdown_action(&env, host_id).await.request,
        Some(LockdownRequest::Unlock(unlock)) if !unlock.key.is_empty()
    ));

    record_cleanup_failure(&env, dpu_id.into(), "attached DPU cleanup failed").await;
    let dpu = db::machine::find_one(&env.pool, &dpu_id, Default::default())
        .await
        .unwrap()
        .unwrap();
    let failure_category = format!(
        "{}/{}",
        dpu.status.failure_details.source,
        dpu.status.failure_details.cause.metric_label()
    );
    assert_eq!(action(&env, host_id).await, fac::Action::noop());
    assert_eq!(operation(&env, host_id).await, admitted);
    // A DPU cleanup callback does not enqueue its host; supply the periodic
    // controller wakeup explicitly so this proof owns failure consumption.
    env.api
        .machine_state_handler_enqueuer
        .enqueue_object(&host_id)
        .await
        .unwrap();
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    let restoring = operation(&env, host_id).await;
    assert!(matches!(
        &restoring.phase,
        NicFirmwarePhase::RestoringLockdown {
            target_index: 0,
            failure: Some(reason),
        } if reason.contains(&dpu_id.to_string()) && reason.contains(&failure_category)
    ));
    assert_eq!(restoring.targets, admitted.targets);
    assert_eq!(restoring.lockdown.step, NicFirmwareLockdownStep::Observe);
    assert!(restoring.lockdown.result.is_none());
    report_lockdown(
        &env,
        host_id,
        &admitted.targets[0],
        status(&admitted.targets[0], true),
    )
    .await;
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    assert!(matches!(
        operation(&env, host_id).await.phase,
        NicFirmwarePhase::Failed { .. }
    ));

    // At the already-authorized submit boundary, missing receipt evidence
    // cannot turn a permanent delivery error into an indefinite pending wait.
    let mut submitting = admitted.clone();
    submitting.phase = NicFirmwarePhase::UpdatingFirmware { target_index: 0 };
    submitting.lockdown = Default::default();
    seed(&env, host_id, submitting.clone()).await;
    assert!(matches!(
        command(action(&env, host_id).await),
        fac::mlx_device_action::Command::ObserveFirmwareUpdate(_)
    ));
    publish(
        &env,
        host_id,
        observation(
            &submitting,
            &submitting.targets[0],
            FirmwareResult::Error(pb::MlxDeviceUpdateFirmwareError {
                code: pb::MlxDeviceUpdateFirmwareErrorCode::Unsupported.into(),
                diagnostic: "helper cannot reconcile the committed request".into(),
            }),
        ),
    )
    .await;
    let blocked = operation(&env, host_id).await;
    publish(
        &env,
        host_id,
        missing(&submitting, &submitting.targets[0], "dpu-failure-helper"),
    )
    .await;
    assert_eq!(operation(&env, host_id).await, blocked);
    assert_eq!(blocked.phase, submitting.phase);
    assert!(blocked.targets[0].receipt.is_none());
    assert_eq!(
        blocked.targets[0].delivery_error_code,
        Some(NicFirmwareDeliveryErrorCode::Unsupported)
    );
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    assert_eq!(operation(&env, host_id).await, blocked);
    assert!(matches!(
        machine(&env, host_id).await.controller_state_outcome,
        Some(PersistentStateHandlerOutcome::Error { err, .. })
            if err.contains(&dpu_id.to_string())
                && err.contains(&failure_category)
                && err.contains("helper cannot reconcile the committed request")
    ));
}

// A failure arriving after successful restoration began must replace the
// success continuation and recheck every card from the first frozen target.
#[crate::sqlx_test]
async fn recorded_failure_restarts_successful_lockdown_restoration(pool: sqlx::PgPool) {
    let env = environment(pool).await;
    let host = create_managed_host(&env).await;
    let host_id = *host.id;
    let mut admitted = fixtures::operation();
    let mut second = fixtures::target();
    second.selector = NicFirmwareCardSelector::Mac("02:00:00:00:00:02".parse().unwrap());
    second.pci_name = "0000:02:00.0".into();
    admitted.targets.push(second);
    let request = admitted.clone();
    for target in &mut admitted.targets {
        target.receipt = Some(complete(&request, target));
    }
    admitted.phase = NicFirmwarePhase::RestoringLockdown {
        target_index: 1,
        failure: None,
    };
    admitted.lockdown.result = Some(NicFirmwareLockdownResult::Status(
        NicFirmwareLockdownStatus::Unlocked,
    ));
    seed(&env, host_id, admitted.clone()).await;
    record_cleanup_failure(&env, host_id.into(), "failure after successful burns").await;
    let failure = machine(&env, host_id).await.status.failure_details;
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    let restarting = operation(&env, host_id).await;
    assert!(matches!(
        &restarting.phase,
        NicFirmwarePhase::RestoringLockdown {
            target_index: 0,
            failure: Some(reason),
        } if reason.contains(&host_id.to_string())
            && reason.contains(&failure.source.to_string())
            && reason.contains(failure.cause.metric_label())
    ));
    assert_eq!(restarting.targets, admitted.targets);
    assert_eq!(restarting.purpose, admitted.purpose);
    assert_eq!(restarting.lockdown.step, NicFirmwareLockdownStep::Observe);
    assert!(restarting.lockdown.result.is_none());
}

// The contact write must finish before FAC decides whether a loaded request
// still has permission to submit; an older host snapshot is insufficient.
#[crate::sqlx_test]
async fn scout_submission_rechecks_failure_after_contact_write_waits(pool: sqlx::PgPool) {
    let env = environment(pool).await;
    let host = create_managed_host(&env).await;
    let host_id = *host.id;
    let mut admitted = fixtures::operation();
    admitted.phase = NicFirmwarePhase::UpdatingFirmware { target_index: 0 };
    admitted.targets[0].supervisor_instance_id = Some("contact-wait-helper".into());
    seed(&env, host_id, admitted.clone()).await;
    assert!(matches!(
        command(action(&env, host_id).await),
        fac::mlx_device_action::Command::UpdateFirmware(_)
    ));

    let mut writer = env.pool.begin().await.unwrap();
    let writer_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(writer.as_mut())
        .await
        .unwrap();
    let failure = model::machine::FailureDetails {
        cause: model::machine::FailureCause::NVMECleanFailed {
            err: "failure committed while FAC waits for the contact row".into(),
        },
        source: model::machine::FailureSource::Scout,
        failed_at: Utc::now(),
    };
    db::machine::update_failure_details_by_machine_id(
        &host_id.into(),
        writer.as_mut(),
        failure.clone(),
    )
    .await
    .unwrap();
    let mut response = Box::pin(action(&env, host_id));
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::select! {
            _ = &mut response => panic!("the contact write must wait for the failure writer"),
            _pid = wait_for_blocked_query(&env.pool, writer_pid, "UPDATE machines SET last_scout_contact_time") => {},
        }
    })
    .await
    .expect("FAC must load the old snapshot before its contact write blocks");
    writer.commit().await.unwrap();
    let response = tokio::time::timeout(std::time::Duration::from_secs(5), response)
        .await
        .expect("FAC must finish after the failure writer commits");
    assert!(matches!(
        command(response),
        fac::mlx_device_action::Command::ObserveFirmwareUpdate(_)
    ));
    assert_eq!(operation(&env, host_id).await, admitted);
    assert_eq!(machine(&env, host_id).await.status.failure_details, failure);
}

// Allocation deletion stops submission, but release must wait for execution
// evidence and independent lockdown restoration before resuming its normal path.
#[crate::sqlx_test]
async fn lost_allocation_owner_stops_submission_but_retains_pending_execution(pool: sqlx::PgPool) {
    // Keep this recovery story's SQLx and snapshot futures off libtest's
    // default thread stack as it crosses several persisted recovery points.
    Box::pin(lost_allocation_owner_recovery(pool)).await;
}

async fn lost_allocation_owner_recovery(pool: sqlx::PgPool) {
    let env = environment(pool).await;
    let host = create_managed_host(&env).await;
    let host_id = *host.id;
    let segment = env.create_vpc_and_tenant_segment().await;
    let owner = allocate_owner(&env, &host, segment).await;
    let mut admitted = fixtures::operation();
    admitted.purpose = NicFirmwarePurpose::Allocation {
        instance_id: owner.id(),
    };
    admitted.phase = NicFirmwarePhase::UpdatingFirmware { target_index: 0 };
    admitted.targets[0].supervisor_instance_id = Some("helper".into());
    seed(&env, host_id, admitted.clone()).await;
    // The live attached owner permits guarded submission; deletion below must
    // change delivery to observation without losing the already authorized work.
    assert!(matches!(
        command(action(&env, host_id).await),
        fac::mlx_device_action::Command::UpdateFirmware(update)
            if update.operation_id == admitted.operation_id
                && update.supervisor_instance_id == admitted.targets[0].supervisor_instance_id
    ));

    // Losing the owner before submission also revokes a pending unlock. Use a
    // valid locked-card context so the ownership guard, not missing key data,
    // is what suppresses that mutation.
    let mut before_submission = admitted.clone();
    before_submission.phase = NicFirmwarePhase::Unlocking { target_index: 0 };
    before_submission.lockdown.step = NicFirmwareLockdownStep::Change;
    let NicFirmwareCardSelector::Mac(mac_address) = before_submission.targets[0].selector else {
        unreachable!()
    };
    before_submission.targets[0].original_lockdown = NicFirmwareLockdownPolicy::Locked {
        ikm_version: 7,
        mac_address,
    };
    env.test_credential_manager
        .set_credentials(
            &CredentialKey::NicLockdownIkm {
                credential_type: NicLockdownIkm::SiteWide { version: 7 },
            },
            &Credentials::UsernamePassword {
                username: String::new(),
                password: "test-ikm".into(),
            },
        )
        .await
        .unwrap();
    seed(&env, host_id, before_submission.clone()).await;
    let mut txn = env.pool.begin().await.unwrap();
    db::instance::mark_as_deleted(owner.id(), txn.as_mut())
        .await
        .unwrap();
    txn.commit().await.unwrap();

    assert_eq!(action(&env, host_id).await, fac::Action::noop());
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    let stopped_before_submission = operation(&env, host_id).await;
    assert!(matches!(
        stopped_before_submission.phase,
        NicFirmwarePhase::RestoringLockdown {
            target_index: 0,
            failure: Some(reason),
        } if reason.contains("lifecycle owner changed")
    ));
    assert_eq!(stopped_before_submission.targets, before_submission.targets);
    assert_eq!(stopped_before_submission.purpose, admitted.purpose);
    assert_eq!(
        stopped_before_submission.lockdown.step,
        NicFirmwareLockdownStep::Observe
    );
    assert!(stopped_before_submission.lockdown.result.is_none());

    // Exercise the later boundary separately: submission may already have
    // happened, so the same deleted owner no longer permits immediate recovery.
    seed(&env, host_id, admitted.clone()).await;
    // The allocated owner is gone, but Core cannot assume the already
    // authorized submit had no effect. Poll for evidence without another submit.
    assert!(matches!(
        command(action(&env, host_id).await),
        fac::mlx_device_action::Command::ObserveFirmwareUpdate(_)
    ));
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    assert_eq!(operation(&env, host_id).await, admitted);
    assert!(
        matches!(machine(&env, host_id).await.controller_state_outcome,
        Some(PersistentStateHandlerOutcome::Wait { reason, .. })
            if reason.contains("waiting for already authorized NIC firmware effects to settle"))
    );

    // This receipt establishes that preparation failed before mutation. It is
    // the evidence that permits lockdown restoration after allocation deletion.
    let failed_prepare = FirmwareOperation {
        operation_id: admitted.operation_id.clone(),
        state: FirmwareOperationState::Failed,
        phase: Some(FirmwareOperationPhase::Prepare),
        report: FirmwareFlashReport::default(),
        diagnostic: Some("image unavailable before mutation".into()),
    };
    publish(
        &env,
        host_id,
        observation(
            &admitted,
            &admitted.targets[0],
            FirmwareResult::Receipt(failed_prepare.clone().into()),
        ),
    )
    .await;
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    let restoring = operation(&env, host_id).await;
    assert_eq!(restoring.targets[0].receipt, Some(failed_prepare.clone()));
    assert_eq!(restoring.purpose, admitted.purpose);
    assert!(
        matches!(restoring.phase, NicFirmwarePhase::RestoringLockdown {
        failure: Some(reason), ..
    } if reason.contains("lifecycle owner changed"))
    );
    report_lockdown(
        &env,
        host_id,
        &admitted.targets[0],
        status(&admitted.targets[0], false),
    )
    .await;
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    let safe_failure = operation(&env, host_id).await;
    assert!(matches!(
        safe_failure.phase,
        NicFirmwarePhase::Failed { .. }
    ));
    assert_eq!(safe_failure.purpose, admitted.purpose);
    assert_eq!(safe_failure.targets[0].receipt, Some(failed_prepare));

    // A later DPU failure blocks deleted-owner handoff and must be visible
    // without replacing the earlier firmware reason retained in the phase.
    let dpu_id = host.dpu().id;
    record_cleanup_failure(&env, dpu_id.into(), "new failure blocks owner release").await;
    env.api
        .machine_state_handler_enqueuer
        .enqueue_object(&host_id)
        .await
        .unwrap();
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    assert_eq!(operation(&env, host_id).await, safe_failure);
    let NicFirmwarePhase::Failed {
        reason: saved_reason,
    } = &safe_failure.phase
    else {
        unreachable!()
    };
    assert!(matches!(
        machine(&env, host_id).await.controller_state_outcome,
        Some(PersistentStateHandlerOutcome::Error { err, .. })
            if err.contains(saved_reason)
                && err.contains(&dpu_id.to_string())
                && err.contains("Scout/nvme_clean_failed")
    ));
    // Explicitly clearing the recorded failure permits the retained ordinary
    // release proof to continue with the same deleted owner and firmware state.
    let mut txn = env.pool.begin().await.unwrap();
    db::machine::clear_failure_details(&dpu_id.into(), txn.as_mut())
        .await
        .unwrap();
    txn.commit().await.unwrap();
    env.api
        .machine_state_handler_enqueuer
        .enqueue_object(&host_id)
        .await
        .unwrap();
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    let released = db::machine::find_one(
        &env.pool,
        &host_id,
        MachineSearchConfig {
            include_history: true,
            ..Default::default()
        },
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        released.state.value,
        ManagedHostState::Assigned {
            instance_state: InstanceState::WaitingForRebootToReady
        }
    );
    assert!(released.history.iter().any(|record| {
        serde_json::from_str::<ManagedHostState>(&record.state).is_ok_and(|state| {
            matches!(state, ManagedHostState::NicFirmwareUpdating { operation } if operation == safe_failure)
        })
    }));
    // The handoff must be consumable by ordinary deletion processing. Its next
    // reboot step retains the deleted owner instead of reopening provisioning.
    let handoff_power = env.redfish_sim.timepoint();
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    let next = machine(&env, host_id).await;
    assert_eq!(
        next.state.value,
        ManagedHostState::Assigned {
            instance_state: InstanceState::Ready
        }
    );
    assert_eq!(
        next.status.last_reboot_requested.unwrap().mode,
        MachineLastRebootRequestedMode::Reboot
    );
    assert_eq!(
        env.redfish_sim.actions_since(&handoff_power).all_hosts(),
        vec![RedfishSimAction::Power(SystemPowerControl::ForceRestart)]
    );
    let mut txn = env.pool.begin().await.unwrap();
    let retained_owner = db::instance::find_by_id(txn.as_mut(), owner.id())
        .await
        .unwrap()
        .unwrap();
    assert!(retained_owner.deleted.is_some());
    txn.commit().await.unwrap();

    // Completed burns still require their committed activation despite a new
    // recorded failure. Neither that failure nor owner loss permits success.
    let mut burned = admitted.clone();
    burned.targets[0].receipt = Some(complete(&burned, &burned.targets[0]));
    burned.phase = NicFirmwarePhase::Rebooting {
        started_at: db::machine::database_now(&env.pool).await.unwrap(),
        last_attempt_at: None,
    };
    seed(&env, host_id, burned.clone()).await;
    record_cleanup_failure(&env, dpu_id.into(), "failure during committed activation").await;
    env.api
        .machine_state_handler_enqueuer
        .enqueue_object(&host_id)
        .await
        .unwrap();
    sqlx::query("UPDATE machines SET last_reboot_time = NULL WHERE id = $1")
        .bind(host_id)
        .execute(&env.pool)
        .await
        .unwrap();
    let activation_power = env.redfish_sim.timepoint();
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    let activating = operation(&env, host_id).await;
    let NicFirmwarePhase::Rebooting {
        started_at,
        last_attempt_at: Some(attempt),
    } = activating.phase
    else {
        panic!("recorded failure must retain the committed reboot attempt")
    };
    assert!(
        matches!(burned.phase, NicFirmwarePhase::Rebooting { started_at: original, .. } if started_at == original)
    );
    assert_eq!(
        machine(&env, host_id)
            .await
            .status
            .last_reboot_requested
            .unwrap()
            .time,
        attempt
    );
    assert_eq!(activating.targets, burned.targets);
    assert_eq!(activating.purpose, burned.purpose);
    assert_eq!(
        env.redfish_sim.actions_since(&activation_power).all_hosts(),
        vec![RedfishSimAction::Power(SystemPowerControl::ForceRestart)]
    );
    env.api
        .reboot_completed(authenticated(
            rpc::forge::MachineRebootCompletedRequest {
                machine_id: Some(host_id.into()),
            },
            host_id.into(),
        ))
        .await
        .unwrap();
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    assert!(matches!(
        operation(&env, host_id).await.phase,
        NicFirmwarePhase::Reobserving { .. }
    ));
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    let restoring = operation(&env, host_id).await;
    assert!(matches!(
        &restoring.phase,
        NicFirmwarePhase::RestoringLockdown {
            target_index: 0,
            failure: Some(reason),
            ..
        } if reason.contains(&dpu_id.to_string()) && reason.contains("Scout/nvme_clean_failed")
    ));
    assert_eq!(restoring.targets, burned.targets);
    assert_eq!(restoring.purpose, burned.purpose);
}

// Each lifecycle resumes its persisted continuation after independent
// lockdown readback, while allocation configuration edits preserve ownership.
#[crate::sqlx_test]
async fn safe_completion_resumes_the_frozen_lifecycle_owner(pool: sqlx::PgPool) {
    enum Continuation {
        InitialPreparation,
        Allocation,
        RestoreBaseline,
    }
    let env = environment(pool).await;
    let segment = env.create_vpc_and_tenant_segment().await;
    for scenario in [
        // Initial preparation preserves the discovered host's reboot-wait choice.
        Continuation::InitialPreparation,
        // Allocation keeps its live instance across ordinary config edits.
        Continuation::Allocation,
        // Release keeps the deleted instance and original cleanup reason.
        Continuation::RestoreBaseline,
    ] {
        let host = create_managed_host(&env).await;
        let host_id = *host.id;
        let mut admitted = fixtures::operation();
        let (purpose, expected) = match scenario {
            Continuation::InitialPreparation => (
                NicFirmwarePurpose::Baseline {
                    continuation: NicFirmwareBaselineContinuation::InitialPreparation {
                        skip_reboot_wait: true,
                    },
                },
                ManagedHostState::HostInit {
                    machine_state: MachineState::Discovered {
                        skip_reboot_wait: true,
                    },
                },
            ),
            Continuation::Allocation | Continuation::RestoreBaseline => {
                let owner = allocate_owner(&env, &host, segment).await;
                if matches!(scenario, Continuation::Allocation) {
                    (
                        NicFirmwarePurpose::Allocation {
                            instance_id: owner.id(),
                        },
                        ManagedHostState::Assigned {
                            instance_state: InstanceState::DpaProvisioning,
                        },
                    )
                } else {
                    let mut txn = env.pool.begin().await.unwrap();
                    db::instance::mark_as_deleted(owner.id(), txn.as_mut())
                        .await
                        .unwrap();
                    txn.commit().await.unwrap();
                    (
                        NicFirmwarePurpose::RestoreBaseline {
                            instance_id: owner.id(),
                            cleanup_context: CleanupContext::Reset,
                        },
                        ManagedHostState::WaitingForCleanup {
                            cleanup_state: CleanupState::Init,
                            cleanup_context: CleanupContext::Reset,
                        },
                    )
                }
            }
        };
        admitted.purpose = purpose.clone();
        admitted.phase = NicFirmwarePhase::RestoringLockdown {
            target_index: 0,
            failure: None,
        };
        admitted.targets[0].receipt = Some(complete(&admitted, &admitted.targets[0]));
        seed(&env, host_id, admitted.clone()).await;
        if let NicFirmwarePurpose::Allocation { instance_id } = purpose {
            let owner = env.one_instance(instance_id).await;
            let previous_version = owner.config_version();
            let mut config = owner.config().inner().clone();
            config.os.as_mut().expect("owner OS config").user_data =
                Some("edited during firmware recovery".into());
            env.api
                .update_instance_config(Request::new(rpc::forge::InstanceConfigUpdateRequest {
                    instance_id: Some(instance_id),
                    if_version_match: Some(previous_version.to_string()),
                    config: Some(config),
                    metadata: Some(owner.metadata().clone()),
                }))
                .await
                .expect("ordinary config edit preserves the allocation owner");
            assert_ne!(
                env.one_instance(instance_id).await.config_version(),
                previous_version
            );
            assert_eq!(operation(&env, host_id).await, admitted);
        }

        report_lockdown(
            &env,
            host_id,
            &admitted.targets[0],
            status(&admitted.targets[0], false),
        )
        .await;
        let before = machine(&env, host_id).await;
        env.run_machine_state_controller_iteration_without_requeue()
            .await;
        let finished = machine(&env, host_id).await;
        assert_eq!(finished.state.value, expected);
        assert_ne!(finished.state.version, before.state.version);
        if let NicFirmwarePurpose::Allocation { instance_id }
        | NicFirmwarePurpose::RestoreBaseline { instance_id, .. } = purpose
        {
            let owner = db::instance::find_by_id(&env.pool, instance_id)
                .await
                .expect("read retained lifecycle owner")
                .expect("lifecycle owner exists");
            assert!(
                admitted
                    .purpose
                    .matches_owner(Some((owner.id, owner.deleted.is_some())))
            );
        }
    }
}

// A stalled BMC request must reach the controller's real attempt deadline while
// retaining the committed burn and activation intent for later reconciliation.
#[crate::sqlx_test]
async fn activation_timeout_keeps_the_committed_attempt_without_accepting_a_reboot(
    pool: sqlx::PgPool,
) {
    let env = environment(pool).await;
    let host = create_managed_host(&env).await;
    let host_id = *host.id;
    let mut admitted = fixtures::operation();
    admitted.targets[0].receipt = Some(complete(&admitted, &admitted.targets[0]));
    let started_at = sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(&env.pool)
        .await
        .expect("read the activation boundary from the database clock");
    admitted.phase = NicFirmwarePhase::Rebooting {
        started_at,
        last_attempt_at: None,
    };
    seed(&env, host_id, admitted.clone()).await;
    sqlx::query("UPDATE machines SET last_reboot_time = NULL WHERE id = $1")
        .bind(host_id)
        .execute(&env.pool)
        .await
        .expect("clear fixture reboot feedback");
    let before = machine(&env, host_id).await;
    let power_before = env.redfish_sim.timepoint();
    let mut power_started = env.redfish_sim.delay_next_power_action(
        SystemPowerControl::ForceRestart,
        std::time::Duration::from_secs(240),
    );
    let mut txn = env.db_txn().await;
    let mut snapshot = host.snapshot(&mut txn).await;
    txn.commit()
        .await
        .expect("commit the activation snapshot read");
    let state = snapshot.host_snapshot.state.value.clone();
    let mut services = env.machine_state_handler_services();
    let mut metrics = MachineMetrics::default();
    let mut pending_db_writes = DbWriteBatch::new();
    let mut ctx = StateHandlerContext {
        services: &mut services,
        metrics: &mut metrics,
        pending_db_writes: &mut pending_db_writes,
    };
    let outcome = {
        let attempt = env.machine_state_handler.handle_object_state(
            &host_id,
            &mut snapshot,
            &state,
            &mut ctx,
        );
        tokio::pin!(attempt);
        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            tokio::select! {
                _ = &mut attempt => panic!("activation returned before the delayed power call"),
                started = &mut power_started => started.expect("the delayed power call must start"),
            }
        })
        .await
        .expect("activation prerequisites must reach the power call");

        // Poll the handler directly so advancing time cannot dispatch a spawned
        // controller task that starts SQL while Tokio's clock is paused. Both
        // the production deadline and the longer simulator sleep already exist.
        tokio::time::pause();
        tokio::time::advance(std::time::Duration::from_secs(120)).await;
        tokio::time::resume();
        tokio::time::timeout(std::time::Duration::from_secs(30), &mut attempt)
            .await
            .expect("the production deadline must cancel the pending Redfish call")
            .expect("a timed out activation must remain recoverable")
    };
    assert!(matches!(outcome, StateHandlerOutcome::Wait { reason, .. }
        if reason.contains("waiting for Scout to report")));
    let mut txn = env.db_txn().await;
    pending_db_writes
        .apply_all(&mut txn)
        .await
        .expect("apply the timed out handler's queued writes");
    txn.commit().await.expect("commit the queued writes");

    let after = machine(&env, host_id).await;
    let timed_out = operation(&env, host_id).await;
    let NicFirmwarePhase::Rebooting {
        started_at: retained_start,
        last_attempt_at: Some(attempted_at),
    } = timed_out.phase
    else {
        panic!("a timeout must preserve the committed activation attempt")
    };
    assert_eq!(retained_start, started_at);
    assert!(attempted_at > started_at);
    let mut expected = admitted;
    expected.phase = timed_out.phase.clone();
    assert_eq!(timed_out, expected);
    assert_eq!(
        after.status.last_reboot_requested,
        before.status.last_reboot_requested
    );
    assert!(after.status.last_reboot_time.is_none());
    assert!(
        env.redfish_sim
            .actions_since(&power_before)
            .all_hosts()
            .is_empty()
    );
    assert_eq!(action(&env, host_id).await, fac::Action::noop());
}

// Key identity is an intervention boundary. Unknown status and an unavailable
// credential are transient evidence: retain the request, then read back again.
#[crate::sqlx_test]
async fn lockdown_blockers_are_visible_and_transient_failures_can_retry(pool: sqlx::PgPool) {
    use model::machine::nic_firmware::NicFirmwareLockdownStatus;

    let env = environment(pool).await;
    let host = create_managed_host(&env).await;
    let host_id = *host.id;
    let mut admitted = fixtures::operation();
    admitted.phase = NicFirmwarePhase::Unlocking { target_index: 0 };
    admitted.targets[0].supervisor_instance_id = Some("helper".into());
    for step in [
        NicFirmwareLockdownStep::Observe,
        NicFirmwareLockdownStep::Change,
    ] {
        let mut keyless = admitted.clone();
        keyless.lockdown.step = step;
        keyless.lockdown.result = (step == NicFirmwareLockdownStep::Observe).then_some(
            NicFirmwareLockdownResult::Status(NicFirmwareLockdownStatus::Locked),
        );
        seed(&env, host_id, keyless.clone()).await;
        env.run_machine_state_controller_iteration_without_requeue()
            .await;
        assert_eq!(operation(&env, host_id).await, keyless);
        assert!(
            matches!(machine(&env, host_id).await.controller_state_outcome,
            Some(PersistentStateHandlerOutcome::Error { err, .. }) if err.contains("frozen key identity"))
        );
        assert_eq!(action(&env, host_id).await, fac::Action::noop());
    }

    seed(&env, host_id, admitted.clone()).await;
    let mut unknown = status(&admitted.targets[0], false);
    let LockdownResult::StatusReport(report) = &mut unknown else {
        unreachable!()
    };
    report.status = pb::LockStatus::Unknown.into();
    report_lockdown(&env, host_id, &admitted.targets[0], unknown).await;
    assert!(matches!(operation(&env, host_id).await.lockdown.result,
        Some(NicFirmwareLockdownResult::Error { diagnostic }) if diagnostic.contains("status is unknown")));
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    let retrying = operation(&env, host_id).await;
    assert_eq!(retrying.phase, admitted.phase);
    assert_eq!(retrying.lockdown.step, NicFirmwareLockdownStep::Observe);
    assert!(retrying.lockdown.result.is_none());
    assert!(
        retrying.targets[0]
            .diagnostic
            .as_deref()
            .unwrap()
            .contains("status is unknown")
    );
    report_lockdown(
        &env,
        host_id,
        &admitted.targets[0],
        status(&admitted.targets[0], false),
    )
    .await;
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    assert_eq!(
        operation(&env, host_id).await.phase,
        NicFirmwarePhase::UpdatingFirmware { target_index: 0 }
    );

    // Version 7 has deliberately not been provisioned in this credential store.
    // The failed render must become correlated evidence before FAC returns Noop.
    let NicFirmwareCardSelector::Mac(mac_address) = admitted.targets[0].selector else {
        unreachable!()
    };
    admitted.targets[0].original_lockdown = NicFirmwareLockdownPolicy::Locked {
        ikm_version: 7,
        mac_address,
    };
    seed(&env, host_id, admitted.clone()).await;
    report_lockdown(
        &env,
        host_id,
        &admitted.targets[0],
        status(&admitted.targets[0], true),
    )
    .await;
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    assert_eq!(
        operation(&env, host_id).await.lockdown.step,
        NicFirmwareLockdownStep::Change
    );
    assert_eq!(action(&env, host_id).await, fac::Action::noop());
    let unavailable = operation(&env, host_id).await;
    assert_eq!(unavailable.phase, admitted.phase);
    assert!(matches!(unavailable.lockdown.result,
        Some(NicFirmwareLockdownResult::Error { diagnostic }) if diagnostic.contains("credentials are unavailable")));
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    let retrying = operation(&env, host_id).await;
    assert_eq!(retrying.lockdown.step, NicFirmwareLockdownStep::Observe);
    assert!(retrying.lockdown.result.is_none());
    assert!(
        retrying.targets[0]
            .diagnostic
            .as_deref()
            .unwrap()
            .contains("credentials are unavailable")
    );
    env.test_credential_manager
        .set_credentials(
            &CredentialKey::NicLockdownIkm {
                credential_type: NicLockdownIkm::SiteWide { version: 7 },
            },
            &Credentials::UsernamePassword {
                username: String::new(),
                password: "recovered-ikm".into(),
            },
        )
        .await
        .unwrap();
    report_lockdown(
        &env,
        host_id,
        &admitted.targets[0],
        status(&admitted.targets[0], true),
    )
    .await;
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    let unlock = report_lockdown(
        &env,
        host_id,
        &admitted.targets[0],
        LockdownResult::Changed(Default::default()),
    )
    .await;
    assert!(matches!(unlock.request, Some(LockdownRequest::Unlock(_))));
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    report_lockdown(
        &env,
        host_id,
        &admitted.targets[0],
        status(&admitted.targets[0], false),
    )
    .await;
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    let recovered = operation(&env, host_id).await;
    assert_eq!(
        recovered.phase,
        NicFirmwarePhase::UpdatingFirmware { target_index: 0 }
    );
    assert_eq!(
        recovered.targets[0].supervisor_instance_id,
        admitted.targets[0].supervisor_instance_id
    );
    assert_eq!(
        recovered.targets[0].original_lockdown,
        admitted.targets[0].original_lockdown
    );
    assert!(recovered.targets[0].diagnostic.is_none());
}

// A timeout while the state-history write is blocked must roll the attempt
// back. Repeating the report after that dependency recovers commits one receipt.
#[crate::sqlx_test]
async fn firmware_report_deadline_rolls_back_before_retry(pool: sqlx::PgPool) {
    let env = environment(pool).await;
    let host = create_managed_host(&env).await;
    let host_id = *host.id;
    let mut admitted = fixtures::operation();
    admitted.phase = NicFirmwarePhase::UpdatingFirmware { target_index: 0 };
    admitted.targets[0].supervisor_instance_id = Some("helper".into());
    seed(&env, host_id, admitted.clone()).await;
    sqlx::query("DELETE FROM machine_state_controller_queued_objects WHERE object_id = $1")
        .bind(host_id.to_string())
        .execute(&env.pool)
        .await
        .unwrap();
    let before = machine(&env, host_id).await;
    let receipt = complete(&admitted, &admitted.targets[0]);
    let report = publication(
        host_id,
        observation(
            &admitted,
            &admitted.targets[0],
            FirmwareResult::Receipt(receipt.clone().into()),
        ),
    );
    let mut gate = env.pool.begin().await.unwrap();
    let gate_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(gate.as_mut())
        .await
        .unwrap();
    sqlx::query("LOCK TABLE machine_state_history IN SHARE MODE")
        .execute(gate.as_mut())
        .await
        .unwrap();
    let error = {
        let attempt = env
            .api
            .publish_mlx_observation_report(authenticated(report.clone(), host_id.into()));
        tokio::pin!(attempt);
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            tokio::select! {
                _ = &mut attempt => panic!("the held history write must not finish"),
                _blocked_pid = wait_for_blocked_query(&env.pool, gate_pid, "INSERT INTO machine_state_history") => {},
            }
        }).await.expect("the real report write must reach the database gate");
        tokio::time::pause();
        tokio::time::advance(std::time::Duration::from_secs(30)).await;
        tokio::time::resume();
        tokio::time::timeout(std::time::Duration::from_secs(5), &mut attempt)
            .await
            .expect("the report persistence deadline must expire")
            .expect_err("a timed out write must not be acknowledged")
    };
    assert_eq!(error.code(), Code::Unavailable);
    let after = machine(&env, host_id).await;
    assert_eq!(after.state.value, before.state.value);
    assert_eq!(after.state.version, before.state.version);
    assert!(!queued(&env, host_id).await);
    gate.rollback().await.unwrap();
    env.api
        .publish_mlx_observation_report(authenticated(report, host_id.into()))
        .await
        .expect("the identical report must retry after the write gate is released");
    let retried = machine(&env, host_id).await;
    assert_eq!(
        operation(&env, host_id).await.targets[0].receipt,
        Some(receipt)
    );
    assert_eq!(
        retried.state.version.version_nr(),
        before.state.version.version_nr() + 1
    );
    assert!(queued(&env, host_id).await);
}

// Ordinary tenant power cannot overtake an admitted uncertain burn, including
// the boot and phone-home writes that precede a normal synchronous reboot.
#[crate::sqlx_test]
async fn instance_power_preserves_firmware_ownership_and_boot_fields(pool: sqlx::PgPool) {
    let env = environment(pool).await;
    let host = create_managed_host(&env).await;
    let host_id = *host.id;
    let segment = env.create_vpc_and_tenant_segment().await;
    let owner = allocate_owner(&env, &host, segment).await;
    sqlx::query("UPDATE instances SET os_phone_home_enabled = true, os_always_boot_with_ipxe = false, phone_home_last_contact = clock_timestamp(), use_custom_pxe_on_boot = false, custom_pxe_reboot_requested = false WHERE id = $1")
        .bind(owner.id()).execute(&env.pool).await.unwrap();
    let mut admitted = fixtures::operation();
    admitted.purpose = NicFirmwarePurpose::Allocation {
        instance_id: owner.id(),
    };
    admitted.phase = NicFirmwarePhase::UpdatingFirmware { target_index: 0 };
    let mut running = complete(&admitted, &admitted.targets[0]);
    running.state = FirmwareOperationState::Running;
    running.report = FirmwareFlashReport::default();
    admitted.targets[0].receipt = Some(running);
    seed(&env, host_id, admitted.clone()).await;
    let before = machine(&env, host_id).await;
    let instance_before = db::instance::find_by_id(&env.pool, owner.id())
        .await
        .unwrap()
        .unwrap();
    assert!(
        instance_before
            .observations
            .phone_home_last_contact
            .is_some()
    );
    let power_before = env.redfish_sim.timepoint();
    let error = env
        .api
        .invoke_instance_power(Request::new(rpc::forge::InstancePowerRequest {
            instance_id: Some(owner.id()),
            operation: rpc::forge::instance_power_request::Operation::PowerReset.into(),
            boot_with_custom_ipxe: true,
            apply_updates_on_reboot: false,
        }))
        .await
        .expect_err("ordinary power must not preempt admitted NIC work");
    assert_eq!(error.code(), Code::FailedPrecondition);
    let after = machine(&env, host_id).await;
    let instance_after = db::instance::find_by_id(&env.pool, owner.id())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after.state.value, before.state.value);
    assert_eq!(after.state.version, before.state.version);
    assert_eq!(
        after.status.last_reboot_requested,
        before.status.last_reboot_requested
    );
    assert_eq!(
        instance_after.config_version,
        instance_before.config_version
    );
    assert_eq!(
        serde_json::to_value(&instance_after.config).unwrap(),
        serde_json::to_value(&instance_before.config).unwrap()
    );
    assert_eq!(
        instance_after.observations.phone_home_last_contact,
        instance_before.observations.phone_home_last_contact
    );
    assert_eq!(
        instance_after.use_custom_pxe_on_boot,
        instance_before.use_custom_pxe_on_boot
    );
    assert_eq!(
        instance_after.custom_pxe_reboot_requested,
        instance_before.custom_pxe_reboot_requested
    );
    assert!(
        env.redfish_sim
            .actions_since(&power_before)
            .all_hosts()
            .is_empty()
    );
}

// Platforms whose Redfish restart removes DPU power retain their existing IPMI
// route, including the exact database-clock firmware attempt timestamp.
#[crate::sqlx_test]
async fn firmware_activation_preserves_the_ipmi_route_and_committed_request_time(
    pool: sqlx::PgPool,
) {
    use std::net::SocketAddr;
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use carbide_secrets::credentials::BmcCredentialType;

    #[derive(Default)]
    struct RecordingIpmi {
        calls: Mutex<Vec<(SocketAddr, bool, CredentialKey)>>,
    }
    #[async_trait]
    impl carbide_ipmi::IPMITool for RecordingIpmi {
        async fn restart(
            &self,
            address: SocketAddr,
            legacy_boot: bool,
            credential_key: &CredentialKey,
        ) -> Result<(), eyre::Report> {
            self.calls
                .lock()
                .unwrap()
                .push((address, legacy_boot, credential_key.clone()));
            Ok(())
        }
        async fn bmc_cold_reset(
            &self,
            _: SocketAddr,
            _: &CredentialKey,
        ) -> Result<(), eyre::Report> {
            panic!("firmware host activation must request a chassis restart")
        }
    }

    let env = environment(pool).await;
    let host = create_managed_host(&env).await;
    let host_id = *host.id;
    let mut txn = env.db_txn().await;
    let address = host.host().bmc_ip(&mut txn).await.unwrap();
    let endpoint = db::explored_endpoints::find_by_ips(txn.as_mut(), vec![address])
        .await
        .unwrap()
        .pop()
        .unwrap();
    let mut report = endpoint.report;
    report.vendor = Some(bmc_vendor::BMCVendor::Lenovo);
    report.model = Some("SR650 V4".into());
    assert_eq!(
        db::explored_endpoints::try_update(
            address,
            endpoint.report_version,
            &report,
            endpoint.waiting_for_explorer_refresh,
            txn.as_mut()
        )
        .await
        .unwrap(),
        db::ConditionalWrite::Applied(())
    );
    txn.commit().await.unwrap();
    let mut admitted = fixtures::operation();
    admitted.targets[0].receipt = Some(complete(&admitted, &admitted.targets[0]));
    admitted.phase = NicFirmwarePhase::Rebooting {
        started_at: db::machine::database_now(&env.pool).await.unwrap(),
        last_attempt_at: None,
    };
    seed(&env, host_id, admitted.clone()).await;
    sqlx::query("UPDATE machines SET last_reboot_time = NULL WHERE id = $1")
        .bind(host_id)
        .execute(&env.pool)
        .await
        .unwrap();
    let mut txn = env.db_txn().await;
    let mut snapshot = host.snapshot(&mut txn).await;
    txn.commit().await.unwrap();
    let bmc_mac = snapshot.host_snapshot.status.bmc_info.mac.unwrap();
    let state = snapshot.host_snapshot.state.value.clone();
    let ipmi = Arc::new(RecordingIpmi::default());
    let mut services = env.machine_state_handler_services();
    services.ipmi_tool = ipmi.clone();
    let mut metrics = MachineMetrics::default();
    let mut writes = DbWriteBatch::new();
    let mut ctx = StateHandlerContext {
        services: &mut services,
        metrics: &mut metrics,
        pending_db_writes: &mut writes,
    };
    let power_before = env.redfish_sim.timepoint();
    let outcome = env
        .machine_state_handler
        .handle_object_state(&host_id, &mut snapshot, &state, &mut ctx)
        .await
        .expect("the IPMI activation attempt must succeed");
    assert!(matches!(outcome, StateHandlerOutcome::Wait { .. }));
    let mut txn = env.db_txn().await;
    writes.apply_all(&mut txn).await.unwrap();
    txn.commit().await.unwrap();
    {
        let calls = ipmi.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        let (address_used, legacy_boot, credential_key) = &calls[0];
        assert_eq!(
            *address_used,
            SocketAddr::new(address, carbide_ipmi::DEFAULT_IPMI_PORT)
        );
        assert!(!legacy_boot);
        assert!(matches!(credential_key, CredentialKey::BmcCredentials {
            credential_type: BmcCredentialType::BmcRoot { bmc_mac_address }
        } if *bmc_mac_address == bmc_mac));
    }
    assert!(
        env.redfish_sim
            .actions_since(&power_before)
            .all_hosts()
            .is_empty()
    );
    let retained = operation(&env, host_id).await;
    let NicFirmwarePhase::Rebooting {
        last_attempt_at: Some(attempted_at),
        ..
    } = retained.phase
    else {
        panic!("IPMI activation must preserve its committed attempt")
    };
    assert_eq!(retained.targets, admitted.targets);
    let requested = machine(&env, host_id)
        .await
        .status
        .last_reboot_requested
        .unwrap();
    assert_eq!(requested.time, attempted_at);
    assert_eq!(requested.mode, MachineLastRebootRequestedMode::Reboot);
}

/// An unresolved legacy command must block both Scout delivery and the persisted
/// controller branch when it consumes a saved operation.
#[crate::sqlx_test]
async fn hardware_dispatch_blocks_saved_nic_work_until_exact_settlement(pool: sqlx::PgPool) {
    use model::hardware_dispatch::{HardwareDispatchOwner, MlxAdminCommand};

    let env = environment(pool).await;
    let host = create_managed_host(&env).await;
    let host_id = *host.id;
    let owner = HardwareDispatchOwner::MlxAdmin {
        machine_id: host_id.into(),
        command: MlxAdminCommand::ConfigSet,
    };
    let token = uuid::Uuid::new_v4();
    let mut txn = env.pool.begin().await.unwrap();
    db::hardware_dispatch::begin(&mut txn, token, &owner)
        .await
        .unwrap();
    txn.commit().await.unwrap();
    // Reproduce a saved NIC operation encountering an already committed command;
    // this fixture write is not a new production admission path.
    let admitted = fixtures::operation();
    seed(&env, host_id, admitted.clone()).await;
    let before = machine(&env, host_id).await;
    assert!(matches!(action(&env, host_id).await, fac::Action::Noop(_)));
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    let blocked = machine(&env, host_id).await;
    assert_eq!(blocked.state.version, before.state.version);
    assert_eq!(operation(&env, host_id).await, admitted);
    assert_eq!(
        blocked.status.last_reboot_requested,
        before.status.last_reboot_requested
    );
    assert!(matches!(blocked.controller_state_outcome,
        Some(PersistentStateHandlerOutcome::Wait { reason, .. })
            if reason == "an earlier hardware command is still awaiting its result"));
    assert!(
        blocked
            .status
            .hardware_dispatch
            .as_ref()
            .unwrap()
            .is_pending()
    );

    // Exact settlement permits saved work to resume. No
    // baseline selection or automatic admission is enabled by this test.
    let mut txn = env.pool.begin().await.unwrap();
    assert!(
        db::hardware_dispatch::settle(&mut txn, host_id, token, &owner)
            .await
            .unwrap()
    );
    txn.commit().await.unwrap();
    assert!(matches!(
        command(action(&env, host_id).await),
        fac::mlx_device_action::Command::ObserveFirmwareUpdate(_)
    ));
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    let resumed = machine(&env, host_id).await;
    assert!(matches!(resumed.controller_state_outcome,
        Some(PersistentStateHandlerOutcome::Wait { reason, .. })
            if reason == "waiting for the initial NIC firmware supervisor observation"));
    assert_eq!(operation(&env, host_id).await, admitted);
}

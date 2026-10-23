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

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use carbide_dpa::DpaInfo;
use carbide_dpa_manager::DpaMonitor;
use carbide_instrument::testing::capture_logs_async;
use carbide_uuid::machine::{HostMachineId, MachineId};
use futures::StreamExt;
use model::dpa_interface::{
    CardState, DpaInterface, DpaInterfaceControllerState, DpaInterfaceType, DpaLockMode,
    DpaSearchConfig, NewDpaInterface,
};
use model::machine::status::MlxDeviceObservation;
use prost::Message;
use rpc::forge::forge_server::Forge;
use rpc::forge::{
    ScoutStreamApiBoundMessage, ScoutStreamInitRequest, scout_stream_api_bound_message,
};
use rpc::forge_agent_control_response as fac;
use rpc::protos::mlx_device::{
    FirmwareFlashReport, FirmwareOperation, FirmwareOperationState,
    MlxAdminDeviceIdentitiesRequest, MlxAdminDeviceReportRequest, MlxDeviceIdentity,
    MlxDeviceIdentityReport, MlxDeviceInfo, MlxDeviceInfoReportResponse, MlxDeviceReport,
    MlxObservation, MlxObservationReport, PublishMlxDeviceReportRequest,
    PublishMlxObservationReportRequest, mlx_device_info_report_response,
};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Streaming};
use tonic_prost::ProstDecoder;

use crate::api::{Api, ScoutStreamType};
use crate::auth::AuthContext;
use crate::tests::common::api_fixtures::{
    TestEnvOverrides, create_managed_host, create_managed_host_multi_dpu, create_test_env,
    create_test_env_with_overrides, get_config,
};

fn machine_request<T>(message: T, machine_id: MachineId) -> Request<T> {
    let mut request = Request::new(message);
    let mut context = AuthContext::default();
    context.principals.push(
        carbide_authn::middleware::Principal::SpiffeMachineIdentifier(machine_id.to_string()),
    );
    request.extensions_mut().insert(context);
    request
}

fn report(machine_id: Option<MachineId>, seconds: i64) -> MlxDeviceReport {
    MlxDeviceReport {
        machine_id,
        timestamp: Some(prost_types::Timestamp { seconds, nanos: 0 }.into()),
        devices: vec![MlxDeviceInfo {
            pci_name: "01:00.0".into(),
            device_type: "ConnectX-8".into(),
            part_number: "unknown-part-number".into(),
            fw_version_current: seconds.to_string(),
            ..Default::default()
        }],
        ..Default::default()
    }
}

async fn observation(
    pool: &sqlx::PgPool,
    machine_id: HostMachineId,
) -> Option<MlxDeviceObservation> {
    db::machine::find_one(pool, &machine_id, Default::default())
        .await
        .unwrap()
        .unwrap()
        .status
        .mlx_device_observation
}

/// Persist an active firmware attempt so receipt tests use a real card ID and
/// controller version without requiring unrelated provisioning transitions.
async fn firmware_test_card(
    pool: &sqlx::PgPool,
    machine_id: HostMachineId,
    mac_address: &str,
    pci_name: &str,
) -> DpaInterface {
    let mut txn = pool.begin().await.unwrap();
    let mut card = db::dpa_interface::persist(
        NewDpaInterface {
            machine_id,
            mac_address: mac_address.parse().unwrap(),
            device_type: "SuperNIC".into(),
            pci_name: pci_name.into(),
            device_description: None,
            interface_type: DpaInterfaceType::Svpc,
        },
        &mut txn,
    )
    .await
    .unwrap();
    let version = card.controller_state.version.increment();
    assert_eq!(
        db::dpa_interface::try_update_controller_state(
            &mut txn,
            card.id,
            card.controller_state.version,
            version,
            &DpaInterfaceControllerState::ApplyFirmware,
        )
        .await
        .unwrap(),
        db::ConditionalWrite::Applied(())
    );
    txn.commit().await.unwrap();
    card.controller_state.value = DpaInterfaceControllerState::ApplyFirmware;
    card.controller_state.version = version;
    card
}

/// Canonical inventory upgrades a saved MST locator without creating a new card
/// or firmware attempt; subsequent reports cannot move or regress that locator.
#[crate::sqlx_test]
async fn canonical_inventory_upgrades_mst_locator_without_restarting_firmware(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let host = create_managed_host(&env).await;
    let host_id: HostMachineId = host.id.into();
    let machine_id: MachineId = host_id.into();
    let mut card = firmware_test_card(
        &env.pool,
        host_id,
        "00:11:22:33:44:55",
        "/dev/mst/mt41692_pciconf0",
    )
    .await;
    let operation = FirmwareOperation {
        operation_id: card.firmware_operation_id(),
        state: FirmwareOperationState::Complete.into(),
        report: Some(FirmwareFlashReport {
            flashed: true,
            reset: Some(true),
            ..Default::default()
        }),
        ..Default::default()
    };
    card.card_state = Some(CardState {
        lockmode: Some(DpaLockMode::Unlocked),
        profile: Some("retained-profile".into()),
        firmware_report: operation.report.clone().map(Into::into),
        firmware_operation: Some(operation.clone().try_into().unwrap()),
        ..Default::default()
    });
    // Non-default intent and a completed receipt make replacing the whole row
    // distinguishable from changing only its locator.
    let mut network_config = card.network_config.value.clone();
    network_config.use_admin_network = Some(false);
    let mut txn = env.db_txn().await;
    assert_eq!(
        db::dpa_interface::try_update_network_config(
            &mut txn,
            &card.id,
            card.network_config.version,
            &network_config,
        )
        .await
        .unwrap(),
        db::ConditionalWrite::Applied(card.id)
    );
    db::dpa_interface::update_card_state(&mut txn, card)
        .await
        .unwrap();
    txn.commit().await.unwrap();
    let before =
        db::dpa_interface::find_by_machine_id(&env.pool, host_id, DpaSearchConfig::default())
            .await
            .unwrap()
            .pop()
            .unwrap();

    for (scenario, reported_pci_name, expected_pci_name) in [
        // The shared numeric parser trims whitespace; the upgrade must require
        // an actually canonical spelling rather than a padded short domain.
        (
            "retain MST locator after whitespace-padded report",
            " 000:cc:00.0",
            "/dev/mst/mt41692_pciconf0",
        ),
        // A new Scout resolves the old MST alias to its full BDF before publishing.
        ("upgrade MST locator", "0000:cc:00.0", "0000:cc:00.0"),
        // A report from an older Scout must not undo the one-way upgrade.
        (
            "retain BDF after legacy report",
            "/dev/mst/mt41692_pciconf0",
            "0000:cc:00.0",
        ),
        // Matching MAC alone is not authority to relocate an established BDF.
        (
            "retain BDF after different PCI report",
            "0000:dd:00.0",
            "0000:cc:00.0",
        ),
    ] {
        let mut device_report = report(Some(machine_id), 2);
        let device = &mut device_report.devices[0];
        device.base_mac = before.mac_address.to_string();
        device.pci_name = reported_pci_name.into();
        device.device_description = "BlueField-3 SuperNIC".into();
        env.api
            .publish_mlx_device_report(machine_request(
                PublishMlxDeviceReportRequest {
                    report: Some(device_report),
                },
                machine_id,
            ))
            .await
            .unwrap();

        // Read independently of the report response and inspect the real Core
        // command builder, not just the saved device-info payload.
        let cards =
            db::dpa_interface::find_by_machine_id(&env.pool, host_id, DpaSearchConfig::default())
                .await
                .unwrap();
        assert_eq!(cards.len(), 1, "{scenario}");
        let card = &cards[0];
        assert_eq!(card.id, before.id, "{scenario}");
        assert_eq!(card.machine_id, before.machine_id, "{scenario}");
        assert_eq!(card.mac_address, before.mac_address, "{scenario}");
        assert_eq!(card.pci_name, expected_pci_name, "{scenario}");
        assert_eq!(
            card.controller_state.value, before.controller_state.value,
            "{scenario}"
        );
        assert_eq!(
            card.controller_state.version, before.controller_state.version,
            "{scenario}"
        );
        assert_eq!(
            card.network_config.value, before.network_config.value,
            "{scenario}"
        );
        assert_eq!(
            card.network_config.version, before.network_config.version,
            "{scenario}"
        );
        assert_eq!(
            serde_json::to_value(&card.card_state).unwrap(),
            serde_json::to_value(&before.card_state).unwrap(),
            "{scenario}"
        );

        let fac::Action::MlxAction(action) =
            env.api.process_scout_req_for_test(host_id).await.unwrap()
        else {
            panic!("{scenario}: expected firmware control action");
        };
        assert_eq!(
            action.device_actions,
            vec![fac::MlxDeviceAction {
                pci_name: expected_pci_name.into(),
                command: Some(fac::mlx_device_action::Command::ApplyFirmwareOperation(
                    fac::MlxDeviceApplyFirmwareOperation {
                        operation_id: operation.operation_id.clone(),
                        profile: None,
                    }
                )),
            }],
            "{scenario}"
        );
    }
}

/// Only a successful completed operation for the current state entry advances
/// the DPA monitor, including no-work completion; omitted or stale results
/// cannot reuse an earlier success.
#[crate::sqlx_test]
async fn firmware_operation_reports_only_complete_the_matching_attempt(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let host = create_managed_host(&env).await;
    let host_id: HostMachineId = host.id.into();
    let machine_id: MachineId = host_id.into();
    let mut txn = env.db_txn().await;
    let mut card = db::dpa_interface::persist(
        NewDpaInterface {
            machine_id: host_id,
            mac_address: "00:11:22:33:44:55".parse().unwrap(),
            device_type: "SuperNIC".into(),
            pci_name: "0000:cc:00.0".into(),
            device_description: None,
            interface_type: DpaInterfaceType::Svpc,
        },
        &mut txn,
    )
    .await
    .unwrap();
    let previous_operation_id = card.firmware_operation_id();
    let mut apply_version = card.controller_state.version.increment();
    let operation_id = format!("dpa-firmware:{}:{apply_version}", card.id);
    assert_eq!(
        db::dpa_interface::try_update_controller_state(
            &mut txn,
            card.id,
            card.controller_state.version,
            apply_version,
            &DpaInterfaceControllerState::ApplyFirmware,
        )
        .await
        .unwrap(),
        db::ConditionalWrite::Applied(())
    );
    // A successful receipt from a prior state entry is still stored when the
    // controller starts another attempt. It must not skip this new work.
    let previous_operation = FirmwareOperation {
        operation_id: previous_operation_id.clone(),
        state: FirmwareOperationState::Complete.into(),
        phase: None,
        report: Some(FirmwareFlashReport {
            flashed: true,
            reset: Some(true),
            verified_image: Some(true),
            verified_version: Some(true),
            observed_version: Some("32.42.1000".into()),
            expected_version: Some("32.42.1000".into()),
        }),
        diagnostic: None,
    };
    card.card_state = Some(CardState {
        lockmode: Some(DpaLockMode::Unlocked),
        profile: Some("retained-profile".into()),
        profile_synced: Some(true),
        firmware_report: previous_operation.report.clone().map(Into::into),
        firmware_operation: Some(previous_operation.clone().try_into().unwrap()),
    });
    db::dpa_interface::update_card_state(&mut txn, card)
        .await
        .unwrap();
    txn.commit().await.unwrap();

    let config = env.config.ewethers_config.clone().unwrap();
    let mut monitor = DpaMonitor::new(
        env.pool.clone(),
        env.pool.clone().into(),
        Arc::new(DpaInfo {
            subnet_ip: config.subnet_ip,
            subnet_mask: config.subnet_mask,
            // ApplyFirmware consumes reports without issuing MQTT commands.
            mqtt_client: None,
        }),
        env.test_meter.meter(),
        config,
        env.config.host_health,
        env.api.work_lock_manager_handle.clone(),
    );
    monitor.run_single_iteration().await.unwrap();
    let cards =
        db::dpa_interface::find_by_machine_id(&env.pool, host_id, DpaSearchConfig::default())
            .await
            .unwrap();
    assert_eq!(cards.len(), 1);
    assert_eq!(
        cards[0].controller_state.value,
        DpaInterfaceControllerState::ApplyFirmware
    );
    assert_eq!(cards[0].controller_state.version, apply_version);
    assert_eq!(
        FirmwareOperation::from(
            cards[0]
                .card_state
                .as_ref()
                .unwrap()
                .firmware_operation
                .clone()
                .unwrap()
        ),
        previous_operation
    );
    assert_eq!(
        env.test_meter
            .formatted_metric("carbide_dpa_monitor_interfaces_scanned_count")
            .as_deref(),
        Some("1")
    );

    struct Case {
        scenario: &'static str,
        operation: Option<FirmwareOperation>,
        accepted: bool,
        expected_state: DpaInterfaceControllerState,
    }
    // Core readers without `firmware_operation` ignore that JSON field and
    // decide from `firmware_report` alone. Exercise that view of persisted data.
    #[derive(serde::Deserialize)]
    struct LegacyCardState {
        firmware_report: Option<carbide_libmlx_model::firmware::result::FirmwareFlashReport>,
    }
    let mut expected_operation = None;
    let mut expected_profile = "retained-profile";
    let legacy_report = FirmwareFlashReport {
        flashed: true,
        ..Default::default()
    };
    let mut completed_version: Option<config_version::ConfigVersion> = None;
    for mut case in [
        // A legacy observation still has no authority to complete this attempt.
        Case {
            scenario: "omitted operation clears prior attempt",
            operation: None,
            accepted: false,
            expected_state: DpaInterfaceControllerState::ApplyFirmware,
        },
        // A delayed success is ignored even when its phase results all passed.
        Case {
            scenario: "stale operation is ignored",
            operation: Some(previous_operation.clone()),
            accepted: false,
            expected_state: DpaInterfaceControllerState::ApplyFirmware,
        },
        // Burn/reset results cannot complete an operation that is still running.
        Case {
            scenario: "running burn cannot complete a legacy controller",
            operation: Some(FirmwareOperation {
                operation_id: operation_id.clone(),
                state: FirmwareOperationState::Running.into(),
                report: Some(FirmwareFlashReport {
                    flashed: true,
                    reset: Some(true),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            accepted: true,
            expected_state: DpaInterfaceControllerState::ApplyFirmware,
        },
        // Legacy controllers do not inspect verification fields themselves.
        Case {
            scenario: "failed verification cannot complete a legacy controller",
            operation: Some(FirmwareOperation {
                operation_id: operation_id.clone(),
                state: FirmwareOperationState::Complete.into(),
                report: Some(FirmwareFlashReport {
                    flashed: true,
                    reset: Some(true),
                    verified_image: Some(false),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            accepted: true,
            expected_state: DpaInterfaceControllerState::ApplyFirmware,
        },
        // Preserve the burn result when activation fails, without advancing.
        Case {
            scenario: "failed reset records partial progress",
            operation: Some(FirmwareOperation {
                operation_id: operation_id.clone(),
                state: FirmwareOperationState::Failed.into(),
                phase: None,
                report: Some(FirmwareFlashReport {
                    flashed: true,
                    reset: Some(false),
                    verified_image: Some(false),
                    verified_version: Some(false),
                    observed_version: None,
                    expected_version: Some("32.43.1014".into()),
                }),
                diagnostic: Some("activation failed".into()),
            }),
            accepted: true,
            expected_state: DpaInterfaceControllerState::ApplyFirmware,
        },
        // A legacy success must not replace the current failed-reset evidence.
        Case {
            scenario: "omission preserves the current partial receipt",
            operation: None,
            accepted: false,
            expected_state: DpaInterfaceControllerState::ApplyFirmware,
        },
        // A delayed receipt cannot erase progress already accepted for this ID.
        Case {
            scenario: "stale receipt preserves the current partial receipt",
            operation: Some(previous_operation),
            accepted: false,
            expected_state: DpaInterfaceControllerState::ApplyFirmware,
        },
        // The terminal label alone cannot override an unsuccessful required phase.
        Case {
            scenario: "complete with failed reset cannot advance",
            operation: Some(FirmwareOperation {
                operation_id: operation_id.clone(),
                state: FirmwareOperationState::Complete.into(),
                phase: None,
                report: Some(FirmwareFlashReport {
                    flashed: true,
                    reset: Some(false),
                    ..Default::default()
                }),
                diagnostic: None,
            }),
            accepted: true,
            expected_state: DpaInterfaceControllerState::ApplyFirmware,
        },
        // Uncertain execution remains visible for intervention, not a retry.
        Case {
            scenario: "manual intervention preserves its diagnostic",
            operation: Some(FirmwareOperation {
                operation_id: operation_id.clone(),
                state: FirmwareOperationState::ManualIntervention.into(),
                phase: None,
                report: Some(FirmwareFlashReport::default()),
                diagnostic: Some("the prior burn result is unknown".into()),
            }),
            accepted: true,
            expected_state: DpaInterfaceControllerState::ApplyFirmware,
        },
        // A request with no selected work completes without inventing a burn
        // or requiring phase results for work that was never requested.
        Case {
            scenario: "completed no-work report permits advancement",
            operation: Some(FirmwareOperation {
                operation_id: operation_id.clone(),
                state: FirmwareOperationState::Complete.into(),
                report: Some(FirmwareFlashReport::default()),
                ..Default::default()
            }),
            accepted: true,
            expected_state: DpaInterfaceControllerState::ApplyProfile,
        },
        // A later success is a positive control: the monitor reaches this card
        // and advances when the stored report actually permits it.
        Case {
            scenario: "later successful report permits advancement",
            operation: Some(FirmwareOperation {
                operation_id: operation_id.clone(),
                state: FirmwareOperationState::Complete.into(),
                phase: None,
                report: Some(FirmwareFlashReport {
                    flashed: true,
                    reset: Some(true),
                    verified_image: Some(true),
                    verified_version: Some(true),
                    observed_version: Some("32.43.1014".into()),
                    expected_version: Some("32.43.1014".into()),
                }),
                diagnostic: None,
            }),
            accepted: true,
            expected_state: DpaInterfaceControllerState::ApplyProfile,
        },
    ] {
        if let Some(version) = completed_version.take() {
            // Exercise the next completion on a new persisted attempt, keeping
            // both successful report shapes on the same controller path.
            apply_version = version.increment();
            let mut txn = env.db_txn().await;
            assert_eq!(
                db::dpa_interface::try_update_controller_state(
                    &mut txn,
                    cards[0].id,
                    version,
                    apply_version,
                    &DpaInterfaceControllerState::ApplyFirmware,
                )
                .await
                .unwrap(),
                db::ConditionalWrite::Applied(())
            );
            txn.commit().await.unwrap();
            case.operation
                .as_mut()
                .expect("next attempt has a completion receipt")
                .operation_id = format!("dpa-firmware:{}:{apply_version}", cards[0].id);
        }
        // A valid field accompanying a stale receipt proves MAC routing reached
        // the merge; otherwise an early drop could masquerade as ID rejection.
        let profile_name = if case.operation.is_some() && !case.accepted {
            expected_profile = case.scenario;
            Some(case.scenario.to_string())
        } else {
            None
        };
        env.api
            .publish_mlx_observation_report(machine_request(
                PublishMlxObservationReportRequest {
                    report: Some(MlxObservationReport {
                        machine_id: Some(machine_id),
                        timestamp: None,
                        observations: vec![MlxObservation {
                            // Matching receipts need no device query. Rejected
                            // IDs must route by MAC before testing the ID guard.
                            device_info: (!case.accepted).then(|| MlxDeviceInfo {
                                base_mac: "00:11:22:33:44:55".into(),
                                pci_name: "0000:cc:00.0".into(),
                                ..Default::default()
                            }),
                            profile_name,
                            // Even a legacy success cannot substitute for a
                            // current correlated operation.
                            firmware_report: Some(legacy_report.clone()),
                            firmware_operation: case.operation.clone(),
                            ..Default::default()
                        }],
                    }),
                },
                machine_id,
            ))
            .await
            .unwrap();

        if case.accepted {
            expected_operation = case.operation.clone();
        }
        let expected_report = expected_operation
            .as_ref()
            .and_then(|operation| operation.report.clone())
            .map(|mut report| {
                report.flashed = case.expected_state == DpaInterfaceControllerState::ApplyProfile;
                report
            })
            .or_else(|| Some(legacy_report.clone()));

        // Independently reload the published report before the controller reads
        // it. Omitted unrelated observation fields must preserve card settings.
        let cards =
            db::dpa_interface::find_by_machine_id(&env.pool, host_id, DpaSearchConfig::default())
                .await
                .unwrap();
        assert_eq!(cards.len(), 1, "{}", case.scenario);
        assert_eq!(
            cards[0].controller_state.value,
            DpaInterfaceControllerState::ApplyFirmware
        );
        assert_eq!(cards[0].controller_state.version, apply_version);
        let state = cards[0].card_state.as_ref().unwrap();
        assert_eq!(state.lockmode, Some(DpaLockMode::Unlocked));
        assert_eq!(state.profile.as_deref(), Some(expected_profile));
        assert_eq!(state.profile_synced, Some(true));
        assert_eq!(
            state
                .firmware_operation
                .clone()
                .map(FirmwareOperation::from),
            expected_operation,
            "{}",
            case.scenario
        );
        assert_eq!(
            state.firmware_report.clone().map(FirmwareFlashReport::from),
            expected_report,
            "{}: retain the legacy report or project the accepted receipt",
            case.scenario
        );
        if expected_operation.is_some() {
            let stored: serde_json::Value =
                sqlx::query_scalar("SELECT card_state FROM dpa_interfaces WHERE id = $1")
                    .bind(cards[0].id)
                    .fetch_one(&env.pool)
                    .await
                    .unwrap();
            let legacy: LegacyCardState = serde_json::from_value(stored).unwrap();
            let report = legacy.firmware_report.unwrap();
            assert_eq!(
                report.flashed && report.reset.unwrap_or(true),
                case.expected_state == DpaInterfaceControllerState::ApplyProfile,
                "{}: legacy completion predicate",
                case.scenario
            );
        }

        monitor.run_single_iteration().await.unwrap();
        // An unchanged state alone could mean the monitor skipped this card.
        // Its last-iteration gauge must show that the one saved card was scanned;
        // the later successful report also proves the same path can advance it.
        assert_eq!(
            env.test_meter
                .formatted_metric("carbide_dpa_monitor_interfaces_scanned_count")
                .as_deref(),
            Some("1"),
            "{}",
            case.scenario
        );
        // Reload the controller's decision and retained report from storage.
        let cards =
            db::dpa_interface::find_by_machine_id(&env.pool, host_id, DpaSearchConfig::default())
                .await
                .unwrap();
        assert_eq!(cards.len(), 1, "{}", case.scenario);
        assert_eq!(cards[0].controller_state.value, case.expected_state);
        let version = cards[0].controller_state.version;
        if case.expected_state == DpaInterfaceControllerState::ApplyFirmware {
            assert_eq!(version, apply_version, "{}", case.scenario);
        } else {
            // increment() chooses its timestamp when the controller transitions,
            // so only the counter can be predicted before that write.
            assert_eq!(version.version_nr(), apply_version.version_nr() + 1);
            assert_ne!(version, apply_version);
            completed_version = Some(version);
        }
        let state = cards[0].card_state.as_ref().unwrap();
        assert_eq!(
            state
                .firmware_operation
                .clone()
                .map(FirmwareOperation::from),
            expected_operation,
            "{}",
            case.scenario
        );
        assert_eq!(
            state.firmware_report.clone().map(FirmwareFlashReport::from),
            expected_report,
            "{}: controller must preserve the legacy report projection",
            case.scenario
        );
    }

    // A receipt for the current `ApplyProfile` version must not replace the
    // completed firmware history, even though its ID passes the routing check.
    let profile_operation_id = format!(
        "dpa-firmware:{}:{}",
        cards[0].id,
        completed_version.expect("the final receipt completed firmware")
    );
    env.api
        .publish_mlx_observation_report(machine_request(
            PublishMlxObservationReportRequest {
                report: Some(MlxObservationReport {
                    machine_id: Some(machine_id),
                    observations: vec![MlxObservation {
                        device_info: Some(MlxDeviceInfo {
                            base_mac: "00:11:22:33:44:55".into(),
                            ..Default::default()
                        }),
                        profile_name: Some("next-profile".into()),
                        firmware_operation: Some(FirmwareOperation {
                            operation_id: profile_operation_id,
                            state: FirmwareOperationState::Failed.into(),
                            report: Some(FirmwareFlashReport::default()),
                            diagnostic: Some("receipt outside ApplyFirmware".into()),
                            ..Default::default()
                        }),
                        ..Default::default()
                    }],
                    ..Default::default()
                }),
            },
            machine_id,
        ))
        .await
        .unwrap();
    let cards =
        db::dpa_interface::find_by_machine_id(&env.pool, host_id, DpaSearchConfig::default())
            .await
            .unwrap();
    let card = &cards[0];
    assert_eq!(
        card.controller_state.value,
        DpaInterfaceControllerState::ApplyProfile
    );
    let state = card.card_state.as_ref().unwrap();
    assert_eq!(state.profile.as_deref(), Some("next-profile"));
    assert_eq!(
        state
            .firmware_operation
            .clone()
            .map(FirmwareOperation::from),
        expected_operation
    );
    assert_eq!(
        state.firmware_report.clone().map(FirmwareFlashReport::from),
        expected_operation.unwrap().report
    );
}

/// Observations merge only after card identity and lock status pass validation.
/// A malformed receipt still preserves independent profile/lock observations.
#[crate::sqlx_test]
async fn firmware_observation_batch_merges_partial_and_malformed_reports(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let host = create_managed_host(&env).await;
    let host_id: HostMachineId = host.id.into();
    let machine_id: MachineId = host_id.into();
    let first = firmware_test_card(&env.pool, host_id, "00:11:22:33:44:55", "cc:00.0").await;
    let second = firmware_test_card(&env.pool, host_id, "00:11:22:33:44:66", "dd:00.0").await;
    let operation = FirmwareOperation {
        operation_id: first.firmware_operation_id(),
        state: FirmwareOperationState::Running.into(),
        report: Some(FirmwareFlashReport {
            flashed: true,
            reset: Some(false),
            ..Default::default()
        }),
        ..Default::default()
    };

    env.api
        .publish_mlx_observation_report(machine_request(
            PublishMlxObservationReportRequest {
                report: Some(MlxObservationReport {
                    machine_id: Some(machine_id),
                    observations: vec![
                        // The burn evidence must survive later observations.
                        MlxObservation {
                            device_info: Some(MlxDeviceInfo {
                                base_mac: first.mac_address.to_string(),
                                ..Default::default()
                            }),
                            firmware_operation: Some(operation.clone()),
                            ..Default::default()
                        },
                        // An invalid lock enum rejects this whole observation;
                        // it cannot overwrite the receipt already merged above.
                        MlxObservation {
                            firmware_operation: Some(FirmwareOperation {
                                state: FirmwareOperationState::Complete.into(),
                                ..operation.clone()
                            }),
                            lock_status: Some(i32::MAX),
                            profile_synced: Some(true),
                            ..Default::default()
                        },
                        // A stale ID without device info cannot identify any
                        // card, even when its ancillary fields are valid.
                        MlxObservation {
                            firmware_operation: Some(FirmwareOperation {
                                operation_id: "stale-operation".into(),
                                ..operation.clone()
                            }),
                            profile_synced: Some(true),
                            ..Default::default()
                        },
                        // A matching ID reaches conversion, where the unknown
                        // state must not reject its valid profile/lock fields or
                        // roll back the other card's already accepted receipt.
                        MlxObservation {
                            firmware_operation: Some(FirmwareOperation {
                                operation_id: second.firmware_operation_id(),
                                state: i32::MAX,
                                report: Some(FirmwareFlashReport::default()),
                                ..Default::default()
                            }),
                            lock_status: Some(rpc::protos::mlx_device::LockStatus::Unlocked.into()),
                            profile_name: Some("valid-profile".into()),
                            profile_synced: Some(true),
                            firmware_report: Some(FirmwareFlashReport {
                                flashed: true,
                                ..Default::default()
                            }),
                            ..Default::default()
                        },
                        // The same card's omitted receipt preserves the earlier
                        // update in this batch, not the pre-request snapshot.
                        MlxObservation {
                            device_info: Some(MlxDeviceInfo {
                                base_mac: first.mac_address.to_string(),
                                ..Default::default()
                            }),
                            profile_name: Some("later-profile".into()),
                            ..Default::default()
                        },
                    ],
                    ..Default::default()
                }),
            },
            machine_id,
        ))
        .await
        .unwrap();

    // Reload both rows independently: success from the endpoint alone would
    // miss a rolled-back receipt, an overwritten mirror, or lost valid fields.
    let cards =
        db::dpa_interface::find_by_machine_id(&env.pool, host_id, DpaSearchConfig::default())
            .await
            .unwrap();
    assert_eq!(cards.len(), 2);
    let first_state = cards
        .iter()
        .find(|card| card.id == first.id)
        .unwrap()
        .card_state
        .as_ref()
        .unwrap();
    assert_eq!(first_state.profile.as_deref(), Some("later-profile"));
    assert_eq!(first_state.lockmode, None);
    assert_eq!(first_state.profile_synced, None);
    assert_eq!(
        first_state
            .firmware_operation
            .clone()
            .map(FirmwareOperation::from),
        Some(operation.clone())
    );
    assert_eq!(
        first_state
            .firmware_report
            .clone()
            .map(FirmwareFlashReport::from),
        Some(FirmwareFlashReport {
            flashed: false,
            ..operation.report.clone().unwrap()
        })
    );
    let second_state = cards
        .iter()
        .find(|card| card.id == second.id)
        .unwrap()
        .card_state
        .as_ref()
        .unwrap();
    assert_eq!(second_state.lockmode, Some(DpaLockMode::Unlocked));
    assert_eq!(second_state.profile.as_deref(), Some("valid-profile"));
    assert_eq!(second_state.profile_synced, Some(true));
    assert!(second_state.firmware_operation.is_none());
    assert!(second_state.firmware_report.is_none());

    // A current operation ID for the first card cannot lend authority to
    // device info for the second. Reject all fields before touching either row.
    env.api
        .publish_mlx_observation_report(machine_request(
            PublishMlxObservationReportRequest {
                report: Some(MlxObservationReport {
                    machine_id: Some(machine_id),
                    observations: vec![MlxObservation {
                        device_info: Some(MlxDeviceInfo {
                            base_mac: second.mac_address.to_string(),
                            ..Default::default()
                        }),
                        firmware_operation: Some(FirmwareOperation {
                            state: FirmwareOperationState::Failed.into(),
                            ..operation
                        }),
                        lock_status: Some(rpc::protos::mlx_device::LockStatus::Locked.into()),
                        profile_name: Some("wrong-card-profile".into()),
                        profile_synced: Some(false),
                        ..Default::default()
                    }],
                    ..Default::default()
                }),
            },
            machine_id,
        ))
        .await
        .unwrap();
    let after =
        db::dpa_interface::find_by_machine_id(&env.pool, host_id, DpaSearchConfig::default())
            .await
            .unwrap();
    assert_eq!(after.len(), cards.len());
    for before in &cards {
        let after = after.iter().find(|card| card.id == before.id).unwrap();
        assert_eq!(
            serde_json::to_value(&after.card_state).unwrap(),
            serde_json::to_value(&before.card_state).unwrap(),
            "contradictory identities must preserve card {}",
            before.id
        );
        assert_eq!(after.controller_state.value, before.controller_state.value);
        assert_eq!(
            after.controller_state.version,
            before.controller_state.version
        );
    }
}

/// A database write failure must reject the whole observation batch so Scout
/// can retry without losing an acknowledged receipt or committing partial state.
#[crate::sqlx_test]
async fn firmware_observation_write_failure_rolls_back_and_allows_retry(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let host = create_managed_host(&env).await;
    let host_id: HostMachineId = host.id.into();
    let machine_id: MachineId = host_id.into();
    let cards = [
        firmware_test_card(&env.pool, host_id, "00:11:22:33:44:55", "cc:00.0").await,
        firmware_test_card(&env.pool, host_id, "00:11:22:33:44:66", "dd:00.0").await,
    ];
    let operations = cards.each_ref().map(|card| FirmwareOperation {
        operation_id: card.firmware_operation_id(),
        state: FirmwareOperationState::Failed.into(),
        report: Some(FirmwareFlashReport {
            flashed: true,
            ..Default::default()
        }),
        ..Default::default()
    });

    // Reject only the second write, after the first card's receipt has reached
    // PostgreSQL. This exercises a real transaction failure, not invalid input.
    sqlx::query(
        "ALTER TABLE dpa_interfaces ADD CONSTRAINT test_reject_card_observation
         CHECK (card_state ->> 'profile' <> 'rejected-profile')",
    )
    .execute(&env.pool)
    .await
    .unwrap();
    let request = PublishMlxObservationReportRequest {
        report: Some(MlxObservationReport {
            machine_id: Some(machine_id),
            observations: vec![
                MlxObservation {
                    firmware_operation: Some(operations[0].clone()),
                    ..Default::default()
                },
                MlxObservation {
                    firmware_operation: Some(operations[1].clone()),
                    profile_name: Some("rejected-profile".into()),
                    ..Default::default()
                },
            ],
            ..Default::default()
        }),
    };
    let (result, logs) = capture_logs_async(
        env.api
            .publish_mlx_observation_report(machine_request(request.clone(), machine_id)),
    )
    .await;
    let error = result.unwrap_err();
    assert_eq!(error.code(), tonic::Code::Internal);
    assert_eq!(error.message(), "internal database error");
    let source = std::error::Error::source(&error)
        .expect("database status source")
        .downcast_ref::<crate::errors::CarbideErrorWithLocation>()
        .expect("Carbide error source");
    let crate::errors::CarbideError::DBError(annotated) = &source.error else {
        panic!("expected the original database error: {source:?}");
    };
    let database_error = annotated
        .source
        .as_database_error()
        .expect("database constraint error");
    assert_eq!(
        database_error.constraint(),
        Some("test_reject_card_observation")
    );
    // The first failed receipt reached PostgreSQL but was rolled back. Its
    // attention warning must wait until the whole batch has committed.
    assert!(
        logs.iter()
            .all(|log| log.message != "NIC firmware operation requires operator attention")
    );

    // An independent read must find neither receipt, including the earlier
    // successful statement, and no controller version may have changed.
    let persisted =
        db::dpa_interface::find_by_machine_id(&env.pool, host_id, DpaSearchConfig::default())
            .await
            .unwrap();
    assert_eq!(persisted.len(), cards.len());
    for before in &cards {
        let after = persisted.iter().find(|card| card.id == before.id).unwrap();
        assert_eq!(
            after.controller_state.version,
            before.controller_state.version
        );
        assert_eq!(
            after.controller_state.value,
            DpaInterfaceControllerState::ApplyFirmware
        );
        assert_eq!(
            serde_json::to_value(&after.card_state).unwrap(),
            serde_json::to_value(&before.card_state).unwrap()
        );
    }

    // Remove the injected failure and submit the identical batch: retry must
    // persist both receipts and publish their attention warnings.
    sqlx::query("ALTER TABLE dpa_interfaces DROP CONSTRAINT test_reject_card_observation")
        .execute(&env.pool)
        .await
        .unwrap();
    let (result, logs) = capture_logs_async(
        env.api
            .publish_mlx_observation_report(machine_request(request, machine_id)),
    )
    .await;
    result.unwrap();
    let attention: Vec<_> = logs
        .iter()
        .filter(|log| log.message == "NIC firmware operation requires operator attention")
        .map(|log| {
            (
                log.field("dpa_interface_id").unwrap().to_string(),
                log.field("operation_id").unwrap().to_string(),
            )
        })
        .collect();
    assert_eq!(
        attention,
        cards
            .iter()
            .zip(&operations)
            .map(|(card, operation)| (card.id.to_string(), operation.operation_id.clone()))
            .collect::<Vec<_>>()
    );
    let persisted =
        db::dpa_interface::find_by_machine_id(&env.pool, host_id, DpaSearchConfig::default())
            .await
            .unwrap();
    assert_eq!(persisted.len(), cards.len());
    for (before, operation) in cards.iter().zip(operations) {
        let after = persisted.iter().find(|card| card.id == before.id).unwrap();
        let state = after.card_state.as_ref().unwrap();
        assert_eq!(
            state
                .firmware_operation
                .clone()
                .map(FirmwareOperation::from),
            Some(operation.clone())
        );
        assert_eq!(
            state.firmware_report.clone().map(FirmwareFlashReport::from),
            Some(FirmwareFlashReport {
                flashed: false,
                ..operation.report.unwrap()
            })
        );
    }
}

/// An observation waiting behind another writer must merge the committed receipt
/// after its row lock is acquired, even when the waiting report omits firmware.
#[crate::sqlx_test]
async fn firmware_observation_waits_for_current_card_state(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let host = create_managed_host(&env).await;
    let host_id: HostMachineId = host.id.into();
    let machine_id: MachineId = host_id.into();
    let card = firmware_test_card(&env.pool, host_id, "00:11:22:33:44:55", "cc:00.0").await;
    let operation = FirmwareOperation {
        operation_id: card.firmware_operation_id(),
        state: FirmwareOperationState::Running.into(),
        report: Some(FirmwareFlashReport {
            flashed: true,
            reset: Some(false),
            ..Default::default()
        }),
        ..Default::default()
    };

    let mut writer = env.pool.begin().await.unwrap();
    let mut locked_card =
        db::dpa_interface::find_svpc_by_machine_id_for_update(&mut writer, host_id)
            .await
            .unwrap()
            .pop()
            .unwrap();
    let writer_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *writer)
        .await
        .unwrap();
    let publish = env.api.publish_mlx_observation_report(machine_request(
        PublishMlxObservationReportRequest {
            report: Some(MlxObservationReport {
                machine_id: Some(machine_id),
                observations: vec![MlxObservation {
                    device_info: Some(MlxDeviceInfo {
                        base_mac: card.mac_address.to_string(),
                        ..Default::default()
                    }),
                    profile_name: Some("concurrent-profile".into()),
                    ..Default::default()
                }],
                ..Default::default()
            }),
        },
        machine_id,
    ));
    let commit_receipt = async {
        // Waiting at the locked read, rather than only at UPDATE, proves the
        // omission cannot be merged against the earlier empty card state.
        db::test_support::postgres::wait_for_blocked_query(
            &env.pool,
            writer_pid,
            "ORDER BY id FOR UPDATE",
        )
        .await;
        locked_card.card_state = Some(CardState {
            firmware_report: operation.report.clone().map(Into::into),
            firmware_operation: Some(operation.clone().try_into().unwrap()),
            ..Default::default()
        });
        db::dpa_interface::update_card_state(&mut writer, locked_card)
            .await
            .unwrap();
        writer.commit().await.unwrap();
    };
    let (published, ()) = tokio::join!(publish, commit_receipt);
    published.unwrap();

    let cards =
        db::dpa_interface::find_by_machine_id(&env.pool, host_id, DpaSearchConfig::default())
            .await
            .unwrap();
    assert_eq!(cards.len(), 1);
    let state = cards[0].card_state.as_ref().unwrap();
    assert_eq!(state.profile.as_deref(), Some("concurrent-profile"));
    assert_eq!(
        state
            .firmware_operation
            .clone()
            .map(FirmwareOperation::from),
        Some(operation.clone())
    );
    assert_eq!(
        state.firmware_report.clone().map(FirmwareFlashReport::from),
        operation.report
    );
}

#[crate::sqlx_test]
async fn startup_retains_generic_observations_without_svpc(pool: sqlx::PgPool) {
    let mut config = get_config();
    config.ewethers_config = None;
    let env = create_test_env_with_overrides(
        pool,
        TestEnvOverrides {
            config: Some(config),
            ..Default::default()
        },
    )
    .await;
    let host = create_managed_host(&env).await;
    let machine_id: MachineId = host.id.into();
    let host_machine_id = HostMachineId::try_from(machine_id).unwrap();
    assert!(!env.api.runtime_config.is_svpc_enabled());

    env.api
        .publish_mlx_device_report(machine_request(
            PublishMlxDeviceReportRequest {
                report: Some(report(Some(machine_id), 2)),
            },
            machine_id,
        ))
        .await
        .unwrap();
    let stored = observation(&env.pool, host_machine_id).await.unwrap();
    assert_eq!(
        stored.devices[0].part_number.as_deref(),
        Some("unknown-part-number")
    );
    assert_eq!(stored.devices[0].base_mac, None);
    assert!(
        db::dpa_interface::find_by_machine_id(
            &env.pool,
            host_machine_id,
            DpaSearchConfig::default()
        )
        .await
        .unwrap()
        .is_empty()
    );

    // An older report and a newer unbound publication must both leave the
    // authenticated snapshot unchanged, even though both RPCs succeed.
    for request in [
        machine_request(
            PublishMlxDeviceReportRequest {
                report: Some(report(Some(machine_id), 1)),
            },
            machine_id,
        ),
        Request::new(PublishMlxDeviceReportRequest {
            report: Some(report(Some(machine_id), 3)),
        }),
    ] {
        env.api.publish_mlx_device_report(request).await.unwrap();
        assert_eq!(
            observation(&env.pool, host_machine_id).await.as_ref(),
            Some(&stored)
        );
    }
}

#[crate::sqlx_test]
async fn startup_snapshot_failure_still_processes_dpa_data(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let host = create_managed_host(&env).await;
    let machine_id: MachineId = host.id.into();
    let host_machine_id = HostMachineId::try_from(machine_id).unwrap();
    assert!(env.api.runtime_config.is_svpc_enabled());

    // Fail only the new snapshot statement; independent DPA writes still work.
    sqlx::query(
        "ALTER TABLE machines ADD CONSTRAINT test_reject_mlx_observation
         CHECK (mlx_device_observation IS NULL)",
    )
    .execute(&env.pool)
    .await
    .unwrap();
    let mut device_report = report(Some(machine_id), 2);
    device_report.devices[0].base_mac = "00:11:22:33:44:55".into();
    device_report.devices[0].device_description = "ConnectX-8 SuperNIC".into();
    let error = env
        .api
        .publish_mlx_device_report(machine_request(
            PublishMlxDeviceReportRequest {
                report: Some(device_report),
            },
            machine_id,
        ))
        .await
        .unwrap_err();
    assert_eq!(error.code(), tonic::Code::Internal);
    assert_eq!(error.message(), "internal database error");
    let source = std::error::Error::source(&error)
        .expect("database status source")
        .downcast_ref::<crate::errors::CarbideErrorWithLocation>()
        .expect("Carbide error source");
    let crate::errors::CarbideError::DBError(annotated) = &source.error else {
        panic!("expected the original database error: {source:?}");
    };
    let database_error = annotated
        .source
        .as_database_error()
        .expect("database constraint error");
    assert_eq!(
        database_error.constraint(),
        Some("test_reject_mlx_observation")
    );
    assert_eq!(observation(&env.pool, host_machine_id).await, None);

    let interfaces = db::dpa_interface::find_by_machine_id(
        &env.pool,
        host_machine_id,
        DpaSearchConfig::default(),
    )
    .await
    .unwrap();
    assert_eq!(interfaces.len(), 1);
    assert_eq!(
        interfaces[0]
            .device_info
            .as_ref()
            .unwrap()
            .fw_version_current
            .as_deref(),
        Some("2")
    );
}

#[crate::sqlx_test]
async fn stored_device_identities_include_secondary_dpu_without_scout(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let host = create_managed_host_multi_dpu(&env, 2).await;
    let machine_id: MachineId = host.id.into();
    let host_machine_id = HostMachineId::try_from(machine_id).unwrap();
    let machine = db::machine::find_one(&env.pool, &host_machine_id, Default::default())
        .await
        .unwrap()
        .unwrap();
    let secondary_interface = machine
        .status
        .interfaces
        .iter()
        .find(|interface| {
            !interface.primary_interface && interface.attached_dpu_machine_id.is_some()
        })
        .expect("fixture has a secondary managed-DPU interface");
    let request = MlxAdminDeviceIdentitiesRequest {
        machine_id: Some(machine_id),
    };
    assert!(!env.api.scout_stream_registry.is_connected(machine_id).await);
    assert_eq!(
        env.api
            .mlx_admin_show_device_identities(Request::new(request.clone()))
            .await
            .unwrap()
            .into_inner()
            .report,
        None
    );

    let devices = [
        MlxDeviceInfo {
            pci_name: "01:00.0".into(),
            device_type: "ConnectX8".into(),
            base_guid: Some("b8599f030023f954".into()),
            fw_version_current: "40.45.1000".into(),
            ..Default::default()
        },
        MlxDeviceInfo {
            pci_name: "02:00.0".into(),
            device_type: "BlueField3".into(),
            base_mac: secondary_interface.mac_address.to_string(),
            ..Default::default()
        },
    ];
    let stored = MlxDeviceObservation {
        observed_at: "2026-09-29T00:00:00Z".parse().unwrap(),
        devices: devices
            .iter()
            .cloned()
            .map(TryInto::try_into)
            .collect::<Result<_, _>>()
            .unwrap(),
    };
    let mut connection = env.pool.acquire().await.unwrap();
    assert_eq!(
        db::machine::update_mlx_device_observation(&mut connection, &host_machine_id, &stored)
            .await
            .unwrap(),
        db::ConditionalWrite::Applied(())
    );
    drop(connection);

    let response = env
        .api
        .mlx_admin_show_device_identities(Request::new(request))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(
        response.report,
        Some(MlxDeviceIdentityReport {
            observed_at: Some(stored.observed_at.into()),
            devices: vec![
                MlxDeviceIdentity {
                    device_info: Some(devices[0].clone()),
                    managed_dpu_machine_ids: vec![],
                },
                MlxDeviceIdentity {
                    device_info: Some(devices[1].clone()),
                    managed_dpu_machine_ids: vec![
                        secondary_interface.attached_dpu_machine_id.unwrap().into(),
                    ],
                },
            ],
        })
    );
    let after = db::machine::find_one(&env.pool, &host_machine_id, Default::default())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after.status.mlx_device_observation, Some(stored));
    assert_eq!(after.state.value, machine.state.value);
    assert_eq!(after.state.version, machine.state.version);
    assert!(!env.api.scout_stream_registry.is_connected(machine_id).await);
}

#[crate::sqlx_test]
async fn device_identity_requests_require_an_existing_host(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    for (scenario, machine_id, expected_code) in [
        ("missing machine ID", None, tonic::Code::InvalidArgument),
        (
            "DPU machine ID",
            Some(
                "fm100dskla0ihp0pn4tv7v1js2k2mo37sl0jjr8141okqg8pjpdpfihaa80"
                    .parse()
                    .unwrap(),
            ),
            tonic::Code::InvalidArgument,
        ),
        (
            "unknown host",
            Some(
                "fm100hseddco33hvlofuqvg543p6p9aj60g76q5cq491g9m9tgtf2dk0530"
                    .parse()
                    .unwrap(),
            ),
            tonic::Code::NotFound,
        ),
    ] {
        let error = env
            .api
            .mlx_admin_show_device_identities(Request::new(MlxAdminDeviceIdentitiesRequest {
                machine_id,
            }))
            .await
            .unwrap_err();
        assert_eq!(error.code(), expected_code, "{scenario}");
    }
}

// Feed the real Tonic decoder so these tests exercise Init authentication and
// the production forwarding task, not a separate observation-only entry point.
pub(super) async fn scout_connection(
    api: &Api,
    machine_id: MachineId,
    authenticated_machine_id: Option<MachineId>,
) -> Result<(mpsc::Sender<ScoutStreamApiBoundMessage>, ScoutStreamType), tonic::Status> {
    let (sender, receiver) = mpsc::channel::<ScoutStreamApiBoundMessage>(4);
    sender
        .send(ScoutStreamApiBoundMessage {
            flow_uuid: None,
            payload: Some(scout_stream_api_bound_message::Payload::Init(
                ScoutStreamInitRequest {
                    machine_id: Some(machine_id),
                },
            )),
        })
        .await
        .unwrap();
    let body = Body::from_stream(ReceiverStream::new(receiver).map(|message| {
        let encoded = message.encode_to_vec();
        let mut frame = Vec::with_capacity(5 + encoded.len());
        frame.push(0);
        frame.extend_from_slice(&u32::try_from(encoded.len()).unwrap().to_be_bytes());
        frame.extend_from_slice(&encoded);
        Ok::<_, Infallible>(frame)
    }));
    let stream = Streaming::new_request(ProstDecoder::new(Default::default()), body, None, None);
    let request = match authenticated_machine_id {
        Some(source) => machine_request(stream, source),
        None => Request::new(stream),
    };
    let response = api.scout_stream(request).await?;
    Ok((sender, response.into_inner()))
}

pub(super) async fn live_report(
    api: &Api,
    machine_id: MachineId,
    sender: &mpsc::Sender<ScoutStreamApiBoundMessage>,
    requests: &mut ScoutStreamType,
    report: MlxDeviceReport,
) {
    let read = api.mlx_admin_show_machine(Request::new(MlxAdminDeviceReportRequest {
        machine_id: Some(machine_id),
    }));
    let reply = async {
        let request = requests.next().await.unwrap().unwrap();
        sender
            .send(ScoutStreamApiBoundMessage {
                flow_uuid: request.flow_uuid,
                payload: Some(
                    scout_stream_api_bound_message::Payload::MlxDeviceInfoReportResponse(
                        MlxDeviceInfoReportResponse {
                            reply: Some(mlx_device_info_report_response::Reply::DeviceReport(
                                report.clone(),
                            )),
                        },
                    ),
                ),
            })
            .await
            .unwrap();
    };
    let (response, ()) =
        tokio::time::timeout(Duration::from_secs(5), async { tokio::join!(read, reply) })
            .await
            .expect("live report did not complete within the test deadline");
    assert_eq!(response.unwrap().into_inner().device_report, Some(report));
}

#[crate::sqlx_test]
async fn live_reports_use_authenticated_stream_identity(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let host = create_managed_host(&env).await;
    let machine_id: MachineId = host.id.into();
    let host_machine_id = HostMachineId::try_from(machine_id).unwrap();
    let other_machine: MachineId = host.dpu().id.into();

    let mismatch = scout_connection(&env.api, machine_id, Some(other_machine)).await;
    assert_eq!(
        mismatch.err().unwrap().code(),
        tonic::Code::PermissionDenied
    );
    assert!(!env.api.scout_stream_registry.is_connected(machine_id).await);

    let (sender, mut requests) = scout_connection(&env.api, machine_id, Some(machine_id))
        .await
        .unwrap();
    live_report(
        &env.api,
        machine_id,
        &sender,
        &mut requests,
        report(None, 1),
    )
    .await;
    let stored = observation(&env.pool, host_machine_id).await.unwrap();
    assert_eq!(stored.devices[0].fw_version_current.as_deref(), Some("1"));

    // A malformed report is still returned to the admin caller as received,
    // but it cannot erase the previous successful observation.
    live_report(
        &env.api,
        machine_id,
        &sender,
        &mut requests,
        MlxDeviceReport {
            timestamp: None,
            ..report(None, 2)
        },
    )
    .await;
    assert_eq!(
        observation(&env.pool, host_machine_id).await.as_ref(),
        Some(&stored)
    );

    // The simulator's unbound stream can still answer reads, without gaining
    // the ability to update a machine's authenticated observations.
    let (unbound_sender, mut unbound_requests) =
        scout_connection(&env.api, machine_id, None).await.unwrap();
    live_report(
        &env.api,
        machine_id,
        &unbound_sender,
        &mut unbound_requests,
        report(None, 3),
    )
    .await;
    assert_eq!(
        observation(&env.pool, host_machine_id).await.as_ref(),
        Some(&stored)
    );
}

#[crate::sqlx_test]
async fn live_report_is_returned_when_snapshot_write_fails(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let host = create_managed_host(&env).await;
    let machine_id: MachineId = host.id.into();
    let host_machine_id = HostMachineId::try_from(machine_id).unwrap();
    let (sender, mut requests) = scout_connection(&env.api, machine_id, Some(machine_id))
        .await
        .unwrap();
    sqlx::query(
        "ALTER TABLE machines ADD CONSTRAINT test_reject_mlx_observation
         CHECK (mlx_device_observation IS NULL)",
    )
    .execute(&env.pool)
    .await
    .unwrap();
    live_report(
        &env.api,
        machine_id,
        &sender,
        &mut requests,
        report(None, 2),
    )
    .await;
    assert_eq!(observation(&env.pool, host_machine_id).await, None);
    sqlx::query("ALTER TABLE machines DROP CONSTRAINT test_reject_mlx_observation")
        .execute(&env.pool)
        .await
        .unwrap();

    live_report(
        &env.api,
        machine_id,
        &sender,
        &mut requests,
        report(None, 3),
    )
    .await;
    assert_eq!(
        observation(&env.pool, host_machine_id)
            .await
            .unwrap()
            .devices[0]
            .fw_version_current
            .as_deref(),
        Some("3")
    );
}

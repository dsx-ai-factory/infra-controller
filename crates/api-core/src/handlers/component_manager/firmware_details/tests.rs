// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashMap;

use carbide_firmware::FirmwareConfig;
use carbide_test_support::{Check, check_values};
use carbide_uuid::rack::RackId;
use config_version::{ConfigVersion, Versioned};
use model::firmware::{Firmware, FirmwareComponent, FirmwareEntry, HostFirmwareConfig};
use model::machine::{HostReprovisionRequest, MAX_FIRMWARE_UPGRADE_RETRIES, ScoutUpgradeResult};
use model::metadata::Metadata;
use model::power_shelf::{PowerShelfConfig, PowerShelfReprovisionRequest};
use model::rack::{
    FirmwareProgressState, RackConfig, RackErrorRecoveryPolicy, RackFirmwareUpgradeState,
    SwitchNvosUpdateState, SwitchNvosUpdateStatus,
};
use model::switch::{SwitchConfig, SwitchReprovisionRequest};

use super::*;

/// A fixed boundary keeps every request/job comparison independent of wall time.
fn accepted_at() -> DateTime<Utc> {
    "2026-11-01T00:00:00Z".parse().expect("fixture timestamp")
}

/// The accepted explicit scope supplies a different target from the submitted
/// job, so tests detect accidental replacement of frozen operation metadata.
fn rack_fixture() -> Rack {
    Rack {
        id: RackId::new("firmware-rack"),
        rack_profile_id: None,
        rack_group_id: None,
        config: RackConfig {
            maintenance_requested: Some(MaintenanceScope {
                requested_at: Some(accepted_at()),
                activities: vec![MaintenanceActivity::FirmwareUpgrade {
                    firmware_version: Some(
                        r#"{"Id":"requested-object","auth":{"password":"manifest-secret"}}"#.into(),
                    ),
                    components: vec![],
                    force_update: false,
                }],
                ..Default::default()
            }),
            ..Default::default()
        },
        controller_state: Versioned::new(
            RackState::Maintenance {
                maintenance_state: RackMaintenanceState::FirmwareUpgrade {
                    rack_firmware_upgrade: FirmwareUpgradeState::WaitForComplete,
                },
            },
            ConfigVersion::initial(),
        ),
        controller_state_outcome: None,
        firmware_upgrade_job: None,
        nvos_update_job: None,
        health_reports: Default::default(),
        created: accepted_at(),
        updated: accepted_at(),
        deleted: None,
        metadata: Metadata::default(),
        version: ConfigVersion::initial(),
    }
}

/// Ready with no device request is the real NVOS-only submission path.
fn switch_fixture() -> Switch {
    Switch {
        id: SwitchId::from(uuid::Uuid::from_u128(1)),
        config: SwitchConfig {
            name: "firmware-switch".into(),
            enable_nmxc: false,
            fabric_manager_config: None,
        },
        status: None,
        deleted: None,
        bmc_mac_address: Some("00:11:22:33:44:55".parse().expect("fixture MAC")),
        bmc_info: None,
        bmc_credential_rotation_requested: false,
        decommission_requested: false,
        controller_state: Versioned::new(SwitchControllerState::Ready, ConfigVersion::initial()),
        controller_state_outcome: None,
        switch_maintenance_requested: None,
        switch_reprovisioning_requested: None,
        firmware_upgrade_status: None,
        nvos_update_status: None,
        fabric_manager_status: None,
        nvlink_domain_uuid: None,
        rack_id: Some(RackId::new("firmware-rack")),
        metadata: Metadata::default(),
        version: ConfigVersion::initial(),
        is_primary: false,
        slot_number: None,
        tray_index: None,
        health_reports: Default::default(),
    }
}

/// A saved shelf request enters Core's wait before the first rack poll can
/// publish the Completed unblock sentinel; no backend shelf job is invented.
fn shelf_fixture() -> PowerShelf {
    PowerShelf {
        id: PowerShelfId::from(uuid::Uuid::from_u128(3)),
        config: PowerShelfConfig {
            name: "shelf".into(),
            capacity: None,
            voltage: None,
        },
        status: None,
        deleted: None,
        controller_state: Versioned::new(
            PowerShelfControllerState::ReProvisioning {
                reprovisioning_state:
                    model::power_shelf::ReProvisioningState::WaitingForRackFirmwareUpgrade,
            },
            ConfigVersion::initial(),
        ),
        controller_state_outcome: None,
        bmc_mac_address: None,
        bmc_credential_rotation_requested: false,
        decommission_requested: false,
        bmc_info: None,
        rack_id: Some(RackId::new("firmware-rack")),
        nvlink_domain_uuid: None,
        power_shelf_maintenance_requested: None,
        power_shelf_reprovisioning_requested: Some(PowerShelfReprovisionRequest {
            requested_at: accepted_at(),
            initiator: "rack-firmware-rack".into(),
            // Rack Start copies its selected activities into the shelf request.
            activities: vec![MaintenanceActivity::FirmwareUpgrade {
                firmware_version: Some(
                    r#"{"Id":"requested-object","auth":{"password":"manifest-secret"}}"#.into(),
                ),
                components: vec![],
                force_update: false,
            }],
        }),
        firmware_upgrade_status: None,
        metadata: Metadata::default(),
        version: ConfigVersion::initial(),
        health_reports: Default::default(),
    }
}

/// The common host fixture has no firmware work until a case adds its records.
fn host_fixture() -> HostMachine {
    let mut host: HostMachine = model::test_support::machine_snapshot::host_machine().into();
    host.state.value = ManagedHostState::Ready;
    host.host_reprovision_requested = None;
    host.rack_id = Some(RackId::new("firmware-rack"));
    host
}

/// This request has no admitted identity; cases may overwrite it or set reset
/// without changing the operation metadata frozen in a controller state.
fn host_request() -> HostReprovisionRequest {
    HostReprovisionRequest {
        requested_at: accepted_at(),
        initiator: "operator".into(),
        started_at: None,
        user_approval_received: false,
        request_reset: Some(false),
    }
}

/// Matches the selected switch and instantiates the unfinished peer that keeps
/// the rack phase alive after one member settles.
fn switch_firmware_job(switch: &Switch) -> FirmwareUpgradeJob {
    let child = FirmwareUpgradeDeviceStatus {
        node_id: switch.id.to_string(),
        mac: switch.bmc_mac_address.expect("fixture MAC").to_string(),
        bmc_ip: "192.0.2.1".into(),
        status: FirmwareProgressState::InProgress,
        job_id: Some("child-job".into()),
        parent_job_id: None,
        error_message: Some("backend-secret".into()),
    };
    let peer = FirmwareUpgradeDeviceStatus {
        node_id: SwitchId::from(uuid::Uuid::from_u128(3)).to_string(),
        mac: "00:11:22:33:44:66".into(),
        bmc_ip: "192.0.2.3".into(),
        job_id: Some("peer-job".into()),
        ..child.clone()
    };
    FirmwareUpgradeJob {
        firmware_id: Some("submitted-object".into()),
        started_at: Some(accepted_at()),
        status: Some(FirmwareProgressState::InProgress),
        switches: vec![child, peer],
        ..Default::default()
    }
}

/// Exact expected fields from the published fixture job, independent of scope.
fn published_firmware() -> CurrentFirmwareWork {
    CurrentFirmwareWork {
        work: FirmwareWork::FirmwareObject {
            object_id: Some("submitted-object".into()),
        },
        job_id: Some("child-job".into()),
        started_at: Some(accepted_at()),
        password_recovery_job_id: None,
    }
}

/// Core wait evidence has no associated backend metadata by default.
fn firmware_wait() -> CurrentFirmwareWork {
    CurrentFirmwareWork {
        work: FirmwareWork::FirmwareObject { object_id: None },
        job_id: None,
        started_at: None,
        password_recovery_job_id: None,
    }
}

/// Exact saved scope evidence uses its requested target, never the job's target.
fn rack_firmware_request() -> FirmwareRequestEvidence {
    FirmwareRequestEvidence {
        source: FirmwareRequestSource::RackMaintenance,
        work: FirmwareWork::FirmwareObject {
            object_id: Some("requested-object".into()),
        },
        requested_at: Some(accepted_at()),
        initiator: None,
    }
}

/// The DTO must use the supplied effective catalog, including runtime overrides,
/// without substituting an empty policy for absence.
#[test]
fn effective_host_defaults_and_absence_reach_the_projector() {
    let component = |version: &str| FirmwareComponent {
        known_firmware: vec![FirmwareEntry {
            version: version.into(),
            default: true,
            ..Default::default()
        }],
        ..Default::default()
    };
    let configured = Firmware {
        vendor: BMCVendor::Dell,
        model: "PowerEdge R750".into(),
        components: HashMap::from([(FirmwareComponentType::Bmc, component("base"))]),
        ..Default::default()
    };
    let config = FirmwareConfig::new(
        Default::default(),
        &HashMap::from([("host".into(), configured)]),
        &HashMap::new(),
    );
    let runtime = HostFirmwareConfig {
        vendor: BMCVendor::Dell,
        model: "PowerEdge R750".into(),
        components: HashMap::from([(FirmwareComponentType::Bmc, component("override"))]),
        ..Default::default()
    };
    let expected = |version: &str| {
        Some(ConfiguredHostFirmware {
            vendor: BMCVendor::Dell,
            model: "PowerEdge R750".into(),
            versions: DesiredFirmwareVersions {
                versions: HashMap::from([(FirmwareComponentType::Bmc, version.into())]),
            },
        })
    };

    check_values(
        [
            Check {
                scenario: "static default",
                input: (vec![], "PowerEdge R750"),
                expect: expected("base"),
            },
            Check {
                scenario: "runtime default",
                input: (vec![runtime], "PowerEdge R750"),
                expect: expected("override"),
            },
            Check {
                scenario: "absent policy",
                input: (vec![], "unconfigured"),
                expect: None,
            },
        ],
        |(overrides, model)| {
            let catalog = config.create_snapshot_with_overrides(overrides);
            let report = EndpointExplorationReport {
                vendor: Some(BMCVendor::Dell),
                model: Some(model.into()),
                ..Default::default()
            };

            host_expectations(&catalog, &report)
        },
    );
}

/// The original unset-start request and an assigned host's replacement remain
/// independent of the scout target/task/start frozen in controller state.
#[test]
fn active_host_target_and_outstanding_request_remain_independent() {
    struct Case {
        requested_at: DateTime<Utc>,
        assigned: bool,
    }
    let expected = |requested_at| StoredFirmwareActions {
        current: vec![CurrentFirmwareWork {
            work: FirmwareWork::Host {
                component: Some(FirmwareComponentType::Bmc),
                version: Some("frozen-version".into()),
            },
            job_id: Some("scout-task".into()),
            started_at: Some(accepted_at() + chrono::Duration::minutes(1)),
            password_recovery_job_id: None,
        }],
        outstanding_requests: vec![FirmwareRequestEvidence {
            source: FirmwareRequestSource::HostReprovision,
            work: FirmwareWork::Host {
                component: None,
                version: None,
            },
            requested_at: Some(requested_at),
            initiator: Some("operator".into()),
        }],
    };
    let replacement_at = accepted_at() + chrono::Duration::minutes(2);

    check_values(
        [
            Check {
                scenario: "unproved original association",
                input: Case {
                    requested_at: accepted_at(),
                    assigned: false,
                },
                expect: expected(accepted_at()),
            },
            Check {
                scenario: "assigned replacement with the same operation",
                input: Case {
                    requested_at: replacement_at,
                    assigned: true,
                },
                expect: expected(replacement_at),
            },
        ],
        |case| {
            let mut host = host_fixture();
            let reprovision_state = HostReprovisionState::WaitingForScoutUpgrade {
                upgrade_task_id: "scout-task".into(),
                firmware_type: FirmwareComponentType::Bmc,
                final_version: "frozen-version".into(),
                power_drains_needed: None,
                started_at: accepted_at() + chrono::Duration::minutes(1),
                deadline: accepted_at() + chrono::Duration::hours(1),
                task_json: "scout-payload-secret".into(),
                result: Some(ScoutUpgradeResult {
                    success: false,
                    exit_code: 1,
                    stdout: "stdout-secret".into(),
                    stderr: "stderr-secret".into(),
                    error: "error-secret".into(),
                }),
            };
            host.state.value = if case.assigned {
                ManagedHostState::Assigned {
                    instance_state: InstanceState::HostReprovision { reprovision_state },
                }
            } else {
                ManagedHostState::HostReprovision {
                    reprovision_state,
                    retry_count: 0,
                }
            };
            host.host_reprovision_requested = Some(HostReprovisionRequest {
                requested_at: case.requested_at,
                ..host_request()
            });

            let actions = machine(&host, None);

            assert!(!format!("{actions:?}").contains("secret"));
            actions
        },
    );
}

/// Exhaustion omits settled controller work but cannot identify an old request
/// versus a replacement/reset. DPU firmware remains a separate controller flow.
#[test]
fn controller_selection_preserves_uncertain_recovery_requests() {
    struct Case {
        state: ManagedHostState,
        request: Option<HostReprovisionRequest>,
    }
    let failed = |retry_count| ManagedHostState::HostReprovision {
        retry_count,
        reprovision_state: HostReprovisionState::FailedFirmwareUpgrade {
            firmware_type: FirmwareComponentType::Bmc,
            report_time: Some(accepted_at()),
            reason: Some("diagnostic-secret".into()),
        },
    };

    check_values(
        [
            Check {
                scenario: "unrelated DPU firmware",
                input: Case {
                    state: ManagedHostState::DPUReprovision {
                        dpu_states: model::machine::DpuReprovisionStates {
                            states: HashMap::from([(
                                model::test_support::machine_snapshot::dpu_machine_id(0),
                                model::machine::ReprovisionState::FirmwareUpgrade,
                            )]),
                        },
                    },
                    request: None,
                },
                expect: StoredFirmwareActions::default(),
            },
            Check {
                scenario: "host retry wait",
                input: Case {
                    state: failed(0),
                    request: None,
                },
                expect: StoredFirmwareActions {
                    current: vec![CurrentFirmwareWork {
                        work: FirmwareWork::Host {
                            component: Some(FirmwareComponentType::Bmc),
                            version: None,
                        },
                        job_id: None,
                        started_at: None,
                        password_recovery_job_id: None,
                    }],
                    outstanding_requests: vec![],
                },
            },
            Check {
                scenario: "ordinary started request survives exhausted failure",
                input: Case {
                    state: failed(MAX_FIRMWARE_UPGRADE_RETRIES),
                    request: Some(HostReprovisionRequest {
                        started_at: Some(accepted_at()),
                        request_reset: Some(false),
                        ..host_request()
                    }),
                },
                expect: StoredFirmwareActions {
                    current: vec![],
                    outstanding_requests: vec![FirmwareRequestEvidence {
                        source: FirmwareRequestSource::HostReprovision,
                        work: FirmwareWork::Host {
                            component: None,
                            version: None,
                        },
                        requested_at: Some(accepted_at()),
                        initiator: Some("operator".into()),
                    }],
                },
            },
        ],
        |case| {
            let mut host = host_fixture();
            host.state.value = case.state;
            host.host_reprovision_requested = case.request;

            let actions = machine(&host, None);

            assert!(!format!("{actions:?}").contains("diagnostic-secret"));
            actions
        },
    );
}

/// Redfish poll-start is not submission time. Checking states retain a known
/// component, while the model's Unknown component does not invent a valid one.
#[test]
fn non_scout_host_metadata_preserves_only_recorded_facts() {
    check_values(
        [
            Check {
                scenario: "opaque Redfish task ID and poll-start time",
                input: HostReprovisionState::WaitingForFirmwareUpgrade {
                    task_id: "unknown".into(),
                    final_version: "bmc-target".into(),
                    firmware_type: FirmwareComponentType::Bmc,
                    power_drains_needed: None,
                    firmware_number: None,
                    started_waiting: Some(accepted_at()),
                },
                expect: CurrentFirmwareWork {
                    work: FirmwareWork::Host {
                        component: Some(FirmwareComponentType::Bmc),
                        version: Some("bmc-target".into()),
                    },
                    job_id: Some("unknown".into()),
                    started_at: None,
                    password_recovery_job_id: None,
                },
            },
            Check {
                scenario: "known checking component without a target or task",
                input: HostReprovisionState::CheckingFirmwareV2 {
                    firmware_type: Some(FirmwareComponentType::Uefi),
                    firmware_number: None,
                },
                expect: CurrentFirmwareWork {
                    work: FirmwareWork::Host {
                        component: Some(FirmwareComponentType::Uefi),
                        version: None,
                    },
                    job_id: None,
                    started_at: None,
                    password_recovery_job_id: None,
                },
            },
            Check {
                scenario: "unknown configured component",
                input: HostReprovisionState::CheckingFirmwareV2 {
                    firmware_type: Some(FirmwareComponentType::Unknown),
                    firmware_number: None,
                },
                expect: CurrentFirmwareWork {
                    work: FirmwareWork::Host {
                        component: None,
                        version: None,
                    },
                    job_id: None,
                    started_at: None,
                    password_recovery_job_id: None,
                },
            },
        ],
        |state| host_current(&state),
    );
}

/// The owning active phase preserves its job's facts without an accepted scope.
/// Typed identity wins over MAC fallback; settled member history stays excluded.
#[test]
fn rack_current_work_uses_published_job_and_matching_member() {
    let switch = switch_fixture();
    let mut rack = rack_fixture();
    rack.firmware_upgrade_job = Some(switch_firmware_job(&switch));

    check_values(
        [
            Check {
                scenario: "published current metadata without a scope",
                input: {
                    let mut r = rack.clone();
                    r.config.maintenance_requested = None;
                    r
                },
                expect: StoredFirmwareActions {
                    current: vec![published_firmware()],
                    outstanding_requests: vec![],
                },
            },
            Check {
                scenario: "empty node ID uses selected management MAC",
                input: {
                    let mut r = rack.clone();
                    r.firmware_upgrade_job.as_mut().expect("job").switches[0]
                        .node_id
                        .clear();
                    r
                },
                expect: StoredFirmwareActions {
                    current: vec![published_firmware()],
                    outstanding_requests: vec![rack_firmware_request()],
                },
            },
            Check {
                scenario: "unrelated typed ID cannot match its MAC",
                input: {
                    let mut r = rack.clone();
                    r.firmware_upgrade_job.as_mut().expect("job").switches[0].node_id =
                        SwitchId::from(uuid::Uuid::from_u128(2)).to_string();
                    r
                },
                expect: StoredFirmwareActions {
                    current: vec![],
                    outstanding_requests: vec![rack_firmware_request()],
                },
            },
            Check {
                scenario: "empty object and first selected placeholder job remain unknown",
                input: {
                    let mut r = rack.clone();
                    let job = r.firmware_upgrade_job.as_mut().expect("job");
                    job.firmware_id = Some(String::new());
                    job.job_id = Some("batch-job".into());
                    job.switches[0].job_id = Some("unknown".into());
                    job.switches[0].parent_job_id = Some("parent-job".into());
                    r
                },
                expect: StoredFirmwareActions {
                    current: vec![CurrentFirmwareWork {
                        started_at: Some(accepted_at()),
                        ..firmware_wait()
                    }],
                    outstanding_requests: vec![rack_firmware_request()],
                },
            },
            Check {
                scenario: "settled firmware member while peer remains",
                input: {
                    let mut r = rack.clone();
                    r.firmware_upgrade_job.as_mut().expect("job").switches[0].status =
                        FirmwareProgressState::Completed;
                    r
                },
                expect: StoredFirmwareActions::default(),
            },
            Check {
                scenario: "terminal history after the owning phase",
                input: {
                    let mut r = rack.clone();
                    r.controller_state.value = RackState::Ready;
                    r.config.maintenance_requested = None;
                    let job = r.firmware_upgrade_job.as_mut().expect("job");
                    job.status = Some(FirmwareProgressState::Completed);
                    job.completed_at = Some(accepted_at());
                    for device in job.all_devices_mut() {
                        device.status = FirmwareProgressState::Completed;
                    }
                    r
                },
                expect: StoredFirmwareActions::default(),
            },
            Check {
                scenario: "absent scope implicit Start is not submission",
                input: {
                    let mut r = rack;
                    r.controller_state.value = RackState::Maintenance {
                        maintenance_state: RackMaintenanceState::FirmwareUpgrade {
                            rack_firmware_upgrade: FirmwareUpgradeState::Start,
                        },
                    };
                    r.config.maintenance_requested = None;
                    r.firmware_upgrade_job = None;
                    r
                },
                expect: StoredFirmwareActions::default(),
            },
        ],
        |rack| {
            let actions = super::switch(&switch, Some(&rack));

            assert!(!format!("{actions:?}").contains("secret"));
            actions
        },
    );
}

/// An accepted empty scope selects both switch update phases. Repeated kinds
/// use the first activity, including its unknown target, rather than merging.
#[test]
fn default_scope_and_first_selected_activity_preserve_intent() {
    let switch = switch_fixture();
    let mut rack = rack_fixture();
    rack.controller_state.value = RackState::Maintenance {
        maintenance_state: RackMaintenanceState::FirmwareUpgrade {
            rack_firmware_upgrade: FirmwareUpgradeState::Start,
        },
    };
    let scope = rack.config.maintenance_requested.as_mut().expect("scope");
    scope.activities.clear();
    let expected_request = |work| FirmwareRequestEvidence {
        source: FirmwareRequestSource::RackMaintenance,
        work,
        requested_at: Some(accepted_at()),
        initiator: None,
    };

    check_values(
        [
            Check {
                scenario: "accepted default scope with unknown targets",
                input: rack.clone(),
                expect: StoredFirmwareActions {
                    current: vec![],
                    outstanding_requests: vec![
                        expected_request(FirmwareWork::FirmwareObject { object_id: None }),
                        expected_request(FirmwareWork::Nvos {
                            object_id: None,
                            version: None,
                        }),
                    ],
                },
            },
            Check {
                scenario: "first activity per kind only",
                input: {
                    let mut r = rack.clone();
                    r.config
                        .maintenance_requested
                        .as_mut()
                        .expect("scope")
                        .activities = vec![
                        MaintenanceActivity::FirmwareUpgrade {
                            firmware_version: Some("{}".into()),
                            components: vec![],
                            force_update: false,
                        },
                        MaintenanceActivity::FirmwareUpgrade {
                            firmware_version: Some(r#"{"Id":"ignored-firmware"}"#.into()),
                            components: vec![],
                            force_update: true,
                        },
                        MaintenanceActivity::NvosUpdate {
                            config_json: r#"{"Id":"first-nvos"}"#.into(),
                        },
                        MaintenanceActivity::NvosUpdate {
                            config_json: r#"{"Id":"ignored-nvos"}"#.into(),
                        },
                    ];
                    r
                },
                expect: StoredFirmwareActions {
                    current: vec![],
                    outstanding_requests: vec![
                        expected_request(FirmwareWork::FirmwareObject { object_id: None }),
                        expected_request(FirmwareWork::Nvos {
                            object_id: Some("first-nvos".into()),
                            version: None,
                        }),
                    ],
                },
            },
            Check {
                scenario: "legacy default scope in later NVOS phase",
                input: {
                    let mut r = rack;
                    r.controller_state.value = RackState::Maintenance {
                        maintenance_state: RackMaintenanceState::NVOSUpdate {
                            nvos_update: NvosUpdateState::Start,
                        },
                    };
                    r.config
                        .maintenance_requested
                        .as_mut()
                        .expect("scope")
                        .requested_at = None;
                    r
                },
                expect: StoredFirmwareActions {
                    current: vec![],
                    outstanding_requests: vec![FirmwareRequestEvidence {
                        source: FirmwareRequestSource::RackMaintenance,
                        work: FirmwareWork::Nvos {
                            object_id: None,
                            version: None,
                        },
                        requested_at: None,
                        initiator: None,
                    }],
                },
            },
        ],
        |rack| super::switch(&switch, Some(&rack)),
    );
}

/// Saved device intent survives unknown/mismatched rack association. Only the
/// exact rack actor may supply the current per-device task metadata.
#[test]
fn switch_requests_survive_unknown_rack_without_borrowing_metadata() {
    let mut switch = switch_fixture();
    switch.controller_state.value = SwitchControllerState::ReProvisioning {
        reprovisioning_state: ReProvisioningState::WaitingForRackFirmwareUpgrade,
    };
    switch.switch_reprovisioning_requested = Some(SwitchReprovisionRequest {
        requested_at: accepted_at(),
        initiator: "rack-firmware-rack".into(),
        activities: vec![
            MaintenanceActivity::FirmwareUpgrade {
                firmware_version: Some(
                    r#"{"Id":"device-firmware","auth":"payload-secret"}"#.into(),
                ),
                components: vec![],
                force_update: false,
            },
            MaintenanceActivity::NvosUpdate {
                config_json: r#"{"Id":"device-nvos","access_token":"payload-secret"}"#.into(),
            },
        ],
    });
    switch.firmware_upgrade_status = Some(RackFirmwareUpgradeStatus {
        task_id: "device-job".into(),
        status: RackFirmwareUpgradeState::InProgress,
        started_at: Some(accepted_at()),
        ended_at: None,
    });
    let expected = |current| StoredFirmwareActions {
        current: vec![current],
        outstanding_requests: vec![
            FirmwareRequestEvidence {
                source: FirmwareRequestSource::SwitchReprovision,
                work: FirmwareWork::FirmwareObject {
                    object_id: Some("device-firmware".into()),
                },
                requested_at: Some(accepted_at()),
                initiator: Some("rack-firmware-rack".into()),
            },
            FirmwareRequestEvidence {
                source: FirmwareRequestSource::SwitchReprovision,
                work: FirmwareWork::Nvos {
                    object_id: Some("device-nvos".into()),
                    version: None,
                },
                requested_at: Some(accepted_at()),
                initiator: Some("rack-firmware-rack".into()),
            },
        ],
    };

    check_values(
        [
            Check {
                scenario: "positive saved switch entrypoint",
                input: switch.clone(),
                expect: expected(CurrentFirmwareWork {
                    job_id: Some("device-job".into()),
                    started_at: Some(accepted_at()),
                    ..firmware_wait()
                }),
            },
            Check {
                scenario: "rack association unknown",
                input: {
                    let mut s = switch.clone();
                    s.rack_id = None;
                    s
                },
                expect: expected(firmware_wait()),
            },
            Check {
                scenario: "end-only terminal firmware leaves independent NVOS request",
                input: {
                    let mut s = switch.clone();
                    s.firmware_upgrade_status = Some(RackFirmwareUpgradeStatus {
                        task_id: "failed-device-job".into(),
                        status: RackFirmwareUpgradeState::Failed {
                            cause: "cause-secret".into(),
                        },
                        started_at: None,
                        ended_at: Some(accepted_at()),
                    });
                    s
                },
                expect: StoredFirmwareActions {
                    current: vec![],
                    outstanding_requests: vec![FirmwareRequestEvidence {
                        source: FirmwareRequestSource::SwitchReprovision,
                        work: FirmwareWork::Nvos {
                            object_id: Some("device-nvos".into()),
                            version: None,
                        },
                        requested_at: Some(accepted_at()),
                        initiator: Some("rack-firmware-rack".into()),
                    }],
                },
            },
            Check {
                scenario: "rack association differs from saved actor",
                input: {
                    let mut s = switch;
                    s.rack_id = Some(RackId::new("other-rack"));
                    s
                },
                expect: expected(firmware_wait()),
            },
        ],
        |switch| {
            let actions = super::switch(&switch, None);

            assert!(!format!("{actions:?}").contains("secret"));
            actions
        },
    );
}

/// The switch controller has its own saved NVOS result, separate from the rack
/// job. Preserve that result's metadata and consume its request only on completion.
#[test]
fn switch_nvos_wait_preserves_device_metadata_until_completion() {
    let mut switch = switch_fixture();
    switch.controller_state.value = SwitchControllerState::ReProvisioning {
        reprovisioning_state: ReProvisioningState::WaitingForNVOSUpgrade,
    };
    switch.switch_reprovisioning_requested = Some(SwitchReprovisionRequest {
        requested_at: accepted_at(),
        initiator: "rack-firmware-rack".into(),
        activities: vec![
            MaintenanceActivity::FirmwareUpgrade {
                firmware_version: Some(r#"{"Id":"finished-firmware"}"#.into()),
                components: vec![],
                force_update: false,
            },
            MaintenanceActivity::NvosUpdate {
                config_json: r#"{"Id":"requested-nvos","auth":"payload-secret"}"#.into(),
            },
        ],
    });
    // Firmware has finished before this controller enters its NVOS wait.
    switch.firmware_upgrade_status = Some(RackFirmwareUpgradeStatus {
        task_id: "finished-firmware-job".into(),
        status: RackFirmwareUpgradeState::Completed,
        started_at: Some(accepted_at()),
        ended_at: Some(accepted_at()),
    });
    let saved_status = SwitchNvosUpdateStatus {
        task_id: "device-image-job".into(),
        firmware_id: "observed-nvos".into(),
        image_filename: "image-path-secret".into(),
        status: SwitchNvosUpdateState::InProgress,
        started_at: Some(accepted_at()),
        ended_at: None,
    };

    check_values(
        [
            Check {
                scenario: "saved device result retains its own target and task",
                input: saved_status.clone(),
                expect: StoredFirmwareActions {
                    current: vec![CurrentFirmwareWork {
                        work: FirmwareWork::Nvos {
                            object_id: Some("observed-nvos".into()),
                            version: None,
                        },
                        job_id: Some("device-image-job".into()),
                        started_at: Some(accepted_at()),
                        password_recovery_job_id: None,
                    }],
                    outstanding_requests: vec![FirmwareRequestEvidence {
                        source: FirmwareRequestSource::SwitchReprovision,
                        work: FirmwareWork::Nvos {
                            object_id: Some("requested-nvos".into()),
                            version: None,
                        },
                        requested_at: Some(accepted_at()),
                        initiator: Some("rack-firmware-rack".into()),
                    }],
                },
            },
            Check {
                scenario: "completed device result settles its saved request",
                input: SwitchNvosUpdateStatus {
                    status: SwitchNvosUpdateState::Completed,
                    ended_at: Some(accepted_at()),
                    ..saved_status
                },
                expect: StoredFirmwareActions::default(),
            },
        ],
        |status| {
            let mut switch = switch.clone();
            switch.nvos_update_status = Some(status);

            super::switch(&switch, None)
        },
    );
}

/// Termination clears child statuses before parent Error. Core's admitted
/// wait and saved request stay observable until the switch owner clears them.
#[test]
fn switch_cleanup_after_parent_error_remains_unknown_controller_work() {
    let mut switch = switch_fixture();
    switch.controller_state.value = SwitchControllerState::ReProvisioning {
        reprovisioning_state: ReProvisioningState::WaitingForRackFirmwareUpgrade,
    };
    switch.switch_reprovisioning_requested = Some(SwitchReprovisionRequest {
        requested_at: accepted_at(),
        initiator: "rack-firmware-rack".into(),
        activities: vec![MaintenanceActivity::FirmwareUpgrade {
            firmware_version: Some("{}".into()),
            components: vec![],
            force_update: false,
        }],
    });
    let mut rack = rack_fixture();
    rack.config.maintenance_requested = None;
    rack.controller_state.value = RackState::Error {
        cause: "rack-secret".into(),
        certificate_failure: None,
        recovery_policy: RackErrorRecoveryPolicy::MaintenanceRequestRequired,
    };

    let actions = super::switch(&switch, Some(&rack));

    assert_eq!(
        actions,
        StoredFirmwareActions {
            current: vec![firmware_wait()],
            outstanding_requests: vec![FirmwareRequestEvidence {
                source: FirmwareRequestSource::SwitchReprovision,
                work: FirmwareWork::FirmwareObject { object_id: None },
                requested_at: Some(accepted_at()),
                initiator: Some("rack-firmware-rack".into()),
            }],
        }
    );
    assert!(!format!("{actions:?}").contains("secret"));
}

/// Scope membership filters pending intent. These Ready fixtures contain no
/// active controller/job evidence; they do not claim to exercise cycle rejection.
#[test]
fn accepted_scope_selects_only_the_requested_member() {
    let switch = switch_fixture();
    let mut rack = rack_fixture();
    rack.controller_state.value = RackState::Ready;
    rack.config
        .maintenance_requested
        .as_mut()
        .expect("scope")
        .switch_ids = vec![switch.id];

    let actions = super::switch(&switch, Some(&rack));

    assert_eq!(
        actions,
        StoredFirmwareActions {
            current: vec![],
            outstanding_requests: vec![rack_firmware_request()],
        }
    );

    // A sibling's accepted request must not become this switch's intent.
    rack.config
        .maintenance_requested
        .as_mut()
        .expect("scope")
        .switch_ids = vec![SwitchId::from(uuid::Uuid::from_u128(2))];
    assert_eq!(
        super::switch(&switch, Some(&rack)),
        StoredFirmwareActions::default()
    );
}

/// Shelf request evidence exists before any poll and never invents an NVOS
/// phase. A Completed unblock marker settles only its associated saved intent.
#[test]
fn shelf_entrypoint_preserves_request_and_excludes_unblock_sentinel() {
    let shelf = shelf_fixture();
    let mut rack = rack_fixture();
    let peer = switch_fixture();
    rack.firmware_upgrade_job = Some(switch_firmware_job(&peer));
    let shelf_request = || FirmwareRequestEvidence {
        source: FirmwareRequestSource::ShelfReprovision,
        work: FirmwareWork::FirmwareObject {
            object_id: Some("requested-object".into()),
        },
        requested_at: Some(accepted_at()),
        initiator: Some("rack-firmware-rack".into()),
    };

    let actions = super::shelf(&shelf, Some(&rack));

    assert_eq!(
        actions,
        StoredFirmwareActions {
            current: vec![firmware_wait()],
            outstanding_requests: vec![rack_firmware_request(), shelf_request()],
        }
    );

    // The real writer emits this marker when its job has no shelf child. The
    // switch peer keeps the rack phase alive, so the scope must also be consumed.
    let mut completed = shelf;
    completed.firmware_upgrade_status = Some(RackFirmwareUpgradeStatus {
        task_id: "unblock-sentinel".into(),
        status: RackFirmwareUpgradeState::Completed,
        started_at: Some(accepted_at()),
        ended_at: Some(accepted_at()),
    });
    assert_eq!(
        super::shelf(&completed, Some(&rack)),
        StoredFirmwareActions::default()
    );

    // Without an owning rack association, that marker cannot erase the saved
    // rack-prefixed request. It still does not establish backend shelf work.
    completed.rack_id = None;
    assert_eq!(
        super::shelf(&completed, None),
        StoredFirmwareActions {
            current: vec![firmware_wait()],
            outstanding_requests: vec![shelf_request()],
        }
    );
}

/// A matching compute child supplies current job facts independently of the
/// saved host and rack request targets and their distinct record origins.
#[test]
fn machine_entrypoint_preserves_rack_current_and_both_request_sources() {
    let mut host = host_fixture();
    host.host_reprovision_requested = Some(HostReprovisionRequest {
        initiator: "rack-firmware-rack".into(),
        ..host_request()
    });
    let mut rack = rack_fixture();
    rack.firmware_upgrade_job = Some(FirmwareUpgradeJob {
        firmware_id: Some("submitted-object".into()),
        started_at: Some(accepted_at()),
        machines: vec![FirmwareUpgradeDeviceStatus {
            node_id: host.id.to_string(),
            mac: String::new(),
            bmc_ip: "192.0.2.1".into(),
            status: FirmwareProgressState::InProgress,
            job_id: Some("child-job".into()),
            parent_job_id: None,
            error_message: None,
        }],
        ..Default::default()
    });

    let actions = machine(&host, Some(&rack));

    assert_eq!(
        actions,
        StoredFirmwareActions {
            current: vec![published_firmware()],
            outstanding_requests: vec![
                rack_firmware_request(),
                FirmwareRequestEvidence {
                    source: FirmwareRequestSource::HostReprovision,
                    work: FirmwareWork::FirmwareObject { object_id: None },
                    requested_at: Some(accepted_at()),
                    initiator: Some("rack-firmware-rack".into()),
                }
            ],
        }
    );
}

/// Hardware completion cannot consume a rack request that an Assigned/Ready
/// host has not admitted through user or automatic approval.
#[test]
fn assigned_unapproved_host_keeps_request_after_hardware_completion() {
    let mut host = host_fixture();
    host.state.value = ManagedHostState::Assigned {
        instance_state: InstanceState::Ready,
    };
    host.host_reprovision_requested = Some(HostReprovisionRequest {
        initiator: "rack-firmware-rack".into(),
        ..host_request()
    });
    host.rack_fw_details = Some(RackFirmwareUpgradeStatus {
        task_id: "finished-hardware".into(),
        status: RackFirmwareUpgradeState::Completed,
        started_at: Some(accepted_at()),
        ended_at: Some(accepted_at()),
    });
    let mut rack = rack_fixture();
    rack.firmware_upgrade_job = Some(FirmwareUpgradeJob {
        firmware_id: Some("submitted-object".into()),
        started_at: Some(accepted_at()),
        completed_at: Some(accepted_at()),
        status: Some(FirmwareProgressState::Completed),
        machines: vec![FirmwareUpgradeDeviceStatus {
            node_id: host.id.to_string(),
            mac: String::new(),
            bmc_ip: "192.0.2.1".into(),
            status: FirmwareProgressState::Completed,
            job_id: Some("finished-hardware".into()),
            parent_job_id: None,
            error_message: None,
        }],
        ..Default::default()
    });

    let actions = machine(&host, Some(&rack));

    assert_eq!(
        actions,
        StoredFirmwareActions {
            current: vec![],
            outstanding_requests: vec![
                rack_firmware_request(),
                FirmwareRequestEvidence {
                    source: FirmwareRequestSource::HostReprovision,
                    work: FirmwareWork::FirmwareObject { object_id: None },
                    requested_at: Some(accepted_at()),
                    initiator: Some("rack-firmware-rack".into()),
                }
            ],
        }
    );
}

/// A host's admitted rack wait can use its current device status without a rack
/// snapshot. A real non-rack overwrite stays separate and cannot lend metadata.
#[test]
fn host_rack_wait_does_not_associate_a_nonrack_replacement() {
    let mut host = host_fixture();
    host.state.value = ManagedHostState::HostReprovision {
        reprovision_state: HostReprovisionState::WaitingForRackFirmwareUpgrade,
        retry_count: 1,
    };
    host.host_reprovision_requested = Some(HostReprovisionRequest {
        initiator: "rack-firmware-rack".into(),
        ..host_request()
    });
    host.rack_fw_details = Some(RackFirmwareUpgradeStatus {
        task_id: "rack-device-job".into(),
        status: RackFirmwareUpgradeState::InProgress,
        started_at: Some(accepted_at()),
        ended_at: None,
    });

    let actions = machine(&host, None);

    assert_eq!(
        actions,
        StoredFirmwareActions {
            current: vec![CurrentFirmwareWork {
                job_id: Some("rack-device-job".into()),
                started_at: Some(accepted_at()),
                ..firmware_wait()
            }],
            outstanding_requests: vec![FirmwareRequestEvidence {
                source: FirmwareRequestSource::HostReprovision,
                work: FirmwareWork::FirmwareObject { object_id: None },
                requested_at: Some(accepted_at()),
                initiator: Some("rack-firmware-rack".into()),
            }],
        }
    );

    // Manual Set overwrites this row while the old controller state can remain.
    let replacement_at = accepted_at() + chrono::Duration::minutes(1);
    host.host_reprovision_requested = Some(HostReprovisionRequest {
        requested_at: replacement_at,
        ..host_request()
    });
    assert_eq!(
        machine(&host, None),
        StoredFirmwareActions {
            current: vec![firmware_wait()],
            outstanding_requests: vec![FirmwareRequestEvidence {
                source: FirmwareRequestSource::HostReprovision,
                work: FirmwareWork::Host {
                    component: None,
                    version: None
                },
                requested_at: Some(replacement_at),
                initiator: Some("operator".into()),
            }],
        }
    );
}

/// NVOS image aggregate results can precede recovery. Exact image and recovery
/// handles remain distinct, and a settled member is excluded while peers finish.
#[test]
fn nvos_member_progress_and_recovery_determine_current_work() {
    struct Case {
        image_status: &'static str,
        password: NvosPasswordUpdateState,
        image_job: Option<&'static str>,
        aggregate_status: &'static str,
        aggregate_completed: bool,
        has_scope: bool,
    }
    let switch = switch_fixture();
    let mut rack = rack_fixture();
    rack.controller_state.value = RackState::Maintenance {
        maintenance_state: RackMaintenanceState::NVOSUpdate {
            nvos_update: NvosUpdateState::WaitForComplete,
        },
    };
    rack.config
        .maintenance_requested
        .as_mut()
        .expect("scope")
        .activities = vec![MaintenanceActivity::NvosUpdate {
        config_json: r#"{"Id":"nvos-object","access_token":"nvos-secret"}"#.into(),
    }];
    let expected_request = || FirmwareRequestEvidence {
        source: FirmwareRequestSource::RackMaintenance,
        work: FirmwareWork::Nvos {
            object_id: Some("nvos-object".into()),
            version: None,
        },
        requested_at: Some(accepted_at()),
        initiator: None,
    };
    let expected_current = |image_job: Option<&str>, recovery: Option<&str>| CurrentFirmwareWork {
        work: FirmwareWork::Nvos {
            object_id: Some("nvos-object".into()),
            version: None,
        },
        job_id: image_job.map(str::to_owned),
        started_at: Some(accepted_at()),
        password_recovery_job_id: recovery.map(str::to_owned),
    };

    check_values(
        [
            Check {
                scenario: "initial all-failed image result before recovery starts",
                input: Case {
                    image_status: "failed",
                    password: NvosPasswordUpdateState::NotStarted,
                    image_job: None,
                    aggregate_status: "failed",
                    aggregate_completed: true,
                    has_scope: true,
                },
                expect: StoredFirmwareActions {
                    current: vec![expected_current(None, None)],
                    outstanding_requests: vec![expected_request()],
                },
            },
            Check {
                scenario: "completed image recovery retains its separate handle",
                input: Case {
                    image_status: "completed",
                    password: NvosPasswordUpdateState::InProgress {
                        job_id: "password-job".into(),
                    },
                    image_job: Some("image-job"),
                    aggregate_status: "in_progress",
                    aggregate_completed: false,
                    has_scope: true,
                },
                expect: StoredFirmwareActions {
                    current: vec![expected_current(Some("image-job"), Some("password-job"))],
                    outstanding_requests: vec![expected_request()],
                },
            },
            Check {
                scenario: "failed image recovery still runs",
                input: Case {
                    image_status: "failed",
                    password: NvosPasswordUpdateState::InProgress {
                        job_id: "password-job".into(),
                    },
                    image_job: Some("image-job"),
                    aggregate_status: "in_progress",
                    aggregate_completed: false,
                    has_scope: true,
                },
                expect: StoredFirmwareActions {
                    current: vec![expected_current(Some("image-job"), Some("password-job"))],
                    outstanding_requests: vec![expected_request()],
                },
            },
            Check {
                scenario: "active NVOS job without an outstanding scope",
                input: Case {
                    image_status: "completed",
                    password: NvosPasswordUpdateState::InProgress {
                        job_id: "password-job".into(),
                    },
                    image_job: Some("image-job"),
                    aggregate_status: "in_progress",
                    aggregate_completed: false,
                    has_scope: false,
                },
                expect: StoredFirmwareActions {
                    current: vec![expected_current(Some("image-job"), Some("password-job"))],
                    outstanding_requests: vec![],
                },
            },
            Check {
                scenario: "selected switch settled while real peer remains",
                input: Case {
                    image_status: "completed",
                    password: NvosPasswordUpdateState::Completed,
                    image_job: Some("image-job"),
                    aggregate_status: "in_progress",
                    aggregate_completed: false,
                    has_scope: true,
                },
                expect: StoredFirmwareActions::default(),
            },
        ],
        |case| {
            let mut rack = rack.clone();
            if !case.has_scope {
                rack.config.maintenance_requested = None;
            }
            let child = NvosUpdateSwitchStatus {
                node_id: switch.id.to_string(),
                mac: "00:11:22:33:44:55".into(),
                bmc_ip: "192.0.2.1".into(),
                nvos_ip: "192.0.2.2".into(),
                status: case.image_status.into(),
                job_id: case.image_job.map(str::to_owned),
                error_message: Some("error-secret".into()),
                password_update: case.password,
            };
            // An initial all-failed submission has not begun recovery anywhere.
            // Later snapshots instantiate a recovering peer to keep the rack open.
            let peer = NvosUpdateSwitchStatus {
                node_id: SwitchId::from(uuid::Uuid::from_u128(2)).to_string(),
                mac: "00:11:22:33:44:66".into(),
                bmc_ip: "192.0.2.3".into(),
                nvos_ip: "192.0.2.4".into(),
                job_id: case.image_job.map(|_| "peer-image-job".into()),
                password_update: if case.aggregate_completed {
                    NvosPasswordUpdateState::NotStarted
                } else {
                    NvosPasswordUpdateState::InProgress {
                        job_id: "peer-password-job".into(),
                    }
                },
                ..child.clone()
            };
            rack.nvos_update_job = Some(NvosUpdateJob {
                firmware_id: "nvos-object".into(),
                version: None,
                started_at: Some(accepted_at()),
                completed_at: case.aggregate_completed.then_some(accepted_at()),
                status: Some(case.aggregate_status.into()),
                local_file_path: "path-secret".into(),
                switches: vec![child, peer],
                ..Default::default()
            });

            let actions = super::switch(&switch, Some(&rack));

            assert!(switch.switch_reprovisioning_requested.is_none());
            assert!(!format!("{actions:?}").contains("secret"));
            actions
        },
    );
}

/// Error recovery may accept a newer NVOS-only scope while retaining the old
/// completed job. Unknown legacy association also cannot consume saved intent.
#[test]
fn retained_nvos_history_does_not_consume_new_or_unknown_scope() {
    let switch = switch_fixture();
    let mut rack = rack_fixture();
    rack.controller_state.value = RackState::Error {
        cause: "previous maintenance terminated".into(),
        certificate_failure: None,
        recovery_policy: RackErrorRecoveryPolicy::MaintenanceRequestRequired,
    };
    rack.config
        .maintenance_requested
        .as_mut()
        .expect("scope")
        .activities = vec![MaintenanceActivity::NvosUpdate {
        config_json: r#"{"Id":"new-nvos"}"#.into(),
    }];
    rack.nvos_update_job = Some(NvosUpdateJob {
        firmware_id: "old-nvos".into(),
        started_at: Some(accepted_at()),
        completed_at: Some(accepted_at()),
        status: Some("completed".into()),
        switches: vec![NvosUpdateSwitchStatus {
            node_id: switch.id.to_string(),
            status: "completed".into(),
            password_update: NvosPasswordUpdateState::Completed,
            ..Default::default()
        }],
        ..Default::default()
    });
    let newer = accepted_at() + chrono::Duration::minutes(1);
    let expected = |requested_at| StoredFirmwareActions {
        current: vec![],
        outstanding_requests: vec![FirmwareRequestEvidence {
            source: FirmwareRequestSource::RackMaintenance,
            work: FirmwareWork::Nvos {
                object_id: Some("new-nvos".into()),
                version: None,
            },
            requested_at,
            initiator: None,
        }],
    };

    check_values(
        [
            Check {
                scenario: "new accepted Error recovery",
                input: Some(newer),
                expect: expected(Some(newer)),
            },
            Check {
                scenario: "unknown legacy acceptance boundary",
                input: None,
                expect: expected(None),
            },
        ],
        |requested_at| {
            let mut rack = rack.clone();
            rack.config
                .maintenance_requested
                .as_mut()
                .expect("scope")
                .requested_at = requested_at;

            super::switch(&switch, Some(&rack))
        },
    );
}

/// Missing/invalid identity never falls back to displaying inline SOT content.
/// A valid identity control proves that useful selected metadata still survives.
#[test]
fn selected_targets_exclude_identity_free_and_malformed_payloads() {
    check_values(
        [
            Check {
                scenario: "valid identity",
                input: r#"{"Id":"safe-id","object_id":"ignored-id","auth":"payload-secret"}"#,
                expect: Some("safe-id".into()),
            },
            Check {
                scenario: "legacy identity key",
                input: r#"{"object_id":"legacy-id","auth":"payload-secret"}"#,
                expect: Some("legacy-id".into()),
            },
            Check {
                scenario: "empty identity",
                input: r#"{"Id":"","auth":"payload-secret"}"#,
                expect: None,
            },
            Check {
                scenario: "identity-free JSON",
                input: r#"{"auth":{"token":"payload-secret"}}"#,
                expect: None,
            },
            Check {
                scenario: "malformed JSON",
                input: r#"{"Id":"payload-secret""#,
                expect: None,
            },
        ],
        |json| {
            let activities = [MaintenanceActivity::FirmwareUpgrade {
                firmware_version: Some(json.into()),
                components: vec![],
                force_update: false,
            }];

            let work = FirmwareActivity::FirmwareObject
                .selected_work(&activities)
                .expect("selected firmware activity");

            assert!(!format!("{work:?}").contains("payload-secret"));
            let FirmwareWork::FirmwareObject { object_id } = work else {
                panic!("firmware target");
            };
            object_id
        },
    );
}

/// A real all-selected request can survive into NMX-C after both firmware and
/// NVOS complete. Neither earlier activity should reappear from that record.
#[test]
fn later_switch_reprovision_phase_excludes_completed_update_requests() {
    let mut switch = switch_fixture();
    switch.controller_state.value = SwitchControllerState::ReProvisioning {
        reprovisioning_state: ReProvisioningState::WaitingForNMXCConfigure,
    };
    switch.switch_reprovisioning_requested = Some(SwitchReprovisionRequest {
        requested_at: accepted_at(),
        initiator: "rack-firmware-rack".into(),
        activities: vec![
            MaintenanceActivity::FirmwareUpgrade {
                firmware_version: Some(r#"{"Id":"old-object"}"#.into()),
                components: vec![],
                force_update: false,
            },
            MaintenanceActivity::NvosUpdate {
                config_json: r#"{"Id":"old-nvos"}"#.into(),
            },
            MaintenanceActivity::ConfigureNmxCluster,
        ],
    });

    let actions = super::switch(&switch, None);

    assert_eq!(actions, StoredFirmwareActions::default());
}

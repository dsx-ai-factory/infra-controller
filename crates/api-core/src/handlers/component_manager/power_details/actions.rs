// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Reports explicit saved maintenance without guessing at hardware progress.
//! Active comes before pending, which is suppressed only when the complete
//! request equals the admitted request. Legacy active states have no request
//! metadata, so they cannot establish equality or supply submission details.

use ::rpc::forge::{
    self as rpc, ComponentPowerActionOperation as Operation, ComponentPowerActionStatus as Status,
};
use chrono::{DateTime, Utc};
use model::machine::{MachineMaintenanceOperation, MachineMaintenanceRequest, ManagedHostState};
use model::power_shelf::{
    PowerShelfControllerState, PowerShelfMaintenanceOperation, PowerShelfMaintenanceRequest,
};
use model::switch::{SwitchControllerState, SwitchMaintenanceOperation, SwitchMaintenanceRequest};

/// `machine` reports host maintenance, preserving component and chassis resets.
pub(super) fn machine(
    state: &ManagedHostState,
    pending: Option<&MachineMaintenanceRequest>,
) -> rpc::ComponentPowerActionDetails {
    let (operation, admitted) = match state {
        ManagedHostState::Maintenance { operation, request } => (Some(operation), request.as_ref()),
        _ => (None, None),
    };
    let active = operation.map(|operation| {
        describe(
            machine_operation(operation),
            Status::Active,
            admitted.map(|request| (request.requested_at, request.initiator.as_str())),
        )
    });
    let pending = pending
        .filter(|request| Some(*request) != admitted)
        .map(|request| {
            describe(
                machine_operation(&request.operation),
                Status::Pending,
                Some((request.requested_at, request.initiator.as_str())),
            )
        });
    rpc::ComponentPowerActionDetails {
        actions: active.into_iter().chain(pending).collect(),
        error: None,
    }
}

/// `switch` omits certificate maintenance while preserving an unrelated pending power request.
pub(super) fn switch(
    state: &SwitchControllerState,
    pending: Option<&SwitchMaintenanceRequest>,
) -> rpc::ComponentPowerActionDetails {
    let (operation, admitted) = match state {
        SwitchControllerState::Maintenance {
            operation, request, ..
        } => (Some(operation), request.as_ref()),
        _ => (None, None),
    };
    let active = operation.and_then(switch_operation).map(|operation| {
        describe(
            operation,
            Status::Active,
            admitted.map(|request| (request.requested_at, request.initiator.as_str())),
        )
    });
    let pending = pending
        .filter(|request| Some(*request) != admitted)
        .and_then(|request| {
            switch_operation(&request.operation).map(|operation| {
                describe(
                    operation,
                    Status::Pending,
                    Some((request.requested_at, request.initiator.as_str())),
                )
            })
        });
    rpc::ComponentPowerActionDetails {
        actions: active.into_iter().chain(pending).collect(),
        error: None,
    }
}

/// `shelf` reports the shelf's saved on/off maintenance and pending request.
pub(super) fn shelf(
    state: &PowerShelfControllerState,
    pending: Option<&PowerShelfMaintenanceRequest>,
) -> rpc::ComponentPowerActionDetails {
    let (operation, admitted) = match state {
        PowerShelfControllerState::Maintenance { operation, request } => {
            (Some(operation), request.as_ref())
        }
        _ => (None, None),
    };
    let active = operation.map(|operation| {
        describe(
            shelf_operation(operation),
            Status::Active,
            admitted.map(|request| (request.requested_at, request.initiator.as_str())),
        )
    });
    let pending = pending
        .filter(|request| Some(*request) != admitted)
        .map(|request| {
            describe(
                shelf_operation(&request.operation),
                Status::Pending,
                Some((request.requested_at, request.initiator.as_str())),
            )
        });
    rpc::ComponentPowerActionDetails {
        actions: active.into_iter().chain(pending).collect(),
        error: None,
    }
}

/// `describe` leaves legacy request metadata absent instead of substituting state-version time.
fn describe(
    mut action: rpc::ComponentPowerAction,
    status: Status,
    request: Option<(DateTime<Utc>, &str)>,
) -> rpc::ComponentPowerAction {
    action.status = status as i32;
    if let Some((requested_at, initiator)) = request {
        action.requested_at = Some(requested_at.into());
        action.initiator = Some(initiator.to_owned());
    }
    action
}

/// `machine_operation` preserves chassis targeting and the recorded shutdown mode.
fn machine_operation(operation: &MachineMaintenanceOperation) -> rpc::ComponentPowerAction {
    let (operation, graceful, chassis_id) = match operation {
        MachineMaintenanceOperation::PowerOn => (Operation::PowerOn, None, None),
        MachineMaintenanceOperation::PowerOff { graceful } => {
            (Operation::PowerOff, Some(*graceful), None)
        }
        MachineMaintenanceOperation::Reset => (Operation::Reset, None, None),
        MachineMaintenanceOperation::ChassisReset { chassis_id } => {
            (Operation::ChassisReset, None, Some(chassis_id.clone()))
        }
    };
    rpc::ComponentPowerAction {
        operation: operation as i32,
        graceful,
        chassis_id,
        ..Default::default()
    }
}

/// `switch_operation` has no power action for certificate-only maintenance.
fn switch_operation(operation: &SwitchMaintenanceOperation) -> Option<rpc::ComponentPowerAction> {
    let (operation, graceful) = match operation {
        SwitchMaintenanceOperation::PowerOn => (Operation::PowerOn, None),
        SwitchMaintenanceOperation::PowerOff { graceful } => (Operation::PowerOff, Some(*graceful)),
        SwitchMaintenanceOperation::Reset => (Operation::Reset, None),
        SwitchMaintenanceOperation::ReconfigureCertificate => return None,
    };
    Some(rpc::ComponentPowerAction {
        operation: operation as i32,
        graceful,
        ..Default::default()
    })
}

/// `shelf_operation` reports the saved on/off operation, including graceful shutdown.
fn shelf_operation(operation: &PowerShelfMaintenanceOperation) -> rpc::ComponentPowerAction {
    let (operation, graceful) = match operation {
        PowerShelfMaintenanceOperation::PowerOn => (Operation::PowerOn, None),
        PowerShelfMaintenanceOperation::PowerOff { graceful } => {
            (Operation::PowerOff, Some(*graceful))
        }
    };
    rpc::ComponentPowerAction {
        operation: operation as i32,
        graceful,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::{Check, check_values};
    use prost::Message;

    use super::*;

    /// Replacing any request field must keep pending work visible; legacy active
    /// states cannot supply submission metadata or prove a duplicate request.
    #[test]
    fn machine_actions_keep_replacements_and_legacy_metadata() {
        let admitted = MachineMaintenanceRequest {
            requested_at: "2026-10-01T00:00:00Z".parse().unwrap(),
            initiator: "operator".into(),
            operation: MachineMaintenanceOperation::PowerOff { graceful: false },
        };
        let active = rpc::ComponentPowerAction {
            status: Status::Active as i32,
            operation: Operation::PowerOff as i32,
            requested_at: Some(admitted.requested_at.into()),
            initiator: Some("operator".into()),
            graceful: Some(false),
            ..Default::default()
        };
        let pending = rpc::ComponentPowerAction {
            status: Status::Pending as i32,
            ..active.clone()
        };
        let state = ManagedHostState::maintenance_for_request(admitted.clone());
        let later = admitted.requested_at + chrono::Duration::seconds(1);

        check_values(
            [
                // Admission leaves the identical pending request set until completion.
                Check {
                    scenario: "identical",
                    input: admitted.clone(),
                    expect: vec![active.clone()],
                },
                // Submitting the same action again creates a distinct pending request.
                Check {
                    scenario: "later",
                    input: MachineMaintenanceRequest {
                        requested_at: later,
                        ..admitted.clone()
                    },
                    expect: vec![
                        active.clone(),
                        rpc::ComponentPowerAction {
                            requested_at: Some(later.into()),
                            ..pending.clone()
                        },
                    ],
                },
                // An explicitly empty initiator remains present and changes equality.
                Check {
                    scenario: "initiator",
                    input: MachineMaintenanceRequest {
                        initiator: String::new(),
                        ..admitted.clone()
                    },
                    expect: vec![
                        active.clone(),
                        rpc::ComponentPowerAction {
                            initiator: Some(String::new()),
                            ..pending.clone()
                        },
                    ],
                },
                // Shutdown mode is part of the operation, not just the PowerOff name.
                Check {
                    scenario: "graceful",
                    input: MachineMaintenanceRequest {
                        operation: MachineMaintenanceOperation::PowerOff { graceful: true },
                        ..admitted.clone()
                    },
                    expect: vec![
                        active.clone(),
                        rpc::ComponentPowerAction {
                            graceful: Some(true),
                            ..pending.clone()
                        },
                    ],
                },
            ],
            |request| {
                let details = machine(&state, Some(&request));
                assert_eq!(details.error, None);
                details.actions
            },
        );

        check_values(
            [
                // Legacy JSON may omit request metadata and the graceful flag.
                // Its default forced-off action cannot prove a pending duplicate.
                Check {
                    scenario: "legacy active state",
                    input: serde_json::from_value(serde_json::json!({
                        "state": "maintenance", "operation": { "operation": "poweroff" },
                    }))
                    .unwrap(),
                    expect: vec![
                        rpc::ComponentPowerAction {
                            requested_at: None,
                            initiator: None,
                            ..active
                        },
                        pending.clone(),
                    ],
                },
                // Saved requests are visible before the controller admits them.
                Check {
                    scenario: "before admission",
                    input: ManagedHostState::Ready,
                    expect: vec![pending],
                },
            ],
            |state| machine(&state, Some(&admitted)).actions,
        );
    }

    /// Switch and shelf requests have independent equality paths. A changed
    /// initiator must survive, and legacy shutdown JSON must preserve forced off
    /// without inventing active metadata or hiding a same-operation request.
    #[test]
    fn switch_and_shelf_actions_keep_replacements_and_legacy_shutdowns() {
        let requested_at = "2026-10-01T00:00:00Z".parse().unwrap();
        let switch_request = SwitchMaintenanceRequest {
            requested_at,
            initiator: "operator".into(),
            operation: SwitchMaintenanceOperation::PowerOff { graceful: false },
        };
        let shelf_request = PowerShelfMaintenanceRequest {
            requested_at,
            initiator: "operator".into(),
            operation: PowerShelfMaintenanceOperation::PowerOff { graceful: false },
        };
        let active = rpc::ComponentPowerAction {
            status: Status::Active as i32,
            operation: Operation::PowerOff as i32,
            requested_at: Some(requested_at.into()),
            initiator: Some("operator".into()),
            graceful: Some(false),
            ..Default::default()
        };
        let pending = rpc::ComponentPowerAction {
            status: Status::Pending as i32,
            ..active.clone()
        };
        let replacement_expected = vec![
            active.clone(),
            rpc::ComponentPowerAction {
                initiator: Some("replacement operator".into()),
                ..pending.clone()
            },
        ];
        let legacy_expected = vec![
            rpc::ComponentPowerAction {
                requested_at: None,
                initiator: None,
                ..active
            },
            pending,
        ];
        // Omitting graceful exercises the old serde default, not explicit false.
        let legacy = serde_json::json!({
            "state": "maintenance", "operation": { "operation": "poweroff" },
        });

        check_values(
            [
                // Comparing only the switch operation/time would lose this replacement.
                Check {
                    scenario: "switch changed initiator",
                    input: (
                        SwitchControllerState::maintenance_for_request(switch_request.clone()),
                        SwitchMaintenanceRequest {
                            initiator: "replacement operator".into(),
                            ..switch_request.clone()
                        },
                    ),
                    expect: replacement_expected.clone(),
                },
                // Legacy switch metadata must stay absent even with the same pending action.
                Check {
                    scenario: "legacy switch shutdown",
                    input: (
                        serde_json::from_value(legacy.clone()).unwrap(),
                        switch_request,
                    ),
                    expect: legacy_expected.clone(),
                },
            ],
            |(state, request)| switch(&state, Some(&request)).actions,
        );

        check_values(
            [
                // Shelf has a separate request type and comparison from switch.
                Check {
                    scenario: "shelf changed initiator",
                    input: (
                        PowerShelfControllerState::Maintenance {
                            operation: shelf_request.operation,
                            request: Some(shelf_request.clone()),
                        },
                        PowerShelfMaintenanceRequest {
                            initiator: "replacement operator".into(),
                            ..shelf_request.clone()
                        },
                    ),
                    expect: replacement_expected,
                },
                // Legacy shelf JSON must expose false rather than an absent shutdown mode.
                Check {
                    scenario: "legacy shelf shutdown",
                    input: (serde_json::from_value(legacy).unwrap(), shelf_request),
                    expect: legacy_expected,
                },
            ],
            |(state, request)| shelf(&state, Some(&request)).actions,
        );
    }

    /// Each component has its own persisted operation enum; all three must
    /// report PowerOn without a shutdown mode or chassis selector.
    #[test]
    fn power_on_mapping_preserves_the_operation_for_each_component_kind() {
        let expected = rpc::ComponentPowerAction {
            operation: Operation::PowerOn as i32,
            ..Default::default()
        };
        for (kind, action) in [
            // Compute must not confuse an on request with its reset variants.
            (
                "compute",
                machine_operation(&MachineMaintenanceOperation::PowerOn),
            ),
            // The switch mapper excludes certificates, but must retain power on.
            (
                "switch",
                switch_operation(&SwitchMaintenanceOperation::PowerOn).unwrap(),
            ),
            // Shelves have a separate operation enum with no reset variant.
            (
                "shelf",
                shelf_operation(&PowerShelfMaintenanceOperation::PowerOn),
            ),
        ] {
            assert_eq!(action, expected, "{kind}");
        }
    }

    /// Component reset and chassis reset must remain distinguishable because
    /// only the latter targets a specific Redfish chassis.
    #[test]
    fn machine_reset_actions_keep_the_chassis_target() {
        check_values(
            [
                // Whole-component reset must not invent a chassis selector.
                Check {
                    scenario: "component reset",
                    input: MachineMaintenanceOperation::Reset,
                    expect: rpc::ComponentPowerAction {
                        operation: Operation::Reset as i32,
                        ..Default::default()
                    },
                },
                // Chassis reset preserves the exact persisted Redfish target.
                Check {
                    scenario: "chassis reset",
                    input: MachineMaintenanceOperation::ChassisReset {
                        chassis_id: "HGX_0".into(),
                    },
                    expect: rpc::ComponentPowerAction {
                        operation: Operation::ChassisReset as i32,
                        chassis_id: Some("HGX_0".into()),
                        ..Default::default()
                    },
                },
            ],
            |operation| machine_operation(&operation),
        );
    }

    /// Certificate work must never appear as power maintenance, even when a
    /// pending power request is waiting behind it or replaces a power request.
    #[test]
    fn switch_actions_exclude_certificate_requests_in_both_positions() {
        let certificate = SwitchMaintenanceRequest {
            requested_at: "2026-10-01T00:00:00Z".parse().unwrap(),
            initiator: "operator".into(),
            operation: SwitchMaintenanceOperation::ReconfigureCertificate,
        };
        let pending_at = certificate.requested_at + chrono::Duration::seconds(1);
        let reset = rpc::ComponentPowerAction {
            status: Status::Active as i32,
            operation: Operation::Reset as i32,
            requested_at: Some(certificate.requested_at.into()),
            initiator: Some("operator".into()),
            ..Default::default()
        };

        check_values(
            [
                // Distinct timestamps ensure certificate filtering, not equality,
                // removes the pending request as well as the active operation.
                Check {
                    scenario: "certificate only",
                    input: (
                        SwitchMaintenanceOperation::ReconfigureCertificate,
                        SwitchMaintenanceOperation::ReconfigureCertificate,
                    ),
                    expect: vec![],
                },
                // Active certificate work must not erase a queued reset.
                Check {
                    scenario: "pending reset",
                    input: (
                        SwitchMaintenanceOperation::ReconfigureCertificate,
                        SwitchMaintenanceOperation::Reset,
                    ),
                    expect: vec![rpc::ComponentPowerAction {
                        status: Status::Pending as i32,
                        requested_at: Some(pending_at.into()),
                        ..reset.clone()
                    }],
                },
                // A pending certificate must not erase the active reset.
                Check {
                    scenario: "active reset",
                    input: (
                        SwitchMaintenanceOperation::Reset,
                        SwitchMaintenanceOperation::ReconfigureCertificate,
                    ),
                    expect: vec![reset],
                },
            ],
            |(active, pending)| {
                let state =
                    SwitchControllerState::maintenance_for_request(SwitchMaintenanceRequest {
                        operation: active,
                        ..certificate.clone()
                    });
                let request = SwitchMaintenanceRequest {
                    operation: pending,
                    requested_at: pending_at,
                    ..certificate.clone()
                };

                switch(&state, Some(&request)).actions
            },
        );
    }

    /// Protobuf presence must survive serialization so clients can distinguish
    /// an older server from a successful read that found no tracked actions.
    #[test]
    fn action_details_preserve_message_presence_on_the_wire() {
        check_values(
            [
                // An older server does not provide the new field at all.
                Check {
                    scenario: "unprovided",
                    input: None,
                    expect: None,
                },
                // A supporting server may confirm that no tracked actions exist.
                Check {
                    scenario: "confirmed empty",
                    input: Some(rpc::ComponentPowerActionDetails::default()),
                    expect: Some(rpc::ComponentPowerActionDetails::default()),
                },
            ],
            |actions| {
                let details = rpc::ComponentPowerDetails {
                    actions,
                    ..Default::default()
                };

                rpc::ComponentPowerDetails::decode(details.encode_to_vec().as_slice())
                    .unwrap()
                    .actions
            },
        );
    }
}

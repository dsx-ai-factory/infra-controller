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

use std::num::NonZeroU64;

use nv_redfish::core::ODataId;
use nv_redfish::resource::ResetType;
use serde_json::json;

use super::*;

fn task_reference() -> OperationReference {
    OperationReference::RedfishTask {
        uri: ODataId::from("/redfish/v1/TaskService/Tasks/42".to_string()),
        retry_after_seconds: Some(5),
    }
}

fn job_reference() -> OperationReference {
    OperationReference::VendorJob {
        job_id: "JID_42".parse().expect("fixture job id is valid"),
        retry_after_seconds: Some(10),
    }
}

#[test]
fn operation_references_are_stable() {
    let cases = [
        (
            task_reference(),
            json!({
                "type": "redfish_task",
                "uri": "/redfish/v1/TaskService/Tasks/42",
                "retry_after_seconds": 5
            }),
        ),
        (
            job_reference(),
            json!({
                "type": "vendor_job",
                "job_id": "JID_42",
                "retry_after_seconds": 10
            }),
        ),
    ];

    for (reference, expected) in cases {
        assert_eq!(
            serde_json::to_value(&reference).expect("reference serializes"),
            expected
        );
        assert_eq!(
            serde_json::from_value::<OperationReference>(expected).expect("reference deserializes"),
            reference
        );
    }

    assert!(
        serde_json::from_value::<OperationReference>(json!({
            "type": "vendor_job",
            "job_id": " \t",
            "retry_after_seconds": null
        }))
        .is_err()
    );
}

#[test]
fn controller_actions_exhaustively_round_trip() {
    let actions = [
        ControllerAction::Power(ResetType::ForceOff),
        ControllerAction::BmcReset,
        ControllerAction::SetLockdown {
            scope: LockdownScope::All,
            state: LockdownDesiredState::Disabled,
        },
        ControllerAction::ClearNvram,
        ControllerAction::RefreshExploration,
        ControllerAction::Wait {
            seconds: NonZeroU64::new(30).expect("wait is nonzero"),
        },
        ControllerAction::ManualIntervention {
            code: "replace-system-board"
                .parse()
                .expect("intervention code is valid"),
        },
    ];

    assert!(NonZeroU64::new(0).is_none());
    for action in actions {
        let encoded = serde_json::to_value(&action).expect("action serializes");
        assert_eq!(
            serde_json::from_value::<ControllerAction>(encoded).expect("action deserializes"),
            action
        );
    }
    assert!(
        serde_json::from_value::<ControllerAction>(
            json!({"type": "wait", "details": {"seconds": 0}})
        )
        .is_err()
    );
    assert!(
        serde_json::from_value::<ControllerAction>(json!({
            "type": "manual_intervention",
            "details": {"code": " "}
        }))
        .is_err()
    );
}

#[test]
fn driver_outcomes_round_trip_and_blocked_is_non_empty() {
    let prerequisite = ControllerAction::SetLockdown {
        scope: LockdownScope::Host,
        state: LockdownDesiredState::Disabled,
    };
    let outcomes = [
        DriverOutcome::Complete {
            follow_up: vec![ControllerAction::BmcReset],
        },
        DriverOutcome::Accepted(task_reference()),
        DriverOutcome::blocked(prerequisite.clone()),
        DriverOutcome::Blocked {
            prerequisite,
            additional_prerequisites: vec![ControllerAction::ClearNvram],
        },
    ];

    for outcome in outcomes {
        let encoded = serde_json::to_value(&outcome).expect("outcome serializes");
        assert_eq!(
            serde_json::from_value::<DriverOutcome>(encoded).expect("outcome deserializes"),
            outcome
        );
    }
    assert_eq!(
        serde_json::to_value(DriverOutcome::complete()).expect("outcome serializes"),
        json!({"outcome": "complete", "details": {"follow_up": []}})
    );
    assert!(
        serde_json::from_value::<DriverOutcome>(json!({
            "outcome": "blocked",
            "details": {"additional_prerequisites": []}
        }))
        .is_err()
    );
}

#[test]
fn operation_identifiers_reject_empty_values() {
    for invalid in ["", " \t"] {
        assert!(invalid.parse::<VendorJobId>().is_err());
        assert!(invalid.parse::<ManualInterventionCode>().is_err());
    }
    assert!("JID_42".parse::<VendorJobId>().is_ok());
    assert!(
        "replace-system-board"
            .parse::<ManualInterventionCode>()
            .is_ok()
    );
}

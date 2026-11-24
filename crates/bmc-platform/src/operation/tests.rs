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

use nv_redfish::core::{AsyncTask, ODataId};
use serde_json::json;

use super::*;

fn task_reference() -> OperationReference {
    OperationReference::RedfishTask {
        uri: ODataId::from("/redfish/v1/TaskService/Tasks/42".to_string()),
        retry_after_seconds: Some(5),
        task: None,
    }
}

/// A task monitor whose `202` body named the Task behind it.
fn monitor_reference() -> OperationReference {
    OperationReference::RedfishTask {
        uri: ODataId::from("/redfish/v1/TaskService/TaskMonitors/42".to_string()),
        retry_after_seconds: None,
        task: Some(ODataId::from(
            "/redfish/v1/TaskService/Tasks/42".to_string(),
        )),
    }
}

fn job_reference() -> OperationReference {
    OperationReference::VendorJob {
        uri: ODataId::from(
            "/redfish/v1/Managers/iDRAC.Embedded.1/Oem/Dell/Jobs/JID_42".to_string(),
        ),
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
            monitor_reference(),
            json!({
                "type": "redfish_task",
                "uri": "/redfish/v1/TaskService/TaskMonitors/42",
                "retry_after_seconds": null,
                "task": "/redfish/v1/TaskService/Tasks/42"
            }),
        ),
        (
            job_reference(),
            json!({
                "type": "vendor_job",
                "uri": "/redfish/v1/Managers/iDRAC.Embedded.1/Oem/Dell/Jobs/JID_42",
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
            "uri": "/redfish/v1/Managers/1/Oem/Dell/Jobs/x",
            "job_id": " \t",
            "retry_after_seconds": null
        }))
        .is_err()
    );
}

#[test]
fn driver_outcomes_are_stable() {
    let cases = [
        (DriverOutcome::complete(), json!({"outcome": "complete"})),
        (
            DriverOutcome::accepted(task_reference()),
            json!({
                "outcome": "accepted",
                "details": {"reference": {
                    "type": "redfish_task",
                    "uri": "/redfish/v1/TaskService/Tasks/42",
                    "retry_after_seconds": 5
                }}
            }),
        ),
    ];

    for (outcome, expected) in cases {
        assert_eq!(
            serde_json::to_value(&outcome).expect("outcome serializes"),
            expected
        );
        assert_eq!(
            serde_json::from_value::<DriverOutcome>(expected).expect("outcome deserializes"),
            outcome
        );
    }
}

/// An accepted request keeps the Task its `202` body named, which a status
/// read falls back to once the monitor is gone.
#[test]
fn an_accepted_request_keeps_the_task_its_202_named() {
    let accepted = ModificationResponse::<()>::Task(AsyncTask {
        location: ODataId::from("/redfish/v1/TaskService/TaskMonitors/42".to_string()).into(),
        retry_after: None,
        task: Some(ODataId::from(
            "/redfish/v1/TaskService/Tasks/42".to_string(),
        )),
    });
    assert_eq!(
        DriverOutcome::from(accepted),
        DriverOutcome::accepted(monitor_reference())
    );
}

#[test]
fn merging_keeps_every_accepted_reference_in_issue_order() {
    let merged = DriverOutcome::complete()
        .merge(DriverOutcome::accepted(task_reference()))
        .merge(DriverOutcome::complete())
        .merge(DriverOutcome::accepted(job_reference()));

    assert_eq!(
        merged.references().cloned().collect::<Vec<_>>(),
        vec![task_reference(), job_reference()]
    );
    let encoded = serde_json::to_value(&merged).expect("merged outcome serializes");
    assert_eq!(
        serde_json::from_value::<DriverOutcome>(encoded).expect("merged outcome deserializes"),
        merged
    );
    assert_eq!(
        DriverOutcome::complete().merge(DriverOutcome::complete()),
        DriverOutcome::complete()
    );
}

#[test]
fn vendor_job_ids_reject_empty_values() {
    for invalid in ["", " \t"] {
        assert!(invalid.parse::<VendorJobId>().is_err());
    }
    assert!("JID_42".parse::<VendorJobId>().is_ok());
}

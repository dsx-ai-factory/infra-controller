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

use std::borrow::Cow;

use serde_json::json;

use crate::json::{JsonExt, JsonPatch};
use crate::redfish;
use crate::redfish::Builder;

pub(super) fn collection(chassis_id: &str) -> redfish::Collection<'static> {
    let odata_id = format!(
        "{}/LeakDetectors",
        redfish::thermal_subsystem::leak_detection_resource(chassis_id).odata_id
    );
    redfish::Collection {
        odata_id: Cow::Owned(odata_id),
        odata_type: Cow::Borrowed("#LeakDetectorCollection.LeakDetectorCollection"),
        name: Cow::Borrowed("Leak Detector Collection"),
    }
}

pub(super) fn resource<'a>(chassis_id: &str, leak_detector_id: &'a str) -> redfish::Resource<'a> {
    let odata_id = format!("{}/{leak_detector_id}", collection(chassis_id).odata_id);
    redfish::Resource {
        odata_id: Cow::Owned(odata_id),
        odata_type: Cow::Borrowed("#LeakDetector.v1_0_0.LeakDetector"),
        id: Cow::Borrowed(leak_detector_id),
        name: Cow::Borrowed("Leak Detector"),
    }
}

#[derive(Debug, Clone)]
pub(crate) struct LeakDetector {
    pub(crate) id: Cow<'static, str>,
    pub(crate) user_label: Option<Cow<'static, str>>,
    pub(crate) detector_state: redfish::resource::Status,
}

impl LeakDetector {
    pub(crate) fn to_json(&self, chassis_id: &str) -> serde_json::Value {
        let mut builder = builder(&resource(chassis_id, &self.id))
            .detector_state(self.detector_state)
            .leak_detector_type("Moisture");
        if let Some(user_label) = &self.user_label {
            builder = builder.user_label(user_label);
        }
        builder.build()
    }
}

fn builder(resource: &redfish::Resource) -> LeakDetectorBuilder {
    LeakDetectorBuilder {
        value: resource.json_patch().patch(json!({
            "Status": redfish::resource::Status::Ok.into_json(),
            "DetectorState": "OK",
            "LeakDetectorType": "Moisture",
        })),
    }
}

struct LeakDetectorBuilder {
    value: serde_json::Value,
}

impl Builder for LeakDetectorBuilder {
    fn apply_patch(self, patch: serde_json::Value) -> Self {
        Self {
            value: self.value.patch(patch),
        }
    }
}

impl LeakDetectorBuilder {
    fn detector_state(self, detector_state: redfish::resource::Status) -> Self {
        self.apply_patch(json!({
            "DetectorState": detector_state.as_str(),
            "Status": detector_state.into_json(),
        }))
    }

    fn leak_detector_type(self, value: &str) -> Self {
        self.add_str_field("LeakDetectorType", value)
    }

    fn user_label(self, value: &str) -> Self {
        self.add_str_field("UserLabel", value)
    }

    fn build(self) -> serde_json::Value {
        self.value
    }
}

pub(crate) fn generate_chassis_leak_detectors(count: usize) -> Vec<LeakDetector> {
    (1..=count)
        .map(|index| LeakDetector {
            id: Cow::Owned(format!("LeakDetector_{index}")),
            user_label: Some(Cow::Owned(format!("Leak Detector {index}"))),
            detector_state: redfish::resource::Status::Ok,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use carbide_test_support::{Check, check_values};

    use super::*;

    #[derive(Debug, PartialEq)]
    struct SerializedState {
        detector_state: String,
        health: String,
        state: String,
    }

    #[test]
    fn detector_state_sets_detector_and_health_status() {
        check_values(
            [
                Check {
                    scenario: "OK detector keeps healthy Redfish status",
                    input: redfish::resource::Status::Ok,
                    expect: SerializedState {
                        detector_state: "OK".to_string(),
                        health: "OK".to_string(),
                        state: "Enabled".to_string(),
                    },
                },
                Check {
                    scenario: "warning detector reports warning Redfish status",
                    input: redfish::resource::Status::Warning,
                    expect: SerializedState {
                        detector_state: "Warning".to_string(),
                        health: "Warning".to_string(),
                        state: "Enabled".to_string(),
                    },
                },
                Check {
                    scenario: "critical detector reports critical Redfish status",
                    input: redfish::resource::Status::Critical,
                    expect: SerializedState {
                        detector_state: "Critical".to_string(),
                        health: "Critical".to_string(),
                        state: "Enabled".to_string(),
                    },
                },
            ],
            |detector_state| {
                let value = LeakDetector {
                    id: Cow::Borrowed("LeakDetector_1"),
                    user_label: None,
                    detector_state,
                }
                .to_json("Chassis_1");

                SerializedState {
                    detector_state: value["DetectorState"].as_str().unwrap().to_string(),
                    health: value["Status"]["Health"].as_str().unwrap().to_string(),
                    state: value["Status"]["State"].as_str().unwrap().to_string(),
                }
            },
        );
    }
}

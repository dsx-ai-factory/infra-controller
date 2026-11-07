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

use super::schema::leak_detector as schema;
use crate::redfish;
use crate::redfish::ModelExt;

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
        odata_type: Cow::Borrowed(redfish::schema::leak_detector::LeakDetector::ODATA_TYPE),
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
        let resource = resource(chassis_id, &self.id);
        let detector = schema::LeakDetector::new(
            resource.odata_id.as_ref(),
            resource.id.as_ref(),
            resource.name.as_ref(),
        )
        .detector_state(self.detector_state.into())
        .status(self.detector_state.into())
        .leak_detector_type(schema::LeakDetectorType::Moisture);
        let detector = if let Some(label) = &self.user_label {
            detector.user_label(label.as_ref())
        } else {
            detector
        };
        detector.into_json()
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

    use super::*;

    #[derive(Debug, PartialEq)]
    struct SerializedState {
        detector_state: String,
        health: String,
        state: String,
    }

    #[test]
    fn detector_state_sets_detector_and_health_status() {
        for (scenario, detector_state, expected) in [
            (
                "OK detector keeps healthy Redfish status",
                redfish::resource::Status::Ok,
                SerializedState {
                    detector_state: "OK".to_string(),
                    health: "OK".to_string(),
                    state: "Enabled".to_string(),
                },
            ),
            (
                "warning detector reports warning Redfish status",
                redfish::resource::Status::Warning,
                SerializedState {
                    detector_state: "Warning".to_string(),
                    health: "Warning".to_string(),
                    state: "Enabled".to_string(),
                },
            ),
            (
                "critical detector reports critical Redfish status",
                redfish::resource::Status::Critical,
                SerializedState {
                    detector_state: "Critical".to_string(),
                    health: "Critical".to_string(),
                    state: "Enabled".to_string(),
                },
            ),
        ] {
            let actual = {
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
            };
            assert_eq!(actual, expected, "{scenario}");
        }
    }
}

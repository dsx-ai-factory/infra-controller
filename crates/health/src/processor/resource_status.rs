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

use super::{CollectorEvent, EventContext, EventProcessor};
use crate::sink::{
    RedfishResourceType, ResourceStatusInfo, ResourceStatusObservation, ResourceStatusReport,
};

fn report_from_resources<'a>(
    resource_type: RedfishResourceType,
    resources: impl Iterator<
        Item = (
            &'a str,
            &'a str,
            Option<&'a nv_redfish::schema::resource::Status>,
        ),
    >,
) -> ResourceStatusReport {
    ResourceStatusReport {
        resource_type,
        resources: resources
            .map(|(id, name, status)| ResourceStatusInfo::from_redfish(id, name, status))
            .collect(),
    }
}

/// Projects source-owned Redfish observations into resource-status log events.
#[derive(Default)]
pub struct ResourceStatusProcessor;

impl ResourceStatusProcessor {
    /// Creates a resource-status processor.
    pub fn new() -> Self {
        Self
    }
}

impl EventProcessor for ResourceStatusProcessor {
    fn processor_type(&self) -> &'static str {
        "resource_status_processor"
    }

    fn process_event(
        &self,
        _context: &EventContext,
        event: &CollectorEvent,
    ) -> Vec<CollectorEvent> {
        let CollectorEvent::ResourceStatusObservation(observation) = event else {
            return Vec::new();
        };

        let report = match observation.as_ref() {
            ResourceStatusObservation::System(resources) => report_from_resources(
                RedfishResourceType::System,
                resources.iter().map(|resource| {
                    (
                        resource.id.as_str(),
                        resource.name.as_str(),
                        resource.status.as_ref(),
                    )
                }),
            ),
            ResourceStatusObservation::Manager(resources) => report_from_resources(
                RedfishResourceType::Manager,
                resources.iter().map(|resource| {
                    (
                        resource.id.as_str(),
                        resource.name.as_str(),
                        resource.status.as_ref(),
                    )
                }),
            ),
            ResourceStatusObservation::Chassis(resources) => report_from_resources(
                RedfishResourceType::Chassis,
                resources.iter().map(|resource| {
                    (
                        resource.id.as_str(),
                        resource.name.as_str(),
                        resource.status.as_ref(),
                    )
                }),
            ),
            ResourceStatusObservation::Firmware(resources) => report_from_resources(
                RedfishResourceType::Firmware,
                resources.iter().map(|resource| {
                    (
                        resource.id.as_str(),
                        resource.name.as_str(),
                        resource.status.as_ref(),
                    )
                }),
            ),
        };

        if report.resources.is_empty() {
            Vec::new()
        } else {
            vec![CollectorEvent::ResourceStatus(Box::new(report))]
        }
    }
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};
    use std::str::FromStr;
    use std::sync::Arc;

    use mac_address::MacAddress;

    use super::*;
    use crate::endpoint::BmcAddr;
    fn context() -> EventContext {
        EventContext {
            endpoint_key: "42:9e:b1:bd:9d:dd".to_string(),
            addr: BmcAddr {
                ip: IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
                port: Some(443),
                mac: Some(MacAddress::from_str("42:9e:b1:bd:9d:dd").expect("valid MAC")),
            },
            collector_type: "test",
            metadata: None,
            rack_id: None,
            labels: Default::default(),
        }
    }

    #[test]
    fn projects_only_resource_status_observations() {
        let processor = ResourceStatusProcessor::new();
        let resource = serde_json::from_value(serde_json::json!({
            "@odata.id": "/redfish/v1/UpdateService/FirmwareInventory/BMC",
            "Id": "BMC",
            "Name": "Baseboard Management Controller",
            "Status": {
                "State": "Enabled",
                "Health": "Warning"
            }
        }))
        .expect("firmware resource");
        let second_resource = serde_json::from_value(serde_json::json!({
            "@odata.id": "/redfish/v1/UpdateService/FirmwareInventory/CPLD",
            "Id": "CPLD",
            "Name": "CPLD",
            "Status": {
                "State": "Enabled",
                "Health": "OK"
            }
        }))
        .expect("second firmware resource");
        let observation = ResourceStatusObservation::Firmware(vec![
            Arc::new(resource),
            Arc::new(second_resource),
        ]);

        let projected = processor.process_event(
            &context(),
            &CollectorEvent::ResourceStatusObservation(Box::new(observation)),
        );
        assert!(matches!(
            projected.as_slice(),
            [CollectorEvent::ResourceStatus(projected)] if projected.resources.len() == 2
                && projected.resource_type == RedfishResourceType::Firmware
                && projected.resources[0].id == "BMC"
                && projected.resources[1].id == "CPLD"
                && projected.resources[0].status.as_ref()
                    .and_then(|status| status.health.as_deref())
                    == Some("Warning")
        ));
        assert!(
            processor
                .process_event(&context(), &CollectorEvent::CollectorRemoved)
                .is_empty()
        );
    }
}

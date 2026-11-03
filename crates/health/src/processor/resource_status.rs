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
        let CollectorEvent::ResourceStatusObservation(info) = event else {
            return Vec::new();
        };

        vec![CollectorEvent::ResourceStatus(info.clone())]
    }
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};
    use std::str::FromStr;

    use mac_address::MacAddress;

    use super::*;
    use crate::endpoint::BmcAddr;
    use crate::sink::{RedfishResourceType, ResourceStatusInfo};

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
        let info = ResourceStatusInfo {
            resource_type: RedfishResourceType::Firmware,
            id: "BMC".to_string(),
            name: "Baseboard Management Controller".to_string(),
            status: None,
        };

        let projected = processor.process_event(
            &context(),
            &CollectorEvent::ResourceStatusObservation(Box::new(info.clone())),
        );
        assert!(matches!(
            projected.as_slice(),
            [CollectorEvent::ResourceStatus(projected)] if projected.id == info.id
                && projected.resource_type == info.resource_type
                && projected.status == info.status
        ));
        assert!(
            processor
                .process_event(&context(), &CollectorEvent::CollectorRemoved)
                .is_empty()
        );
    }
}

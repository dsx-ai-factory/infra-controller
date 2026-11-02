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

use std::sync::Arc;

use nv_redfish::ServiceRoot;
use nv_redfish::core::Bmc;
use serde::Serialize;

use crate::HealthError;
use crate::collectors::{IterationResult, PeriodicCollector};
use crate::endpoint::BmcEndpoint;
use crate::sink::{
    CollectorEvent, DataSink, EventContext, RedfishCondition, RedfishResourceType, RedfishStatus,
    ResourceStatusInfo,
};

/// Dependencies used by one periodic Redfish resource-status collector.
pub struct ResourceStatusCollectorConfig {
    /// Destination for resource-status events; collection still runs when omitted.
    pub data_sink: Option<Arc<dyn DataSink>>,
}

/// Periodically reads status from top-level Redfish resources for one BMC.
pub struct ResourceStatusCollector<B: Bmc> {
    bmc: Arc<B>,
    event_context: EventContext,
    data_sink: Option<Arc<dyn DataSink>>,
}

impl<B: Bmc + 'static> PeriodicCollector<B> for ResourceStatusCollector<B> {
    type Config = ResourceStatusCollectorConfig;

    fn new_runner(
        bmc: Arc<B>,
        endpoint: Arc<BmcEndpoint>,
        config: Self::Config,
    ) -> Result<Self, HealthError> {
        Ok(Self {
            bmc,
            event_context: EventContext::from_endpoint(
                endpoint.as_ref(),
                "resource_status_collector",
            ),
            data_sink: config.data_sink,
        })
    }

    async fn run_iteration(&mut self) -> Result<IterationResult, HealthError> {
        self.run_status_iteration().await
    }

    fn collector_type(&self) -> &'static str {
        "resource_status_collector"
    }

    async fn stop(&mut self) {
        self.emit_event(CollectorEvent::CollectorRemoved);
    }
}

impl<B: Bmc + 'static> ResourceStatusCollector<B> {
    fn emit_event(&self, event: CollectorEvent) {
        if let Some(data_sink) = &self.data_sink {
            data_sink.handle_event(&self.event_context, &event);
        }
    }

    fn record_failure<E: std::fmt::Debug>(&self, resource_type: &str, error: E) {
        tracing::warn!(
            ?error,
            resource_type,
            rack_id = self.event_context.rack_id().map(tracing::field::display),
            "Resource status fetch failed"
        );
    }

    async fn run_status_iteration(&self) -> Result<IterationResult, HealthError> {
        let service_root = ServiceRoot::new(self.bmc.clone()).await?;
        let mut entity_count = 0;
        let mut fetch_failures = 0;

        match service_root.systems().await {
            Ok(Some(collection)) => match collection.members().await {
                Ok(systems) => {
                    for system in systems {
                        let raw = system.raw();
                        self.emit_event(resource_status_event(
                            RedfishResourceType::System,
                            &raw.id,
                            &raw.name,
                            raw.status.as_ref(),
                        ));
                        entity_count += 1;
                    }
                }
                Err(error) => {
                    self.record_failure("system", error);
                    fetch_failures += 1;
                }
            },
            Ok(None) => {}
            Err(error) => {
                self.record_failure("system", error);
                fetch_failures += 1;
            }
        }

        match service_root.managers().await {
            Ok(Some(collection)) => match collection.members().await {
                Ok(managers) => {
                    for manager in managers {
                        let raw = manager.raw();
                        self.emit_event(resource_status_event(
                            RedfishResourceType::Manager,
                            &raw.id,
                            &raw.name,
                            raw.status.as_ref(),
                        ));
                        entity_count += 1;
                    }
                }
                Err(error) => {
                    self.record_failure("manager", error);
                    fetch_failures += 1;
                }
            },
            Ok(None) => {}
            Err(error) => {
                self.record_failure("manager", error);
                fetch_failures += 1;
            }
        }

        match service_root.chassis().await {
            Ok(Some(collection)) => match collection.members().await {
                Ok(chassis) => {
                    for chassis in chassis {
                        let raw = chassis.raw();
                        self.emit_event(resource_status_event(
                            RedfishResourceType::Chassis,
                            &raw.id,
                            &raw.name,
                            raw.status.as_ref(),
                        ));
                        entity_count += 1;
                    }
                }
                Err(error) => {
                    self.record_failure("chassis", error);
                    fetch_failures += 1;
                }
            },
            Ok(None) => {}
            Err(error) => {
                self.record_failure("chassis", error);
                fetch_failures += 1;
            }
        }

        match service_root.update_service().await {
            Ok(Some(update_service)) => match update_service.firmware_inventories().await {
                Ok(firmware_items) => {
                    for firmware_item in firmware_items.into_iter().flatten() {
                        let raw = firmware_item.raw();
                        self.emit_event(resource_status_event(
                            RedfishResourceType::Firmware,
                            &raw.id,
                            &raw.name,
                            raw.status.as_ref(),
                        ));
                        entity_count += 1;
                    }
                }
                Err(error) => {
                    self.record_failure("firmware", error);
                    fetch_failures += 1;
                }
            },
            Ok(None) => {}
            Err(error) => {
                self.record_failure("firmware", error);
                fetch_failures += 1;
            }
        }

        Ok(IterationResult {
            refresh_triggered: true,
            entity_count: Some(entity_count),
            fetch_failures,
        })
    }
}

fn resource_status_event(
    resource_type: RedfishResourceType,
    id: &str,
    name: &str,
    status: Option<&nv_redfish::schema::resource::Status>,
) -> CollectorEvent {
    CollectorEvent::ResourceStatus(Box::new(ResourceStatusInfo {
        resource_type,
        id: id.to_string(),
        name: name.to_string(),
        status: status.map(redfish_status),
    }))
}

fn redfish_status(status: &nv_redfish::schema::resource::Status) -> RedfishStatus {
    RedfishStatus {
        state: status.state.flatten().and_then(redfish_enum_string),
        health: status.health.flatten().and_then(redfish_enum_string),
        health_rollup: status.health_rollup.flatten().and_then(redfish_enum_string),
        conditions: status
            .conditions
            .as_ref()
            .and_then(Option::as_ref)
            .map(|conditions| {
                conditions
                    .iter()
                    .map(|condition| RedfishCondition {
                        timestamp: condition.timestamp.map(|value| value.to_string()),
                        message_id: condition.message_id.clone(),
                        severity: condition.severity.and_then(redfish_enum_string),
                        condition_type: condition
                            .condition_type
                            .flatten()
                            .and_then(redfish_enum_string),
                    })
                    .collect()
            }),
    }
}

fn redfish_enum_string(value: impl Serialize) -> Option<String> {
    match serde_json::to_value(value).ok()? {
        serde_json::Value::String(value) if value != "UnsupportedValue" => Some(value),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn maps_resource_identity_and_status_without_inventory_fields() {
        let status = serde_json::from_value(json!({
            "State": "Enabled",
            "Health": "Warning",
            "HealthRollup": "Critical",
            "Conditions": [{
                "Timestamp": "2026-10-30T10:00:00Z",
                "MessageId": "ResourceEvent.1.0.ResourceErrorsDetected",
                "Severity": "Warning",
                "ConditionType": "Alert"
            }]
        }))
        .expect("resource status");

        let event = resource_status_event(
            RedfishResourceType::Firmware,
            "BMC",
            "Baseboard Management Controller",
            Some(&status),
        );
        let CollectorEvent::ResourceStatus(info) = event else {
            panic!("resource status event");
        };

        assert_eq!(info.resource_type, RedfishResourceType::Firmware);
        assert_eq!(info.id, "BMC");
        assert_eq!(info.name, "Baseboard Management Controller");
        assert_eq!(
            info.status,
            Some(RedfishStatus {
                state: Some("Enabled".to_string()),
                health: Some("Warning".to_string()),
                health_rollup: Some("Critical".to_string()),
                conditions: Some(vec![RedfishCondition {
                    timestamp: Some("2026-10-30T10:00:00Z".to_string()),
                    message_id: "ResourceEvent.1.0.ResourceErrorsDetected".to_string(),
                    severity: Some("Warning".to_string()),
                    condition_type: Some("Alert".to_string()),
                }]),
            })
        );
    }

    #[test]
    fn preserves_missing_status_as_unknown_for_every_resource_type() {
        for resource_type in [
            RedfishResourceType::System,
            RedfishResourceType::Manager,
            RedfishResourceType::Chassis,
            RedfishResourceType::Firmware,
        ] {
            let event = resource_status_event(resource_type, "resource-1", "Resource", None);
            let CollectorEvent::ResourceStatus(info) = event else {
                panic!("resource status event");
            };
            assert_eq!(info.resource_type, resource_type);
            assert_eq!(info.status, None);
        }
    }
}

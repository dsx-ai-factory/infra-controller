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
use std::sync::Arc;
use std::time::Instant;

use arc_swap::ArcSwapOption;
use nv_redfish::Resource;
use nv_redfish::chassis::{Chassis, PowerSupply};
use nv_redfish::computer_system::{ComputerSystem, Drive, Memory, Processor, Storage};
use nv_redfish::core::{Bmc, ToSnakeCase};
use nv_redfish::schema::power_subsystem::PowerSubsystem;
use nv_redfish::schema::resource::Status;
use nv_redfish::sensor::SensorLink;

use crate::metrics::MetricLabel;

pub(crate) struct DerivedMetric {
    pub(crate) metric_type: &'static str,
    pub(crate) unit: &'static str,
    pub(crate) value: f64,
    /// Metric-specific labels appended after the entity attributes.
    pub(crate) labels: Vec<MetricLabel>,
}

/// Unit for informational gauges whose value is always `1.0` and whose
/// content lives in the labels.
const STATE_UNIT: &str = "state";

/// Chassis power evidence collected only for power-shelf endpoints.
pub(crate) struct ShelfPower {
    /// The chassis `PowerSubsystem`, when linked and fetched successfully.
    pub(crate) subsystem: Option<Arc<PowerSubsystem>>,
}

/// Appends `<prefix>_state` and `<prefix>_health` labels from a Redfish `Status`.
fn push_status_labels(
    labels: &mut Vec<MetricLabel>,
    state_key: &'static str,
    health_key: &'static str,
    status: &Status,
) {
    if let Some(state) = status.state.flatten() {
        labels.push((Cow::Borrowed(state_key), state.to_snake_case().to_string()));
    }
    if let Some(health) = status.health.flatten() {
        labels.push((
            Cow::Borrowed(health_key),
            health.to_snake_case().to_string(),
        ));
    }
}

/// Builds an informational status gauge, or `None` when the status is absent.
fn status_metric(
    metric_type: &'static str,
    state_key: &'static str,
    health_key: &'static str,
    status: Option<&Status>,
) -> Option<DerivedMetric> {
    let status = status?;
    let mut labels = Vec::with_capacity(2);
    push_status_labels(&mut labels, state_key, health_key, status);
    Some(DerivedMetric {
        metric_type,
        unit: STATE_UNIT,
        value: 1.0,
        labels,
    })
}

pub(crate) enum DiscoveredEntity<B: Bmc> {
    Processor {
        entity: Arc<Processor<B>>,
        system: Arc<ComputerSystem<B>>,
        sensors: Vec<SensorLink<B>>,
    },
    Memory {
        entity: Arc<Memory<B>>,
        system: Arc<ComputerSystem<B>>,
        sensors: Vec<SensorLink<B>>,
    },
    Drive {
        entity: Arc<Drive<B>>,
        storage: Arc<Storage<B>>,
        system: Arc<ComputerSystem<B>>,
        sensors: Vec<SensorLink<B>>,
    },
    PowerSupply {
        entity: Arc<PowerSupply<B>>,
        chassis: Arc<Chassis<B>>,
        sensors: Vec<SensorLink<B>>,
    },
    Chassis {
        entity: Arc<Chassis<B>>,
        sensors: Vec<SensorLink<B>>,
        /// Present only when discovery ran for a power-shelf endpoint.
        shelf_power: Option<ShelfPower>,
    },
}

impl<B: Bmc> DiscoveredEntity<B> {
    pub(crate) fn sensors(&self) -> &[SensorLink<B>] {
        match self {
            DiscoveredEntity::Processor { sensors, .. }
            | DiscoveredEntity::Memory { sensors, .. }
            | DiscoveredEntity::Drive { sensors, .. }
            | DiscoveredEntity::PowerSupply { sensors, .. }
            | DiscoveredEntity::Chassis { sensors, .. } => sensors,
        }
    }

    pub(crate) fn entity_type(&self) -> &'static str {
        match self {
            DiscoveredEntity::Processor { .. } => "processor",
            DiscoveredEntity::Memory { .. } => "memory",
            DiscoveredEntity::Drive { .. } => "drive",
            DiscoveredEntity::PowerSupply { .. } => "powersupply",
            DiscoveredEntity::Chassis { .. } => "chassis",
        }
    }

    pub(crate) fn physical_context_fallback(&self) -> &'static str {
        match self {
            DiscoveredEntity::Processor { .. } => "cpu",
            DiscoveredEntity::Memory { .. } => "memory",
            DiscoveredEntity::Drive { .. } => "storage_device",
            DiscoveredEntity::PowerSupply { .. } => "power_supply",
            DiscoveredEntity::Chassis { .. } => "chassis",
        }
    }

    pub(crate) fn base_attributes(&self) -> Vec<MetricLabel> {
        match self {
            DiscoveredEntity::Processor { entity, system, .. } => vec![
                (Cow::Borrowed("processor_id"), entity.raw().base.id.clone()),
                (Cow::Borrowed("system_id"), system.raw().base.id.clone()),
            ],
            DiscoveredEntity::Memory { entity, system, .. } => vec![
                (Cow::Borrowed("memory_id"), entity.raw().base.id.clone()),
                (Cow::Borrowed("system_id"), system.raw().base.id.clone()),
            ],
            DiscoveredEntity::Drive {
                entity,
                system,
                storage,
                ..
            } => vec![
                (Cow::Borrowed("drive_id"), entity.raw().base.id.clone()),
                (Cow::Borrowed("storage_id"), storage.raw().base.id.clone()),
                (Cow::Borrowed("system_id"), system.raw().base.id.clone()),
            ],
            DiscoveredEntity::PowerSupply {
                entity, chassis, ..
            } => vec![
                (
                    Cow::Borrowed("powersupply_id"),
                    entity.raw().base.id.clone(),
                ),
                (Cow::Borrowed("chassis_id"), chassis.raw().base.id.clone()),
            ],
            DiscoveredEntity::Chassis { entity, .. } => {
                vec![(Cow::Borrowed("chassis_id"), entity.raw().base.id.clone())]
            }
        }
    }

    pub(crate) fn entity_specific_attributes(&self) -> Vec<MetricLabel> {
        let mut attrs = Vec::new();
        match self {
            DiscoveredEntity::Processor { entity, .. } => {
                if let Some(processor_type) = entity.raw().processor_type.flatten() {
                    attrs.push((
                        Cow::Borrowed("processor_type"),
                        processor_type.to_snake_case().to_string(),
                    ));
                }
                if let Some(model) = entity.raw().model.clone().flatten() {
                    attrs.push((Cow::Borrowed("model"), model));
                }
            }
            DiscoveredEntity::Memory { entity, .. } => {
                if let Some(device_type) = entity.raw().memory_device_type.flatten() {
                    attrs.push((
                        Cow::Borrowed("device_type"),
                        device_type.to_snake_case().to_string(),
                    ));
                }
                if let Some(model) = entity.raw().model.clone().flatten() {
                    attrs.push((Cow::Borrowed("model"), model));
                }
            }
            DiscoveredEntity::Drive { entity, .. } => {
                if let Some(model) = entity.raw().model.clone().flatten() {
                    attrs.push((Cow::Borrowed("model"), model));
                }
            }
            DiscoveredEntity::PowerSupply { entity, .. } => {
                if let Some(model) = entity.raw().model.clone().flatten() {
                    attrs.push((Cow::Borrowed("model"), model));
                }
            }
            DiscoveredEntity::Chassis { entity, .. } => {
                if let Some(model) = entity.raw().model.clone().flatten() {
                    attrs.push((Cow::Borrowed("model"), model));
                }
            }
        }
        attrs
    }

    pub(crate) fn key(&self) -> String {
        match self {
            DiscoveredEntity::Processor { entity, .. } => entity.odata_id().to_string(),
            DiscoveredEntity::Memory { entity, .. } => entity.odata_id().to_string(),
            DiscoveredEntity::Drive { entity, .. } => entity.odata_id().to_string(),
            DiscoveredEntity::PowerSupply { entity, .. } => entity.odata_id().to_string(),
            DiscoveredEntity::Chassis { entity, .. } => entity.odata_id().to_string(),
        }
    }

    pub(crate) fn derived_metrics(&self) -> Vec<DerivedMetric> {
        match self {
            DiscoveredEntity::Drive { entity, .. } => entity
                .raw()
                .predicted_media_life_left_percent
                .flatten()
                .map(|value| {
                    vec![DerivedMetric {
                        metric_type: "drive_predicted_media_life_left",
                        unit: "percentage",
                        value,
                        labels: Vec::new(),
                    }]
                })
                .unwrap_or_default(),
            DiscoveredEntity::PowerSupply { entity, .. } => {
                let raw = entity.raw();
                let mut metrics = Vec::with_capacity(2);
                if let Some(value) = raw.power_capacity_watts.flatten() {
                    metrics.push(DerivedMetric {
                        metric_type: "powersupply_capacity",
                        unit: "watts",
                        value,
                        labels: Vec::new(),
                    });
                }
                metrics.extend(status_metric(
                    "powersupply_status",
                    "powersupply_state",
                    "powersupply_health",
                    raw.status.as_ref(),
                ));
                metrics
            }
            DiscoveredEntity::Chassis {
                entity,
                shelf_power: Some(shelf_power),
                ..
            } => {
                let raw = entity.raw();
                let mut metrics = Vec::with_capacity(3);
                if let Some(value) = raw.max_power_watts.flatten() {
                    metrics.push(DerivedMetric {
                        metric_type: "chassis_max_power",
                        unit: "watts",
                        value,
                        labels: Vec::new(),
                    });
                }
                if let Some(mut metric) = status_metric(
                    "chassis_status",
                    "chassis_state",
                    "chassis_health",
                    raw.status.as_ref(),
                ) {
                    if let Some(power_state) = raw.power_state.flatten() {
                        metric.labels.push((
                            Cow::Borrowed("chassis_power_state"),
                            power_state.to_snake_case().to_string(),
                        ));
                    }
                    metrics.push(metric);
                }
                metrics.extend(status_metric(
                    "power_subsystem_status",
                    "power_subsystem_state",
                    "power_subsystem_health",
                    shelf_power
                        .subsystem
                        .as_ref()
                        .and_then(|subsystem| subsystem.status.as_ref()),
                ));
                metrics
            }
            _ => Vec::new(),
        }
    }
}

pub(crate) struct EntityInventory<B: Bmc> {
    pub(crate) entities: Vec<DiscoveredEntity<B>>,
    pub(crate) discovered_at: Instant,
    pub(crate) generation: u64,
}

pub(crate) type SharedInventory<B> = Arc<ArcSwapOption<EntityInventory<B>>>;

#[cfg(test)]
mod tests {
    use carbide_test_support::{Check, check_values};

    use super::*;
    use crate::collectors::projection_test_support::{ProjectionFixture, TestBmc, TestEntity};

    #[derive(Debug, PartialEq)]
    struct ObservedDerivedMetric {
        metric_type: &'static str,
        unit: &'static str,
        value: f64,
        labels: Vec<(String, String)>,
    }

    fn label(key: &str, value: &str) -> (String, String) {
        (key.to_string(), value.to_string())
    }

    #[derive(Debug, PartialEq)]
    struct ObservedEntity {
        sensor_ids: Vec<String>,
        entity_type: &'static str,
        physical_context: &'static str,
        base_attributes: Vec<(String, String)>,
        entity_specific_attributes: Vec<(String, String)>,
        key: String,
        derived_metrics: Vec<ObservedDerivedMetric>,
    }

    fn observe(entity: DiscoveredEntity<TestBmc>) -> ObservedEntity {
        ObservedEntity {
            sensor_ids: entity
                .sensors()
                .iter()
                .map(|sensor| sensor.odata_id().to_string())
                .collect(),
            entity_type: entity.entity_type(),
            physical_context: entity.physical_context_fallback(),
            base_attributes: entity
                .base_attributes()
                .into_iter()
                .map(|(key, value)| (key.into_owned(), value))
                .collect(),
            entity_specific_attributes: entity
                .entity_specific_attributes()
                .into_iter()
                .map(|(key, value)| (key.into_owned(), value))
                .collect(),
            key: entity.key(),
            derived_metrics: entity
                .derived_metrics()
                .into_iter()
                .map(|metric| ObservedDerivedMetric {
                    metric_type: metric.metric_type,
                    unit: metric.unit,
                    value: metric.value,
                    labels: metric
                        .labels
                        .into_iter()
                        .map(|(key, value)| (key.into_owned(), value))
                        .collect(),
                })
                .collect(),
        }
    }

    #[tokio::test]
    async fn inventory_projection_cases() {
        let fixture = ProjectionFixture::new().await;

        check_values(
            [
                Check {
                    scenario: "populated processor",
                    input: fixture.entity(TestEntity::Processor).await,
                    expect: ObservedEntity {
                        sensor_ids: vec![
                            "/redfish/v1/Chassis/CH0/Sensors/CPU0_Voltage".to_string(),
                        ],
                        entity_type: "processor",
                        physical_context: "cpu",
                        base_attributes: vec![
                            ("processor_id".to_string(), "CPU0".to_string()),
                            ("system_id".to_string(), "SYS0".to_string()),
                        ],
                        entity_specific_attributes: vec![
                            ("processor_type".to_string(), "cpu".to_string()),
                            ("model".to_string(), "Grace".to_string()),
                        ],
                        key: "/redfish/v1/Systems/SYS0/Processors/CPU0".to_string(),
                        derived_metrics: vec![],
                    },
                },
                Check {
                    scenario: "sparse processor",
                    input: fixture.entity(TestEntity::SparseProcessor).await,
                    expect: ObservedEntity {
                        sensor_ids: vec![],
                        entity_type: "processor",
                        physical_context: "cpu",
                        base_attributes: vec![
                            ("processor_id".to_string(), "CPU-sparse".to_string()),
                            ("system_id".to_string(), "SYS0".to_string()),
                        ],
                        entity_specific_attributes: vec![],
                        key: "/redfish/v1/Systems/SYS0/Processors/CPU-sparse".to_string(),
                        derived_metrics: vec![],
                    },
                },
                Check {
                    scenario: "populated memory",
                    input: fixture.entity(TestEntity::Memory).await,
                    expect: ObservedEntity {
                        sensor_ids: vec![],
                        entity_type: "memory",
                        physical_context: "memory",
                        base_attributes: vec![
                            ("memory_id".to_string(), "DIMM0".to_string()),
                            ("system_id".to_string(), "SYS0".to_string()),
                        ],
                        entity_specific_attributes: vec![
                            ("device_type".to_string(), "ddr5".to_string()),
                            ("model".to_string(), "HMCG94AGBRA".to_string()),
                        ],
                        key: "/redfish/v1/Systems/SYS0/Memory/DIMM0".to_string(),
                        derived_metrics: vec![],
                    },
                },
                Check {
                    scenario: "sparse memory",
                    input: fixture.entity(TestEntity::SparseMemory).await,
                    expect: ObservedEntity {
                        sensor_ids: vec![],
                        entity_type: "memory",
                        physical_context: "memory",
                        base_attributes: vec![
                            ("memory_id".to_string(), "DIMM-sparse".to_string()),
                            ("system_id".to_string(), "SYS0".to_string()),
                        ],
                        entity_specific_attributes: vec![],
                        key: "/redfish/v1/Systems/SYS0/Memory/DIMM-sparse".to_string(),
                        derived_metrics: vec![],
                    },
                },
                Check {
                    scenario: "populated drive",
                    input: fixture.entity(TestEntity::Drive).await,
                    expect: ObservedEntity {
                        sensor_ids: vec![],
                        entity_type: "drive",
                        physical_context: "storage_device",
                        base_attributes: vec![
                            ("drive_id".to_string(), "D0".to_string()),
                            ("storage_id".to_string(), "ST0".to_string()),
                            ("system_id".to_string(), "SYS0".to_string()),
                        ],
                        entity_specific_attributes: vec![(
                            "model".to_string(),
                            "NVMe-1".to_string(),
                        )],
                        key: "/redfish/v1/Systems/SYS0/Storage/ST0/Drives/D0".to_string(),
                        derived_metrics: vec![ObservedDerivedMetric {
                            metric_type: "drive_predicted_media_life_left",
                            unit: "percentage",
                            value: 80.0,
                            labels: vec![],
                        }],
                    },
                },
                Check {
                    scenario: "sparse drive",
                    input: fixture.entity(TestEntity::SparseDrive).await,
                    expect: ObservedEntity {
                        sensor_ids: vec![],
                        entity_type: "drive",
                        physical_context: "storage_device",
                        base_attributes: vec![
                            ("drive_id".to_string(), "D-sparse".to_string()),
                            ("storage_id".to_string(), "ST0".to_string()),
                            ("system_id".to_string(), "SYS0".to_string()),
                        ],
                        entity_specific_attributes: vec![],
                        key: "/redfish/v1/Systems/SYS0/Storage/ST0/Drives/D-sparse".to_string(),
                        derived_metrics: vec![],
                    },
                },
                Check {
                    scenario: "populated power supply",
                    input: fixture.entity(TestEntity::PowerSupply).await,
                    expect: ObservedEntity {
                        sensor_ids: vec![],
                        entity_type: "powersupply",
                        physical_context: "power_supply",
                        base_attributes: vec![
                            ("powersupply_id".to_string(), "PS0".to_string()),
                            ("chassis_id".to_string(), "CH0".to_string()),
                        ],
                        entity_specific_attributes: vec![(
                            "model".to_string(),
                            "PSU-3KW".to_string(),
                        )],
                        key: "/redfish/v1/Chassis/CH0/PowerSubsystem/PowerSupplies/PS0".to_string(),
                        derived_metrics: vec![
                            ObservedDerivedMetric {
                                metric_type: "powersupply_capacity",
                                unit: "watts",
                                value: 3000.0,
                                labels: vec![],
                            },
                            ObservedDerivedMetric {
                                metric_type: "powersupply_status",
                                unit: "state",
                                value: 1.0,
                                labels: vec![
                                    label("powersupply_state", "enabled"),
                                    label("powersupply_health", "warning"),
                                ],
                            },
                        ],
                    },
                },
                Check {
                    scenario: "sparse power supply",
                    input: fixture.entity(TestEntity::SparsePowerSupply).await,
                    expect: ObservedEntity {
                        sensor_ids: vec![],
                        entity_type: "powersupply",
                        physical_context: "power_supply",
                        base_attributes: vec![
                            ("powersupply_id".to_string(), "PS-sparse".to_string()),
                            ("chassis_id".to_string(), "CH0".to_string()),
                        ],
                        entity_specific_attributes: vec![],
                        key: "/redfish/v1/Chassis/CH0/PowerSubsystem/PowerSupplies/PS-sparse"
                            .to_string(),
                        derived_metrics: vec![],
                    },
                },
                Check {
                    scenario: "populated chassis",
                    input: fixture.entity(TestEntity::Chassis).await,
                    expect: ObservedEntity {
                        sensor_ids: vec![],
                        entity_type: "chassis",
                        physical_context: "chassis",
                        base_attributes: vec![("chassis_id".to_string(), "CH0".to_string())],
                        entity_specific_attributes: vec![("model".to_string(), "HGX".to_string())],
                        key: "/redfish/v1/Chassis/CH0".to_string(),
                        derived_metrics: vec![],
                    },
                },
                Check {
                    scenario: "power-shelf chassis emits power evidence",
                    input: fixture.entity(TestEntity::ShelfChassis).await,
                    expect: ObservedEntity {
                        sensor_ids: vec![],
                        entity_type: "chassis",
                        physical_context: "chassis",
                        base_attributes: vec![("chassis_id".to_string(), "CH0".to_string())],
                        entity_specific_attributes: vec![("model".to_string(), "HGX".to_string())],
                        key: "/redfish/v1/Chassis/CH0".to_string(),
                        derived_metrics: vec![
                            ObservedDerivedMetric {
                                metric_type: "chassis_max_power",
                                unit: "watts",
                                value: 33000.0,
                                labels: vec![],
                            },
                            ObservedDerivedMetric {
                                metric_type: "chassis_status",
                                unit: "state",
                                value: 1.0,
                                labels: vec![
                                    label("chassis_state", "standby_offline"),
                                    label("chassis_health", "ok"),
                                    label("chassis_power_state", "on"),
                                ],
                            },
                            ObservedDerivedMetric {
                                metric_type: "power_subsystem_status",
                                unit: "state",
                                value: 1.0,
                                labels: vec![
                                    label("power_subsystem_state", "enabled"),
                                    label("power_subsystem_health", "ok"),
                                ],
                            },
                        ],
                    },
                },
                Check {
                    scenario: "sparse chassis",
                    input: fixture.entity(TestEntity::SparseChassis).await,
                    expect: ObservedEntity {
                        sensor_ids: vec![],
                        entity_type: "chassis",
                        physical_context: "chassis",
                        base_attributes: vec![("chassis_id".to_string(), "CH-sparse".to_string())],
                        entity_specific_attributes: vec![],
                        key: "/redfish/v1/Chassis/CH-sparse".to_string(),
                        derived_metrics: vec![],
                    },
                },
            ],
            observe,
        );
    }
}

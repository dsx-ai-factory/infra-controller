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
use nv_redfish::sensor::SensorLink;

use crate::metrics::MetricLabel;

pub(crate) struct DerivedMetric {
    pub(crate) metric_type: &'static str,
    pub(crate) unit: &'static str,
    pub(crate) value: f64,
}

/// Identity of the GPU currently occupying a GPU slot.
///
/// A GPU's Redfish path (`HGX_GPU_SXM_1`, `GPU_SXM_1`) names a socket on the
/// baseboard and survives a GPU swap, so it cannot serve as the device
/// identity; the UUID here can. On NVIDIA hardware it matches the NVML GPU
/// UUID without its `GPU-` prefix.
///
/// Read from the resource that reports the GPU, which carries these fields
/// directly: the `Processor` for a GPU processor and the `Chassis` for a GPU
/// module. Both were observed to report identical values per slot, so neither
/// needs a link traversal to the other.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct GpuIdentity {
    pub(crate) uuid: Option<String>,
    pub(crate) serial: Option<String>,
    pub(crate) model: Option<String>,
    /// Serial number of the enclosing GPU chassis, which on SXM baseboards is
    /// the GPU module's own serial rather than the host chassis serial.
    pub(crate) chassis_serial: Option<String>,
}

impl GpuIdentity {
    pub(crate) fn is_empty(&self) -> bool {
        self.uuid.is_none() && self.serial.is_none() && self.model.is_none()
    }

    pub(crate) fn attributes(&self) -> Vec<MetricLabel> {
        let mut attrs = Vec::new();
        if let Some(uuid) = &self.uuid {
            attrs.push((Cow::Borrowed("gpu_uuid"), uuid.clone()));
        }
        if let Some(serial) = &self.serial {
            attrs.push((Cow::Borrowed("gpu_serial"), serial.clone()));
        }
        if let Some(model) = &self.model {
            attrs.push((Cow::Borrowed("gpu_model"), model.clone()));
        }
        if let Some(chassis_serial) = &self.chassis_serial {
            attrs.push((Cow::Borrowed("gpu_chassis_serial"), chassis_serial.clone()));
        }
        attrs
    }
}

/// Strips the trailing slash some BMCs append to an `@odata.id`, so that an
/// event origin and the resource it names compare equal.
pub(crate) fn normalize_odata_id(odata_id: &str) -> &str {
    odata_id.trim_end_matches('/')
}

pub(crate) enum DiscoveredEntity<B: Bmc> {
    Processor {
        entity: Arc<Processor<B>>,
        system: Arc<ComputerSystem<B>>,
        sensors: Vec<SensorLink<B>>,
        /// Populated only when `attributes.gpu_identity` is enabled and the
        /// processor reports `ProcessorType == GPU`.
        gpu: Option<GpuIdentity>,
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
        /// Populated only when `attributes.gpu_identity` is enabled and the
        /// chassis holds a GPU.
        gpu: Option<GpuIdentity>,
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
            DiscoveredEntity::Processor { entity, gpu, .. } => {
                if let Some(processor_type) = entity.raw().processor_type.flatten() {
                    attrs.push((
                        Cow::Borrowed("processor_type"),
                        processor_type.to_snake_case().to_string(),
                    ));
                }
                if let Some(model) = entity.raw().model.clone().flatten() {
                    attrs.push((Cow::Borrowed("model"), model));
                }
                if let Some(gpu) = gpu {
                    attrs.extend(gpu.attributes());
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
            DiscoveredEntity::Chassis { entity, gpu, .. } => {
                if let Some(model) = entity.raw().model.clone().flatten() {
                    attrs.push((Cow::Borrowed("model"), model));
                }
                if let Some(gpu) = gpu {
                    attrs.extend(gpu.attributes());
                }
            }
        }
        attrs
    }

    /// GPU identity for this entity, when it reports a GPU.
    pub(crate) fn gpu_identity(&self) -> Option<&GpuIdentity> {
        match self {
            DiscoveredEntity::Chassis { gpu, .. } | DiscoveredEntity::Processor { gpu, .. } => {
                gpu.as_ref()
            }
            _ => None,
        }
    }

    /// Redfish path by which an SSE `origin_of_condition` names this entity.
    ///
    /// A GPU event's origin is either the GPU chassis
    /// (`/redfish/v1/Chassis/HGX_GPU_SXM_1`) or the GPU processor
    /// (`/redfish/v1/Systems/HGX_Baseboard_0/Processors/GPU_SXM_1`), so both
    /// are matchable. The comparison is on the whole path rather than its last
    /// segment: a platform that reuses one id across the `Processors` and
    /// `Chassis` collections would otherwise let a chassis origin resolve to a
    /// processor, which reports no `gpu_chassis_serial`.
    /// Redfish `Id` of the slot this entity describes.
    ///
    /// A driver event names its GPU in message text by this id rather than by a
    /// path, so resolving those records needs a key the text can be compared
    /// against. Prefer [`Self::gpu_origin_path`] wherever a path is available,
    /// since an id is only unique within its own collection.
    pub(crate) fn gpu_slot_id(&self) -> Option<String> {
        match self {
            DiscoveredEntity::Chassis { entity, .. } => Some(entity.raw().base.id.clone()),
            DiscoveredEntity::Processor { entity, .. } => Some(entity.raw().base.id.clone()),
            _ => None,
        }
    }

    pub(crate) fn gpu_origin_path(&self) -> Option<String> {
        let odata_id = match self {
            DiscoveredEntity::Chassis { entity, .. } => entity.odata_id().to_string(),
            DiscoveredEntity::Processor { entity, .. } => entity.odata_id().to_string(),
            _ => return None,
        };
        Some(normalize_odata_id(&odata_id).to_string())
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
                    }]
                })
                .unwrap_or_default(),
            DiscoveredEntity::PowerSupply { entity, .. } => entity
                .raw()
                .power_capacity_watts
                .flatten()
                .map(|value| {
                    vec![DerivedMetric {
                        metric_type: "powersupply_capacity",
                        unit: "watts",
                        value,
                    }]
                })
                .unwrap_or_default(),
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
                        derived_metrics: vec![ObservedDerivedMetric {
                            metric_type: "powersupply_capacity",
                            unit: "watts",
                            value: 3000.0,
                        }],
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

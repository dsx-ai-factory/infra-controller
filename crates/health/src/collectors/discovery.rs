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

use std::collections::HashSet;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use futures::{StreamExt, stream};
use nv_redfish::core::{Bmc, EntityTypeRef, ToSnakeCase};
use nv_redfish::{Resource, ServiceRoot};

use crate::HealthError;
use crate::collectors::inventory::{
    DiscoveredEntity, EntityInventory, GpuIdentity, SharedInventory,
};
use crate::collectors::runtime::{IterationResult, PeriodicCollector};
use crate::endpoint::BmcEndpoint;

/// Configuration for the entity discovery collector
pub struct EntityDiscoveryCollectorConfig<B: Bmc> {
    pub(crate) shared: SharedInventory<B>,

    /// Bounds local fan-out to the endpoint Redfish operation limit.
    pub request_concurrency: NonZeroUsize,

    /// Label GPU telemetry with the identity of the device that produced it.
    ///
    /// Read from resources discovery already fetches, so this adds no Redfish
    /// requests; it is opt-in only because it adds metric labels.
    pub gpu_identity: bool,
}

pub struct EntityDiscoveryCollector<B: Bmc> {
    endpoint: Arc<BmcEndpoint>,
    bmc: Arc<B>,
    shared: SharedInventory<B>,
    request_concurrency: usize,
    gpu_identity: bool,
    generation: u64,
}

impl<B: Bmc + 'static> PeriodicCollector<B> for EntityDiscoveryCollector<B> {
    type Config = EntityDiscoveryCollectorConfig<B>;

    fn new_runner(
        bmc: Arc<B>,
        endpoint: Arc<BmcEndpoint>,
        config: Self::Config,
    ) -> Result<Self, HealthError> {
        Ok(Self {
            endpoint,
            bmc,
            shared: config.shared,
            request_concurrency: config.request_concurrency.get(),
            gpu_identity: config.gpu_identity,
            generation: 0,
        })
    }

    async fn run_iteration(&mut self) -> Result<IterationResult, HealthError> {
        let fetch_failures = AtomicUsize::new(0);
        let entities = self.discover_entities(&fetch_failures).await?;
        let entity_count = entities.len();

        self.generation = self.generation.wrapping_add(1);
        self.shared.store(Some(Arc::new(EntityInventory {
            entities,
            discovered_at: std::time::Instant::now(),
            generation: self.generation,
        })));

        tracing::info!(
            bmc = %self.endpoint.addr.mac,
            rack_id = self.endpoint.rack_id.as_ref().map(tracing::field::display),
            entity_count,
            generation = self.generation,
            "Published entity inventory snapshot"
        );

        Ok(IterationResult {
            refresh_triggered: true,
            entity_count: Some(entity_count),
            fetch_failures: fetch_failures.load(Ordering::Relaxed),
        })
    }

    fn collector_type(&self) -> &'static str {
        "entity_discovery_collector"
    }

    async fn stop(&mut self) {
        // Clear the snapshot so readers stop emitting for a removed endpoint.
        self.shared.store(None);
    }
}

impl<B: Bmc + 'static> EntityDiscoveryCollector<B> {
    fn record_failure<T, E: std::fmt::Debug>(
        &self,
        result: Result<T, E>,
        context: &str,
        fetch_failures: &AtomicUsize,
    ) -> Option<T> {
        match result {
            Ok(value) => Some(value),
            Err(error) => {
                fetch_failures.fetch_add(1, Ordering::Relaxed);
                tracing::warn!(
                    ?error,
                    context,
                    bmc_address = ?self.endpoint.addr,
                    rack_id = self.endpoint.rack_id.as_ref().map(tracing::field::display),
                    "Discovery fetch failed"
                );
                None
            }
        }
    }

    async fn discover_entities(
        &self,
        fetch_failures: &AtomicUsize,
    ) -> Result<Vec<DiscoveredEntity<B>>, HealthError> {
        let service_root = ServiceRoot::new(self.bmc.clone()).await?;

        let mut entities = Vec::new();
        let mut sensor_ids = HashSet::new();

        if let Some(systems) = service_root.systems().await? {
            for system in systems.members().await? {
                let system = Arc::new(system);

                self.discover_processors(&system, fetch_failures, &mut entities, &mut sensor_ids)
                    .await;
                self.discover_memory(&system, fetch_failures, &mut entities, &mut sensor_ids)
                    .await;
                self.discover_drives(&system, fetch_failures, &mut entities, &mut sensor_ids)
                    .await;
            }
        }

        // Which processors are GPUs is the authoritative evidence that a chassis
        // holds a GPU, and processors are all discovered by now. Collected once
        // rather than per chassis, since every chassis consults the same set.
        let gpu_processors = if self.gpu_identity {
            gpu_processor_ids(&entities)
        } else {
            HashSet::new()
        };

        if let Some(chassis_list) = service_root.chassis().await? {
            for chassis in chassis_list.members().await? {
                let chassis = Arc::new(chassis);

                self.discover_power_supplies(
                    &chassis,
                    fetch_failures,
                    &mut entities,
                    &mut sensor_ids,
                )
                .await;
                self.discover_chassis(
                    &chassis,
                    &gpu_processors,
                    fetch_failures,
                    &mut entities,
                    &mut sensor_ids,
                )
                .await;
            }
        }

        Ok(entities)
    }

    async fn discover_processors(
        &self,
        system: &Arc<nv_redfish::computer_system::ComputerSystem<B>>,
        fetch_failures: &AtomicUsize,
        entities: &mut Vec<DiscoveredEntity<B>>,
        sensor_ids: &mut HashSet<String>,
    ) {
        let processors = self
            .record_failure(system.processors().await, "get processors", fetch_failures)
            .flatten()
            .unwrap_or_default();

        let discovered: Vec<_> = stream::iter(processors)
            .map(|processor| async move {
                let processor = Arc::new(processor);
                let env = processor.environment_sensor_links().await;
                let metric = processor.metrics_sensor_links().await;
                (processor, env, metric)
            })
            .buffer_unordered(self.request_concurrency)
            .collect()
            .await;

        for (entity, env, metric) in discovered {
            let env = self
                .record_failure(env, "get processor environment sensors", fetch_failures)
                .unwrap_or_default();
            let metric = self
                .record_failure(metric, "get processor metric sensors", fetch_failures)
                .unwrap_or_default();
            let sensors: Vec<_> = env.into_iter().chain(metric).collect();
            for sensor in &sensors {
                sensor_ids.insert(sensor.odata_id().to_string());
            }
            let gpu = if self.gpu_identity {
                gpu_identity_from_processor(&entity)
            } else {
                None
            };
            entities.push(DiscoveredEntity::Processor {
                entity,
                system: system.clone(),
                sensors,
                gpu,
            });
        }
    }

    async fn discover_memory(
        &self,
        system: &Arc<nv_redfish::computer_system::ComputerSystem<B>>,
        fetch_failures: &AtomicUsize,
        entities: &mut Vec<DiscoveredEntity<B>>,
        sensor_ids: &mut HashSet<String>,
    ) {
        let memory_modules = self
            .record_failure(
                system.memory_modules().await,
                "get memory modules",
                fetch_failures,
            )
            .flatten()
            .unwrap_or_default();

        let discovered: Vec<_> = stream::iter(memory_modules)
            .map(|memory| async move {
                let memory = Arc::new(memory);
                let sensors = memory.environment_sensor_links().await;
                (memory, sensors)
            })
            .buffer_unordered(self.request_concurrency)
            .collect()
            .await;

        for (entity, sensors) in discovered {
            let sensors = self
                .record_failure(sensors, "get memory environment sensors", fetch_failures)
                .unwrap_or_default();
            for sensor in &sensors {
                sensor_ids.insert(sensor.odata_id().to_string());
            }
            entities.push(DiscoveredEntity::Memory {
                entity,
                system: system.clone(),
                sensors,
            });
        }
    }

    async fn discover_drives(
        &self,
        system: &Arc<nv_redfish::computer_system::ComputerSystem<B>>,
        fetch_failures: &AtomicUsize,
        entities: &mut Vec<DiscoveredEntity<B>>,
        sensor_ids: &mut HashSet<String>,
    ) {
        let storage_list = self
            .record_failure(
                system.storage_controllers().await,
                "get storage",
                fetch_failures,
            )
            .flatten()
            .unwrap_or_default();

        for storage in storage_list {
            let storage = Arc::new(storage);
            let drives = self
                .record_failure(storage.drives().await, "get drives", fetch_failures)
                .flatten()
                .unwrap_or_default();

            let discovered: Vec<_> = stream::iter(drives)
                .map(|drive| async move {
                    let drive = Arc::new(drive);
                    let sensors = drive.environment_sensor_links().await;
                    (drive, sensors)
                })
                .buffer_unordered(self.request_concurrency)
                .collect()
                .await;

            for (entity, sensors) in discovered {
                let sensors = self
                    .record_failure(sensors, "get drive environment sensors", fetch_failures)
                    .unwrap_or_default();
                for sensor in &sensors {
                    sensor_ids.insert(sensor.odata_id().to_string());
                }
                entities.push(DiscoveredEntity::Drive {
                    entity,
                    storage: storage.clone(),
                    system: system.clone(),
                    sensors,
                });
            }
        }
    }

    async fn discover_power_supplies(
        &self,
        chassis: &Arc<nv_redfish::chassis::Chassis<B>>,
        fetch_failures: &AtomicUsize,
        entities: &mut Vec<DiscoveredEntity<B>>,
        sensor_ids: &mut HashSet<String>,
    ) {
        let power_supplies = self
            .record_failure(
                chassis.power_supplies().await,
                "get power supplies",
                fetch_failures,
            )
            .unwrap_or_default();

        let discovered: Vec<_> = stream::iter(power_supplies)
            .map(|ps| async move {
                let ps = Arc::new(ps);
                let sensors = ps.metrics_sensor_links().await;
                (ps, sensors)
            })
            .buffer_unordered(self.request_concurrency)
            .collect()
            .await;

        for (entity, sensors) in discovered {
            let sensors = self
                .record_failure(sensors, "get power supply metric sensors", fetch_failures)
                .unwrap_or_default();
            for sensor in &sensors {
                sensor_ids.insert(sensor.odata_id().to_string());
            }
            entities.push(DiscoveredEntity::PowerSupply {
                entity,
                chassis: chassis.clone(),
                sensors,
            });
        }
    }

    async fn discover_chassis(
        &self,
        chassis: &Arc<nv_redfish::chassis::Chassis<B>>,
        gpu_processors: &HashSet<String>,
        fetch_failures: &AtomicUsize,
        entities: &mut Vec<DiscoveredEntity<B>>,
        sensor_ids: &mut HashSet<String>,
    ) {
        let sensors = match chassis.sensor_links().await {
            Ok(Some(sensors)) => sensors,
            Ok(None) => Vec::new(),
            Err(error) => {
                fetch_failures.fetch_add(1, Ordering::Relaxed);
                tracing::warn!(
                    ?error,
                    bmc_address = ?self.endpoint.addr,
                    rack_id = self.endpoint.rack_id.as_ref().map(tracing::field::display),
                    "Failed to get chassis sensors"
                );
                Vec::new()
            }
        };

        let sensors: Vec<_> = sensors
            .into_iter()
            .filter(|sensor| sensor_ids.insert(sensor.odata_id().to_string()))
            .collect();

        let gpu = if self.gpu_identity {
            gpu_identity_from_chassis(chassis, gpu_processors)
        } else {
            None
        };

        // A sensorless chassis is normally not worth tracking, but one holding a
        // GPU is still needed to attribute SSE log records.
        if sensors.is_empty() && gpu.is_none() {
            return;
        }

        entities.push(DiscoveredEntity::Chassis {
            entity: chassis.clone(),
            sensors,
            gpu,
        });
    }
}

/// Whether a processor is a GPU, per the Redfish `ProcessorType` enumeration.
///
/// Schema-defined rather than inferred from the id, which matters because ids
/// containing `GPU` are not exclusive to GPUs: HGX baseboards expose an
/// `HGX_ERoT_GPU_SXM_1` root-of-trust device per GPU, carrying its own
/// unrelated UUID.
pub(in crate::collectors) fn is_gpu_processor<B: Bmc>(
    processor: &nv_redfish::computer_system::Processor<B>,
) -> bool {
    processor
        .raw()
        .processor_type
        .flatten()
        .is_some_and(|processor_type| processor_type.to_snake_case() == "gpu")
}

/// Read a GPU processor's identity from its own Redfish resource.
///
/// Yields nothing for non-GPU processors, which is the common case: a host CPU
/// must not be labelled with `gpu_*` attributes.
pub(in crate::collectors) fn gpu_identity_from_processor<B: Bmc>(
    processor: &nv_redfish::computer_system::Processor<B>,
) -> Option<GpuIdentity> {
    if !is_gpu_processor(processor) {
        return None;
    }

    let raw = processor.raw();
    let identity = GpuIdentity {
        uuid: raw.uuid.flatten().map(|uuid| uuid.to_string()),
        serial: raw.serial_number.clone().flatten(),
        model: raw.model.clone().flatten(),
        // A processor does not report its enclosure's serial, and resolving the
        // link would cost a fetch. On SXM hardware the GPU chassis reports the
        // same serial as the GPU itself, so nothing is lost that `gpu_serial`
        // does not already carry.
        chassis_serial: None,
    };

    (!identity.is_empty()).then_some(identity)
}

/// Redfish ids of the GPUs among the discovered processors.
///
/// Keyed by `@odata.id` so a chassis can be matched against its
/// `Links/Processors` entries without refetching them.
pub(in crate::collectors) fn gpu_processor_ids<B: Bmc>(
    entities: &[DiscoveredEntity<B>],
) -> HashSet<String> {
    entities
        .iter()
        .filter_map(|entity| match entity {
            DiscoveredEntity::Processor { entity, .. } if is_gpu_processor(entity) => {
                Some(entity.odata_id().to_string())
            }
            _ => None,
        })
        .collect()
}

/// Whether a chassis holds a GPU.
///
/// Selection must be affirmative: a chassis reports a UUID whether or not it
/// holds a GPU, so labelling by elimination would attribute `gpu_*` attributes
/// to NVSwitch modules and root-of-trust components, corrupting the per-device
/// history these attributes exist to preserve.
///
/// A `Links/Processors` entry naming a discovered GPU is the authoritative
/// signal; [`id_names_gpu_module`] covers platforms that omit it.
fn is_gpu_chassis<B: Bmc>(
    chassis: &nv_redfish::chassis::Chassis<B>,
    gpu_processors: &HashSet<String>,
) -> bool {
    let raw = chassis.raw();

    let links_a_gpu_processor = raw
        .links
        .as_ref()
        .and_then(|links| links.processors.as_ref())
        .is_some_and(|processors| {
            processors
                .iter()
                .any(|processor| gpu_processors.contains(&processor.odata_id().to_string()))
        });

    links_a_gpu_processor || id_names_gpu_module(&raw.base.id)
}

/// Whether a chassis id names a GPU module, for platforms that expose GPU
/// modules without a corresponding GPU `Processor` to link to.
///
/// This is the same `HGX_GPU_` convention the SKU GPU-count check already
/// relies on. A substring test would be wrong here: an HGX baseboard exposes an
/// `HGX_ERoT_GPU_SXM_1` root-of-trust component per GPU, which reports its own
/// unrelated UUID and would otherwise be labelled as the GPU itself.
fn id_names_gpu_module(id: &str) -> bool {
    id.starts_with("HGX_GPU_") && !id.contains("NVSwitch")
}

/// Read the identity of the GPU a chassis holds, from the chassis' own resource.
///
/// A GPU module chassis reports the GPU's UUID, serial and model directly, so
/// no traversal to the processor or PCIe device is needed.
pub(in crate::collectors) fn gpu_identity_from_chassis<B: Bmc>(
    chassis: &nv_redfish::chassis::Chassis<B>,
    gpu_processors: &HashSet<String>,
) -> Option<GpuIdentity> {
    if !is_gpu_chassis(chassis, gpu_processors) {
        return None;
    }

    let raw = chassis.raw();
    let serial = raw.serial_number.clone().flatten();
    let identity = GpuIdentity {
        uuid: raw.uuid.flatten().map(|uuid| uuid.to_string()),
        serial: serial.clone(),
        model: raw.model.clone().flatten(),
        // On SXM baseboards the enclosing chassis *is* the GPU module, so its
        // serial is the module serial rather than a host chassis serial.
        chassis_serial: serial,
    };

    (!identity.is_empty()).then_some(identity)
}

#[cfg(test)]
mod gpu_chassis_naming_tests {
    use super::id_names_gpu_module;

    #[test]
    fn gpu_module_chassis_ids_are_recognised() {
        for id in ["HGX_GPU_SXM_1", "HGX_GPU_SXM_8", "HGX_GPU_0"] {
            assert!(id_names_gpu_module(id), "{id} names a GPU module");
        }
    }

    /// The per-GPU root-of-trust component sits beside the GPU and reports its
    /// own UUID, so claiming it as the GPU would attribute one device's identity
    /// to another. Its id contains the GPU's name, which is why the test is
    /// anchored at the start rather than a substring match.
    #[test]
    fn erot_and_nvswitch_chassis_ids_are_rejected() {
        for id in [
            "HGX_ERoT_GPU_SXM_1",
            "HGX_NVSwitch_0",
            "HGX_GPU_NVSwitch_0",
            "HGX_Chassis_0",
            "CPUBaseboard",
            "",
        ] {
            assert!(!id_names_gpu_module(id), "{id} does not name a GPU module");
        }
    }
}

#[cfg(test)]
mod bmc_mock_integration_tests {
    use std::collections::{BTreeMap, HashSet};

    use bmc_mock::test_support::{TestBmc, TestBmcHandle, nvidia_dgx_h100_bmc, wiwynn_gb200_bmc};
    use nv_redfish::Resource as _;

    use super::{gpu_identity_from_chassis, gpu_identity_from_processor, is_gpu_processor};
    use crate::collectors::inventory::GpuIdentity;

    /// Resolve a GPU identity for every processor the mock BMC exposes, keyed by
    /// processor id, and return the `@odata.id`s of the GPUs among them. Mirrors
    /// what the discovery collector does over the systems it enumerates.
    async fn identities_by_processor(
        h: &TestBmcHandle,
    ) -> (BTreeMap<String, Option<GpuIdentity>>, HashSet<String>) {
        let systems = h
            .service_root
            .systems()
            .await
            .expect("systems collection")
            .expect("systems collection is present");

        let mut identities = BTreeMap::new();
        let mut gpu_processors = HashSet::new();
        for system in systems.members().await.expect("system members") {
            for processor in system
                .processors()
                .await
                .expect("processors")
                .unwrap_or_default()
            {
                if is_gpu_processor::<TestBmc>(&processor) {
                    gpu_processors.insert(processor.odata_id().to_string());
                }
                identities.insert(
                    processor.id().to_string(),
                    gpu_identity_from_processor::<TestBmc>(&processor),
                );
            }
        }
        (identities, gpu_processors)
    }

    /// Resolve a GPU identity for every chassis the mock BMC exposes, keyed by
    /// chassis id, against the GPU processors discovered first.
    async fn identities_by_chassis(
        h: &TestBmcHandle,
        gpu_processors: &HashSet<String>,
    ) -> BTreeMap<String, Option<GpuIdentity>> {
        let chassis_list = h
            .service_root
            .chassis()
            .await
            .expect("chassis collection")
            .expect("chassis collection is present");

        let mut identities = BTreeMap::new();
        for chassis in chassis_list.members().await.expect("chassis members") {
            identities.insert(
                chassis.id().to_string(),
                gpu_identity_from_chassis::<TestBmc>(&chassis, gpu_processors),
            );
        }
        identities
    }

    fn assert_distinct_uuids(identities: &[(&String, &GpuIdentity)], expected: usize) {
        let uuids: std::collections::BTreeSet<_> = identities
            .iter()
            .filter_map(|(_, identity)| identity.uuid.clone())
            .collect();
        assert_eq!(
            uuids.len(),
            expected,
            "each GPU must report a distinct UUID; a shared UUID would make the label useless"
        );
    }

    /// The path that carries GPU sensor metrics on real HGX hardware, where GPU
    /// sensors are attributed to `Processor` entities rather than to the chassis.
    #[tokio::test]
    async fn dgx_h100_resolves_identity_for_every_gpu_processor() {
        let (identities, gpu_processors) =
            identities_by_processor(&nvidia_dgx_h100_bmc().await).await;

        let gpus: Vec<_> = identities
            .iter()
            .filter_map(|(id, identity)| identity.as_ref().map(|i| (id, i)))
            .collect();
        assert_eq!(gpus.len(), 8, "DGX H100 exposes 8 GPU processors");
        assert_eq!(gpu_processors.len(), 8);

        for (processor_id, identity) in &gpus {
            assert!(identity.uuid.is_some(), "{processor_id} must carry a UUID");
            assert!(
                identity.serial.is_some(),
                "{processor_id} must carry a serial"
            );
            assert_eq!(
                identity.model.as_deref(),
                Some("H100 80GB HBM3"),
                "{processor_id} model"
            );
        }

        assert_distinct_uuids(&gpus, 8);
    }

    #[tokio::test]
    async fn dgx_h100_resolves_identity_for_every_gpu_chassis() {
        let h = nvidia_dgx_h100_bmc().await;
        let (_, gpu_processors) = identities_by_processor(&h).await;
        let identities = identities_by_chassis(&h, &gpu_processors).await;

        let gpus: Vec<_> = identities
            .iter()
            .filter_map(|(id, identity)| identity.as_ref().map(|i| (id, i)))
            .collect();
        assert_eq!(gpus.len(), 8, "DGX H100 exposes 8 GPU chassis");

        for (chassis_id, identity) in &gpus {
            assert!(identity.uuid.is_some(), "{chassis_id} must carry a UUID");
            assert!(
                identity.serial.is_some(),
                "{chassis_id} must carry a serial"
            );
            assert_eq!(
                identity.model.as_deref(),
                Some("H100 80GB HBM3"),
                "{chassis_id} model"
            );
            assert!(
                identity.chassis_serial.is_some(),
                "{chassis_id} must carry the GPU module chassis serial"
            );
        }

        assert_distinct_uuids(&gpus, 8);
    }

    /// The chassis and the processor must agree, since a sensor attributed to one
    /// and a log record attributed to the other describe the same physical GPU.
    #[tokio::test]
    async fn dgx_h100_chassis_and_processor_agree_on_identity() {
        let h = nvidia_dgx_h100_bmc().await;
        let (by_processor, gpu_processors) = identities_by_processor(&h).await;
        let by_chassis = identities_by_chassis(&h, &gpu_processors).await;

        for slot in 1..=8 {
            let processor = by_processor
                .get(&format!("GPU_SXM_{slot}"))
                .and_then(Option::as_ref)
                .expect("GPU processor identity");
            let chassis = by_chassis
                .get(&format!("HGX_GPU_SXM_{slot}"))
                .and_then(Option::as_ref)
                .expect("GPU chassis identity");

            assert_eq!(processor.uuid, chassis.uuid, "slot {slot} UUID");
            assert_eq!(processor.serial, chassis.serial, "slot {slot} serial");
            assert_eq!(processor.model, chassis.model, "slot {slot} model");
        }
    }

    #[tokio::test]
    async fn non_gpu_resources_resolve_no_identity() {
        let h = nvidia_dgx_h100_bmc().await;
        let (by_processor, gpu_processors) = identities_by_processor(&h).await;
        let by_chassis = identities_by_chassis(&h, &gpu_processors).await;

        for (processor_id, identity) in &by_processor {
            if processor_id.starts_with("GPU_") {
                continue;
            }
            assert!(
                identity.is_none(),
                "{processor_id} is not a GPU but resolved {identity:?}"
            );
        }

        for (chassis_id, identity) in &by_chassis {
            if chassis_id.starts_with("HGX_GPU_SXM_") {
                continue;
            }
            assert!(
                identity.is_none(),
                "{chassis_id} is not a GPU chassis but resolved {identity:?}"
            );
        }
    }

    #[tokio::test]
    async fn gb200_resolves_identity_for_every_gpu() {
        let h = wiwynn_gb200_bmc().await;
        let (by_processor, gpu_processors) = identities_by_processor(&h).await;
        let by_chassis = identities_by_chassis(&h, &gpu_processors).await;

        let processors: Vec<_> = by_processor
            .iter()
            .filter_map(|(id, identity)| identity.as_ref().map(|i| (id, i)))
            .collect();
        let chassis: Vec<_> = by_chassis
            .iter()
            .filter_map(|(id, identity)| identity.as_ref().map(|i| (id, i)))
            .collect();

        assert_eq!(processors.len(), 4, "Wiwynn GB200 exposes 4 GPU processors");
        assert_eq!(chassis.len(), 4, "Wiwynn GB200 exposes 4 GPU chassis");

        assert_distinct_uuids(&processors, 4);
        assert_distinct_uuids(&chassis, 4);
    }
}

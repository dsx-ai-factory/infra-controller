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
use nv_redfish::ServiceRoot;
use nv_redfish::core::Bmc;

use crate::HealthError;
use crate::collectors::inventory::{
    DiscoveredEntity, EntityInventory, SharedInventory, ShelfPower,
};
use crate::collectors::runtime::{IterationResult, PeriodicCollector};
use crate::endpoint::BmcEndpoint;

/// Configuration for the entity discovery collector
pub struct EntityDiscoveryCollectorConfig<B: Bmc> {
    pub(crate) shared: SharedInventory<B>,

    /// Bounds local fan-out to the endpoint Redfish operation limit.
    pub request_concurrency: NonZeroUsize,

    /// Collect chassis and power-subsystem status. Enabled for power-shelf
    /// endpoints only.
    pub collect_shelf_power: bool,
}

pub struct EntityDiscoveryCollector<B: Bmc> {
    endpoint: Arc<BmcEndpoint>,
    bmc: Arc<B>,
    shared: SharedInventory<B>,
    request_concurrency: usize,
    collect_shelf_power: bool,
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
            collect_shelf_power: config.collect_shelf_power,
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
                self.discover_chassis(&chassis, fetch_failures, &mut entities, &mut sensor_ids)
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
            entities.push(DiscoveredEntity::Processor {
                entity,
                system: system.clone(),
                sensors,
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

        let shelf_power = if self.collect_shelf_power {
            Some(self.discover_shelf_power(chassis, fetch_failures).await)
        } else {
            None
        };

        if sensors.is_empty() && shelf_power.is_none() {
            return;
        }

        entities.push(DiscoveredEntity::Chassis {
            entity: chassis.clone(),
            sensors,
            shelf_power,
        });
    }

    async fn discover_shelf_power(
        &self,
        chassis: &nv_redfish::chassis::Chassis<B>,
        fetch_failures: &AtomicUsize,
    ) -> ShelfPower {
        let Some(power_subsystem_ref) = &chassis.raw().power_subsystem else {
            return ShelfPower { subsystem: None };
        };
        let subsystem = self.record_failure(
            power_subsystem_ref.get(self.bmc.as_ref()).await,
            "get power subsystem",
            fetch_failures,
        );
        ShelfPower { subsystem }
    }
}

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

use carbide_uuid::machine::MachineId;
use nv_redfish::core::{Bmc, ToSnakeCase};
use nv_redfish::resource::State;

use crate::HealthError;
use crate::api_client::ApiClientWrapper;
use crate::collectors::inventory::{DiscoveredEntity, SharedInventory};
use crate::collectors::runtime::{IterationResult, PeriodicCollector};
use crate::endpoint::{BmcEndpoint, EndpointMetadata};
use crate::sink::{
    Classification, CollectorEvent, DataSink, EventContext, HealthReport, HealthReportAlert,
    HealthReportSuccess, HealthReportTarget, Probe, ReportSource,
};

/// Resolved expected-GPU state for the endpoint's assigned SKU.
#[derive(Clone, Debug)]
enum Expected {
    /// No SKU assigned yet — nothing to validate.
    NoSku,
    /// The assigned SKU id, but its manifest could not be found.
    SkuMissing(String),
    /// Expected GPU count from the SKU manifest.
    Count(u32),
}

pub struct GpuInventoryCollectorConfig<B: Bmc> {
    pub data_sink: Option<Arc<dyn DataSink>>,
    pub api_client: Arc<ApiClientWrapper>,
    /// Shared entity inventory published by the entity-discovery collector for
    /// this endpoint. GPU counts are read from here rather than re-queried, so
    /// the two collectors share a single Redfish enumeration.
    pub(crate) shared: SharedInventory<B>,
}

pub struct GpuInventoryCollector<B: Bmc> {
    endpoint: Arc<BmcEndpoint>,
    event_context: EventContext,
    /// Entity inventory published by the entity-discovery collector.
    shared: SharedInventory<B>,
    data_sink: Option<Arc<dyn DataSink>>,
    api_client: Arc<ApiClientWrapper>,
    /// Machine id for this endpoint. The assigned SKU is re-read live each iteration
    /// (not cached) so SKU assignments/changes after start are honored.
    machine_id: Option<MachineId>,
}

impl<B: Bmc + 'static> GpuInventoryCollector<B> {
    /// Resolve the expected GPU count from the machine's currently-assigned SKU.
    /// Re-reads the SKU live every call (no caching) so assignments/changes after
    /// the collector starts are picked up.
    async fn resolve_expected(&self) -> Result<Expected, HealthError> {
        let Some(machine_id) = self.machine_id else {
            return Ok(Expected::NoSku);
        };
        let Some(sku_id) = self.api_client.machine_hw_sku(machine_id).await? else {
            return Ok(Expected::NoSku);
        };
        let skus = self
            .api_client
            .find_skus_by_ids(vec![sku_id.clone()])
            .await?;
        Ok(match skus.into_iter().next() {
            None => Expected::SkuMissing(sku_id),
            Some(sku) => Expected::Count(
                sku.components
                    .map(|c| c.gpus.iter().map(|g| g.count).sum())
                    .unwrap_or(0),
            ),
        })
    }

    /// Count the machine's GPUs from the shared entity inventory that the
    /// entity-discovery collector publishes for this endpoint — reusing the
    /// existing Redfish enumeration instead of re-querying the BMC.
    ///
    /// Returns `None` when no inventory snapshot exists yet (discovery hasn't run
    /// for this endpoint), so the caller skips the iteration rather than treating
    /// an unknown count as zero and false-alerting.
    ///
    /// GPUs surface in the inventory two ways depending on how they attach; we
    /// count both and take the max — no vendor table, works across Dell / Lenovo /
    /// Supermicro / NVIDIA regardless of GPU form:
    /// - **SXM / HGX baseboards** (H100 SXM, GB200, GB300, GH200) → one
    ///   `HGX_GPU_*` chassis per GPU (`is_hgx_gpu_chassis`).
    /// - **PCIe cards** (L40 / L40S, H100 PCIe, A100) → Redfish `Processors` with
    ///   `ProcessorType == GPU` (`is_gpu_processor`).
    ///
    /// Some platforms (e.g. GB200) expose the *same* GPUs as both chassis and
    /// processors, so we take the max rather than the sum to avoid double-counting.
    /// `max` is deliberate: `min` would false-alert on platforms that populate only
    /// one view, and `sum` would double-count dual-view platforms.
    ///
    /// Resources reported with `Status.State == Absent` (a defined but empty slot)
    /// are excluded so a placeholder entry can't mask a genuinely missing GPU — see
    /// [`is_present`] for the treatment of the other, degraded-but-present states.
    ///
    /// Limitation: this trusts the discovered inventory — if a failed GPU still
    /// appears (stale inventory, still `Enabled`), the count won't drop. GPUs
    /// exposed ONLY under `PCIeDevices` (neither an HGX chassis nor a GPU
    /// `Processor`) are not counted; add a PCIe path once a platform that needs it
    /// is confirmed via Redfish.
    fn count_gpus(&self) -> Option<u32> {
        let inventory = self.shared.load_full()?;
        Some(Self::count_gpus_in(&inventory.entities))
    }

    /// GPU count for a set of discovered entities: `max(HGX_GPU_* chassis,
    /// ProcessorType==GPU processors)`.
    fn count_gpus_in(entities: &[DiscoveredEntity<B>]) -> u32 {
        let chassis_gpus = entities
            .iter()
            .filter(|e| Self::is_hgx_gpu_chassis(e))
            .count() as u32;
        let processor_gpus = entities
            .iter()
            .filter(|e| Self::is_gpu_processor(e))
            .count() as u32;
        chassis_gpus.max(processor_gpus)
    }

    /// HGX path: each GPU module is one `HGX_GPU_*` chassis (`HGX_GPU_SXM_*` on
    /// Viking/H100, `HGX_GPU_*` on GH200/GB200/GB300). NVSwitch trays excluded.
    fn is_hgx_gpu_chassis(entity: &DiscoveredEntity<B>) -> bool {
        let DiscoveredEntity::Chassis { entity, .. } = entity else {
            return false;
        };
        let raw = entity.raw();
        let id = &raw.base.id;
        let state = raw
            .status
            .as_ref()
            .and_then(|s| s.state.as_ref())
            .and_then(|s| s.as_ref());
        id.starts_with("HGX_GPU_") && !id.contains("NVSwitch") && is_present(state)
    }

    /// Standard-server path: GPUs exposed as Redfish Processors
    /// (`ProcessorType == GPU`). Vendor-neutral across Dell / Lenovo / HPE /
    /// Supermicro and model-agnostic — counts every GPU (H100 PCIe, L40 / L40S,
    /// A100, …), confirmed on hardware (Lenovo ThinkSystem SR670 V2 with 8x L40
    /// reports 8 `ProcessorType=GPU` processors via XCC).
    fn is_gpu_processor(entity: &DiscoveredEntity<B>) -> bool {
        let DiscoveredEntity::Processor { entity, .. } = entity else {
            return false;
        };
        let raw = entity.raw();
        let state = raw
            .status
            .as_ref()
            .and_then(|s| s.state.as_ref())
            .and_then(|s| s.as_ref());
        raw.processor_type
            .flatten()
            .is_some_and(|pt| pt.to_snake_case() == "gpu")
            && is_present(state)
    }

    fn emit_alert(&self, message: String) {
        tracing::warn!(
            bmc_mac_address = %self.endpoint.addr.mac,
            reason = %message,
            rack_id = self.event_context.rack_id().map(tracing::field::display),
            "GPU inventory alert"
        );
        let report = HealthReport {
            source: ReportSource::GpuInventory,
            target: Some(HealthReportTarget::Machine),
            observed_at: Some(chrono::Utc::now()),
            successes: Vec::new(),
            alerts: vec![HealthReportAlert {
                probe_id: Probe::GpuInventory,
                target: None,
                message,
                classifications: vec![Classification::PreventAllocations],
            }],
        };
        self.emit(report);
    }

    fn emit(&self, report: HealthReport) {
        if let Some(sink) = &self.data_sink {
            sink.handle_event(
                &self.event_context,
                &CollectorEvent::HealthReport(Arc::new(report)),
            );
        }
    }
}

/// Whether a discovered resource is physically present and should be counted.
///
/// Redfish may list a resource with `Status.State == Absent` — a defined but
/// empty slot with no device installed (per the DMTF Redfish resource-status
/// model). Counting such a "GPU" would mask a genuinely missing one and defeat
/// the #301 shortage check, so it is excluded.
///
/// Every other state — including a missing `Status`, and non-operational-but-
/// installed states like `Disabled`, `UnavailableOffline` or `StandbyOffline` —
/// is treated as present. Those represent physical hardware that is degraded, not
/// absent; a GPU *count* shortage means missing hardware, and degraded health is
/// surfaced by other signals rather than by dropping the count (which would
/// otherwise raise a spurious "fewer GPUs than the SKU expects" alert).
fn is_present(state: Option<&State>) -> bool {
    !matches!(state, Some(State::Absent))
}

/// Build the health report for a GPU-count comparison — the core of issue #301:
/// alert when the BMC sees fewer GPUs than the SKU expects, success otherwise.
fn gpu_count_report(expected: u32, actual: u32) -> HealthReport {
    if actual < expected {
        HealthReport {
            source: ReportSource::GpuInventory,
            target: Some(HealthReportTarget::Machine),
            observed_at: Some(chrono::Utc::now()),
            successes: Vec::new(),
            alerts: vec![HealthReportAlert {
                probe_id: Probe::GpuInventory,
                target: None,
                message: format!(
                    "Expected gpu count ({expected}) does not match actual ({actual}) \
                     as seen out-of-band via BMC"
                ),
                classifications: vec![Classification::PreventAllocations],
            }],
        }
    } else {
        HealthReport {
            source: ReportSource::GpuInventory,
            target: Some(HealthReportTarget::Machine),
            observed_at: Some(chrono::Utc::now()),
            successes: vec![HealthReportSuccess {
                probe_id: Probe::GpuInventory,
                target: None,
            }],
            alerts: Vec::new(),
        }
    }
}

impl<B: Bmc + 'static> PeriodicCollector<B> for GpuInventoryCollector<B> {
    type Config = GpuInventoryCollectorConfig<B>;

    fn new_runner(
        _bmc: Arc<B>,
        endpoint: Arc<BmcEndpoint>,
        config: Self::Config,
    ) -> Result<Self, HealthError> {
        let event_context =
            EventContext::from_endpoint(endpoint.as_ref(), "gpu_inventory_collector");
        let machine_id = match &endpoint.metadata {
            Some(EndpointMetadata::Machine(m)) => m.machine_id,
            _ => None,
        };
        Ok(Self {
            endpoint,
            event_context,
            shared: config.shared,
            data_sink: config.data_sink,
            api_client: config.api_client,
            machine_id,
        })
    }

    async fn run_iteration(&mut self) -> Result<IterationResult, HealthError> {
        let expected_count = match self.resolve_expected().await? {
            // No SKU assigned, or the SKU declares zero GPUs (e.g. a CPU-only node):
            // nothing to validate. Emit a success so any prior shortage alert on
            // this machine clears (recovery), rather than lingering forever.
            Expected::NoSku | Expected::Count(0) => {
                self.emit(gpu_count_report(0, 0));
                return Ok(IterationResult {
                    refresh_triggered: false,
                    entity_count: None,
                    fetch_failures: 0,
                });
            }
            Expected::SkuMissing(sku_id) => {
                self.emit_alert(format!("The assigned sku {sku_id} does not exist"));
                return Ok(IterationResult {
                    refresh_triggered: false,
                    // Actual GPU count was never queried (bad SKU reference), so it
                    // is unknown here — report None rather than a misleading 0.
                    entity_count: None,
                    fetch_failures: 0,
                });
            }
            Expected::Count(n) => n,
        };

        // Read the GPU count from the shared entity inventory (populated by the
        // entity-discovery collector). If discovery hasn't produced a snapshot yet,
        // skip this iteration rather than false-alerting on a not-yet-known count.
        let Some(actual) = self.count_gpus() else {
            tracing::debug!(
                bmc_mac_address = %self.endpoint.addr.mac,
                rack_id = self.event_context.rack_id().map(tracing::field::display),
                "Entity inventory not ready yet; skipping GPU inventory iteration"
            );
            return Ok(IterationResult {
                refresh_triggered: false,
                entity_count: None,
                fetch_failures: 0,
            });
        };

        let report = gpu_count_report(expected_count, actual);
        if !report.alerts.is_empty() {
            tracing::warn!(
                bmc_mac_address = %self.endpoint.addr.mac,
                expected_gpu_count = expected_count,
                actual_gpu_count = actual,
                rack_id = self.event_context.rack_id().map(tracing::field::display),
                "GPU count below SKU expectation"
            );
        }
        self.emit(report);

        Ok(IterationResult {
            refresh_triggered: false,
            entity_count: Some(actual as usize),
            fetch_failures: 0,
        })
    }

    fn collector_type(&self) -> &'static str {
        "gpu_inventory_collector"
    }
}

/// Integration tests for GPU counting. They build `DiscoveredEntity` values from
/// realistic Redfish trees served in-process by `bmc-mock` — the same entity
/// shapes the entity-discovery collector publishes — then count GPUs from them,
/// exercising the real inventory-backed path:
/// - `is_hgx_gpu_chassis` (`HGX_GPU_*`) is the same code for H100 SXM, GH200,
///   GB200 and GB300 — GB300 pins it to an exact count.
/// - `is_gpu_processor` (`ProcessorType == GPU`) is validated against GB200, the
///   only in-process fixture that models GPUs as Redfish Processors — the same
///   signal PCIe GPUs (L40/L40S, H100 PCIe) produce on Dell/Lenovo/HPE.
#[cfg(test)]
mod bmc_mock_integration_tests {
    use std::sync::Arc;

    use bmc_mock::test_support::{
        TestBmc, TestBmcHandle, dell_poweredge_r750_bmc, dgx_gb300_bmc, wiwynn_gb200_bmc,
    };

    use super::{GpuInventoryCollector, gpu_count_report};
    use crate::collectors::inventory::DiscoveredEntity;
    use crate::sink::{Classification, Probe};

    /// Build the discovered-entity list (processors + chassis) from a mock BMC,
    /// mirroring how the entity-discovery collector populates the shared inventory.
    async fn entities_from(h: &TestBmcHandle) -> Vec<DiscoveredEntity<TestBmc>> {
        let root = h.service_root.as_ref();
        let mut entities = Vec::new();

        if let Some(systems) = root.systems().await.expect("systems") {
            for system in systems.members().await.expect("system members") {
                let system = Arc::new(system);
                let processors = system
                    .processors()
                    .await
                    .expect("processors")
                    .unwrap_or_default();
                for processor in processors {
                    entities.push(DiscoveredEntity::Processor {
                        entity: Arc::new(processor),
                        system: system.clone(),
                        sensors: Vec::new(),
                        gpu: None,
                    });
                }
            }
        }

        if let Some(chassis_list) = root.chassis().await.expect("chassis") {
            for chassis in chassis_list.members().await.expect("chassis members") {
                entities.push(DiscoveredEntity::Chassis {
                    entity: Arc::new(chassis),
                    sensors: Vec::new(),
                    gpu: None,
                });
            }
        }

        entities
    }

    fn count(entities: &[DiscoveredEntity<TestBmc>]) -> u32 {
        GpuInventoryCollector::<TestBmc>::count_gpus_in(entities)
    }

    #[test]
    fn alerts_when_fewer_gpus_than_sku_expects() {
        // Issue #301 core: BMC sees 6 GPUs, the SKU expects 8 -> shortage alert.
        let report = gpu_count_report(8, 6);
        assert!(report.successes.is_empty());
        assert_eq!(report.alerts.len(), 1);
        let alert = &report.alerts[0];
        assert_eq!(alert.probe_id, Probe::GpuInventory);
        assert_eq!(
            alert.classifications,
            vec![Classification::PreventAllocations]
        );
        assert!(
            alert.message.contains('6') && alert.message.contains('8'),
            "message should name actual and expected: {}",
            alert.message
        );
    }

    #[test]
    fn success_when_gpu_count_matches_or_exceeds_sku() {
        // Exact match -> success, no alert.
        let matched = gpu_count_report(8, 8);
        assert!(matched.alerts.is_empty());
        assert_eq!(matched.successes.len(), 1);
        // More GPUs than the SKU expects is not a shortage -> still success.
        assert!(gpu_count_report(4, 5).alerts.is_empty());
        // Clear/no-op case (no SKU or SKU declares 0 GPUs) -> success, no alert,
        // which clears any prior shortage alert on the machine.
        let cleared = gpu_count_report(0, 0);
        assert!(cleared.alerts.is_empty());
        assert_eq!(cleared.successes.len(), 1);
    }

    #[tokio::test]
    async fn dgx_gb300_counts_four_gpus_from_inventory() {
        // DGX GB300 exposes 4 HGX_GPU_ chassis; counting from the discovered
        // inventory must find exactly 4 — the number the #301 decision compares.
        let h = dgx_gb300_bmc().await;
        let entities = entities_from(&h).await;
        assert_eq!(count(&entities), 4, "DGX GB300: 4 HGX_GPU_ chassis");
        // Feed the count into the #301 decision: SKU expecting 8 -> shortage.
        assert_eq!(gpu_count_report(8, count(&entities)).alerts.len(), 1);
        assert!(gpu_count_report(4, count(&entities)).alerts.is_empty());
    }

    #[tokio::test]
    async fn gb200_counts_gpus_via_both_views_without_double_counting() {
        // GB200 lists the same GPUs both as HGX_GPU_* chassis and as GPU
        // processors. Both views must find them, and count_gpus_in takes the max
        // (not the sum), so they are not double-counted.
        let h = wiwynn_gb200_bmc().await;
        let entities = entities_from(&h).await;

        let chassis = entities
            .iter()
            .filter(|e| GpuInventoryCollector::<TestBmc>::is_hgx_gpu_chassis(e))
            .count();
        let processors = entities
            .iter()
            .filter(|e| GpuInventoryCollector::<TestBmc>::is_gpu_processor(e))
            .count();

        assert!(chassis > 0, "expected HGX_GPU_* chassis, got {chassis}");
        assert!(processors > 0, "expected GPU processors, got {processors}");
        assert_eq!(count(&entities) as usize, chassis.max(processors));
    }

    #[tokio::test]
    async fn dell_r750_gpuless_counts_zero() {
        // The R750 fixture is a GPU-less server: neither view finds a GPU.
        let h = dell_poweredge_r750_bmc().await;
        let entities = entities_from(&h).await;
        assert_eq!(count(&entities), 0);
    }

    #[test]
    fn absent_state_is_not_counted_but_degraded_is() {
        use nv_redfish::resource::State;

        // Absent = defined-but-empty slot -> not present, must be excluded so a
        // missing GPU isn't masked by a placeholder entry.
        assert!(!super::is_present(Some(&State::Absent)));
        // Present, degraded-but-installed, and missing-state -> counted (physical
        // hardware is there; degraded health is a separate signal).
        assert!(super::is_present(Some(&State::Enabled)));
        assert!(super::is_present(Some(&State::UnavailableOffline)));
        assert!(super::is_present(None));
    }
}

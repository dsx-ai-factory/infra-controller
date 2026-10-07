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

//! Machine trait and association info metrics for the dedicated per-object
//! endpoint (`docs/design/per-object-state-metrics.md`). Traits live only on
//! the `_info` series — on state series they would multiply cardinality and
//! churn on correction; associations get one series per relationship.

use carbide_health_metrics::{PerObjectGauge, PerObjectMetricsRegistry};
use model::machine::ManagedHostStateSnapshot;

/// `carbide_object_info` plus association info gauges, recorded by the
/// machine state handler each iteration.
#[derive(Clone, Debug)]
pub struct MachinePerObjectInfo {
    object_info: PerObjectGauge,
    dpu_info: PerObjectGauge,
    instance_info: PerObjectGauge,
}

impl MachinePerObjectInfo {
    pub fn new(
        registry: &PerObjectMetricsRegistry,
        prometheus_registry: &prometheus::Registry,
        hold_period: std::time::Duration,
    ) -> prometheus::Result<Self> {
        Ok(Self {
            object_info: registry.gauge(
                prometheus_registry,
                "carbide_object_info",
                "Stable traits of an object, for joining onto its per-object series \
                 (cf. kube_node_info). Trait labels are best-effort empty when unknown.",
                &[
                    "object_type",
                    "object_id",
                    "rack_id",
                    "sku",
                    "vendor",
                    "model",
                ],
                hold_period,
            )?,
            dpu_info: registry.gauge(
                prometheus_registry,
                "carbide_machine_dpu_info",
                "Host-to-DPU association, one series per pair. DPUs are not state-controller \
                 objects and have no state series of their own (the host's substate reflects \
                 its least-progressed DPU); use dpu_id to join DPU-level telemetry onto the \
                 host, and machine_id to reach the host's state series.",
                &["machine_id", "dpu_id"],
                hold_period,
            )?,
            instance_info: registry.gauge(
                prometheus_registry,
                "carbide_machine_instance_info",
                "Machine-to-instance association; exists only while an instance is \
                 provisioned on the machine.",
                &["machine_id", "instance_id", "tenant_org"],
                hold_period,
            )?,
        })
    }

    pub fn record(&self, state: &ManagedHostStateSnapshot) {
        let machine_id = state.host_snapshot.id.to_string();
        // Vendor/model come from exploration reports, so they may be unknown.
        let dmi_data = state
            .host_snapshot
            .status
            .hardware_info
            .as_ref()
            .and_then(|hardware_info| hardware_info.dmi_data.as_ref());
        // Label values in the gauges' schema order; the names are applied at
        // collection time.
        self.object_info.set(
            "machine",
            &machine_id,
            1.0,
            vec![
                "machine".to_string(),
                machine_id.clone(),
                state
                    .host_snapshot
                    .rack_id
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_default(),
                state
                    .host_snapshot
                    .config
                    .hw_sku
                    .clone()
                    .unwrap_or_default(),
                dmi_data
                    .map(|dmi| dmi.sys_vendor.clone())
                    .unwrap_or_default(),
                dmi_data
                    .map(|dmi| dmi.product_name.clone())
                    .unwrap_or_default(),
            ],
        );
        // Declared associations, not loaded snapshots: a host whose DPU
        // snapshots are missing must keep its association series.
        self.dpu_info.set_all(
            "machine",
            &machine_id,
            state
                .host_snapshot
                .associated_dpu_machine_ids()
                .iter()
                .map(|dpu_id| (1.0, vec![machine_id.clone(), dpu_id.to_string()]))
                .collect(),
        );
        match &state.instance {
            Some(instance) => self.instance_info.set(
                "machine",
                &machine_id,
                1.0,
                vec![
                    machine_id.clone(),
                    instance.id.to_string(),
                    instance.config.tenant.tenant_organization_id.to_string(),
                ],
            ),
            None => self.instance_info.clear("machine", &machine_id),
        }
    }
}

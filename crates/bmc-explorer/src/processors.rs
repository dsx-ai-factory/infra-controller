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

use model::site_explorer::Processor as ModelProcessor;
use nv_redfish::Bmc;
use nv_redfish::computer_system::{ComputerSystem, Processor};
use nv_redfish::oem::nvidia::NvidiaProcessor;
use nv_redfish::oem::nvidia::schema::nvidia_processor::MnnvLinkTopology;
use nv_redfish::schema::processor::Processor as ProcessorSchema;

pub(super) struct ExploredProcessor<B: Bmc> {
    processor: Processor<B>,
}

impl<B: Bmc> ExploredProcessor<B> {
    /// Collects processor resources without fetching linked inventory or converting to the model.
    pub(super) async fn explore(system: &ComputerSystem<B>) -> Vec<Self> {
        let processors = match system.processors().await {
            Ok(Some(processors)) => processors,
            Ok(None) => return Vec::new(),
            Err(error) => {
                tracing::warn!(%error, system_id = %system.raw().id, "Failed to fetch processors");
                return Vec::new();
            }
        };
        let mut processors: Vec<_> = processors
            .into_iter()
            .map(|processor| Self { processor })
            .collect();
        processors.sort_by(|left, right| left.processor.raw().id.cmp(&right.processor.raw().id));
        processors.dedup_by(|left, right| left.processor.raw().id == right.processor.raw().id);
        processors
    }

    /// Converts collected data to the minimal persisted inventory without BMC requests.
    pub(super) fn to_model(&self) -> ModelProcessor {
        let raw = self.processor.raw();
        let oem = match self.processor.oem_nvidia() {
            Ok(oem) => oem,
            Err(error) => {
                tracing::warn!(%error, processor_id = %raw.id, "Failed to parse NVIDIA processor OEM data");
                None
            }
        };
        let topology = match oem.as_ref() {
            Some(NvidiaProcessor::Gpu(gpu)) => {
                gpu.mnnv_link_topology.as_ref().and_then(Option::as_ref)
            }
            _ => None,
        };
        raw.to_model(topology)
    }
}

/// Converts a collected processor schema to normalized inventory without BMC requests.
pub trait ProcessorExt {
    /// OEM data is decoded by the backend before converting the schema.
    fn to_model(&self, topology: Option<&MnnvLinkTopology>) -> ModelProcessor;
}

impl ProcessorExt for ProcessorSchema {
    fn to_model(&self, topology: Option<&MnnvLinkTopology>) -> ModelProcessor {
        let value = |field: Option<Option<i64>>| {
            field
                .flatten()
                .and_then(|value| i32::try_from(value).ok().filter(|value| *value >= 0))
        };
        ModelProcessor {
            id: self.id.clone(),
            model: self.model.clone().flatten(),
            physical_slot_number: topology.and_then(|topology| value(topology.tray_slot_number)),
            compute_tray_index: topology.and_then(|topology| value(topology.tray_slot_index)),
        }
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;

    use super::{MnnvLinkTopology, ProcessorExt, ProcessorSchema};

    #[test]
    fn position_validates_oem_fields_independently() {
        let processor: ProcessorSchema = serde_json::from_value(serde_json::json!({
            "@odata.id": "/processors/GPU_0", "Id": "GPU_0", "Name": "GPU 0"
        }))
        .unwrap();
        value_scenarios!(run = |(slot, index): (Option<i64>, Option<i64>)| {
            let topology: MnnvLinkTopology = serde_json::from_value(serde_json::json!({
                "TraySlotNumber": slot, "TraySlotIndex": index
            })).unwrap();
            let model = processor.to_model(Some(&topology));
            (model.physical_slot_number, model.compute_tray_index)
        };
            "processor topology" {
                (Some(26), Some(16)) => (Some(26), Some(16)),
                (Some(0), Some(0)) => (Some(0), Some(0)),
                (Some(-1), Some(16)) => (None, Some(16)),
                (Some(26), Some(2147483648_i64)) => (Some(26), None),
                (None, None) => (None, None),
            }
        );
    }
}

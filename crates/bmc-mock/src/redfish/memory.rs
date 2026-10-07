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

//! Mock Redfish `Memory` resources and their `MemoryMetrics`.
//!
//! GB-family trays report HBM stacks here, and their metrics carry the
//! NVIDIA OEM row-remapping counters -- the only place those counters
//! are exposed out of band.

use std::borrow::Cow;

use serde_json::json;

use crate::json::{JsonExt, JsonPatch};
use crate::redfish;
use crate::redfish::Builder;

pub(super) fn system_collection(system_id: &str) -> redfish::Collection<'static> {
    let odata_id = format!("/redfish/v1/Systems/{system_id}/Memory");
    redfish::Collection {
        odata_id: Cow::Owned(odata_id),
        odata_type: Cow::Borrowed("#MemoryCollection.MemoryCollection"),
        name: Cow::Borrowed("Memory Collection"),
    }
}

pub(super) fn system_resource<'a>(system_id: &str, memory_id: &'a str) -> redfish::Resource<'a> {
    let odata_id = format!("/redfish/v1/Systems/{system_id}/Memory/{memory_id}");
    redfish::Resource {
        odata_id: Cow::Owned(odata_id),
        odata_type: Cow::Borrowed("#Memory.v1_20_0.Memory"),
        id: Cow::Borrowed(memory_id),
        name: Cow::Borrowed("Memory"),
    }
}

pub(super) fn metrics_resource(system_id: &str, memory_id: &str) -> redfish::Resource<'static> {
    let odata_id = format!("/redfish/v1/Systems/{system_id}/Memory/{memory_id}/MemoryMetrics");
    redfish::Resource {
        odata_id: Cow::Owned(odata_id),
        odata_type: Cow::Borrowed("#MemoryMetrics.v1_7_0.MemoryMetrics"),
        id: Cow::Borrowed("MemoryMetrics"),
        name: Cow::Borrowed("Memory Metrics"),
    }
}

/// A mock Redfish `Memory` plus its associated `MemoryMetrics` resource.
pub(crate) struct Memory {
    pub(crate) id: Cow<'static, str>,
    resource: serde_json::Value,
    metrics: serde_json::Value,
}

impl Memory {
    pub(crate) fn to_json(&self) -> serde_json::Value {
        self.resource.clone()
    }

    pub(crate) fn metrics_json(&self) -> serde_json::Value {
        self.metrics.clone()
    }
}

struct MemoryBuilder {
    id: Cow<'static, str>,
    value: serde_json::Value,
}

impl Builder for MemoryBuilder {
    fn apply_patch(self, patch: serde_json::Value) -> Self {
        Self {
            value: self.value.patch(patch),
            id: self.id,
        }
    }
}

impl MemoryBuilder {
    fn metrics(self, metrics: &redfish::Resource<'_>) -> Self {
        self.apply_patch(metrics.nav_property("Metrics"))
    }

    fn status(self, status: redfish::resource::Status) -> Self {
        self.apply_patch(json!({ "Status": status.into_json() }))
    }

    fn build(self, metrics: serde_json::Value) -> Memory {
        Memory {
            id: self.id,
            resource: self.value,
            metrics,
        }
    }
}

/// An HBM stack as a GB-family tray reports it.
pub(crate) fn hbm(system_id: &str, memory_id: &str, capacity_mib: u64) -> Memory {
    let metrics = metrics_resource(system_id, memory_id);
    let resource = system_resource(system_id, memory_id);
    let metrics_json = nvidia_hbm_metrics(&metrics, memory_id);

    MemoryBuilder {
        id: Cow::Owned(memory_id.to_string()),
        value: resource.json_patch(),
    }
    .apply_patch(json!({
        "MemoryDeviceType": "HBM3",
        "MemoryType": "DRAM",
        "Manufacturer": "NVIDIA",
        "CapacityMiB": capacity_mib,
        "OperatingSpeedMhz": 2619,
    }))
    .status(redfish::resource::Status::Ok)
    .metrics(&metrics)
    .build(metrics_json)
}

fn nvidia_hbm_metrics(resource: &redfish::Resource<'_>, memory_id: &str) -> serde_json::Value {
    resource.json_patch().patch(json!({
        "Name": format!("{memory_id} Memory Metrics"),
        "BlockSizeBytes": 4096,
        "BandwidthPercent": 12.5,
        "OperatingSpeedMHz": 2619,
        "CurrentPeriod": {
            "CorrectableECCErrorCount": 0,
            "UncorrectableECCErrorCount": 0
        },
        "LifeTime": {
            "CorrectableECCErrorCount": 7,
            "UncorrectableECCErrorCount": 0
        },
        "Oem": {
            "Nvidia": {
                "@odata.type": "#NvidiaMemoryMetrics.v1_2_0.NvidiaMemoryMetrics",
                "RowRemapping": {
                    "CorrectableRowRemappingCount": 3,
                    "UncorrectableRowRemappingCount": 0,
                    "MaxAvailabilityBankCount": 40,
                    "HighAvailabilityBankCount": 2,
                    "PartialAvailabilityBankCount": 0,
                    "LowAvailabilityBankCount": 0,
                    // Firmware built against the original schema spells
                    // this one without the second `i`, and consumers are
                    // expected to accept either.
                    "NoAvailablityBankCount": 0
                }
            }
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::hbm;

    #[test]
    fn hbm_links_to_its_metrics() {
        let memory = hbm("HGX_Baseboard_0", "GPU_0_DRAM_0", 186 * 1024);

        let resource = memory.to_json();
        assert_eq!(resource["Id"], "GPU_0_DRAM_0");
        assert_eq!(resource["MemoryDeviceType"], "HBM3");
        assert_eq!(
            resource["Metrics"]["@odata.id"],
            "/redfish/v1/Systems/HGX_Baseboard_0/Memory/GPU_0_DRAM_0/MemoryMetrics"
        );
    }

    #[test]
    fn hbm_metrics_carry_the_nvidia_row_remapping_extension() {
        let memory = hbm("HGX_Baseboard_0", "GPU_0_DRAM_0", 186 * 1024);
        let metrics = memory.metrics_json();

        // Standard properties the metrics collector projects.
        assert_eq!(metrics["BlockSizeBytes"], 4096);
        assert_eq!(metrics["LifeTime"]["CorrectableECCErrorCount"], 7);

        // Row remapping is OEM-only, and deliberately mixes both
        // spellings of the bank counters.
        let remapping = &metrics["Oem"]["Nvidia"]["RowRemapping"];
        assert_eq!(remapping["CorrectableRowRemappingCount"], 3);
        assert_eq!(remapping["MaxAvailabilityBankCount"], 40);
        assert_eq!(remapping["NoAvailablityBankCount"], 0);
    }
}

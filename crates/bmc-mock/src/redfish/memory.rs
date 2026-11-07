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

use crate::redfish;
use crate::redfish::ModelExt;

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
        odata_type: Cow::Borrowed(redfish::schema::memory::Memory::ODATA_TYPE),
        id: Cow::Borrowed(memory_id),
        name: Cow::Borrowed("Memory"),
    }
}

pub(super) fn metrics_resource(system_id: &str, memory_id: &str) -> redfish::Resource<'static> {
    let odata_id = format!("/redfish/v1/Systems/{system_id}/Memory/{memory_id}/MemoryMetrics");
    redfish::Resource {
        odata_id: Cow::Owned(odata_id),
        odata_type: Cow::Borrowed(redfish::schema::memory_metrics::MemoryMetrics::ODATA_TYPE),
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

/// An HBM stack as a GB-family tray reports it.
pub(crate) fn hbm(system_id: &str, memory_id: &str, capacity_mib: u64) -> Memory {
    use super::schema::memory::{Memory as MemoryModel, MemoryDeviceType, MemoryType};
    let metrics = metrics_resource(system_id, memory_id);
    let resource = system_resource(system_id, memory_id);
    Memory {
        id: memory_id.to_string().into(),
        resource: MemoryModel::new(
            resource.odata_id.as_ref(),
            resource.id.as_ref(),
            resource.name.as_ref(),
        )
        .memory_device_type(MemoryDeviceType::Hbm3)
        .memory_type(MemoryType::Dram)
        .manufacturer("NVIDIA")
        .capacity_mi_b(
            capacity_mib
                .try_into()
                .expect("fixture memory capacity fits i64"),
        )
        .operating_speed_mhz(2619)
        .status(redfish::resource::Status::Ok.into())
        .metrics(metrics.odata_id.as_ref())
        .into_json(),
        metrics: nvidia_hbm_metrics(&metrics, memory_id),
    }
}

fn nvidia_hbm_metrics(resource: &redfish::Resource<'_>, memory_id: &str) -> serde_json::Value {
    use super::schema::memory_metrics::{CurrentPeriod, LifeTime, MemoryMetrics};
    MemoryMetrics::new(
        resource.odata_id.as_ref(),
        resource.id.as_ref(),
        format!("{memory_id} Memory Metrics"),
    )
    .block_size_bytes(4096)
    .bandwidth_percent(nv_redfish_server_core::FiniteF64::try_from(12.5).expect("finite bandwidth"))
    .operating_speed_mhz(2619)
    .current_period(
        CurrentPeriod::new()
            .correctable_ecc_error_count(0)
            .uncorrectable_ecc_error_count(0),
    )
    .life_time(
        LifeTime::new()
            .correctable_ecc_error_count(7)
            .uncorrectable_ecc_error_count(0),
    )
    .oem(redfish::resource::oem(
        [(
            "Nvidia".to_string(),
            redfish::oem::object(
                // Retain the type spelling reported by the firmware capture.
                "#NvidiaMemoryMetrics.v1_2_0.NvidiaMemoryMetrics",
                super::schema::nvidia_memory_metrics::NvidiaMemoryMetrics::new().row_remapping(
                    super::schema::nvidia_memory_metrics::RowRemapping::new()
                        .correctable_row_remapping_count(3)
                        .uncorrectable_row_remapping_count(0)
                        .max_availability_bank_count(40)
                        .high_availability_bank_count(2)
                        .partial_availability_bank_count(0)
                        .low_availability_bank_count(0)
                        // Original firmware omits the second `i`; CSDL retains both spellings.
                        .no_availablity_bank_count(0),
                ),
            ),
        )]
        .into(),
    ))
    .into_json()
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

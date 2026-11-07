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

use serde_json::json;

use super::schema::resource::Uuid;
use crate::json::JsonExt;
use crate::redfish;
use crate::redfish::ModelExt;

pub(super) fn system_collection(system_id: &str) -> redfish::Collection<'static> {
    let odata_id = format!("/redfish/v1/Systems/{system_id}/Processors");
    redfish::Collection {
        odata_id: Cow::Owned(odata_id),
        odata_type: Cow::Borrowed("#ProcessorCollection.ProcessorCollection"),
        name: Cow::Borrowed("Processors Collection"),
    }
}

pub(super) fn system_resource<'a>(system_id: &str, processor_id: &'a str) -> redfish::Resource<'a> {
    let odata_id = format!("/redfish/v1/Systems/{system_id}/Processors/{processor_id}");
    redfish::Resource {
        odata_id: Cow::Owned(odata_id),
        odata_type: Cow::Borrowed(redfish::schema::processor::Processor::ODATA_TYPE),
        id: Cow::Borrowed(processor_id),
        name: Cow::Borrowed("Processor"),
    }
}

pub(super) fn metrics_resource(system_id: &str, processor_id: &str) -> redfish::Resource<'static> {
    let odata_id =
        format!("/redfish/v1/Systems/{system_id}/Processors/{processor_id}/ProcessorMetrics");
    redfish::Resource {
        odata_id: Cow::Owned(odata_id),
        odata_type: Cow::Borrowed(redfish::schema::processor_metrics::ProcessorMetrics::ODATA_TYPE),
        id: Cow::Borrowed("ProcessorMetrics"),
        name: Cow::Borrowed("Processor Metrics"),
    }
}

/// A mock Redfish `Processor` plus its associated `ProcessorMetrics` resource.
pub(crate) struct Processor {
    pub(crate) id: Cow<'static, str>,
    resource: serde_json::Value,
    metrics: serde_json::Value,
}

impl Processor {
    pub(crate) fn to_json(&self) -> serde_json::Value {
        self.resource.clone()
    }

    pub(crate) fn metrics_json(&self) -> serde_json::Value {
        self.metrics.clone()
    }

    /// Adds the Vera Rubin compute-tray position reported by an NVIDIA GPU.
    pub(crate) fn with_mnnvlink_topology(
        mut self,
        tray_slot_number: i64,
        tray_slot_index: i64,
    ) -> Self {
        use super::schema::nvidia_processor::{MnnvLinkTopology, NvidiaGpu};
        let topology = NvidiaGpu::new().mnnv_link_topology(
            MnnvLinkTopology::new()
                .tray_slot_number(tray_slot_number)
                .tray_slot_index(tray_slot_index),
        );
        self.resource = self
            .resource
            .patch(json!({"Oem": {"Nvidia": redfish::oem::object(
                "#NvidiaProcessor.v1_4_0.NvidiaGPU", topology
            )}}));
        self
    }
}

/// Identity fields an NVIDIA GPU reports on its `Processor` resource.
///
/// Real hardware reports the same values on the enclosing GPU chassis, so a
/// fixture should keep the two in agreement.
pub(crate) struct GpuIdentity<'a> {
    pub(crate) uuid: Uuid,
    pub(crate) serial_number: &'a str,
    pub(crate) model: &'a str,
}

pub(crate) fn gpu(
    system_id: &str,
    processor_id: &str,
    core_voltage_sensor_uri: &str,
    identity: &GpuIdentity<'_>,
) -> Processor {
    let metrics = metrics_resource(system_id, processor_id);
    let metrics_json = nvidia_gpu_metrics(&metrics, processor_id, core_voltage_sensor_uri);
    let resource = system_resource(system_id, processor_id);
    Processor {
        id: processor_id.to_string().into(),
        resource: redfish::schema::processor::Processor::new(
            resource.odata_id.as_ref(),
            resource.id.as_ref(),
            resource.name.as_ref(),
        )
        .processor_type(redfish::schema::processor::ProcessorType::Gpu)
        .uuid(identity.uuid)
        .serial_number(identity.serial_number)
        .model(identity.model)
        .status(redfish::resource::Status::Ok.into())
        .metrics(metrics.odata_id.as_ref())
        .into_json(),
        metrics: metrics_json,
    }
}

fn nvidia_gpu_metrics(
    resource: &redfish::Resource<'_>,
    processor_id: &str,
    core_voltage_sensor_uri: &str,
) -> serde_json::Value {
    use nv_redfish_server_core::FiniteF64;

    use super::schema::pcie_device::PcieErrors;
    use super::schema::processor_metrics::{CacheMetricsTotal, LifeTime, ProcessorMetrics};
    use super::schema::sensor::SensorExcerptVoltage;
    ProcessorMetrics::new(
        resource.odata_id.as_ref(),
        resource.id.as_ref(),
        format!("{processor_id} Processor Metrics"),
    )
    .bandwidth_percent(FiniteF64::try_from(0.0).expect("finite bandwidth"))
    .operating_speed_mhz(0)
    .power_limit_throttle_duration("PT0S")
    .thermal_limit_throttle_duration("PT0S")
    .cache_metrics_total(
        CacheMetricsTotal::new().life_time(
            LifeTime::new()
                .correctable_ecc_error_count(0)
                .uncorrectable_ecc_error_count(0),
        ),
    )
    .core_voltage(
        SensorExcerptVoltage::new()
            .data_source_uri(core_voltage_sensor_uri)
            .reading(FiniteF64::try_from(0.8).expect("finite core voltage")),
    )
    .pcie_errors(
        PcieErrors::new()
            .correctable_error_count(2)
            .fatal_error_count(0)
            .l0to_recovery_count(2)
            .nak_received_count(0)
            .nak_sent_count(0)
            .non_fatal_error_count(0)
            .replay_count(0)
            .replay_rollover_count(0)
            .unsupported_request_count(0),
    )
    .oem(redfish::resource::oem(
        [("Nvidia".to_string(), nvidia_gpu_oem())].into(),
    ))
    .into_json()
}

/// The NVIDIA OEM extension a GB-family GPU reports on its
/// `ProcessorMetrics`.
///
/// `@odata.type` names the GPU shape, which nv-redfish uses to pick
/// between `NvidiaProcessorMetrics::Gpu` and `::Generic`. Properties
/// from both the shape-specific schema and the shared v1_1_0 base are
/// present, because firmware sends them together and consumers project
/// them together.
fn nvidia_gpu_oem() -> serde_json::Value {
    use nv_redfish_server_core::FiniteF64;

    use super::schema::nvidia_processor_metrics::NvidiaGpuProcessorMetrics;
    let number = |value| FiniteF64::try_from(value).expect("finite simulated GPU metric");
    let model = NvidiaGpuProcessorMetrics::new()
        .sm_activity_percent(number(62.5))
        .sm_occupancy_percent(number(44.0))
        .graphics_engine_activity_percent(number(71.0))
        .tensor_core_activity_percent(number(18.25))
        .fp64activity_percent(number(4.0))
        .fp32activity_percent(number(9.5))
        .fp16activity_percent(number(12.0))
        .nv_dec_utilization_percent(number(0.0))
        .nv_jpg_utilization_percent(number(0.0))
        .nv_ofa_utilization_percent(number(0.0))
        .dmma_utilization_percent(number(3.0))
        .hmma_utilization_percent(number(6.0))
        .imma_utilization_percent(number(1.5))
        .pcie_rx_bytes(45388)
        .pcie_tx_bytes(51108)
        .pcie_raw_tx_bandwidth_gbps(number(12.0))
        .pcie_raw_rx_bandwidth_gbps(number(11.5))
        .nv_link_raw_tx_bandwidth_gbps(number(340.0))
        .nv_link_raw_rx_bandwidth_gbps(number(338.5))
        .nv_link_data_tx_bandwidth_gbps(number(300.0))
        .nv_link_data_rx_bandwidth_gbps(number(298.0))
        .hardware_violation_throttle_duration("PT0S")
        .global_software_violation_throttle_duration("PT0S")
        .accumulated_gpu_context_utilization_duration("PT10M")
        .accumulated_sm_utilization_duration("PT5M30S")
        // Firmware reports this single-element list when no throttling is active.
        .throttle_reasons(["NA"])
        .sm_utilization_percent(number(0.0))
        .integer_activity_utilization_percent(number(2.0))
        .nv_enc_utilization_percent(number(0.0))
        .host_memory_cache_hit_percent(number(88.0))
        .host_memory_cache_miss_percent(number(12.0))
        .peer_memory_cache_hit_percent(number(91.0))
        .peer_memory_cache_miss_percent(number(9.0))
        .dram_memory_cache_hit_percent(number(96.5))
        .dram_memory_cache_miss_percent(number(3.5))
        .c2c_raw_tx_bandwidth_gbps(number(420.0))
        .c2c_raw_rx_bandwidth_gbps(number(418.0))
        .c2c_data_tx_bandwidth_gbps(number(400.0))
        .c2c_data_rx_bandwidth_gbps(number(398.0))
        .sramecc_error_threshold_exceeded(false);
    // Captured firmware advertises v1_4_0 while including later C2C/cache fields.
    let mut document = redfish::oem::object(
        "#NvidiaProcessorMetrics.v1_4_0.NvidiaGPUProcessorMetrics",
        model,
    );
    // The capture capitalizes JPG, whereas CSDL spells this property NVJpg.
    let fields = document.as_object_mut().expect("OEM model is an object");
    let jpg = fields
        .remove("NVJpgUtilizationPercent")
        .expect("generated JPG metric");
    fields.insert("NVJPGUtilizationPercent".to_string(), jpg);
    document
}

#[cfg(test)]
mod tests {
    use super::{GpuIdentity, gpu};

    fn test_identity() -> GpuIdentity<'static> {
        GpuIdentity {
            uuid: "beea8cdf-7b7d-035c-ed07-360e311fcbe1"
                .parse()
                .expect("fixture GPU UUID"),
            serial_number: "1655023015625",
            model: "H100 80GB HBM3",
        }
    }

    #[test]
    fn gpu_processor_links_to_metrics_and_chassis_sensor() {
        let processor = gpu(
            "HGX_Baseboard_0",
            "GPU_0",
            "/redfish/v1/Chassis/HGX_GPU_0/Sensors/Voltage_1",
            &test_identity(),
        );

        let resource = processor.to_json();
        assert_eq!(resource["Id"], "GPU_0");
        assert_eq!(resource["ProcessorType"], "GPU");
        // Identity of the GPU in the socket, which hardware reports here and a
        // consumer needs to distinguish one physical GPU from another.
        assert_eq!(resource["UUID"], "beea8cdf-7b7d-035c-ed07-360e311fcbe1");
        assert_eq!(resource["SerialNumber"], "1655023015625");
        assert_eq!(resource["Model"], "H100 80GB HBM3");
        assert_eq!(
            resource["Metrics"]["@odata.id"],
            "/redfish/v1/Systems/HGX_Baseboard_0/Processors/GPU_0/ProcessorMetrics"
        );

        let metrics = processor.metrics_json();
        assert_eq!(
            metrics["@odata.id"],
            "/redfish/v1/Systems/HGX_Baseboard_0/Processors/GPU_0/ProcessorMetrics"
        );
        // CoreVoltage is sensor-backed, so the health metrics collector skips it
        // (it is emitted by the sensor collector via this DataSourceUri instead).
        assert_eq!(
            metrics["CoreVoltage"]["DataSourceUri"],
            "/redfish/v1/Chassis/HGX_GPU_0/Sensors/Voltage_1"
        );
        // Fields the metrics collector flattens and emits.
        assert_eq!(metrics["PCIeErrors"]["CorrectableErrorCount"], 2);
        assert_eq!(metrics["PowerLimitThrottleDuration"], "PT0S");
        assert_eq!(metrics["ThermalLimitThrottleDuration"], "PT0S");
    }

    #[test]
    fn gpu_metrics_carry_the_nvidia_oem_extension() {
        let processor = gpu(
            "HGX_Baseboard_0",
            "GPU_0",
            "/redfish/v1/Chassis/HGX_GPU_0/Sensors/Voltage_1",
            &test_identity(),
        );
        let metrics = processor.metrics_json();
        let oem = &metrics["Oem"]["Nvidia"];

        // The `@odata.type` is what selects the GPU shape over the
        // generic one when the extension is parsed.
        assert_eq!(
            oem["@odata.type"],
            "#NvidiaProcessorMetrics.v1_4_0.NvidiaGPUProcessorMetrics"
        );
        // One property from the shared v1_1_0 base...
        assert_eq!(oem["SMActivityPercent"], 62.5);
        // ...and one that only the GPU shape declares.
        assert_eq!(oem["C2CDataTxBandwidthGbps"], 400.0);
        // The firmware spelling must survive serialization through the CSDL model.
        assert_eq!(oem["NVJPGUtilizationPercent"].as_f64(), Some(0.0));
        assert!(oem.get("NVJpgUtilizationPercent").is_none());
    }

    #[test]
    fn gpu_processor_can_report_mnnvlink_topology() {
        let processor = gpu(
            "HGX_Baseboard_0",
            "GPU_0",
            "/redfish/v1/Chassis/HGX_GPU_0/Sensors/Voltage_1",
            &test_identity(),
        )
        .with_mnnvlink_topology(26, 16);

        assert_eq!(
            processor.to_json()["Oem"]["Nvidia"]["MNNVLinkTopology"],
            serde_json::json!({
                "TraySlotNumber": 26,
                "TraySlotIndex": 16
            })
        );
    }
}

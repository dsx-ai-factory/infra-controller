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

use crate::json::{JsonExt, JsonPatch};
use crate::redfish;
use crate::redfish::Builder;

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
        odata_type: Cow::Borrowed("#Processor.v1_20_0.Processor"),
        id: Cow::Borrowed(processor_id),
        name: Cow::Borrowed("Processor"),
    }
}

pub(super) fn metrics_resource(system_id: &str, processor_id: &str) -> redfish::Resource<'static> {
    let odata_id =
        format!("/redfish/v1/Systems/{system_id}/Processors/{processor_id}/ProcessorMetrics");
    redfish::Resource {
        odata_id: Cow::Owned(odata_id),
        odata_type: Cow::Borrowed("#ProcessorMetrics.v1_6_1.ProcessorMetrics"),
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
}

fn builder(resource: &redfish::Resource) -> ProcessorBuilder {
    ProcessorBuilder {
        id: Cow::Owned(resource.id.to_string()),
        value: resource.json_patch(),
    }
}

struct ProcessorBuilder {
    id: Cow<'static, str>,
    value: serde_json::Value,
}

impl Builder for ProcessorBuilder {
    fn apply_patch(self, patch: serde_json::Value) -> Self {
        Self {
            value: self.value.patch(patch),
            id: self.id,
        }
    }
}

impl ProcessorBuilder {
    fn processor_type(self, value: &str) -> Self {
        self.add_str_field("ProcessorType", value)
    }

    /// Device identity that survives a socket swap.
    ///
    /// On an NVIDIA GPU this matches the NVML GPU UUID without its `GPU-`
    /// prefix, and the UUID the enclosing GPU chassis reports.
    fn uuid(self, value: &str) -> Self {
        self.add_str_field("UUID", value)
    }

    fn serial_number(self, value: &str) -> Self {
        self.add_str_field("SerialNumber", value)
    }

    fn model(self, value: &str) -> Self {
        self.add_str_field("Model", value)
    }

    fn metrics(self, metrics: &redfish::Resource<'_>) -> Self {
        self.apply_patch(metrics.nav_property("Metrics"))
    }

    fn status(self, status: redfish::resource::Status) -> Self {
        self.apply_patch(json!({ "Status": status.into_json() }))
    }

    fn build(self, metrics: serde_json::Value) -> Processor {
        Processor {
            id: self.id,
            resource: self.value,
            metrics,
        }
    }
}

/// Identity fields an NVIDIA GPU reports on its `Processor` resource.
///
/// Real hardware reports the same values on the enclosing GPU chassis, so a
/// fixture should keep the two in agreement.
pub(crate) struct GpuIdentity<'a> {
    pub(crate) uuid: &'a str,
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
    builder(&system_resource(system_id, processor_id))
        .processor_type("GPU")
        .uuid(identity.uuid)
        .serial_number(identity.serial_number)
        .model(identity.model)
        .status(redfish::resource::Status::Ok)
        .metrics(&metrics)
        .build(metrics_json)
}

fn nvidia_gpu_metrics(
    resource: &redfish::Resource<'_>,
    processor_id: &str,
    core_voltage_sensor_uri: &str,
) -> serde_json::Value {
    resource.json_patch().patch(json!({
        "Name": format!("{processor_id} Processor Metrics"),
        "BandwidthPercent": 0,
        "OperatingSpeedMHz": 0,
        "PowerLimitThrottleDuration": "PT0S",
        "ThermalLimitThrottleDuration": "PT0S",
        "CacheMetricsTotal": {
            "LifeTime": {
                "CorrectableECCErrorCount": 0,
                "UncorrectableECCErrorCount": 0
            }
        },
        "CoreVoltage": {
            "DataSourceUri": core_voltage_sensor_uri,
            "Reading": 0.8
        },
        "PCIeErrors": {
            "CorrectableErrorCount": 2,
            "FatalErrorCount": 0,
            "L0ToRecoveryCount": 2,
            "NAKReceivedCount": 0,
            "NAKSentCount": 0,
            "NonFatalErrorCount": 0,
            "ReplayCount": 0,
            "ReplayRolloverCount": 0,
            "UnsupportedRequestCount": 0
        },
        "Oem": {
            "Nvidia": nvidia_gpu_oem()
        }
    }))
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
    json!({
        "@odata.type": "#NvidiaProcessorMetrics.v1_4_0.NvidiaGPUProcessorMetrics",

        // Shared base (v1_1_0) properties.
        "SMActivityPercent": 62.5,
        "SMOccupancyPercent": 44.0,
        "GraphicsEngineActivityPercent": 71.0,
        "TensorCoreActivityPercent": 18.25,
        "FP64ActivityPercent": 4.0,
        "FP32ActivityPercent": 9.5,
        "FP16ActivityPercent": 12.0,
        "NVDecUtilizationPercent": 0,
        "NVJPGUtilizationPercent": 0,
        "NVOfaUtilizationPercent": 0,
        "DMMAUtilizationPercent": 3.0,
        "HMMAUtilizationPercent": 6.0,
        "IMMAUtilizationPercent": 1.5,
        "PCIeRXBytes": 45388,
        "PCIeTXBytes": 51108,
        "PCIeRawTxBandwidthGbps": 12.0,
        "PCIeRawRxBandwidthGbps": 11.5,
        "NVLinkRawTxBandwidthGbps": 340.0,
        "NVLinkRawRxBandwidthGbps": 338.5,
        "NVLinkDataTxBandwidthGbps": 300.0,
        "NVLinkDataRxBandwidthGbps": 298.0,
        "HardwareViolationThrottleDuration": "PT0S",
        "GlobalSoftwareViolationThrottleDuration": "PT0S",
        "AccumulatedGPUContextUtilizationDuration": "PT10M",
        "AccumulatedSMUtilizationDuration": "PT5M30S",
        // Firmware spells "nothing is throttling" as this single-element
        // list rather than an empty one.
        "ThrottleReasons": ["NA"],

        // GPU-shape-only properties.
        "SMUtilizationPercent": 0,
        "IntegerActivityUtilizationPercent": 2.0,
        "NVEncUtilizationPercent": 0,
        "HostMemoryCacheHitPercent": 88.0,
        "HostMemoryCacheMissPercent": 12.0,
        "PeerMemoryCacheHitPercent": 91.0,
        "PeerMemoryCacheMissPercent": 9.0,
        "DRAMMemoryCacheHitPercent": 96.5,
        "DRAMMemoryCacheMissPercent": 3.5,
        "C2CRawTxBandwidthGbps": 420.0,
        "C2CRawRxBandwidthGbps": 418.0,
        "C2CDataTxBandwidthGbps": 400.0,
        "C2CDataRxBandwidthGbps": 398.0,
        "SRAMECCErrorThresholdExceeded": false
    })
}

#[cfg(test)]
mod tests {
    use super::{GpuIdentity, gpu};

    fn test_identity() -> GpuIdentity<'static> {
        GpuIdentity {
            uuid: "beea8cdf-7b7d-035c-ed07-360e311fcbe1",
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
    }
}

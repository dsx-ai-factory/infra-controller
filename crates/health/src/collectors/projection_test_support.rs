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

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{OriginalUri, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
pub(in crate::collectors) use bmc_mock::test_support::TestBmc;
use bmc_mock::test_support::axum_http_client::AxumRouterHttpClient;
use nv_redfish::ServiceRoot;
use nv_redfish::bmc_http::{BmcCredentials, CacheSettings};
use nv_redfish::chassis::{Chassis, PowerSupply};
use nv_redfish::computer_system::{ComputerSystem, Drive, Memory, Processor, Storage};
use serde_json::{Value, json};
use url::Url;

use super::inventory::{DiscoveredEntity, ShelfPower};

#[derive(Clone)]
enum MockResponse {
    Json(Value),
    Malformed,
}

async fn resource_response(
    State(resources): State<Arc<HashMap<String, MockResponse>>>,
    OriginalUri(uri): OriginalUri,
) -> Response {
    match resources.get(uri.path()) {
        Some(MockResponse::Json(value)) => Json(value.clone()).into_response(),
        Some(MockResponse::Malformed) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/json")],
            "not valid JSON",
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

fn resource(path: &str, odata_type: &str, id: &str, name: &str, attributes: Value) -> Value {
    let Value::Object(mut attributes) = attributes else {
        panic!("test resource attributes must be an object");
    };
    attributes.insert("@odata.id".to_string(), Value::String(path.to_string()));
    attributes.insert(
        "@odata.type".to_string(),
        Value::String(odata_type.to_string()),
    );
    attributes.insert("Id".to_string(), Value::String(id.to_string()));
    attributes.insert("Name".to_string(), Value::String(name.to_string()));
    Value::Object(attributes)
}

fn collection(path: &str, odata_type: &str, name: &str, members: &[&str]) -> Value {
    json!({
        "@odata.id": path,
        "@odata.type": odata_type,
        "Name": name,
        "Members": members
            .iter()
            .map(|member| json!({ "@odata.id": member }))
            .collect::<Vec<_>>()
    })
}

fn insert(resources: &mut HashMap<String, MockResponse>, path: &str, value: Value) {
    resources.insert(path.to_string(), MockResponse::Json(value));
}

fn insert_resource(
    resources: &mut HashMap<String, MockResponse>,
    path: &str,
    odata_type: &str,
    id: &str,
    name: &str,
    attributes: Value,
) {
    insert(
        resources,
        path,
        resource(path, odata_type, id, name, attributes),
    );
}

fn reference(path: &str) -> Value {
    json!({ "@odata.id": path })
}

const TELEMETRY_SERVICE: &str = "/redfish/v1/TelemetryService";

/// A telemetry service publishing two reports: one healthy and one the
/// NVIDIA OEM extension marks stale.
fn insert_telemetry_service(resources: &mut HashMap<String, MockResponse>) {
    const DEFINITIONS: &str = "/redfish/v1/TelemetryService/MetricDefinitions";
    const REPORTS: &str = "/redfish/v1/TelemetryService/MetricReports";

    let definition = |id: &str| format!("{DEFINITIONS}/{id}");
    let report = |id: &str| format!("{REPORTS}/{id}");

    insert_resource(
        resources,
        TELEMETRY_SERVICE,
        "#TelemetryService.v1_3_1.TelemetryService",
        "TelemetryService",
        "Telemetry Service",
        json!({
            "ServiceEnabled": true,
            "MetricDefinitions": reference(DEFINITIONS),
            "MetricReports": reference(REPORTS)
        }),
    );

    let definition_paths = [definition("TotalGPUPowerWatts"), definition("GPU0_Temp")];
    let definition_members: Vec<_> = definition_paths.iter().map(String::as_str).collect();
    insert(
        resources,
        DEFINITIONS,
        collection(
            DEFINITIONS,
            "#MetricDefinitionCollection.MetricDefinitionCollection",
            "Metric Definitions",
            &definition_members,
        ),
    );
    insert_resource(
        resources,
        &definition("TotalGPUPowerWatts"),
        "#MetricDefinition.v1_3_3.MetricDefinition",
        "TotalGPUPowerWatts",
        "Total GPU power",
        json!({ "MetricDataType": "Decimal", "Units": "W" }),
    );
    insert_resource(
        resources,
        &definition("GPU0_Temp"),
        "#MetricDefinition.v1_3_3.MetricDefinition",
        "GPU0_Temp",
        "GPU temperature",
        json!({ "MetricDataType": "Decimal", "Units": "Cel" }),
    );

    let report_paths = [report("PlatformEnvironmentMetrics"), report("StaleReport")];
    let report_members: Vec<_> = report_paths.iter().map(String::as_str).collect();
    insert(
        resources,
        REPORTS,
        collection(
            REPORTS,
            "#MetricReportCollection.MetricReportCollection",
            "Metric Reports",
            &report_members,
        ),
    );
    insert_resource(
        resources,
        &report("PlatformEnvironmentMetrics"),
        "#MetricReport.v1_5_0.MetricReport",
        "PlatformEnvironmentMetrics",
        "Platform environment metrics",
        json!({
            "Timestamp": "2026-01-01T00:00:00Z",
            "MetricValues": [
                {
                    "MetricId": "TotalGPUPowerWatts",
                    "MetricValue": "612.5",
                    "MetricProperty": "/redfish/v1/Chassis/CH0/Sensors/TotalPower"
                },
                {
                    "MetricId": "GPU0_Temp",
                    "MetricValue": "48",
                    "MetricProperty": "/redfish/v1/Chassis/CH0/Sensors/GPU0_Temp"
                },
                // No definition declares a unit for this one.
                { "MetricId": "FanPWM", "MetricValue": "30" },
                // Discrete state, so there is no gauge to publish.
                { "MetricId": "PowerState", "MetricValue": "Enabled" },
                // No id to name a series after.
                { "MetricValue": "1" }
            ]
        }),
    );
    insert_resource(
        resources,
        &report("StaleReport"),
        "#MetricReport.v1_5_0.MetricReport",
        "StaleReport",
        "Stale report",
        json!({
            "MetricValues": [
                {
                    "MetricId": "TotalGPUPowerWatts",
                    "MetricValue": "999.0"
                }
            ],
            "Oem": {
                "Nvidia": {
                    "@odata.type": "#NvidiaMetricReport.v1_0_0.NvidiaMetricReport",
                    "MetricValueStale": true
                }
            }
        }),
    );
}

fn mock_resources() -> HashMap<String, MockResponse> {
    const SYSTEM: &str = "/redfish/v1/Systems/SYS0";
    const PROCESSORS: &str = "/redfish/v1/Systems/SYS0/Processors";
    const MEMORY: &str = "/redfish/v1/Systems/SYS0/Memory";
    const STORAGE: &str = "/redfish/v1/Systems/SYS0/Storage";
    const CHASSIS: &str = "/redfish/v1/Chassis/CH0";
    const POWER_SUBSYSTEM: &str = "/redfish/v1/Chassis/CH0/PowerSubsystem";
    const POWER_SUPPLIES: &str = "/redfish/v1/Chassis/CH0/PowerSubsystem/PowerSupplies";

    let processor = |id: &str| format!("{PROCESSORS}/{id}");
    let memory = |id: &str| format!("{MEMORY}/{id}");
    let storage = |id: &str| format!("{STORAGE}/{id}");
    let drive = |id: &str| format!("{STORAGE}/ST0/Drives/{id}");
    let chassis = |id: &str| format!("/redfish/v1/Chassis/{id}");
    let power_supply = |id: &str| format!("{POWER_SUPPLIES}/{id}");

    let mut resources = HashMap::new();
    insert_resource(
        &mut resources,
        "/redfish/v1",
        "#ServiceRoot.v1_15_0.ServiceRoot",
        "RootService",
        "Root Service",
        json!({
            "Links": {
                "Sessions": reference("/redfish/v1/SessionService/Sessions")
            },
            "Systems": reference("/redfish/v1/Systems"),
            "Chassis": reference("/redfish/v1/Chassis"),
            "TelemetryService": reference(TELEMETRY_SERVICE)
        }),
    );
    insert_telemetry_service(&mut resources);
    insert(
        &mut resources,
        "/redfish/v1/Systems",
        collection(
            "/redfish/v1/Systems",
            "#ComputerSystemCollection.ComputerSystemCollection",
            "Systems",
            &[SYSTEM],
        ),
    );
    insert_resource(
        &mut resources,
        SYSTEM,
        "#ComputerSystem.v1_20_1.ComputerSystem",
        "SYS0",
        "Test system",
        json!({
            "Processors": reference(PROCESSORS),
            "Memory": reference(MEMORY),
            "Storage": reference(STORAGE)
        }),
    );

    let processor_paths = [
        processor("CPU0"),
        processor("GPU0"),
        processor("CPU-sparse"),
        processor("CPU-empty"),
        processor("CPU-malformed"),
    ];
    let processor_members: Vec<_> = processor_paths.iter().map(String::as_str).collect();
    insert(
        &mut resources,
        PROCESSORS,
        collection(
            PROCESSORS,
            "#ProcessorCollection.ProcessorCollection",
            "Processors",
            &processor_members,
        ),
    );
    let cpu = processor("CPU0");
    let cpu_metrics = format!("{cpu}/Metrics");
    insert_resource(
        &mut resources,
        &cpu,
        "#Processor.v1_20_0.Processor",
        "CPU0",
        "Processor",
        json!({
            "ProcessorType": "CPU",
            "Model": "Grace",
            "Metrics": reference(&cpu_metrics)
        }),
    );
    insert_resource(
        &mut resources,
        &cpu_metrics,
        "#ProcessorMetrics.v1_6_1.ProcessorMetrics",
        "Metrics",
        "Processor metrics",
        json!({
            "BandwidthPercent": 42.0,
            "CoreVoltage": {
                "DataSourceUri": "/redfish/v1/Chassis/CH0/Sensors/CPU0_Voltage",
                "Reading": 1.2
            }
        }),
    );
    // A GPU whose metrics carry the NVIDIA OEM extension, so the OEM
    // projection is exercised alongside the standard one.
    let gpu = processor("GPU0");
    let gpu_metrics = format!("{gpu}/Metrics");
    insert_resource(
        &mut resources,
        &gpu,
        "#Processor.v1_20_0.Processor",
        "GPU0",
        "Graphics processor",
        json!({
            "ProcessorType": "GPU",
            "Model": "NVIDIA GB100",
            "Metrics": reference(&gpu_metrics)
        }),
    );
    insert_resource(
        &mut resources,
        &gpu_metrics,
        "#ProcessorMetrics.v1_6_1.ProcessorMetrics",
        "Metrics",
        "Graphics processor metrics",
        json!({
            "BandwidthPercent": 55.0,
            "Oem": {
                "Nvidia": {
                    "@odata.type": "#NvidiaProcessorMetrics.v1_4_0.NvidiaGPUProcessorMetrics",
                    "SMActivityPercent": 71.5,
                    "SMUtilizationPercent": 64.0,
                    "TensorCoreActivityPercent": 12.25,
                    "PCIeTXBytes": 51108,
                    "PCIeRXBytes": 45388,
                    "NVLinkDataTxBandwidthGbps": 18.5,
                    "SRAMECCErrorThresholdExceeded": false,
                    "HardwareViolationThrottleDuration": "PT2S",
                    "ThrottleReasons": ["SWPowerCap", "HWSlowdown"]
                }
            }
        }),
    );

    #[derive(Clone, Copy)]
    enum ProcessorMetricsResource {
        Missing,
        Empty,
        Malformed,
    }

    for (id, metrics) in [
        ("CPU-sparse", ProcessorMetricsResource::Missing),
        ("CPU-empty", ProcessorMetricsResource::Empty),
        ("CPU-malformed", ProcessorMetricsResource::Malformed),
    ] {
        let path = processor(id);
        let metrics_path = format!("{path}/Metrics");
        let attributes = match metrics {
            ProcessorMetricsResource::Missing => json!({}),
            ProcessorMetricsResource::Empty | ProcessorMetricsResource::Malformed => {
                json!({ "Metrics": reference(&metrics_path) })
            }
        };
        insert_resource(
            &mut resources,
            &path,
            "#Processor.v1_20_0.Processor",
            id,
            "Test processor",
            attributes,
        );
        match metrics {
            ProcessorMetricsResource::Empty => insert_resource(
                &mut resources,
                &metrics_path,
                "#ProcessorMetrics.v1_6_1.ProcessorMetrics",
                "Metrics",
                "Empty processor metrics",
                json!({}),
            ),
            ProcessorMetricsResource::Malformed => {
                resources.insert(metrics_path, MockResponse::Malformed);
            }
            ProcessorMetricsResource::Missing => {}
        }
    }

    let memory_paths = [memory("DIMM0"), memory("DIMM-oem"), memory("DIMM-sparse")];
    let memory_members: Vec<_> = memory_paths.iter().map(String::as_str).collect();
    insert(
        &mut resources,
        MEMORY,
        collection(
            MEMORY,
            "#MemoryCollection.MemoryCollection",
            "Memory",
            &memory_members,
        ),
    );
    let dimm = memory("DIMM0");
    let dimm_metrics = format!("{dimm}/Metrics");
    insert_resource(
        &mut resources,
        &dimm,
        "#Memory.v1_20_0.Memory",
        "DIMM0",
        "Memory",
        json!({
            "MemoryDeviceType": "DDR5",
            "Model": "HMCG94AGBRA",
            "Metrics": reference(&dimm_metrics)
        }),
    );
    insert_resource(
        &mut resources,
        &dimm_metrics,
        "#MemoryMetrics.v1_7_0.MemoryMetrics",
        "Metrics",
        "Memory metrics",
        json!({ "BlockSizeBytes": 4096 }),
    );
    // HBM reports row remapping through the NVIDIA OEM extension.
    let oem_dimm = memory("DIMM-oem");
    let oem_dimm_metrics = format!("{oem_dimm}/Metrics");
    insert_resource(
        &mut resources,
        &oem_dimm,
        "#Memory.v1_20_0.Memory",
        "DIMM-oem",
        "HBM memory",
        json!({
            "MemoryDeviceType": "HBM3",
            "Model": "GB100 HBM",
            "Metrics": reference(&oem_dimm_metrics)
        }),
    );
    insert_resource(
        &mut resources,
        &oem_dimm_metrics,
        "#MemoryMetrics.v1_7_0.MemoryMetrics",
        "Metrics",
        "HBM memory metrics",
        json!({
            "BlockSizeBytes": 8192,
            "Oem": {
                "Nvidia": {
                    "@odata.type": "#NvidiaMemoryMetrics.v1_2_0.NvidiaMemoryMetrics",
                    "RowRemapping": {
                        "CorrectableRowRemappingCount": 3,
                        "UncorrectableRowRemappingCount": 1,
                        "MaxAvailabilityBankCount": 40,
                        "NoAvailablityBankCount": 2
                    }
                }
            }
        }),
    );

    let sparse_memory = memory("DIMM-sparse");
    insert_resource(
        &mut resources,
        &sparse_memory,
        "#Memory.v1_20_0.Memory",
        "DIMM-sparse",
        "Sparse memory",
        json!({}),
    );

    let storage_controller = storage("ST0");
    insert(
        &mut resources,
        STORAGE,
        collection(
            STORAGE,
            "#StorageCollection.StorageCollection",
            "Storage",
            &[&storage_controller],
        ),
    );
    let drive_paths = [drive("D0"), drive("D-sparse")];
    insert_resource(
        &mut resources,
        &storage_controller,
        "#Storage.v1_15_0.Storage",
        "ST0",
        "Storage",
        json!({
            "Drives": drive_paths.iter().map(|path| reference(path)).collect::<Vec<_>>()
        }),
    );
    let disk = drive("D0");
    let disk_metrics = format!("{disk}/Metrics");
    insert_resource(
        &mut resources,
        &disk,
        "#Drive.v1_19_0.Drive",
        "D0",
        "Drive",
        json!({
            "Model": "NVMe-1",
            "PredictedMediaLifeLeftPercent": 80.0,
            "Metrics": reference(&disk_metrics)
        }),
    );
    insert_resource(
        &mut resources,
        &disk_metrics,
        "#DriveMetrics.v1_2_0.DriveMetrics",
        "Metrics",
        "Drive metrics",
        json!({ "BadBlockCount": 4 }),
    );
    let sparse_drive = drive("D-sparse");
    insert_resource(
        &mut resources,
        &sparse_drive,
        "#Drive.v1_19_0.Drive",
        "D-sparse",
        "Sparse drive",
        json!({}),
    );

    let sparse_chassis = chassis("CH-sparse");
    insert(
        &mut resources,
        "/redfish/v1/Chassis",
        collection(
            "/redfish/v1/Chassis",
            "#ChassisCollection.ChassisCollection",
            "Chassis",
            &[CHASSIS, &sparse_chassis],
        ),
    );
    insert_resource(
        &mut resources,
        CHASSIS,
        "#Chassis.v1_27_0.Chassis",
        "CH0",
        "Chassis",
        json!({
            "ChassisType": "RackMount",
            "Model": "HGX",
            "MaxPowerWatts": 33000,
            "PowerState": "On",
            "Status": { "Health": "OK", "State": "StandbyOffline" },
            "PowerSubsystem": reference(POWER_SUBSYSTEM)
        }),
    );
    insert_resource(
        &mut resources,
        &sparse_chassis,
        "#Chassis.v1_27_0.Chassis",
        "CH-sparse",
        "Sparse chassis",
        json!({ "ChassisType": "RackMount" }),
    );
    insert_resource(
        &mut resources,
        POWER_SUBSYSTEM,
        "#PowerSubsystem.v1_1_0.PowerSubsystem",
        "PowerSubsystem",
        "Power subsystem",
        json!({
            "Status": { "Health": "OK", "State": "Enabled" },
            "PowerSupplies": reference(POWER_SUPPLIES)
        }),
    );

    let power_supply_paths = [power_supply("PS0"), power_supply("PS-sparse")];
    let power_supply_members: Vec<_> = power_supply_paths.iter().map(String::as_str).collect();
    insert(
        &mut resources,
        POWER_SUPPLIES,
        collection(
            POWER_SUPPLIES,
            "#PowerSupplyCollection.PowerSupplyCollection",
            "Power supplies",
            &power_supply_members,
        ),
    );
    let psu = power_supply("PS0");
    let psu_metrics = format!("{psu}/Metrics");
    insert_resource(
        &mut resources,
        &psu,
        "#PowerSupply.v1_5_0.PowerSupply",
        "PS0",
        "Power supply",
        json!({
            "Model": "PSU-3KW",
            "PowerCapacityWatts": 3000.0,
            "Status": { "Health": "Warning", "State": "Enabled" },
            "Metrics": reference(&psu_metrics)
        }),
    );
    insert_resource(
        &mut resources,
        &psu_metrics,
        "#PowerSupplyMetrics.v1_0_0.PowerSupplyMetrics",
        "Metrics",
        "Power supply metrics",
        json!({ "OutputPowerWatts": { "Reading": 500.0 } }),
    );
    let sparse_psu = power_supply("PS-sparse");
    insert_resource(
        &mut resources,
        &sparse_psu,
        "#PowerSupply.v1_5_0.PowerSupply",
        "PS-sparse",
        "Sparse power supply",
        json!({}),
    );

    resources
}

#[derive(Clone, Copy)]
pub(in crate::collectors) enum TestEntity {
    Processor,
    NvidiaGpuProcessor,
    SparseProcessor,
    ProcessorWithEmptyMetrics,
    ProcessorWithMalformedMetrics,
    Memory,
    NvidiaOemMemory,
    SparseMemory,
    Drive,
    SparseDrive,
    PowerSupply,
    SparsePowerSupply,
    Chassis,
    /// `CH0` discovered on a power-shelf endpoint, with its power subsystem.
    ShelfChassis,
    SparseChassis,
}

pub(in crate::collectors) struct ProjectionFixture {
    bmc: Arc<TestBmc>,
    system: Arc<ComputerSystem<TestBmc>>,
    storage: Arc<Storage<TestBmc>>,
    processors: HashMap<String, Arc<Processor<TestBmc>>>,
    memory: HashMap<String, Arc<Memory<TestBmc>>>,
    drives: HashMap<String, Arc<Drive<TestBmc>>>,
    chassis: HashMap<String, Arc<Chassis<TestBmc>>>,
    power_supplies: HashMap<String, Arc<PowerSupply<TestBmc>>>,
}

impl ProjectionFixture {
    pub(in crate::collectors) async fn new() -> Self {
        let resources = Arc::new(mock_resources());
        let router = Router::new()
            .fallback(resource_response)
            .with_state(resources);
        let bmc = Arc::new(TestBmc::new(
            AxumRouterHttpClient::new(router),
            Url::parse("https://projection-test.local").expect("test URL should parse"),
            BmcCredentials::new("root".to_string(), "password".to_string()),
            CacheSettings::with_capacity(64),
        ));
        let root = ServiceRoot::new(bmc.clone())
            .await
            .expect("test service root should load");

        let system = Arc::new(
            root.systems()
                .await
                .expect("systems should load")
                .expect("systems link should exist")
                .members()
                .await
                .expect("system members should load")
                .into_iter()
                .next()
                .expect("test system should exist"),
        );
        let processors = system
            .processors()
            .await
            .expect("processors should load")
            .expect("processors link should exist")
            .into_iter()
            .map(|entity| (entity.raw().base.id.clone(), Arc::new(entity)))
            .collect();
        let memory = system
            .memory_modules()
            .await
            .expect("memory should load")
            .expect("memory link should exist")
            .into_iter()
            .map(|entity| (entity.raw().base.id.clone(), Arc::new(entity)))
            .collect();
        let storage = Arc::new(
            system
                .storage_controllers()
                .await
                .expect("storage should load")
                .expect("storage link should exist")
                .into_iter()
                .next()
                .expect("test storage should exist"),
        );
        let drives = storage
            .drives()
            .await
            .expect("drives should load")
            .expect("drives should exist")
            .into_iter()
            .map(|entity| (entity.raw().base.id.clone(), Arc::new(entity)))
            .collect();

        let chassis: HashMap<_, _> = root
            .chassis()
            .await
            .expect("chassis should load")
            .expect("chassis link should exist")
            .members()
            .await
            .expect("chassis members should load")
            .into_iter()
            .map(|entity| (entity.raw().base.id.clone(), Arc::new(entity)))
            .collect();
        let parent_chassis = chassis
            .get("CH0")
            .expect("parent chassis should exist")
            .clone();
        let power_supplies = parent_chassis
            .power_supplies()
            .await
            .expect("power supplies should load")
            .into_iter()
            .map(|entity| (entity.raw().base.id.clone(), Arc::new(entity)))
            .collect();

        Self {
            bmc,
            system,
            storage,
            processors,
            memory,
            drives,
            chassis,
            power_supplies,
        }
    }

    pub(in crate::collectors) fn bmc(&self) -> Arc<TestBmc> {
        self.bmc.clone()
    }

    fn processor(&self, id: &str) -> Arc<Processor<TestBmc>> {
        self.processors
            .get(id)
            .unwrap_or_else(|| panic!("processor {id} should exist"))
            .clone()
    }

    fn memory(&self, id: &str) -> Arc<Memory<TestBmc>> {
        self.memory
            .get(id)
            .unwrap_or_else(|| panic!("memory {id} should exist"))
            .clone()
    }

    fn drive(&self, id: &str) -> Arc<Drive<TestBmc>> {
        self.drives
            .get(id)
            .unwrap_or_else(|| panic!("drive {id} should exist"))
            .clone()
    }

    fn chassis(&self, id: &str) -> Arc<Chassis<TestBmc>> {
        self.chassis
            .get(id)
            .unwrap_or_else(|| panic!("chassis {id} should exist"))
            .clone()
    }

    fn power_supply(&self, id: &str) -> Arc<PowerSupply<TestBmc>> {
        self.power_supplies
            .get(id)
            .unwrap_or_else(|| panic!("power supply {id} should exist"))
            .clone()
    }

    pub(in crate::collectors) async fn entity(
        &self,
        entity: TestEntity,
    ) -> DiscoveredEntity<TestBmc> {
        match entity {
            TestEntity::Processor => {
                let entity = self.processor("CPU0");
                // Inventory receives the sensor links that discovery already combined.
                // One metric-linked sensor is enough to exercise that projection boundary.
                let sensors = entity
                    .metrics_sensor_links()
                    .await
                    .expect("processor sensors should load");
                DiscoveredEntity::Processor {
                    entity,
                    system: self.system.clone(),
                    sensors,
                    gpu: None,
                }
            }
            TestEntity::NvidiaGpuProcessor => DiscoveredEntity::Processor {
                entity: self.processor("GPU0"),
                system: self.system.clone(),
                sensors: Vec::new(),
                gpu: None,
            },
            TestEntity::SparseProcessor => DiscoveredEntity::Processor {
                entity: self.processor("CPU-sparse"),
                system: self.system.clone(),
                sensors: Vec::new(),
                gpu: None,
            },
            TestEntity::ProcessorWithEmptyMetrics => DiscoveredEntity::Processor {
                entity: self.processor("CPU-empty"),
                system: self.system.clone(),
                sensors: Vec::new(),
                gpu: None,
            },
            TestEntity::ProcessorWithMalformedMetrics => DiscoveredEntity::Processor {
                entity: self.processor("CPU-malformed"),
                system: self.system.clone(),
                sensors: Vec::new(),
                gpu: None,
            },
            TestEntity::Memory => DiscoveredEntity::Memory {
                entity: self.memory("DIMM0"),
                system: self.system.clone(),
                sensors: Vec::new(),
            },
            TestEntity::NvidiaOemMemory => DiscoveredEntity::Memory {
                entity: self.memory("DIMM-oem"),
                system: self.system.clone(),
                sensors: Vec::new(),
            },
            TestEntity::SparseMemory => DiscoveredEntity::Memory {
                entity: self.memory("DIMM-sparse"),
                system: self.system.clone(),
                sensors: Vec::new(),
            },
            TestEntity::Drive => DiscoveredEntity::Drive {
                entity: self.drive("D0"),
                storage: self.storage.clone(),
                system: self.system.clone(),
                sensors: Vec::new(),
            },
            TestEntity::SparseDrive => DiscoveredEntity::Drive {
                entity: self.drive("D-sparse"),
                storage: self.storage.clone(),
                system: self.system.clone(),
                sensors: Vec::new(),
            },
            TestEntity::PowerSupply => DiscoveredEntity::PowerSupply {
                entity: self.power_supply("PS0"),
                chassis: self.chassis("CH0"),
                sensors: Vec::new(),
            },
            TestEntity::SparsePowerSupply => DiscoveredEntity::PowerSupply {
                entity: self.power_supply("PS-sparse"),
                chassis: self.chassis("CH0"),
                sensors: Vec::new(),
            },
            TestEntity::Chassis => DiscoveredEntity::Chassis {
                entity: self.chassis("CH0"),
                sensors: Vec::new(),
                shelf_power: None,
                gpu: None,
            },
            TestEntity::ShelfChassis => {
                let entity = self.chassis("CH0");
                let subsystem = entity
                    .raw()
                    .power_subsystem
                    .as_ref()
                    .expect("CH0 links a power subsystem")
                    .get(self.bmc.as_ref())
                    .await
                    .expect("power subsystem should load");
                DiscoveredEntity::Chassis {
                    entity,
                    sensors: Vec::new(),
                    shelf_power: Some(ShelfPower {
                        subsystem: Some(subsystem),
                    }),
                    gpu: None,
                }
            }
            TestEntity::SparseChassis => DiscoveredEntity::Chassis {
                entity: self.chassis("CH-sparse"),
                sensors: Vec::new(),
                shelf_power: None,
                gpu: None,
            },
        }
    }
}

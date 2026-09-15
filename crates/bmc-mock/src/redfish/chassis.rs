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

use axum::Router;
use axum::extract::{Path, State};
use axum::response::Response;
use axum::routing::get;
use serde_json::json;

use crate::bmc_state::BmcState;
use crate::json::{JsonExt, JsonPatch};
use crate::redfish::Builder;
use crate::{http, redfish};

pub(super) fn resource<'a>(chassis_id: &'a str) -> redfish::Resource<'a> {
    let odata_id = format!("{}/{chassis_id}", collection().odata_id);
    redfish::Resource {
        odata_id: Cow::Owned(odata_id),
        odata_type: Cow::Borrowed("#Chassis.v1_23_0.Chassis"),
        id: Cow::Borrowed(chassis_id),
        name: Cow::Borrowed("Chassis"),
    }
}

pub(super) fn collection() -> redfish::Collection<'static> {
    redfish::Collection {
        odata_id: Cow::Borrowed("/redfish/v1/Chassis"),
        odata_type: Cow::Borrowed("#ChassisCollection.ChassisCollection"),
        name: Cow::Borrowed("Chassis Collection"),
    }
}

fn builder(resource: &redfish::Resource) -> ChassisBuilder {
    ChassisBuilder {
        value: resource.json_patch(),
    }
}

pub(crate) fn add_routes(r: Router<BmcState>) -> Router<BmcState> {
    const CHASSIS_ID: &str = "{chassis_id}";
    const NET_ADAPTER_ID: &str = "{network_adapter_id}";
    const NET_FUNC_ID: &str = "{function_id}";
    const PCIE_DEVICE_ID: &str = "{pcie_device_id}";
    const SENSOR_ID: &str = "{sensor_id}";
    const POWER_SUPPLY_ID: &str = "{power_supply_id}";
    const LEAK_DETECTOR_ID: &str = "{leak_detector_id}";
    r.route(&collection().odata_id, get(get_chassis_collection))
        .route(&resource(CHASSIS_ID).odata_id, get(get_chassis))
        .route(
            &redfish::network_adapter::chassis_collection(CHASSIS_ID).odata_id,
            get(get_chassis_network_adapters),
        )
        .route(
            &redfish::network_adapter::chassis_resource(CHASSIS_ID, NET_ADAPTER_ID).odata_id,
            get(get_chassis_network_adapter),
        )
        .route(
            &redfish::network_device_function::chassis_collection(CHASSIS_ID, NET_ADAPTER_ID)
                .odata_id,
            get(get_chassis_network_adapters_network_device_functions_list),
        )
        .route(
            &redfish::network_device_function::chassis_resource(
                CHASSIS_ID,
                NET_ADAPTER_ID,
                NET_FUNC_ID,
            )
            .odata_id,
            get(get_chassis_network_adapters_network_device_function),
        )
        .route(
            &redfish::pcie_device::chassis_collection(CHASSIS_ID).odata_id,
            get(get_chassis_pcie_devices),
        )
        .route(
            &redfish::pcie_device::chassis_resource(CHASSIS_ID, PCIE_DEVICE_ID).odata_id,
            get(get_pcie_device),
        )
        .route(
            &redfish::sensor::chassis_collection(CHASSIS_ID).odata_id,
            get(get_chassis_sensors),
        )
        .route(
            &redfish::sensor::chassis_resource(CHASSIS_ID, SENSOR_ID).odata_id,
            get(get_chassis_sensor),
        )
        .route(
            &redfish::assembly::chassis_resource(CHASSIS_ID).odata_id,
            get(get_chassis_assembly),
        )
        .route(
            &redfish::power_subsystem::resource(CHASSIS_ID).odata_id,
            get(get_chassis_power_subsystem),
        )
        .route(
            &redfish::power_supply::collection(CHASSIS_ID).odata_id,
            get(get_chassis_power_supply_collection),
        )
        .route(
            &redfish::power_supply::resource(CHASSIS_ID, POWER_SUPPLY_ID).odata_id,
            get(get_chassis_power_supply),
        )
        .route(
            &redfish::thermal_subsystem::resource(CHASSIS_ID).odata_id,
            get(get_chassis_thermal_subsystem),
        )
        .route(
            &redfish::thermal_subsystem::leak_detection_resource(CHASSIS_ID).odata_id,
            get(get_chassis_leak_detection),
        )
        .route(
            &redfish::leak_detector::collection(CHASSIS_ID).odata_id,
            get(get_chassis_leak_detector_collection),
        )
        .route(
            &redfish::leak_detector::resource(CHASSIS_ID, LEAK_DETECTOR_ID).odata_id,
            get(get_chassis_leak_detector),
        )
}

pub(crate) struct SingleChassisConfig {
    pub(crate) id: Cow<'static, str>,
    pub(crate) serial_number: Option<Cow<'static, str>>,
    /// Identity of the device the chassis holds, which for a GPU module is the
    /// GPU's own UUID.
    pub(crate) uuid: Option<Cow<'static, str>>,
    pub(crate) manufacturer: Option<Cow<'static, str>>,
    pub(crate) model: Option<Cow<'static, str>>,
    pub(crate) part_number: Option<Cow<'static, str>>,
    pub(crate) sku: Option<Cow<'static, str>>,
    pub(crate) network_adapters: Option<Vec<redfish::network_adapter::NetworkAdapter>>,
    pub(crate) pcie_devices: Option<Vec<redfish::pcie_device::PCIeDevice>>,
    pub(crate) sensors: Option<Vec<redfish::sensor::Sensor>>,
    pub(crate) leak_detectors: Option<Vec<redfish::leak_detector::LeakDetector>>,
    pub(crate) chassis_type: Cow<'static, str>,
    pub(crate) assembly: Option<serde_json::Value>,
    pub(crate) power_supplies: Option<Vec<redfish::power_supply::PowerSupply>>,
    pub(crate) oem: Option<serde_json::Value>,
}

impl SingleChassisConfig {
    // To use with ..SingleChassisConfig::defaults() to fill config
    // with defaults.
    pub(crate) fn defaults() -> SingleChassisConfig {
        Self {
            id: "".into(),
            chassis_type: "".into(),
            serial_number: None,
            uuid: None,
            manufacturer: None,
            model: None,
            part_number: None,
            sku: None,
            network_adapters: None,
            pcie_devices: None,
            sensors: None,
            leak_detectors: None,
            assembly: None,
            power_supplies: None,
            oem: None,
        }
    }
}

pub(crate) struct ChassisConfig {
    pub(crate) chassis: Vec<SingleChassisConfig>,
}

pub(crate) struct ChassisState {
    chassis: Vec<SingleChassisState>,
}

impl ChassisState {
    pub(crate) fn from_config(config: ChassisConfig) -> Self {
        let chassis = config
            .chassis
            .into_iter()
            .map(SingleChassisState::new)
            .collect();
        Self { chassis }
    }

    pub(crate) fn find(&self, chassis_id: &str) -> Option<&SingleChassisState> {
        self.chassis
            .iter()
            .find(|c| c.config.id.as_ref() == chassis_id)
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = &SingleChassisState> {
        self.chassis.iter()
    }
}

pub(crate) struct SingleChassisState {
    pub(crate) config: SingleChassisConfig,
}

impl SingleChassisState {
    fn new(config: SingleChassisConfig) -> Self {
        Self { config }
    }

    pub(crate) fn pcie_devices_resources(&self) -> Vec<redfish::Resource<'static>> {
        self.config
            .pcie_devices
            .iter()
            .flat_map(|v| v.iter())
            .map(|dev| redfish::pcie_device::chassis_resource(&self.config.id, &dev.id))
            .collect::<Vec<_>>()
    }

    fn find_network_adapter(&self, id: &str) -> Option<&redfish::network_adapter::NetworkAdapter> {
        self.config
            .network_adapters
            .as_ref()
            .and_then(|adapters| adapters.iter().find(|na| na.id == id))
    }

    fn find_pcie_device(&self, id: &str) -> Option<&redfish::pcie_device::PCIeDevice> {
        self.config
            .pcie_devices
            .as_ref()
            .and_then(|devs| devs.iter().find(|v| v.id == id))
    }

    fn find_sensor(&self, id: &str) -> Option<&redfish::sensor::Sensor> {
        self.config
            .sensors
            .as_ref()
            .and_then(|sensors| sensors.iter().find(|sensor| sensor.id.as_ref() == id))
    }

    fn find_power_supply(&self, id: &str) -> Option<&redfish::power_supply::PowerSupply> {
        self.config
            .power_supplies
            .as_ref()
            .and_then(|v| v.iter().find(|v| v.id == id))
    }

    fn leak_detectors(&self) -> Vec<redfish::leak_detector::LeakDetector> {
        self.config.leak_detectors.clone().unwrap_or_default()
    }

    fn find_leak_detector(&self, id: &str) -> Option<&redfish::leak_detector::LeakDetector> {
        self.config
            .leak_detectors
            .as_ref()
            .and_then(|leak_detectors| {
                leak_detectors
                    .iter()
                    .find(|detector| detector.id.as_ref() == id)
            })
    }
}

async fn get_chassis_collection(State(state): State<BmcState>) -> Response {
    let members = state
        .chassis_state
        .chassis
        .iter()
        .map(|chassis| resource(&chassis.config.id).entity_ref())
        .collect::<Vec<_>>();
    collection().with_members(&members).into_ok_response()
}

async fn get_chassis(State(state): State<BmcState>, Path(chassis_id): Path<String>) -> Response {
    let Some(chassis_state) = state.chassis_state.find(&chassis_id) else {
        return http::not_found();
    };
    let config = &chassis_state.config;
    let pcie_devices = config
        .pcie_devices
        .is_some()
        .then_some(redfish::pcie_device::chassis_collection(&chassis_id));

    let network_adapters = config
        .network_adapters
        .is_some()
        .then_some(redfish::network_adapter::chassis_collection(&chassis_id));

    let sensors = config
        .sensors
        .is_some()
        .then_some(redfish::sensor::chassis_collection(&chassis_id));

    let assembly = config
        .assembly
        .is_some()
        .then_some(redfish::assembly::chassis_resource(&chassis_id));

    let power_subsystem = config
        .power_supplies
        .is_some()
        .then_some(redfish::power_subsystem::resource(&chassis_id));

    let thermal_subsystem = config
        .leak_detectors
        .is_some()
        .then_some(redfish::thermal_subsystem::resource(&chassis_id));

    let mut b = builder(&resource(&chassis_id))
        .chassis_type(&config.chassis_type)
        .maybe_with(ChassisBuilder::assembly, &assembly)
        .maybe_with(ChassisBuilder::pcie_devices, &pcie_devices)
        .maybe_with(ChassisBuilder::network_adapters, &network_adapters)
        .maybe_with(ChassisBuilder::sensors, &sensors)
        .maybe_with(ChassisBuilder::serial_number, &config.serial_number)
        .maybe_with(ChassisBuilder::uuid, &config.uuid)
        .maybe_with(ChassisBuilder::manufacturer, &config.manufacturer)
        .maybe_with(ChassisBuilder::part_number, &config.part_number)
        .maybe_with(ChassisBuilder::sku, &config.sku)
        .maybe_with(ChassisBuilder::power_subsystem, &power_subsystem)
        .maybe_with(ChassisBuilder::thermal_subsystem, &thermal_subsystem)
        .maybe_with(ChassisBuilder::model, &config.model);

    if let Some(oem) = &config.oem {
        b = b.oem(oem)
    }

    b.build().into_ok_response()
}

async fn get_chassis_network_adapters(
    State(state): State<BmcState>,
    Path(chassis_id): Path<String>,
) -> Response {
    state
        .chassis_state
        .find(&chassis_id)
        .and_then(|chassis_state| chassis_state.config.network_adapters.as_ref())
        .map(|network_adapters| {
            network_adapters
                .iter()
                .map(|na| {
                    redfish::network_adapter::chassis_resource(&chassis_id, &na.id).entity_ref()
                })
                .collect::<Vec<_>>()
        })
        .map(|members| {
            redfish::network_adapter::chassis_collection(&chassis_id)
                .with_members(&members)
                .into_ok_response()
        })
        .unwrap_or_else(http::not_found)
}

async fn get_chassis_network_adapter(
    State(state): State<BmcState>,
    Path((chassis_id, network_adapter_id)): Path<(String, String)>,
) -> Response {
    let Some(chassis_state) = state.chassis_state.find(&chassis_id) else {
        return http::not_found();
    };
    chassis_state
        .find_network_adapter(&network_adapter_id)
        .map(|eth| eth.to_json().into_ok_response())
        .unwrap_or_else(http::not_found)
}

async fn get_chassis_network_adapters_network_device_functions_list(
    State(state): State<BmcState>,
    Path((chassis_id, network_adapter_id)): Path<(String, String)>,
) -> Response {
    state
        .chassis_state
        .find(&chassis_id)
        .and_then(|chassis_state| chassis_state.find_network_adapter(&network_adapter_id))
        .map(|network_adapter| {
            let members = network_adapter
                .functions
                .iter()
                .map(|f| {
                    redfish::network_device_function::chassis_resource(
                        &chassis_id,
                        &network_adapter_id,
                        &f.id,
                    )
                    .entity_ref()
                })
                .collect::<Vec<_>>();
            redfish::network_device_function::chassis_collection(&chassis_id, &network_adapter_id)
                .with_members(&members)
                .into_ok_response()
        })
        .unwrap_or_else(http::not_found)
}

async fn get_chassis_network_adapters_network_device_function(
    State(state): State<BmcState>,
    Path((chassis_id, network_adapter_id, function_id)): Path<(String, String, String)>,
) -> Response {
    state
        .chassis_state
        .find(&chassis_id)
        .and_then(|chassis_state| chassis_state.find_network_adapter(&network_adapter_id))
        .and_then(|network_adapter| network_adapter.find_function(&function_id))
        .map(|function| function.to_json().into_ok_response())
        .unwrap_or_else(http::not_found)
}

async fn get_pcie_device(
    State(state): State<BmcState>,
    Path((chassis_id, pcie_device_id)): Path<(String, String)>,
) -> Response {
    state
        .chassis_state
        .find(&chassis_id)
        .and_then(|chassis_state| chassis_state.find_pcie_device(&pcie_device_id))
        .map(|pcie_device| pcie_device.to_json().into_ok_response())
        .unwrap_or_else(http::not_found)
}

async fn get_chassis_pcie_devices(
    State(state): State<BmcState>,
    Path(chassis_id): Path<String>,
) -> Response {
    state
        .chassis_state
        .find(&chassis_id)
        .and_then(|chassis_state| chassis_state.config.pcie_devices.as_ref())
        .map(|pcie_devices| {
            pcie_devices
                .iter()
                .map(|v| redfish::pcie_device::chassis_resource(&chassis_id, &v.id).entity_ref())
                .collect::<Vec<_>>()
        })
        .map(|members| {
            redfish::pcie_device::chassis_collection(&chassis_id)
                .with_members(&members)
                .into_ok_response()
        })
        .unwrap_or_else(http::not_found)
}

async fn get_chassis_sensors(
    State(state): State<BmcState>,
    Path(chassis_id): Path<String>,
) -> Response {
    state
        .chassis_state
        .find(&chassis_id)
        .and_then(|chassis_state| chassis_state.config.sensors.as_ref())
        .map(|sensors| {
            sensors
                .iter()
                .map(|sensor| {
                    redfish::sensor::chassis_resource(&chassis_id, &sensor.id).entity_ref()
                })
                .collect::<Vec<_>>()
        })
        .map(|members| {
            redfish::sensor::chassis_collection(&chassis_id)
                .with_members(&members)
                .into_ok_response()
        })
        .unwrap_or_else(http::not_found)
}

async fn get_chassis_sensor(
    State(state): State<BmcState>,
    Path((chassis_id, sensor_id)): Path<(String, String)>,
) -> Response {
    state
        .chassis_state
        .find(&chassis_id)
        .and_then(|chassis_state| chassis_state.find_sensor(&sensor_id))
        .map(|sensor| sensor.to_json().into_ok_response())
        .unwrap_or_else(http::not_found)
}

async fn get_chassis_assembly(
    State(state): State<BmcState>,
    Path(chassis_id): Path<String>,
) -> Response {
    state
        .chassis_state
        .find(&chassis_id)
        .and_then(|chassis_state| chassis_state.config.assembly.clone())
        .map(|assembly| assembly.into_ok_response())
        .unwrap_or_else(http::not_found)
}

async fn get_chassis_power_subsystem(
    State(state): State<BmcState>,
    Path(chassis_id): Path<String>,
) -> Response {
    state
        .chassis_state
        .find(&chassis_id)
        .and_then(|chassis_state| {
            chassis_state.config.power_supplies.as_ref().map(|_| {
                redfish::power_subsystem::builder(&redfish::power_subsystem::resource(&chassis_id))
                    .power_supplies(redfish::power_supply::collection(&chassis_id))
                    .build()
            })
        })
        .map(|power_subsystem| power_subsystem.into_ok_response())
        .unwrap_or_else(http::not_found)
}

async fn get_chassis_power_supply_collection(
    State(state): State<BmcState>,
    Path(chassis_id): Path<String>,
) -> Response {
    state
        .chassis_state
        .find(&chassis_id)
        .and_then(|chassis_state| chassis_state.config.power_supplies.as_ref())
        .map(|power_supplies| {
            power_supplies
                .iter()
                .map(|ps| redfish::power_supply::resource(&chassis_id, &ps.id).entity_ref())
                .collect::<Vec<_>>()
        })
        .map(|members| {
            redfish::power_supply::collection(&chassis_id)
                .with_members(&members)
                .into_ok_response()
        })
        .unwrap_or_else(http::not_found)
}

async fn get_chassis_power_supply(
    State(state): State<BmcState>,
    Path((chassis_id, power_supply_id)): Path<(String, String)>,
) -> Response {
    let Some(chassis_state) = state.chassis_state.find(&chassis_id) else {
        return http::not_found();
    };
    chassis_state
        .find_power_supply(&power_supply_id)
        .map(|v| v.to_json().into_ok_response())
        .unwrap_or_else(http::not_found)
}

async fn get_chassis_thermal_subsystem(
    State(state): State<BmcState>,
    Path(chassis_id): Path<String>,
) -> Response {
    state
        .chassis_state
        .find(&chassis_id)
        .map(|_| {
            redfish::thermal_subsystem::builder(&redfish::thermal_subsystem::resource(&chassis_id))
                .leak_detection(&redfish::thermal_subsystem::leak_detection_resource(
                    &chassis_id,
                ))
                .build()
                .into_ok_response()
        })
        .unwrap_or_else(http::not_found)
}

async fn get_chassis_leak_detection(
    State(state): State<BmcState>,
    Path(chassis_id): Path<String>,
) -> Response {
    state
        .chassis_state
        .find(&chassis_id)
        .map(|_| {
            redfish::thermal_subsystem::leak_detection_builder(
                &redfish::thermal_subsystem::leak_detection_resource(&chassis_id),
            )
            .leak_detectors(&redfish::leak_detector::collection(&chassis_id))
            .build()
            .into_ok_response()
        })
        .unwrap_or_else(http::not_found)
}

async fn get_chassis_leak_detector_collection(
    State(state): State<BmcState>,
    Path(chassis_id): Path<String>,
) -> Response {
    state
        .chassis_state
        .find(&chassis_id)
        .map(SingleChassisState::leak_detectors)
        .map(|leak_detectors| {
            leak_detectors
                .iter()
                .map(|detector| {
                    redfish::leak_detector::resource(&chassis_id, &detector.id).entity_ref()
                })
                .collect::<Vec<_>>()
        })
        .map(|members| {
            redfish::leak_detector::collection(&chassis_id)
                .with_members(&members)
                .into_ok_response()
        })
        .unwrap_or_else(http::not_found)
}

async fn get_chassis_leak_detector(
    State(state): State<BmcState>,
    Path((chassis_id, leak_detector_id)): Path<(String, String)>,
) -> Response {
    state
        .chassis_state
        .find(&chassis_id)
        .and_then(|chassis_state| chassis_state.find_leak_detector(&leak_detector_id))
        .map(|detector| detector.to_json(&chassis_id).into_ok_response())
        .unwrap_or_else(http::not_found)
}

struct ChassisBuilder {
    value: serde_json::Value,
}

impl Builder for ChassisBuilder {
    fn apply_patch(self, patch: serde_json::Value) -> Self {
        Self {
            value: self.value.patch(patch),
        }
    }
}

impl ChassisBuilder {
    fn serial_number(self, v: &str) -> Self {
        self.add_str_field("SerialNumber", v)
    }

    /// Device identity that survives a chassis-slot swap.
    ///
    /// On an NVIDIA GPU module this matches the NVML GPU UUID without its
    /// `GPU-` prefix, and the UUID the GPU's `Processor` reports.
    fn uuid(self, v: &str) -> Self {
        self.add_str_field("UUID", v)
    }

    fn chassis_type(self, v: &str) -> Self {
        self.add_str_field("ChassisType", v)
    }

    fn manufacturer(self, v: &str) -> Self {
        self.add_str_field("Manufacturer", v)
    }

    fn part_number(self, v: &str) -> Self {
        self.add_str_field("PartNumber", v)
    }

    fn model(self, v: &str) -> Self {
        self.add_str_field("Model", v)
    }

    fn sku(self, v: &str) -> Self {
        self.add_str_field("SKU", v)
    }

    fn assembly(self, v: &redfish::Resource<'_>) -> Self {
        self.apply_patch(v.nav_property("Assembly"))
    }

    fn network_adapters(self, v: &redfish::Collection<'_>) -> Self {
        self.apply_patch(v.nav_property("NetworkAdapters"))
    }

    fn pcie_devices(self, v: &redfish::Collection<'_>) -> Self {
        self.apply_patch(v.nav_property("PCIeDevices"))
    }

    fn sensors(self, v: &redfish::Collection<'_>) -> Self {
        self.apply_patch(v.nav_property("Sensors"))
    }

    fn power_subsystem(self, v: &redfish::Resource<'_>) -> Self {
        self.apply_patch(v.nav_property("PowerSubsystem"))
    }

    fn thermal_subsystem(self, v: &redfish::Resource<'_>) -> Self {
        self.apply_patch(v.nav_property("ThermalSubsystem"))
    }

    fn oem(self, v: &serde_json::Value) -> Self {
        self.apply_patch(json!({"Oem": v}))
    }

    fn build(self) -> serde_json::Value {
        self.value
    }
}

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
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::{Json, Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post};
use serde_json::json;

use crate::bmc_state::BmcState;
use crate::json::{JsonExt, JsonPatch, json_patch};
use crate::redfish::Builder;
use crate::{
    BootOptionKind, Callbacks, LogServices, MachineRouterOptions, MockPowerState,
    POWER_CYCLE_DELAY, SetSystemPowerError, http, redfish,
};

pub(super) fn collection() -> redfish::Collection<'static> {
    redfish::Collection {
        odata_id: Cow::Borrowed("/redfish/v1/Systems"),
        odata_type: Cow::Borrowed("#ComputerSystemCollection.ComputerSystemCollection"),
        name: Cow::Borrowed("Computer System Collection"),
    }
}

pub(super) fn resource<'a>(system_id: &'a str) -> redfish::Resource<'a> {
    let odata_id = format!("/redfish/v1/Systems/{system_id}");
    redfish::Resource {
        odata_id: Cow::Owned(odata_id),
        odata_type: Cow::Borrowed("#ComputerSystem.v1_20_1.ComputerSystem"),
        id: Cow::Borrowed(system_id),
        name: Cow::Borrowed("System"),
    }
}

pub(super) fn reset_target(system_id: &str) -> String {
    format!(
        "{}/Actions/ComputerSystem.Reset",
        resource(system_id).odata_id
    )
}

/// Return the HPE iLO boot settings resource used for persistent boot ordering.
fn hpe_boot_resource(system_id: &str) -> redfish::Resource<'static> {
    redfish::Resource {
        odata_id: Cow::Owned(format!(
            "/redfish/v1/Systems/{system_id}/Bios/oem/hpe/boot/"
        )),
        odata_type: Cow::Borrowed("#HpeServerBootSettings.v2_0_0.HpeServerBootSettings"),
        id: Cow::Borrowed("boot"),
        name: Cow::Borrowed("Boot Settings"),
    }
}

/// HPE iLO settings payload for its persistent boot order.
#[derive(serde::Deserialize)]
#[serde(rename_all = "PascalCase")]
struct HpeBootSettingsPatch {
    persistent_boot_config_order: Vec<String>,
}

pub(crate) fn add_routes(
    r: Router<BmcState>,
    bmc_vendor: redfish::oem::BmcVendor,
) -> Router<BmcState> {
    const SYSTEM_ID: &str = "{system_id}";
    const ETH_ID: &str = "{eth_id}";
    const BOOT_OPTION_ID: &str = "{boot_option_id}";
    const LOG_SERVICE_ID: &str = "{log_service_id}";
    const LOG_ENTRY_ID: &str = "{log_entry_id}";
    const PROCESSOR_ID: &str = "{processor_id}";
    const MEMORY_ID: &str = "{memory_id}";
    let bios = redfish::bios::resource(SYSTEM_ID);
    let routes = r
        .route(&collection().odata_id, get(get_system_collection))
        .route(
            &resource(SYSTEM_ID).odata_id,
            get(get_system).patch(patch_system),
        )
        .route(&reset_target(SYSTEM_ID), post(post_reset_system))
        .route(
            &bmc_vendor.make_settings_odata_id(&resource(SYSTEM_ID)),
            patch(patch_settings),
        )
        .route(
            &redfish::ethernet_interface::system_resource(SYSTEM_ID, ETH_ID).odata_id,
            get(get_ethernet_interface),
        )
        .route(
            &redfish::ethernet_interface::system_collection(SYSTEM_ID).odata_id,
            get(get_ethernet_interface_collection),
        )
        .route(
            &redfish::secure_boot::resource(SYSTEM_ID).odata_id,
            get(get_secure_boot).patch(patch_secure_boot),
        )
        .route(
            &redfish::boot_option::collection(SYSTEM_ID).odata_id,
            get(get_boot_options_collection),
        )
        .route(
            &redfish::boot_option::resource(SYSTEM_ID, BOOT_OPTION_ID).odata_id,
            get(get_boot_option),
        )
        .route(
            &bmc_vendor
                .make_settings_odata_id(&redfish::boot_option::resource(SYSTEM_ID, BOOT_OPTION_ID)),
            patch(patch_boot_option_settings),
        )
        .route(&bios.odata_id, get(get_bios).patch(patch_bios_settings))
        .route(
            &redfish::log_service::system_collection(SYSTEM_ID).odata_id,
            get(get_log_services_collection),
        )
        .route(
            &redfish::log_service::system_resource(SYSTEM_ID, LOG_SERVICE_ID).odata_id,
            get(get_log_service),
        )
        .route(
            &redfish::log_service::system_entries_collection(SYSTEM_ID, LOG_SERVICE_ID).odata_id,
            get(get_log_service_entries),
        )
        .route(
            &format!(
                "{}/{}",
                redfish::log_service::system_entries_collection(SYSTEM_ID, LOG_SERVICE_ID).odata_id,
                LOG_ENTRY_ID
            ),
            get(get_log_service_entry),
        )
        .route(
            &redfish::storage::system_collection(SYSTEM_ID).odata_id,
            get(get_storage_collection),
        )
        .route(
            &redfish::processor::system_collection(SYSTEM_ID).odata_id,
            get(get_processors_collection),
        )
        .route(
            &redfish::processor::system_resource(SYSTEM_ID, PROCESSOR_ID).odata_id,
            get(get_processor),
        )
        .route(
            &redfish::processor::metrics_resource(SYSTEM_ID, PROCESSOR_ID).odata_id,
            get(get_processor_metrics),
        )
        .route(
            &redfish::memory::system_collection(SYSTEM_ID).odata_id,
            get(get_memory_collection),
        )
        .route(
            &redfish::memory::system_resource(SYSTEM_ID, MEMORY_ID).odata_id,
            get(get_memory),
        )
        .route(
            &redfish::memory::metrics_resource(SYSTEM_ID, MEMORY_ID).odata_id,
            get(get_memory_metrics),
        )
        .route(
            &bmc_vendor.make_settings_odata_id(&bios),
            patch(patch_bios_settings),
        )
        .route(
            &redfish::bios::change_password_target(&bios),
            post(change_bios_password_action),
        );
    if matches!(bmc_vendor, redfish::oem::BmcVendor::Hpe) {
        let hpe_boot = hpe_boot_resource(SYSTEM_ID);
        // CombinedServer normalizes requests by removing trailing slashes before
        // routing them, while the Redfish resource still advertises canonical
        // trailing-slash OData identifiers.
        let hpe_boot_path = hpe_boot.odata_id.trim_end_matches('/');
        routes.route(hpe_boot_path, get(get_hpe_boot)).route(
            &format!("{hpe_boot_path}/settings"),
            patch(patch_hpe_boot_settings),
        )
    } else {
        routes
    }
}

pub(crate) struct SingleSystemConfig {
    pub(crate) id: Cow<'static, str>,
    pub(crate) eth_interfaces: Option<Vec<redfish::ethernet_interface::EthernetInterface>>,
    pub(crate) serial_number: Option<Cow<'static, str>>,
    pub(crate) manufacturer: Option<Cow<'static, str>>,
    pub(crate) model: Option<Cow<'static, str>>,
    pub(crate) bios_version: Option<Cow<'static, str>>,
    pub(crate) boot_order_mode: BootOrderMode,
    pub(crate) callbacks: Option<Arc<dyn Callbacks>>,
    pub(crate) chassis: Vec<Cow<'static, str>>,
    pub(crate) boot_options: Option<Vec<redfish::boot_option::BootOption>>,
    pub(crate) bios_mode: BiosMode,
    pub(crate) base_bios: Option<serde_json::Value>,
    pub(crate) log_services: Option<Arc<dyn LogServices>>,
    pub(crate) storage: Option<Vec<redfish::storage::Storage>>,
    pub(crate) processors: Option<Vec<redfish::processor::Processor>>,
    pub(crate) memory: Option<Vec<redfish::memory::Memory>>,
    pub(crate) secure_boot_available: bool,
    pub(crate) serial_console: Option<redfish::serial_console::SerialConsole>,
    pub(crate) oem: Oem,
}

pub(crate) struct Config {
    pub(crate) systems: Vec<SingleSystemConfig>,
}

pub struct SystemState {
    systems: Vec<SingleSystemState>,
}

#[derive(Default)]
struct BootSourceOverride {
    mode: Option<String>,
    enabled: Option<String>,
    target: Option<String>,
}

pub(crate) struct SingleSystemState {
    config: SingleSystemConfig,
    serial_console_ssh_port_override: Mutex<Option<u16>>,
    virtual_media: Option<redfish::virtual_media::VirtualMediaState>,
    boot_order_override: Mutex<Option<Vec<String>>>,
    // HPE iLO uses OEM structured boot strings here, not the BootOption IDs
    // exposed by the standard ComputerSystem BootOrder property.
    hpe_boot_order_override: Mutex<Option<Vec<String>>>,
    boot_option_overrides: Mutex<HashMap<String, serde_json::Value>>,
    boot_source_override: Mutex<BootSourceOverride>,
    secure_boot_enabled: Arc<AtomicBool>,
    bios_overrides: Arc<Mutex<serde_json::Value>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BootOrderMode {
    Generic,
    OrderedCollection,
    ViaSettings, // Set boot order using /Settings resource
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BiosMode {
    DellOem,
    Generic,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Oem {
    NvidiaBluefield,
    Generic,
}

impl SystemState {
    pub(crate) fn from_config(config: Config, options: &MachineRouterOptions) -> Self {
        Self::from_configs(config.systems, options.virtual_media_devices.clone())
    }

    pub(crate) fn systems(&self) -> &[SingleSystemState] {
        &self.systems
    }

    pub(crate) fn find(&self, system_id: &str) -> Option<&SingleSystemState> {
        self.systems
            .iter()
            .find(|system| system.config.id.as_ref() == system_id)
    }

    fn from_configs(
        configs: Vec<SingleSystemConfig>,
        virtual_media_devices: Option<Vec<redfish::virtual_media::DeviceConfig>>,
    ) -> Self {
        let mut virtual_media =
            virtual_media_devices.map(redfish::virtual_media::VirtualMediaState::new);
        let systems = configs
            .into_iter()
            .map(|config| {
                let virtual_media = if config.callbacks.is_some() {
                    virtual_media.take()
                } else {
                    None
                };
                SingleSystemState::new(config, virtual_media)
            })
            .collect();
        Self { systems }
    }

    pub(crate) fn controlled_system(&self) -> Option<&SingleSystemState> {
        self.systems
            .iter()
            .find(|system| system.config.callbacks.is_some())
    }

    pub fn resolve_current_boot_selection(&self) -> Option<BootOptionKind> {
        self.systems
            .iter()
            .find_map(|system| system.resolve_current_boot_selection())
    }

    pub(crate) fn on_boot_completed(&self) {
        self.systems.iter().for_each(|s| s.on_boot_completed())
    }

    /// Returns whether any system advertises an enabled SSH serial console.
    pub fn has_enabled_ssh_serial_console(&self) -> bool {
        self.systems.iter().any(|system| {
            system
                .config
                .serial_console
                .as_ref()
                .is_some_and(redfish::serial_console::SerialConsole::has_enabled_ssh)
        })
    }

    /// Overrides the advertised SSH serial-console port only for systems whose
    /// hardware profile already declares an enabled SSH serial console.
    pub fn set_serial_console_ssh_port(&self, port: Option<u16>) -> bool {
        let mut updated = false;
        for system in &self.systems {
            if system
                .config
                .serial_console
                .as_ref()
                .is_some_and(redfish::serial_console::SerialConsole::has_enabled_ssh)
            {
                *system
                    .serial_console_ssh_port_override
                    .lock()
                    .expect("mutex poisoned") = port;
                updated = true;
            }
        }
        updated
    }

    /// Advertises a simulated SSH console even when the captured hardware profile omits the
    /// optional Redfish `SerialConsole` property.
    pub(crate) fn set_simulated_serial_console_ssh_port(&self, port: Option<u16>) -> bool {
        self.systems.iter().for_each(|system| {
            *system
                .serial_console_ssh_port_override
                .lock()
                .expect("mutex poisoned") = port;
        });
        !self.systems.is_empty()
    }
}

impl SingleSystemState {
    fn new(
        config: SingleSystemConfig,
        virtual_media: Option<redfish::virtual_media::VirtualMediaState>,
    ) -> Self {
        Self {
            config,
            virtual_media,
            serial_console_ssh_port_override: Mutex::new(None),
            boot_order_override: Mutex::new(None),
            hpe_boot_order_override: Mutex::new(None),
            boot_option_overrides: Mutex::new(HashMap::new()),
            boot_source_override: Mutex::new(BootSourceOverride::default()),
            secure_boot_enabled: Arc::new(AtomicBool::new(false)),
            bios_overrides: Arc::new(Mutex::new(serde_json::json!({}))),
        }
    }

    pub(crate) fn on_boot_completed(&self) {
        let mut src = self.boot_source_override.lock().unwrap();
        if src.enabled.as_ref().is_some_and(|v| v == "Once") {
            src.enabled = Some("Disabled".into())
        }
    }

    fn find_processor(&self, processor_id: &str) -> Option<&redfish::processor::Processor> {
        self.config
            .processors
            .iter()
            .flatten()
            .find(|processor| processor.id == processor_id)
    }

    fn find_memory(&self, memory_id: &str) -> Option<&redfish::memory::Memory> {
        self.config
            .memory
            .iter()
            .flatten()
            .find(|memory| memory.id == memory_id)
    }

    pub(crate) fn find_boot_option(
        &self,
        option_id: &str,
    ) -> Option<&redfish::boot_option::BootOption> {
        self.config
            .boot_options
            .iter()
            .flatten()
            .find(|v| v.id == option_id)
    }

    fn boot_option(&self, option_id: &str) -> Option<serde_json::Value> {
        let option = self.find_boot_option(option_id)?;
        let overrides = self.boot_option_overrides.lock().expect("mutex poisoned");
        Some(
            option.to_json().patch(
                overrides
                    .get(option_id)
                    .cloned()
                    .unwrap_or_else(|| json!({})),
            ),
        )
    }

    fn patch_boot_option(&self, option_id: &str, patch_request: serde_json::Value) -> bool {
        if self.find_boot_option(option_id).is_none() {
            return false;
        }

        let mut overrides = self.boot_option_overrides.lock().expect("mutex poisoned");
        let current = overrides
            .entry(option_id.to_string())
            .or_insert_with(|| json!({}));
        *current = current.clone().patch(patch_request);
        true
    }

    fn set_boot_order_override(&self, boot_order: Vec<String>) {
        *self.boot_order_override.lock().unwrap() = Some(boot_order);
    }

    fn boot_order_override(&self) -> Option<Vec<String>> {
        self.boot_order_override.lock().unwrap().clone()
    }

    /// Return the HPE OEM persistent order without changing standard BootOrder state.
    fn hpe_boot_order(&self) -> Vec<String> {
        self.hpe_boot_order_override
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| {
                self.config
                    .boot_options
                    .iter()
                    .flatten()
                    .map(|option| {
                        let prefix = match option.kind {
                            BootOptionKind::Disk => "HD",
                            BootOptionKind::Network => "NIC",
                        };
                        format!("{prefix}.BootOption.{}", option.boot_reference())
                    })
                    .collect()
            })
    }

    /// Persist an HPE OEM boot order independently from standard BootOrder state.
    fn set_hpe_boot_order(&self, boot_order: Vec<String>) {
        *self.hpe_boot_order_override.lock().unwrap() = Some(boot_order);
    }

    pub(crate) fn virtual_media(&self) -> Option<&redfish::virtual_media::VirtualMediaState> {
        self.virtual_media.as_ref()
    }

    pub(crate) fn boot_source_override(&self) -> serde_json::Value {
        let boot_source_override = self.boot_source_override.lock().unwrap();
        let mut value = serde_json::Map::new();
        if let Some(mode) = &boot_source_override.mode {
            value.insert(
                "BootSourceOverrideMode".to_string(),
                serde_json::Value::String(mode.clone()),
            );
        }
        if let Some(enabled) = &boot_source_override.enabled {
            value.insert(
                "BootSourceOverrideEnabled".to_string(),
                serde_json::Value::String(enabled.clone()),
            );
        }
        if let Some(target) = &boot_source_override.target {
            value.insert(
                "BootSourceOverrideTarget".to_string(),
                serde_json::Value::String(target.clone()),
            );
        }
        serde_json::Value::Object(value)
    }

    fn apply_boot_source_override(&self, boot: &serde_json::Value) {
        let has_override = [
            "BootSourceOverrideMode",
            "BootSourceOverrideEnabled",
            "BootSourceOverrideTarget",
        ]
        .iter()
        .any(|field| boot.get(field).is_some());
        if !has_override {
            return;
        }

        let mut boot_source_override = self.boot_source_override.lock().unwrap();
        if let Some(value) = boot.get("BootSourceOverrideMode") {
            boot_source_override.mode = value.as_str().map(ToString::to_string);
        }
        if let Some(value) = boot.get("BootSourceOverrideEnabled") {
            boot_source_override.enabled = value.as_str().map(ToString::to_string);
        }
        if let Some(value) = boot.get("BootSourceOverrideTarget") {
            boot_source_override.target = value.as_str().map(ToString::to_string);
        }
    }

    fn resolve_current_boot_selection(&self) -> Option<BootOptionKind> {
        let src = self.boot_source_override.lock().unwrap();
        if src.enabled.as_ref().is_some_and(|v| v != "Disabled")
            && src.mode.as_ref().is_some_and(|v| v == "UEFI")
            && let Some(target) = src.target.as_ref()
        {
            match target.as_str() {
                "Hdd" => Some(BootOptionKind::Disk),
                "UefiHttp" | "Pxe" => Some(BootOptionKind::Network),
                _ => None,
            }
            .filter(|kind| {
                self.config
                    .boot_options
                    .iter()
                    .flatten()
                    .any(|opt| opt.kind == *kind)
            })
        } else {
            None
        }
        .or_else(|| {
            self.boot_order_override().and_then(|overrides| {
                overrides.first().and_then(|optref| {
                    self.config
                        .boot_options
                        .iter()
                        .flatten()
                        .find(|v| v.boot_reference() == optref)
                        .map(|opt| opt.kind)
                })
            })
        })
        .or_else(|| {
            self.config
                .boot_options
                .as_ref()?
                .first()
                .map(|opt| opt.kind)
        })
    }
}

async fn get_system_collection(State(state): State<BmcState>) -> Response {
    // Delta power shelves serve no `Systems` collection at all (the endpoint
    // 404s), which is the condition site-explorer's Delta path handles.
    if !state.exposes_computer_systems {
        return http::not_found();
    }
    let members = state
        .system_state
        .systems()
        .iter()
        .map(|system| resource(&system.config.id).entity_ref())
        .collect::<Vec<_>>();
    collection().with_members(&members).into_ok_response()
}

async fn get_system(State(state): State<BmcState>, Path(system_id): Path<String>) -> Response {
    let Some(system_state) = state.system_state.find(&system_id) else {
        return http::not_found();
    };

    let mut b = builder(&resource(&system_id)).link_chassis(&system_state.config.chassis);

    let config = &system_state.config;

    if let Some(state) = config
        .callbacks
        .as_ref()
        .map(|callbacks| callbacks.get_power_state())
    {
        b = b.power_state(state)
    }

    if config.boot_options.is_some() {
        if let Some(boot_order) = system_state.boot_order_override() {
            b = b.boot_order(&boot_order.iter().map(String::as_str).collect::<Vec<_>>());
        } else {
            b = b.boot_order(
                &config
                    .boot_options
                    .iter()
                    .flatten()
                    .map(|v| v.boot_reference())
                    .collect::<Vec<_>>(),
            );
        }
    }

    let boot_source_override = system_state.boot_source_override();
    if boot_source_override
        .as_object()
        .is_some_and(|value| !value.is_empty())
    {
        b = b.boot_source_override(boot_source_override);
    }

    if system_state.virtual_media().is_some() {
        b = b.virtual_media(&redfish::virtual_media::collection(&system_id));
    }

    b = match config.oem {
        Oem::Generic => b,
        Oem::NvidiaBluefield => {
            b.oem_nvidia(&redfish::oem::nvidia::bluefield::resource(&system_id))
        }
    };

    let simulated_ssh_port = *system_state
        .serial_console_ssh_port_override
        .lock()
        .expect("mutex poisoned");
    let serial_console = match (&config.serial_console, simulated_ssh_port) {
        (Some(serial_console), Some(port)) => Some(serial_console.with_ssh_port(port)),
        (Some(serial_console), None) => Some(serial_console.clone()),
        (None, Some(port)) => Some(redfish::serial_console::simulated_ssh(port)),
        (None, None) => None,
    };
    if let Some(serial_console) = serial_console {
        b = b.serial_console(&serial_console);
    }

    let pcie_devices = config
        .chassis
        .iter()
        .flat_map(|chassis_id| state.chassis_state.find(chassis_id))
        .flat_map(|chassis| chassis.pcie_devices_resources().into_iter())
        .collect::<Vec<_>>();

    let bios = config
        .base_bios
        .is_some()
        .then_some(redfish::bios::resource(&system_id));

    let boot_options = config
        .boot_options
        .is_some()
        .then_some(redfish::boot_option::collection(&system_id));

    let ethernet_interfaces = config
        .eth_interfaces
        .is_some()
        .then_some(redfish::ethernet_interface::system_collection(&system_id));

    let log_services = config
        .log_services
        .is_some()
        .then_some(redfish::log_service::system_collection(&system_id));

    let storage = config
        .storage
        .is_some()
        .then_some(redfish::storage::system_collection(&system_id));

    let processors = config
        .processors
        .is_some()
        .then_some(redfish::processor::system_collection(&system_id));

    let memory = config
        .memory
        .is_some()
        .then_some(redfish::memory::system_collection(&system_id));

    let secure_boot = config
        .secure_boot_available
        .then_some(redfish::secure_boot::resource(&system_id));

    b.maybe_with(SystemBuilder::serial_number, &config.serial_number)
        .maybe_with(SystemBuilder::manufacturer, &config.manufacturer)
        .maybe_with(SystemBuilder::model, &config.model)
        .maybe_with(SystemBuilder::bios_version, &config.bios_version)
        .maybe_with(SystemBuilder::bios, &bios)
        .maybe_with(SystemBuilder::boot_options, &boot_options)
        .maybe_with(SystemBuilder::ethernet_interfaces, &ethernet_interfaces)
        .maybe_with(SystemBuilder::log_services, &log_services)
        .maybe_with(SystemBuilder::storage, &storage)
        .maybe_with(SystemBuilder::processors, &processors)
        .maybe_with(SystemBuilder::memory, &memory)
        .maybe_with(SystemBuilder::secure_boot, &secure_boot)
        .pcie_devices(&pcie_devices)
        .build()
        .into_ok_response()
}

async fn get_ethernet_interface(
    State(state): State<BmcState>,
    Path((system_id, interface_id)): Path<(String, String)>,
) -> Response {
    let Some(system_state) = state.system_state.find(&system_id) else {
        return http::not_found();
    };
    system_state
        .config
        .eth_interfaces
        .iter()
        .flatten()
        .find(|eth| eth.id == interface_id)
        .map(|eth| eth.to_json().into_ok_response())
        .unwrap_or_else(http::not_found)
}

async fn get_ethernet_interface_collection(
    State(state): State<BmcState>,
    Path(system_id): Path<String>,
) -> Response {
    let Some(system_state) = state.system_state.find(&system_id) else {
        return http::not_found();
    };
    let members = system_state
        .config
        .eth_interfaces
        .iter()
        .flatten()
        .map(|eth| redfish::ethernet_interface::system_resource(&system_id, &eth.id).entity_ref())
        .collect::<Vec<_>>();
    redfish::ethernet_interface::system_collection(&system_id)
        .with_members(&members)
        .into_ok_response()
}

async fn patch_settings(
    State(state): State<BmcState>,
    Path(system_id): Path<String>,
    Json(patch_settings): Json<serde_json::Value>,
) -> Response {
    let Some(system_state) = state.system_state.find(&system_id) else {
        return http::not_found();
    };
    if let Some(boot) = patch_settings.get("Boot") {
        if let Some(new_boot_order) = boot
            .get("BootOrder")
            .and_then(serde_json::Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(serde_json::Value::as_str)
                    .map(ToString::to_string)
                    .collect()
            })
        {
            match system_state.config.boot_order_mode {
                BootOrderMode::ViaSettings => {
                    system_state.set_boot_order_override(new_boot_order);
                }
                _ => {
                    return json!("Boot order setup must use ComputerSystem resource")
                        .into_response(StatusCode::BAD_REQUEST);
                }
            }
        }
        system_state.apply_boot_source_override(boot);
    }
    json!({}).into_ok_response()
}

async fn patch_system(
    State(state): State<BmcState>,
    Path(system_id): Path<String>,
    Json(patch_system): Json<serde_json::Value>,
) -> Response {
    let Some(system_state) = state.system_state.find(&system_id) else {
        return http::not_found();
    };
    let boot = patch_system.get("Boot");
    let response = if let Some(new_boot_order) = boot
        .and_then(|obj| obj.get("BootOrder"))
        .and_then(serde_json::Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(serde_json::Value::as_str)
                .map(ToString::to_string)
                .collect()
        }) {
        match system_state.config.boot_order_mode {
            BootOrderMode::OrderedCollection => {
                system_state.set_boot_order_override(new_boot_order);
                if matches!(&state.oem_state, redfish::oem::State::DellIdrac(_)) {
                    redfish::oem::dell::idrac::create_job_with_location(state.clone())
                } else {
                    json!({}).into_ok_response()
                }
            }
            BootOrderMode::ViaSettings => json!("Boot order setup must use Settings resource")
                .into_response(StatusCode::BAD_REQUEST),
            BootOrderMode::Generic => {
                system_state.set_boot_order_override(new_boot_order);
                json!({}).into_ok_response()
            }
        }
    } else {
        json!({}).into_ok_response()
    };
    if let Some(boot) = boot {
        system_state.apply_boot_source_override(boot);
    }
    response
}

async fn post_reset_system(
    State(state): State<BmcState>,
    Path(system_id): Path<String>,
    Json(mut power_request): Json<serde_json::Value>,
) -> Response {
    let Some(system_state) = state.system_state.find(&system_id) else {
        return http::not_found();
    };
    let Some(callbacks) = system_state.config.callbacks.as_ref() else {
        return http::not_found();
    };
    let Some(reset_type) = power_request
        .get_mut("ResetType")
        .map(std::mem::take)
        .and_then(|v| serde_json::from_value(v).ok())
    else {
        return json!("Valid ResetType is expected field in Reset action")
            .into_response(StatusCode::BAD_REQUEST);
    };

    // Reply with a failure if the power request is invalid for the current state.
    // Note: This logic is duplicated with that in machine-a-tron's MachineStateMachine, because
    // we don't want to block waiting for the power control implementation to reply. Doing so may
    // introduce a deadlock if the API server holds a lock on the row for this machine
    // while issuing a redfish call, and MachineStateMachine is blocked waiting for the row lock
    // to be released.
    match callbacks.set_power_state(reset_type) {
        Ok(_) => json!({}).into_ok_response(),
        Err(SetSystemPowerError::BadRequest(_)) => StatusCode::BAD_REQUEST.into_response(),
        Err(SetSystemPowerError::CommandSendError(_)) => {
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

async fn get_secure_boot(State(state): State<BmcState>, Path(system_id): Path<String>) -> Response {
    let Some(system_state) = state.system_state.find(&system_id) else {
        return http::not_found();
    };
    let secure_boot_enabled = system_state.secure_boot_enabled.load(Ordering::Relaxed);
    redfish::secure_boot::builder(&redfish::secure_boot::resource(&system_id))
        .secure_boot_enable(secure_boot_enabled)
        .secure_boot_current_boot(secure_boot_enabled)
        .build()
        .into_ok_response()
}

async fn patch_secure_boot(
    State(state): State<BmcState>,
    Path(system_id): Path<String>,
    Json(secure_boot_request): Json<serde_json::Value>,
) -> Response {
    let Some(system_state) = state.system_state.find(&system_id) else {
        return http::not_found();
    };
    if let Some(v) = secure_boot_request
        .get("SecureBootEnable")
        .and_then(serde_json::Value::as_bool)
    {
        system_state.secure_boot_enabled.store(v, Ordering::Relaxed);
    }
    json!({}).into_ok_response()
}

async fn get_boot_options_collection(
    State(state): State<BmcState>,
    Path(system_id): Path<String>,
) -> Response {
    let Some(system_state) = state.system_state.find(&system_id) else {
        return http::not_found();
    };
    let Some(boot_options) = &system_state.config.boot_options else {
        return http::not_found();
    };
    let boot_options_order = match system_state.config.boot_order_mode {
        BootOrderMode::OrderedCollection => {
            // Some BMC clients infer the active first option from collection
            // order, so reflect a successfully applied BootOrder override.
            if let Some(boot_order) = system_state.boot_order_override() {
                let mut indices = (0..boot_options.len()).collect::<Vec<_>>();
                indices.sort_by_key(|&i| {
                    boot_order
                        .iter()
                        .enumerate()
                        .find(|(_, id)| *id == &boot_options[i].id)
                        .map(|(idx, _)| idx)
                        .unwrap_or(boot_options.len())
                });
                indices
            } else {
                (0..boot_options.len()).collect::<Vec<_>>()
            }
        }
        BootOrderMode::Generic | BootOrderMode::ViaSettings => (0..boot_options.len()).collect(),
    };
    let members = boot_options_order
        .into_iter()
        .map(|idx| redfish::boot_option::resource(&system_id, &boot_options[idx].id).entity_ref())
        .collect::<Vec<_>>();
    redfish::boot_option::collection(&system_id)
        .with_members(&members)
        .into_ok_response()
}

async fn get_boot_option(
    State(state): State<BmcState>,
    Path((system_id, boot_option_id)): Path<(String, String)>,
) -> Response {
    state
        .system_state
        .find(&system_id)
        .and_then(|system_state| system_state.boot_option(&boot_option_id))
        .map(JsonExt::into_ok_response)
        .unwrap_or_else(http::not_found)
}

async fn patch_boot_option_settings(
    State(state): State<BmcState>,
    Path((system_id, boot_option_id)): Path<(String, String)>,
    Json(patch_request): Json<serde_json::Value>,
) -> Response {
    let Some(system_state) = state.system_state.find(&system_id) else {
        return http::not_found();
    };
    if !system_state.patch_boot_option(&boot_option_id, patch_request) {
        return http::not_found();
    }
    json!({}).into_ok_response()
}

/// Return the HPE iLO persistent boot-order resource.
async fn get_hpe_boot(State(state): State<BmcState>, Path(system_id): Path<String>) -> Response {
    let Some(system_state) = state.system_state.find(&system_id) else {
        return http::not_found();
    };
    let boot_order = system_state.hpe_boot_order();
    hpe_boot_resource(&system_id)
        .json_patch()
        .patch(json!({
            "BootSources": [],
            "DefaultBootOrder": ["PcieSlotNic", "PcieSlotStorage"],
            "PersistentBootConfigOrder": boot_order,
        }))
        .into_ok_response()
}

/// Apply the HPE iLO persistent boot order staged through its settings resource.
async fn patch_hpe_boot_settings(
    State(state): State<BmcState>,
    Path(system_id): Path<String>,
    Json(request): Json<HpeBootSettingsPatch>,
) -> Response {
    let Some(system_state) = state.system_state.find(&system_id) else {
        return http::not_found();
    };
    system_state.set_hpe_boot_order(request.persistent_boot_config_order);
    json!({}).into_ok_response()
}

async fn get_log_services_collection(
    State(state): State<BmcState>,
    Path(system_id): Path<String>,
) -> Response {
    state
        .system_state
        .find(&system_id)
        .and_then(|system_state| system_state.config.log_services.as_ref())
        .map(|log_services| {
            let members = log_services
                .services()
                .into_iter()
                .map(|service| {
                    redfish::log_service::system_resource(&system_id, service.id()).entity_ref()
                })
                .collect::<Vec<_>>();
            redfish::boot_option::collection(&system_id)
                .with_members(&members)
                .into_ok_response()
        })
        .unwrap_or_else(http::not_found)
}

async fn get_log_service(
    State(state): State<BmcState>,
    Path((system_id, log_service_id)): Path<(String, String)>,
) -> Response {
    state
        .system_state
        .find(&system_id)
        .and_then(|system_state| system_state.config.log_services.as_ref())
        .and_then(|log_services| log_services.find(&log_service_id))
        .map(|_log_service| {
            redfish::log_service::builder(&redfish::log_service::system_resource(
                &system_id,
                &log_service_id,
            ))
            .entries(&redfish::log_service::system_entries_collection(
                &system_id,
                &log_service_id,
            ))
            .build()
            .into_ok_response()
        })
        .unwrap_or_else(http::not_found)
}

async fn get_log_service_entries(
    State(state): State<BmcState>,
    Path((system_id, log_service_id)): Path<(String, String)>,
) -> Response {
    state
        .system_state
        .find(&system_id)
        .and_then(|system_state| system_state.config.log_services.as_ref())
        .and_then(|log_services| log_services.find(&log_service_id))
        .map(|log_service| {
            let collection =
                redfish::log_service::system_entries_collection(&system_id, &log_service_id);
            let members = log_service.entries(&collection);
            collection
                .with_members(&members)
                .patch(json!({"Description": "Log services collection"})) // Required by libredfish
                .into_ok_response()
        })
        .unwrap_or_else(http::not_found)
}

async fn get_log_service_entry(
    State(state): State<BmcState>,
    Path((system_id, log_service_id, entry_id)): Path<(String, String, String)>,
) -> Response {
    state
        .system_state
        .find(&system_id)
        .and_then(|system_state| system_state.config.log_services.as_ref())
        .and_then(|log_services| log_services.find(&log_service_id))
        .and_then(|log_service| {
            let collection =
                redfish::log_service::system_entries_collection(&system_id, &log_service_id);
            log_service.entries(&collection).into_iter().find(|entry| {
                entry
                    .get("Id")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|id| id == entry_id)
            })
        })
        .map(|entry| entry.into_ok_response())
        .unwrap_or_else(http::not_found)
}

async fn get_storage_collection(
    State(state): State<BmcState>,
    Path(system_id): Path<String>,
) -> Response {
    state
        .system_state
        .find(&system_id)
        .and_then(|system_state| system_state.config.storage.as_ref())
        .map(|storage| {
            let members = storage
                .iter()
                .map(|storage| {
                    redfish::storage::system_resource(&system_id, &storage.id).entity_ref()
                })
                .collect::<Vec<_>>();
            redfish::boot_option::collection(&system_id)
                .with_members(&members)
                .into_ok_response()
        })
        .unwrap_or_else(http::not_found)
}

async fn get_processors_collection(
    State(state): State<BmcState>,
    Path(system_id): Path<String>,
) -> Response {
    state
        .system_state
        .find(&system_id)
        .and_then(|system_state| system_state.config.processors.as_ref())
        .map(|processors| {
            let members = processors
                .iter()
                .map(|processor| {
                    redfish::processor::system_resource(&system_id, &processor.id).entity_ref()
                })
                .collect::<Vec<_>>();
            redfish::processor::system_collection(&system_id)
                .with_members(&members)
                .into_ok_response()
        })
        .unwrap_or_else(http::not_found)
}

async fn get_processor(
    State(state): State<BmcState>,
    Path((system_id, processor_id)): Path<(String, String)>,
) -> Response {
    state
        .system_state
        .find(&system_id)
        .and_then(|system_state| system_state.find_processor(&processor_id))
        .map(|processor| processor.to_json().into_ok_response())
        .unwrap_or_else(http::not_found)
}

async fn get_processor_metrics(
    State(state): State<BmcState>,
    Path((system_id, processor_id)): Path<(String, String)>,
) -> Response {
    state
        .system_state
        .find(&system_id)
        .and_then(|system_state| system_state.find_processor(&processor_id))
        .map(|processor| processor.metrics_json().into_ok_response())
        .unwrap_or_else(http::not_found)
}

async fn get_memory_collection(
    State(state): State<BmcState>,
    Path(system_id): Path<String>,
) -> Response {
    state
        .system_state
        .find(&system_id)
        .and_then(|system_state| system_state.config.memory.as_ref())
        .map(|memory| {
            let members = memory
                .iter()
                .map(|memory| redfish::memory::system_resource(&system_id, &memory.id).entity_ref())
                .collect::<Vec<_>>();
            redfish::memory::system_collection(&system_id)
                .with_members(&members)
                .into_ok_response()
        })
        .unwrap_or_else(http::not_found)
}

async fn get_memory(
    State(state): State<BmcState>,
    Path((system_id, memory_id)): Path<(String, String)>,
) -> Response {
    state
        .system_state
        .find(&system_id)
        .and_then(|system_state| system_state.find_memory(&memory_id))
        .map(|memory| memory.to_json().into_ok_response())
        .unwrap_or_else(http::not_found)
}

async fn get_memory_metrics(
    State(state): State<BmcState>,
    Path((system_id, memory_id)): Path<(String, String)>,
) -> Response {
    state
        .system_state
        .find(&system_id)
        .and_then(|system_state| system_state.find_memory(&memory_id))
        .map(|memory| memory.metrics_json().into_ok_response())
        .unwrap_or_else(http::not_found)
}

async fn get_bios(State(state): State<BmcState>, Path(system_id): Path<String>) -> Response {
    state
        .system_state
        .find(&system_id)
        .and_then(|system_state| {
            system_state.config.base_bios.as_ref().map(|base_bios| {
                let overrides = system_state
                    .bios_overrides
                    .lock()
                    .expect("mutex is poisoned");
                base_bios
                    .clone()
                    .patch(overrides.clone())
                    .into_ok_response()
            })
        })
        .unwrap_or_else(http::not_found)
}

async fn patch_bios_settings(
    State(state): State<BmcState>,
    Path(system_id): Path<String>,
    Json(patch_bios_request): Json<serde_json::Value>,
) -> Response {
    let Some(system_state) = state.system_state.find(&system_id) else {
        return http::not_found();
    };
    match system_state.config.bios_mode {
        BiosMode::DellOem => {
            // Clear is transformed to Enabled state after reboot. Check if we
            // need to apply this logic here.
            const TPM2_HIERARCHY: &str = "Tpm2Hierarchy";
            const ATTRIBUTES: &str = "Attributes";
            let tpm2_clear_to_enabled = patch_bios_request
                .as_object()
                .and_then(|obj| obj.get(ATTRIBUTES))
                .and_then(|v| v.as_object())
                .and_then(|obj| obj.get(TPM2_HIERARCHY))
                .and_then(|v| v.as_str())
                .is_some_and(|v| v == "Clear");
            let patch_bios_request = if tpm2_clear_to_enabled {
                patch_bios_request.patch(json!({ATTRIBUTES: {
                    TPM2_HIERARCHY: "Enabled"
                }}))
            } else {
                patch_bios_request
            };
            json_patch(
                &mut system_state.bios_overrides.lock().expect("mutex poisoned"),
                patch_bios_request,
            );
            redfish::oem::dell::idrac::create_job_with_location(state)
        }
        BiosMode::Generic => {
            json_patch(
                &mut system_state.bios_overrides.lock().expect("mutex poisoned"),
                patch_bios_request,
            );
            http::ok_no_content()
        }
    }
}

async fn change_bios_password_action(Path(_system_id): Path<String>) -> Response {
    json!({}).into_ok_response()
}

fn builder(resource: &redfish::Resource) -> SystemBuilder {
    SystemBuilder {
        value: resource.json_patch(),
    }
}

struct SystemBuilder {
    value: serde_json::Value,
}

impl Builder for SystemBuilder {
    fn apply_patch(self, patch: serde_json::Value) -> Self {
        Self {
            value: self.value.patch(patch),
        }
    }
}

impl SystemBuilder {
    fn serial_console(self, value: &redfish::serial_console::SerialConsole) -> Self {
        self.apply_patch(json!({ "SerialConsole": value.to_json() }))
    }

    fn serial_number(self, v: &str) -> Self {
        self.add_str_field("SerialNumber", v)
    }

    fn manufacturer(self, v: &str) -> Self {
        self.add_str_field("Manufacturer", v)
    }

    fn model(self, v: &str) -> Self {
        self.add_str_field("Model", v)
    }

    fn bios_version(self, version: &str) -> Self {
        self.add_str_field("BiosVersion", version)
    }

    fn ethernet_interfaces(self, v: &redfish::Collection<'_>) -> Self {
        self.apply_patch(v.nav_property("EthernetInterfaces"))
    }

    fn boot_order(self, boot_order: &[&str]) -> Self {
        self.apply_patch(json!({"Boot": {"BootOrder": boot_order}}))
    }

    fn boot_options(self, boot_options: &redfish::Collection<'_>) -> Self {
        self.apply_patch(json!({"Boot": boot_options.nav_property("BootOptions")}))
    }

    fn boot_source_override(self, value: serde_json::Value) -> Self {
        self.apply_patch(json!({"Boot": value}))
    }

    fn virtual_media(self, value: &redfish::Collection<'_>) -> Self {
        self.apply_patch(value.nav_property("VirtualMedia"))
    }

    fn secure_boot(self, secure_boot: &redfish::Resource<'_>) -> Self {
        self.apply_patch(secure_boot.nav_property("SecureBoot"))
    }

    fn pcie_devices(self, devices: &[redfish::Resource<'_>]) -> Self {
        let devices = devices.iter().map(|r| r.entity_ref()).collect::<Vec<_>>();
        self.apply_patch(json!({"PCIeDevices": devices}))
    }

    fn bios(self, resource: &redfish::Resource<'_>) -> Self {
        self.apply_patch(resource.nav_property("Bios"))
    }

    fn power_state(self, state: MockPowerState) -> Self {
        let power_state = match state {
            MockPowerState::On => "On",
            MockPowerState::Off => "Off",
            MockPowerState::PowerCycling { since } => {
                if since.elapsed() < POWER_CYCLE_DELAY {
                    "Off"
                } else {
                    "On"
                }
            }
        };
        self.add_str_field("PowerState", power_state)
    }

    fn log_services(self, log_services: &redfish::Collection<'_>) -> Self {
        self.apply_patch(log_services.nav_property("LogServices"))
    }

    fn storage(self, storage: &redfish::Collection<'_>) -> Self {
        self.apply_patch(storage.nav_property("Storage"))
    }

    fn processors(self, processors: &redfish::Collection<'_>) -> Self {
        self.apply_patch(processors.nav_property("Processors"))
    }

    fn memory(self, memory: &redfish::Collection<'_>) -> Self {
        self.apply_patch(memory.nav_property("Memory"))
    }

    fn link_chassis(self, ids: &[Cow<'static, str>]) -> Self {
        let chassis = ids
            .iter()
            .map(|id| redfish::chassis::resource(id).entity_ref())
            .collect::<Vec<_>>();
        self.apply_patch(json!({"Links": {"Chassis": chassis}}))
    }

    fn oem_nvidia(self, resource: &redfish::Resource<'_>) -> Self {
        self.apply_patch(json!({"Oem": {"Nvidia": resource.entity_ref()}}))
    }

    fn build(self) -> serde_json::Value {
        self.value
    }
}

#[cfg(test)]
mod tests {
    use axum::Router;
    use axum::body::{Body, to_bytes};
    use axum::http::header::CONTENT_TYPE;
    use axum::http::{Method, Request, StatusCode};
    use tower::ServiceExt;
    use tower_http::normalize_path::NormalizePathLayer;

    use super::*;
    use crate::test_support::{NoopCallbacks, host_info};
    use crate::{HardwareType, MachineRouterOptions, machine_router};

    /// Reads one successful JSON response from the in-process mock router.
    async fn get_json(router: &Router, path: &str) -> serde_json::Value {
        let response = router
            .clone()
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    /// HPE OEM ordering round-trips without corrupting standard BootOption IDs.
    #[tokio::test]
    async fn hpe_boot_order_is_persisted_separately_from_standard_boot_order() {
        let router = machine_router(
            &host_info(HardwareType::HpeProliantDl380aGen11),
            Arc::new(NoopCallbacks),
            "test-host-id".to_string(),
            false,
            MachineRouterOptions::default(),
        )
        .0
        .layer(NormalizePathLayer::trim_trailing_slash());
        let boot_path = hpe_boot_resource("1").odata_id;
        let initial = get_json(&router, &boot_path).await;
        assert_eq!(
            initial["PersistentBootConfigOrder"],
            json!(["NIC.BootOption.Boot0000", "HD.BootOption.Boot0001",])
        );

        let updated_order = json!(["HD.BootOption.Boot0001", "NIC.BootOption.Boot0000",]);
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::PATCH)
                    .uri(format!("{boot_path}settings/"))
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({"PersistentBootConfigOrder": updated_order}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let updated = get_json(&router, &boot_path).await;
        assert_eq!(updated["PersistentBootConfigOrder"], updated_order);
        let system = get_json(&router, &resource("1").odata_id).await;
        assert_eq!(system["Boot"]["BootOrder"], json!(["Boot0000", "Boot0001"]));
    }

    #[tokio::test]
    async fn simulated_ssh_port_can_be_added_without_profile_serial_console_data() {
        let (router, state) = machine_router(
            &host_info(HardwareType::LenovoGB300Nvl),
            Arc::new(NoopCallbacks),
            "test-host-id".to_string(),
            false,
            MachineRouterOptions::default(),
        );
        let router = router.layer(NormalizePathLayer::trim_trailing_slash());

        assert!(!state.has_enabled_ssh_serial_console());
        assert!(!state.set_serial_console_ssh_port(Some(3222)));
        assert!(state.set_simulated_serial_console_ssh_port(Some(3222)));

        for system_id in ["HGX_Baseboard_0", "System_0"] {
            let system = get_json(&router, &resource(system_id).odata_id).await;
            assert_eq!(system["SerialConsole"]["SSH"]["ServiceEnabled"], true);
            assert_eq!(system["SerialConsole"]["SSH"]["Port"], 3222);
        }
    }
}

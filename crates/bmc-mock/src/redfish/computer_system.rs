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
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};

use axum::Router;
use axum::extract::{Json, Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post};
use nv_redfish::schema::resource::PowerState;
use serde_json::json;

use crate::bmc_state::BmcState;
use crate::json::{JsonExt, json_patch};
use crate::redfish::ModelExt;
use crate::{ActionError, BootOptionKind, Callbacks, MachineRouterOptions, http, redfish};

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
        odata_type: Cow::Borrowed(redfish::schema::computer_system::ComputerSystem::ODATA_TYPE),
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
        odata_type: Cow::Borrowed(
            redfish::schema::hpe_server_boot_settings::HpeServerBootSettings::ODATA_TYPE,
        ),
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

pub(crate) fn add_routes<C: Callbacks + 'static>(
    r: Router<BmcState<C>>,
    bmc_vendor: redfish::oem::BmcVendor,
) -> Router<BmcState<C>> {
    const SYSTEM_ID: &str = "{system_id}";
    const ETH_ID: &str = "{eth_id}";
    const BOOT_OPTION_ID: &str = "{boot_option_id}";
    const LOG_SERVICE_ID: &str = "{log_service_id}";
    const LOG_ENTRY_ID: &str = "{log_entry_id}";
    const PROCESSOR_ID: &str = "{processor_id}";
    const MEMORY_ID: &str = "{memory_id}";
    let bios = redfish::bios::resource(SYSTEM_ID);
    let routes = r
        .route(&collection().odata_id, get(get_system_collection::<C>))
        .route(
            &resource(SYSTEM_ID).odata_id,
            get(get_system::<C>).patch(patch_system::<C>),
        )
        .route(&reset_target(SYSTEM_ID), post(post_reset_system::<C>))
        .route(
            &bmc_vendor.make_settings_odata_id(&resource(SYSTEM_ID)),
            patch(patch_settings::<C>),
        )
        .route(
            &redfish::ethernet_interface::system_resource(SYSTEM_ID, ETH_ID).odata_id,
            get(get_ethernet_interface::<C>),
        )
        .route(
            &redfish::ethernet_interface::system_collection(SYSTEM_ID).odata_id,
            get(get_ethernet_interface_collection::<C>),
        )
        .route(
            &redfish::secure_boot::resource(SYSTEM_ID).odata_id,
            get(get_secure_boot::<C>).patch(patch_secure_boot::<C>),
        )
        .route(
            &redfish::boot_option::collection(SYSTEM_ID).odata_id,
            get(get_boot_options_collection::<C>),
        )
        .route(
            &redfish::boot_option::resource(SYSTEM_ID, BOOT_OPTION_ID).odata_id,
            get(get_boot_option::<C>),
        )
        .route(
            &bmc_vendor
                .make_settings_odata_id(&redfish::boot_option::resource(SYSTEM_ID, BOOT_OPTION_ID)),
            patch(patch_boot_option_settings::<C>),
        )
        .route(
            &bios.odata_id,
            get(get_bios::<C>).patch(patch_bios_settings::<C>),
        )
        .route(
            &redfish::log_service::system_collection(SYSTEM_ID).odata_id,
            get(get_log_services_collection::<C>),
        )
        .route(
            &redfish::log_service::system_resource(SYSTEM_ID, LOG_SERVICE_ID).odata_id,
            get(get_log_service::<C>),
        )
        .route(
            &redfish::log_service::system_clear_log_target(SYSTEM_ID, LOG_SERVICE_ID),
            post(post_clear_log::<C>),
        )
        .route(
            &redfish::log_service::system_entries_collection(SYSTEM_ID, LOG_SERVICE_ID).odata_id,
            get(get_log_service_entries::<C>),
        )
        .route(
            &format!(
                "{}/{}",
                redfish::log_service::system_entries_collection(SYSTEM_ID, LOG_SERVICE_ID).odata_id,
                LOG_ENTRY_ID
            ),
            get(get_log_service_entry::<C>),
        )
        .route(
            &redfish::storage::system_collection(SYSTEM_ID).odata_id,
            get(get_storage_collection::<C>),
        )
        .route(
            &redfish::processor::system_collection(SYSTEM_ID).odata_id,
            get(get_processors_collection::<C>),
        )
        .route(
            &redfish::processor::system_resource(SYSTEM_ID, PROCESSOR_ID).odata_id,
            get(get_processor::<C>),
        )
        .route(
            &redfish::processor::metrics_resource(SYSTEM_ID, PROCESSOR_ID).odata_id,
            get(get_processor_metrics::<C>),
        )
        .route(
            &redfish::memory::system_collection(SYSTEM_ID).odata_id,
            get(get_memory_collection::<C>),
        )
        .route(
            &redfish::memory::system_resource(SYSTEM_ID, MEMORY_ID).odata_id,
            get(get_memory::<C>),
        )
        .route(
            &redfish::memory::metrics_resource(SYSTEM_ID, MEMORY_ID).odata_id,
            get(get_memory_metrics::<C>),
        )
        .route(
            &bmc_vendor.make_settings_odata_id(&bios),
            patch(patch_bios_settings::<C>),
        )
        .route(
            &redfish::bios::change_password_target(&bios),
            post(change_bios_password_action::<C>),
        )
        .route(
            &redfish::bios::reset_target(&bios),
            post(reset_bios_action::<C>),
        );
    if matches!(bmc_vendor, redfish::oem::BmcVendor::Hpe) {
        let hpe_boot = hpe_boot_resource(SYSTEM_ID);
        // CombinedServer normalizes requests by removing trailing slashes before
        // routing them, while the Redfish resource still advertises canonical
        // trailing-slash OData identifiers.
        let hpe_boot_path = hpe_boot.odata_id.trim_end_matches('/');
        routes.route(hpe_boot_path, get(get_hpe_boot::<C>)).route(
            &format!("{hpe_boot_path}/settings"),
            patch(patch_hpe_boot_settings::<C>),
        )
    } else {
        routes
    }
}

pub(crate) struct SingleSystemConfig<C: Callbacks> {
    pub(crate) id: Cow<'static, str>,
    pub(crate) eth_interfaces: Option<Vec<redfish::ethernet_interface::EthernetInterface>>,
    pub(crate) serial_number: Option<Cow<'static, str>>,
    pub(crate) manufacturer: Option<Cow<'static, str>>,
    pub(crate) model: Option<Cow<'static, str>>,
    pub(crate) bios_version: Option<Cow<'static, str>>,
    pub(crate) boot_order_mode: BootOrderMode,
    pub(crate) callbacks: Option<Arc<C>>,
    pub(crate) chassis: Vec<Cow<'static, str>>,
    pub(crate) boot_options: Option<Vec<redfish::boot_option::BootOption>>,
    pub(crate) bios_mode: BiosMode,
    pub(crate) base_bios: Option<serde_json::Value>,
    pub(crate) log_services: Option<redfish::log_service::LogServices>,
    pub(crate) storage: Option<Vec<redfish::storage::Storage>>,
    pub(crate) processors: Option<Vec<redfish::processor::Processor>>,
    pub(crate) memory: Option<Vec<redfish::memory::Memory>>,
    pub(crate) secure_boot_available: bool,
    pub(crate) serial_console: Option<redfish::serial_console::SerialConsole>,
    pub(crate) oem: Oem,
}

pub(crate) struct Config<C: Callbacks> {
    pub(crate) systems: Vec<SingleSystemConfig<C>>,
}

pub struct SystemState<C: Callbacks> {
    systems: Vec<SingleSystemState<C>>,
}

pub(crate) type UefiPasswords = BTreeMap<String, String>;

#[derive(Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct BootSourceOverride {
    mode: Option<String>,
    enabled: Option<String>,
    target: Option<String>,
}

pub(crate) struct SingleSystemState<C: Callbacks> {
    config: SingleSystemConfig<C>,
    power_state: Mutex<Option<PowerState>>,
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
    /// BIOS setup passwords by `PasswordName`. A name that is absent has no password set.
    uefi_passwords: Arc<Mutex<UefiPasswords>>,
}

#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct PersistedSystemState {
    bios_overrides: serde_json::Value,
    boot_order: Option<Vec<String>>,
    hpe_boot_order: Option<Vec<String>>,
    boot_options: HashMap<String, serde_json::Value>,
    boot_source: BootSourceOverride,
    secure_boot_enabled: bool,
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

impl<C: Callbacks> SystemState<C> {
    pub(crate) fn persisted(&self) -> std::collections::BTreeMap<String, PersistedSystemState> {
        self.systems
            .iter()
            .map(|system| {
                // Boot resolution takes these locks in a different order. Release
                // each guard before reading the next field.
                let bios_overrides = system
                    .bios_overrides
                    .lock()
                    .expect("mutex poisoned")
                    .clone();
                let boot_order = system
                    .boot_order_override
                    .lock()
                    .expect("mutex poisoned")
                    .clone();
                let hpe_boot_order = system
                    .hpe_boot_order_override
                    .lock()
                    .expect("mutex poisoned")
                    .clone();
                let boot_options = system
                    .boot_option_overrides
                    .lock()
                    .expect("mutex poisoned")
                    .clone();
                let boot_source = system
                    .boot_source_override
                    .lock()
                    .expect("mutex poisoned")
                    .clone();
                (
                    system.config.id.to_string(),
                    PersistedSystemState {
                        bios_overrides,
                        boot_order,
                        hpe_boot_order,
                        boot_options,
                        boot_source,
                        secure_boot_enabled: system.secure_boot_enabled.load(Ordering::SeqCst),
                    },
                )
            })
            .collect()
    }

    pub(crate) fn validate_persisted(
        &self,
        saved: &std::collections::BTreeMap<String, PersistedSystemState>,
    ) -> bool {
        saved.len() == self.systems.len()
            && saved
                .iter()
                .all(|(id, state)| self.find(id).is_some() && state.bios_overrides.is_object())
    }

    pub(crate) fn restore_persisted(
        &self,
        saved: &std::collections::BTreeMap<String, PersistedSystemState>,
    ) {
        for (id, state) in saved {
            let system = self.find(id).expect("validated system identity");
            *system.bios_overrides.lock().expect("mutex poisoned") = state.bios_overrides.clone();
            *system.boot_order_override.lock().expect("mutex poisoned") = state.boot_order.clone();
            *system
                .hpe_boot_order_override
                .lock()
                .expect("mutex poisoned") = state.hpe_boot_order.clone();
            *system.boot_option_overrides.lock().expect("mutex poisoned") =
                state.boot_options.clone();
            *system.boot_source_override.lock().expect("mutex poisoned") =
                state.boot_source.clone();
            system
                .secure_boot_enabled
                .store(state.secure_boot_enabled, Ordering::SeqCst);
        }
    }

    pub(crate) fn power_state(&self) -> Option<PowerState> {
        *self
            .controlled_system()?
            .power_state
            .lock()
            .expect("power state lock poisoned")
    }

    /// Updates the power observation exposed for the controlled system.
    /// `PowerState` is omitted from Redfish responses until an observation is published.
    /// Unrecognized observations (`UnsupportedValue`) are reported as JSON null.
    /// Returns false when this BMC has no controlled system.
    pub fn set_power_state(&self, power_state: PowerState) -> bool {
        let Some(system) = self.controlled_system() else {
            return false;
        };
        *system
            .power_state
            .lock()
            .expect("power state lock poisoned") = Some(power_state);
        true
    }

    pub(crate) fn from_config(config: Config<C>, options: &MachineRouterOptions) -> Self {
        Self::from_configs(config.systems, options.virtual_media_devices.clone())
    }

    pub(crate) fn systems(&self) -> &[SingleSystemState<C>] {
        &self.systems
    }

    pub(crate) fn find(&self, system_id: &str) -> Option<&SingleSystemState<C>> {
        self.systems
            .iter()
            .find(|system| system.config.id.as_ref() == system_id)
    }

    pub(crate) fn effect_snapshots(
        &self,
        updates: &redfish::update_service::UpdateServiceState,
    ) -> HashMap<String, crate::persistence::BmcSystemEffectSnapshot> {
        self.systems
            .iter()
            .map(|system| {
                (
                    system.config.id.to_string(),
                    crate::persistence::BmcSystemEffectSnapshot {
                        bios_attributes: system
                            .bios_json()
                            .and_then(|bios| bios.get("Attributes").cloned()),
                        uefi_passwords_set: system
                            .uefi_passwords
                            .lock()
                            .expect("mutex poisoned")
                            .values()
                            .any(|password| !password.is_empty()),
                        boot_progress: updates.dpu_boot_progress(system.powered_on()),
                    },
                )
            })
            .collect()
    }

    pub(crate) fn uefi_password_sources(
        &self,
    ) -> impl Iterator<Item = (&str, Weak<Mutex<UefiPasswords>>)> {
        self.systems.iter().map(|system| {
            (
                system.config.id.as_ref(),
                Arc::downgrade(&system.uefi_passwords),
            )
        })
    }

    pub(crate) fn restore_uefi_passwords(
        &self,
        passwords: &BTreeMap<String, UefiPasswords>,
    ) -> Result<(), &'static str> {
        if passwords.keys().any(|id| self.find(id).is_none()) {
            return Err("UEFI password snapshot references an unknown system");
        }
        for system in &self.systems {
            *system.uefi_passwords.lock().expect("mutex poisoned") = passwords
                .get(system.config.id.as_ref())
                .cloned()
                .unwrap_or_default();
        }
        Ok(())
    }

    /// `@odata.id` of the controllable host system: the one wired to power
    /// callbacks, else the first system.
    pub(crate) fn primary_system_odata_id(&self) -> Option<String> {
        self.systems
            .iter()
            .find(|system| system.config.callbacks.is_some())
            .or(self.systems.first())
            .map(|system| resource(&system.config.id).odata_id.into_owned())
    }

    /// Append a lifecycle entry to the first system event log. Returns the new
    /// LogEntry's `@odata.id`, or `None` when no system has a log service.
    pub(crate) fn record_log(
        &self,
        draft: redfish::log_service::LogEntryDraft,
        created: &str,
    ) -> Option<String> {
        self.systems.iter().find_map(|system| {
            let log = system.config.log_services.as_ref()?.primary()?;
            let id = log.append(draft.clone(), created);
            let collection =
                redfish::log_service::system_entries_collection(&system.config.id, log.id());
            Some(format!("{}/{id}", collection.odata_id))
        })
    }

    fn from_configs(
        configs: Vec<SingleSystemConfig<C>>,
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

    pub(crate) fn controlled_system(&self) -> Option<&SingleSystemState<C>> {
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

impl<C: Callbacks> SingleSystemState<C> {
    fn new(
        config: SingleSystemConfig<C>,
        virtual_media: Option<redfish::virtual_media::VirtualMediaState>,
    ) -> Self {
        Self {
            config,
            power_state: Mutex::new(None),
            virtual_media,
            serial_console_ssh_port_override: Mutex::new(None),
            boot_order_override: Mutex::new(None),
            hpe_boot_order_override: Mutex::new(None),
            boot_option_overrides: Mutex::new(HashMap::new()),
            boot_source_override: Mutex::new(BootSourceOverride::default()),
            secure_boot_enabled: Arc::new(AtomicBool::new(false)),
            bios_overrides: Arc::new(Mutex::new(serde_json::json!({}))),
            uefi_passwords: Arc::default(),
        }
    }

    fn bios_json(&self) -> Option<serde_json::Value> {
        self.config.base_bios.as_ref().map(|base_bios| {
            let overrides = self.bios_overrides.lock().expect("mutex is poisoned");
            base_bios
                .clone()
                .patch(redfish::bios::actions(&redfish::bios::resource(
                    &self.config.id,
                )))
                .patch(overrides.clone())
        })
    }

    fn powered_on(&self) -> bool {
        self.power_state
            .lock()
            .expect("power state lock poisoned")
            .is_none_or(|power_state| power_state == PowerState::On)
    }

    /// Applies a BIOS setup password change the way the BIOS does: `old` must match the stored
    /// password (an unset password reads as empty) and an empty `new` clears it.
    /// Returns false when the change is refused.
    fn change_uefi_password(&self, name: &str, old: &str, new: &str) -> bool {
        let mut passwords = self.uefi_passwords.lock().expect("mutex poisoned");
        let current = passwords.get(name).map_or("", String::as_str);
        // NICo clears with the credential it recorded when it set the password, so an unset
        // password is only ever cleared with an empty old value, like any other change.
        if current != old {
            return false;
        }
        if new.is_empty() {
            passwords.remove(name);
        } else {
            passwords.insert(name.to_owned(), new.to_owned());
        }
        true
    }

    /// Drops every BIOS attribute change so the hardware profile's attributes show again.
    /// Setup passwords survive a BIOS reset and are cleared by their own action.
    fn reset_bios(&self) {
        *self.bios_overrides.lock().expect("mutex poisoned") = json!({});
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

    /// Resolve the first configured HPE OEM boot entry, then standard
    /// BootOrder, then the profile default. Unknown or unconfigured HPE entries
    /// are skipped. Returns None when no boot option is configured; callers
    /// leave domain configuration unchanged in that case. Temporary overrides
    /// are excluded.
    pub(crate) fn resolve_persistent_boot_selection(&self) -> Option<BootOptionKind> {
        self.hpe_boot_order_override
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|order| {
                order.iter().find_map(|entry| {
                    let (kind, reference) = entry
                        .strip_prefix("HD.BootOption.")
                        .map(|reference| (BootOptionKind::Disk, reference))
                        .or_else(|| {
                            entry
                                .strip_prefix("NIC.BootOption.")
                                .map(|reference| (BootOptionKind::Network, reference))
                        })?;
                    self.config
                        .boot_options
                        .iter()
                        .flatten()
                        .find(|option| option.kind == kind && option.boot_reference() == reference)
                        .map(|option| option.kind)
                })
            })
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
        .or_else(|| self.resolve_persistent_boot_selection())
    }
}

async fn get_system_collection<C: Callbacks>(State(state): State<BmcState<C>>) -> Response {
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

async fn get_system<C: Callbacks>(
    State(state): State<BmcState<C>>,
    Path(system_id): Path<String>,
) -> Response {
    use super::schema::computer_system::{Boot, ComputerSystem, Links};
    use super::schema::resource::PowerState as ResponsePowerState;
    let Some(system_state) = state.system_state.find(&system_id) else {
        return http::not_found();
    };
    let config = &system_state.config;
    let resource = resource(&system_id);
    let power_state = *system_state
        .power_state
        .lock()
        .expect("power state lock poisoned");
    let boot = config.boot_options.as_ref().map(|options| {
        let order = system_state.boot_order_override().unwrap_or_else(|| {
            options
                .iter()
                .map(|v| v.boot_reference().to_string())
                .collect()
        });
        Boot::new().boot_order(order).boot_options(
            redfish::boot_option::collection(&system_id)
                .odata_id
                .as_ref(),
        )
    });
    let pcie_devices = config
        .chassis
        .iter()
        .flat_map(|id| state.chassis_state.find(id))
        .flat_map(|chassis| chassis.pcie_devices_resources())
        .map(|v| v.odata_id.into_owned())
        .collect::<Vec<_>>();
    let model = ComputerSystem {
        serial_number: config.serial_number.as_ref().map(|v| Some(v.to_string())),
        manufacturer: config.manufacturer.as_ref().map(|v| Some(v.to_string())),
        model: config.model.as_ref().map(|v| Some(v.to_string())),
        bios_version: config.bios_version.as_ref().map(|v| Some(v.to_string())),
        power_state: power_state.map(|v| match v {
            PowerState::On => Some(ResponsePowerState::On),
            PowerState::Off => Some(ResponsePowerState::Off),
            PowerState::PoweringOn => Some(ResponsePowerState::PoweringOn),
            PowerState::PoweringOff => Some(ResponsePowerState::PoweringOff),
            PowerState::Paused => Some(ResponsePowerState::Paused),
            PowerState::Sleeping => Some(ResponsePowerState::Sleeping),
            PowerState::Hibernating => Some(ResponsePowerState::Hibernating),
            PowerState::UnsupportedValue => None,
        }),
        bios: config.base_bios.as_ref().map(|_| {
            redfish::bios::resource(&system_id)
                .odata_id
                .into_owned()
                .into()
        }),
        ethernet_interfaces: config.eth_interfaces.as_ref().map(|_| {
            redfish::ethernet_interface::system_collection(&system_id)
                .odata_id
                .into_owned()
                .into()
        }),
        log_services: config.log_services.as_ref().map(|_| {
            redfish::log_service::system_collection(&system_id)
                .odata_id
                .into_owned()
                .into()
        }),
        storage: config.storage.as_ref().map(|_| {
            redfish::storage::system_collection(&system_id)
                .odata_id
                .into_owned()
                .into()
        }),
        processors: config.processors.as_ref().map(|_| {
            redfish::processor::system_collection(&system_id)
                .odata_id
                .into_owned()
                .into()
        }),
        memory: config.memory.as_ref().map(|_| {
            redfish::memory::system_collection(&system_id)
                .odata_id
                .into_owned()
                .into()
        }),
        secure_boot: config.secure_boot_available.then(|| {
            redfish::secure_boot::resource(&system_id)
                .odata_id
                .into_owned()
                .into()
        }),
        virtual_media: system_state.virtual_media().map(|_| {
            redfish::virtual_media::collection(&system_id)
                .odata_id
                .into_owned()
                .into()
        }),
        boot,
        ..ComputerSystem::new(
            resource.odata_id.as_ref(),
            resource.id.as_ref(),
            resource.name.as_ref(),
        )
    }
    .links(
        Links::new().chassis(
            config
                .chassis
                .iter()
                .map(|id| redfish::chassis::resource(id).odata_id.into_owned()),
        ),
    )
    .pcie_devices(pcie_devices);
    let mut document = model.into_json();
    if let Some(boot_progress) = state
        .update_service_state
        .dpu_boot_progress(power_state.is_none_or(|power_state| power_state == PowerState::On))
    {
        document = document.patch(json!({"BootProgress": boot_progress}));
    }
    // Bound actions are not emitted by the server schema generator yet.
    if config.callbacks.is_some() {
        document = document.patch(json!({"Actions": {"#ComputerSystem.Reset": {
            "target": reset_target(&system_id),
            "ResetType@Redfish.AllowableValues": ["On", "ForceOn", "GracefulShutdown", "ForceOff", "GracefulRestart", "ForceRestart", "PowerCycle"]
        }}}));
    }
    // This state is validated by the PATCH handler and can include vendor boot settings.
    let boot_source_override = system_state.boot_source_override();
    if boot_source_override
        .as_object()
        .is_some_and(|v| !v.is_empty())
    {
        document = document.patch(json!({"Boot": boot_source_override}));
    }
    if matches!(config.oem, Oem::NvidiaBluefield) {
        document = document.patch(json!({"Oem": {"Nvidia": redfish::oem::nvidia::bluefield::resource(&system_id).entity_ref()}}));
    }
    let port = *system_state
        .serial_console_ssh_port_override
        .lock()
        .expect("mutex poisoned");
    let serial_console = match (&config.serial_console, port) {
        (Some(console), Some(port)) => Some(console.with_ssh_port(port)),
        (Some(console), None) => Some(console.clone()),
        (None, Some(port)) => Some(redfish::serial_console::simulated_ssh(port)),
        (None, None) => None,
    };
    if let Some(console) = serial_console {
        document = document.patch(json!({"SerialConsole": console.to_json()}));
    }
    document.into_ok_response()
}

async fn get_ethernet_interface<C: Callbacks>(
    State(state): State<BmcState<C>>,
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

async fn get_ethernet_interface_collection<C: Callbacks>(
    State(state): State<BmcState<C>>,
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

async fn patch_settings<C: Callbacks>(
    State(state): State<BmcState<C>>,
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
    if matches!(state.bmc_vendor, redfish::oem::BmcVendor::Ami) {
        http::ok_no_content()
    } else {
        json!({}).into_ok_response()
    }
}

async fn patch_system<C: Callbacks>(
    State(state): State<BmcState<C>>,
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

async fn post_reset_system<C: Callbacks>(
    State(state): State<BmcState<C>>,
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
    match callbacks.computer_system_reset(reset_type).await {
        Ok(_) => {
            state.record_event(redfish::log_service::LogEntryDraft::reset_requested(
                &resource(&system_id).odata_id,
                reset_type,
            ));
            json!({}).into_ok_response()
        }
        Err(ActionError::BadRequest(_)) => StatusCode::BAD_REQUEST.into_response(),
        Err(ActionError::Internal(_)) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

async fn get_secure_boot<C: Callbacks>(
    State(state): State<BmcState<C>>,
    Path(system_id): Path<String>,
) -> Response {
    let Some(system_state) = state.system_state.find(&system_id) else {
        return http::not_found();
    };
    let secure_boot_enabled = system_state.secure_boot_enabled.load(Ordering::Relaxed);
    redfish::secure_boot::builder(&redfish::secure_boot::resource(&system_id))
        .secure_boot_enable(secure_boot_enabled)
        .secure_boot_current_boot(if secure_boot_enabled {
            redfish::schema::secure_boot::SecureBootCurrentBootType::Enabled
        } else {
            redfish::schema::secure_boot::SecureBootCurrentBootType::Disabled
        })
        .into_json()
        .into_ok_response()
}

async fn patch_secure_boot<C: Callbacks>(
    State(state): State<BmcState<C>>,
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

async fn get_boot_options_collection<C: Callbacks>(
    State(state): State<BmcState<C>>,
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

async fn get_boot_option<C: Callbacks>(
    State(state): State<BmcState<C>>,
    Path((system_id, boot_option_id)): Path<(String, String)>,
) -> Response {
    state
        .system_state
        .find(&system_id)
        .and_then(|system_state| system_state.boot_option(&boot_option_id))
        .map(JsonExt::into_ok_response)
        .unwrap_or_else(http::not_found)
}

async fn patch_boot_option_settings<C: Callbacks>(
    State(state): State<BmcState<C>>,
    Path((system_id, boot_option_id)): Path<(String, String)>,
    Json(patch_request): Json<serde_json::Value>,
) -> Response {
    let Some(system_state) = state.system_state.find(&system_id) else {
        return http::not_found();
    };
    if !system_state.patch_boot_option(&boot_option_id, patch_request) {
        return http::not_found();
    }
    if matches!(state.bmc_vendor, redfish::oem::BmcVendor::Ami) {
        http::ok_no_content()
    } else {
        json!({}).into_ok_response()
    }
}

/// Return the HPE iLO persistent boot-order resource.
async fn get_hpe_boot<C: Callbacks>(
    State(state): State<BmcState<C>>,
    Path(system_id): Path<String>,
) -> Response {
    let Some(system_state) = state.system_state.find(&system_id) else {
        return http::not_found();
    };
    let boot_order = system_state.hpe_boot_order();
    let resource = hpe_boot_resource(&system_id);
    super::schema::hpe_server_boot_settings::HpeServerBootSettings::new(
        resource.odata_id.as_ref(),
        resource.id.as_ref(),
        resource.name.as_ref(),
    )
    .boot_sources([])
    .default_boot_order(["PcieSlotNic", "PcieSlotStorage"])
    .persistent_boot_config_order(boot_order)
    .into_json()
    .into_ok_response()
}

/// Apply the HPE iLO persistent boot order staged through its settings resource.
async fn patch_hpe_boot_settings<C: Callbacks>(
    State(state): State<BmcState<C>>,
    Path(system_id): Path<String>,
    Json(request): Json<HpeBootSettingsPatch>,
) -> Response {
    let Some(system_state) = state.system_state.find(&system_id) else {
        return http::not_found();
    };
    system_state.set_hpe_boot_order(request.persistent_boot_config_order);
    json!({}).into_ok_response()
}

async fn get_log_services_collection<C: Callbacks>(
    State(state): State<BmcState<C>>,
    Path(system_id): Path<String>,
) -> Response {
    state
        .system_state
        .find(&system_id)
        .and_then(|system_state| system_state.config.log_services.as_ref())
        .map(|log_services| {
            let members = log_services
                .services()
                .iter()
                .map(|service| {
                    redfish::log_service::system_resource(&system_id, service.id()).entity_ref()
                })
                .collect::<Vec<_>>();
            redfish::log_service::system_collection(&system_id)
                .with_members(&members)
                .into_ok_response()
        })
        .unwrap_or_else(http::not_found)
}

async fn get_log_service<C: Callbacks>(
    State(state): State<BmcState<C>>,
    Path((system_id, log_service_id)): Path<(String, String)>,
) -> Response {
    state
        .system_state
        .find(&system_id)
        .and_then(|system_state| system_state.config.log_services.as_ref())
        .and_then(|log_services| log_services.find(&log_service_id))
        .map(|log_service| {
            redfish::log_service::builder(&redfish::log_service::system_resource(
                &system_id,
                &log_service_id,
            ))
            .entries(
                redfish::log_service::system_entries_collection(&system_id, &log_service_id)
                    .odata_id
                    .as_ref(),
            )
            .capacity(
                log_service.capacity(),
                &redfish::log_service::system_clear_log_target(&system_id, &log_service_id),
            )
            .into_ok_response()
        })
        .unwrap_or_else(http::not_found)
}

async fn post_clear_log<C: Callbacks>(
    State(state): State<BmcState<C>>,
    Path((system_id, log_service_id)): Path<(String, String)>,
) -> Response {
    state
        .system_state
        .find(&system_id)
        .and_then(|system_state| system_state.config.log_services.as_ref())
        .and_then(|log_services| log_services.find(&log_service_id))
        .map(|log_service| {
            log_service.clear();
            http::ok_no_content()
        })
        .unwrap_or_else(http::not_found)
}

async fn get_log_service_entries<C: Callbacks>(
    State(state): State<BmcState<C>>,
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
            let mut response = collection
                .with_members(&log_service.entries(&collection))
                .patch(json!({
                    "Description": "Log services collection", // Required by libredfish
                }))
                .into_ok_response();
            if let Some(page_size) = log_service.page_size() {
                response
                    .extensions_mut()
                    .insert(redfish::query_router::PageSize(page_size));
            }
            response
        })
        .unwrap_or_else(http::not_found)
}

async fn get_log_service_entry<C: Callbacks>(
    State(state): State<BmcState<C>>,
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
            log_service.entry(&collection, &entry_id)
        })
        .map(|entry| entry.into_ok_response())
        .unwrap_or_else(http::not_found)
}

async fn get_storage_collection<C: Callbacks>(
    State(state): State<BmcState<C>>,
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
            redfish::storage::system_collection(&system_id)
                .with_members(&members)
                .into_ok_response()
        })
        .unwrap_or_else(http::not_found)
}

async fn get_processors_collection<C: Callbacks>(
    State(state): State<BmcState<C>>,
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

async fn get_processor<C: Callbacks>(
    State(state): State<BmcState<C>>,
    Path((system_id, processor_id)): Path<(String, String)>,
) -> Response {
    state
        .system_state
        .find(&system_id)
        .and_then(|system_state| system_state.find_processor(&processor_id))
        .map(|processor| processor.to_json().into_ok_response())
        .unwrap_or_else(http::not_found)
}

async fn get_processor_metrics<C: Callbacks>(
    State(state): State<BmcState<C>>,
    Path((system_id, processor_id)): Path<(String, String)>,
) -> Response {
    state
        .system_state
        .find(&system_id)
        .and_then(|system_state| system_state.find_processor(&processor_id))
        .map(|processor| processor.metrics_json().into_ok_response())
        .unwrap_or_else(http::not_found)
}

async fn get_memory_collection<C: Callbacks>(
    State(state): State<BmcState<C>>,
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

async fn get_memory<C: Callbacks>(
    State(state): State<BmcState<C>>,
    Path((system_id, memory_id)): Path<(String, String)>,
) -> Response {
    state
        .system_state
        .find(&system_id)
        .and_then(|system_state| system_state.find_memory(&memory_id))
        .map(|memory| memory.to_json().into_ok_response())
        .unwrap_or_else(http::not_found)
}

async fn get_memory_metrics<C: Callbacks>(
    State(state): State<BmcState<C>>,
    Path((system_id, memory_id)): Path<(String, String)>,
) -> Response {
    state
        .system_state
        .find(&system_id)
        .and_then(|system_state| system_state.find_memory(&memory_id))
        .map(|memory| memory.metrics_json().into_ok_response())
        .unwrap_or_else(http::not_found)
}

async fn get_bios<C: Callbacks>(
    State(state): State<BmcState<C>>,
    Path(system_id): Path<String>,
) -> Response {
    state
        .system_state
        .find(&system_id)
        .and_then(SingleSystemState::bios_json)
        .map(JsonExt::into_ok_response)
        .unwrap_or_else(http::not_found)
}

async fn patch_bios_settings<C: Callbacks>(
    State(state): State<BmcState<C>>,
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

/// Body libredfish posts to `Bios.ChangePassword`.
#[derive(serde::Deserialize)]
#[serde(rename_all = "PascalCase")]
struct ChangeBiosPasswordRequest {
    password_name: String,
    old_password: String,
    new_password: String,
}

async fn change_bios_password_action<C: Callbacks>(
    State(state): State<BmcState<C>>,
    Path(system_id): Path<String>,
    Json(request): Json<ChangeBiosPasswordRequest>,
) -> Response {
    let Some(system_state) = state.system_state.find(&system_id) else {
        return http::not_found();
    };
    if system_state.config.base_bios.is_none() {
        return http::not_found();
    }
    if system_state.change_uefi_password(
        &request.password_name,
        &request.old_password,
        &request.new_password,
    ) {
        json!({}).into_ok_response()
    } else {
        http::bad_request("the old BIOS password does not match")
    }
}

/// libredfish posts an empty body, `{}` or `{"ResetType": "Reset"}` depending on the vendor,
/// so the body is not read.
async fn reset_bios_action<C: Callbacks>(
    State(state): State<BmcState<C>>,
    Path(system_id): Path<String>,
) -> Response {
    // A system without a BIOS resource has nothing to reset, as GET Bios says with its 404.
    let Some(system_state) = state
        .system_state
        .find(&system_id)
        .filter(|system_state| system_state.config.base_bios.is_some())
    else {
        return http::not_found();
    };
    system_state.reset_bios();
    http::ok_no_content()
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
    use crate::test_support::{TestBmcConfig, TestCallbacks, create_test_bmc, host_info};
    use crate::{HardwareType, MachineRouterOptions};

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

    #[tokio::test]
    async fn published_power_state_only_changes_the_controlled_system() {
        let (router, state) = create_test_bmc(
            &host_info(HardwareType::NvidiaDgxGb300),
            TestBmcConfig::default(),
            "test-host-id".to_string(),
            false,
            MachineRouterOptions::default(),
        );
        assert!(state.system_state.set_power_state(PowerState::Off));
        for (system_id, expected) in [("HGX_Baseboard_0", None), ("System_0", Some(json!("Off")))] {
            let response = router
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(format!("/redfish/v1/Systems/{system_id}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
            let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(body.get("PowerState"), expected.as_ref());
        }
    }

    #[tokio::test]
    async fn published_power_state_preserves_low_power_states_and_marks_unknown() {
        let (router, state) = create_test_bmc(
            &host_info(HardwareType::NvidiaDgxGb300),
            TestBmcConfig::default(),
            "test-host-id".to_string(),
            false,
            MachineRouterOptions::default(),
        );
        for (observation, expected) in [
            (PowerState::Paused, json!("Paused")),
            (PowerState::Sleeping, json!("Sleeping")),
            (PowerState::Hibernating, json!("Hibernating")),
            (PowerState::UnsupportedValue, serde_json::Value::Null),
        ] {
            assert!(state.system_state.set_power_state(observation));
            let body = get_json(&router, "/redfish/v1/Systems/System_0").await;
            assert_eq!(body.get("PowerState"), Some(&expected), "{observation:?}");
        }
    }

    #[tokio::test]
    async fn log_services_discovery_names_the_log_collection() {
        let (router, _) = create_test_bmc(
            &host_info(HardwareType::DellPowerEdgeR750),
            TestBmcConfig::default(),
            String::new(),
            false,
            MachineRouterOptions::default(),
        );
        let path = "/redfish/v1/Systems/System.Embedded.1/LogServices";
        let collection = get_json(&router, path).await;
        assert_eq!(collection["@odata.id"], path);
        assert_eq!(
            collection["@odata.type"],
            "#LogServiceCollection.LogServiceCollection"
        );
        assert_eq!(collection["Members@odata.count"], 1);
        let member = collection["Members"][0]["@odata.id"].as_str().unwrap();
        assert_eq!(member, format!("{path}/EventLog"));
        let service = get_json(&router, member).await;
        assert_eq!(service["Id"], "EventLog");
    }

    /// One POST to the in-process mock router with an empty JSON body.
    async fn post_empty(router: &Router, path: &str) -> StatusCode {
        router
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(path)
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap()
            .status()
    }

    fn dell_router() -> (Router, BmcState<TestCallbacks>) {
        create_test_bmc(
            &host_info(HardwareType::DellPowerEdgeR750),
            TestBmcConfig::default(),
            String::new(),
            false,
            MachineRouterOptions::default(),
        )
    }

    #[tokio::test]
    async fn the_system_advertises_its_reset_action() {
        let (router, _) = dell_router();
        let system_id = "System.Embedded.1";
        let system = get_json(&router, &resource(system_id).odata_id).await;
        let action = &system["Actions"]["#ComputerSystem.Reset"];
        assert_eq!(action["target"], reset_target(system_id));
        assert!(
            action["ResetType@Redfish.AllowableValues"]
                .as_array()
                .unwrap()
                .contains(&json!("ForceOff"))
        );
    }

    #[tokio::test]
    async fn the_log_service_states_its_bound_and_clears_on_request() {
        let (router, state) = dell_router();
        let service_path = "/redfish/v1/Systems/System.Embedded.1/LogServices/EventLog";
        let service = get_json(&router, service_path).await;
        assert_eq!(service["OverWritePolicy"], "WrapsWhenFull");
        assert_eq!(
            service["MaxNumberOfRecords"],
            redfish::log_service::DEFAULT_MAX_RECORDS
        );
        let target = service["Actions"]["#LogService.ClearLog"]["target"]
            .as_str()
            .unwrap()
            .to_owned();
        assert_eq!(
            target,
            format!("{service_path}/Actions/LogService.ClearLog")
        );

        let entries_path = format!("{service_path}/Entries");
        let system = "/redfish/v1/Systems/System.Embedded.1";
        state.record_log(redfish::log_service::LogEntryDraft::powered_on(system));
        assert_eq!(
            get_json(&router, &entries_path).await["Members@odata.count"],
            2
        );

        assert_eq!(post_empty(&router, &target).await, StatusCode::NO_CONTENT);
        assert_eq!(
            get_json(&router, &entries_path).await["Members@odata.count"],
            0
        );
        let (_, reused) = state.record_log(redfish::log_service::LogEntryDraft::powered_on(system));
        assert_eq!(
            reused.as_deref(),
            Some(format!("{entries_path}/0").as_str()),
            "a cleared log numbers from zero again"
        );
        assert_eq!(
            post_empty(
                &router,
                "/redfish/v1/Systems/System.Embedded.1/LogServices/Missing/Actions/LogService.ClearLog"
            )
            .await,
            StatusCode::NOT_FOUND
        );
    }

    #[tokio::test]
    async fn storage_discovery_names_the_storage_collection() {
        let (router, _) = create_test_bmc(
            &host_info(HardwareType::DellPowerEdgeR750),
            TestBmcConfig::default(),
            String::new(),
            false,
            MachineRouterOptions::default(),
        );
        let path = "/redfish/v1/Systems/System.Embedded.1/Storage";
        let collection = get_json(&router, path).await;
        assert_eq!(collection["@odata.id"], path);
        assert_eq!(
            collection["@odata.type"],
            "#StorageCollection.StorageCollection"
        );
        let members = collection["Members"].as_array().unwrap();
        assert!(
            members.is_empty(),
            "the fixture advertises an empty Storage collection"
        );
        assert_eq!(collection["Members@odata.count"], 0);
    }

    /// HPE OEM ordering round-trips without corrupting standard BootOption IDs.
    #[tokio::test]
    async fn hpe_boot_order_is_persisted_separately_from_standard_boot_order() {
        let router = create_test_bmc(
            &host_info(HardwareType::HpeProliantDl380aGen11),
            TestBmcConfig::default(),
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
        let (router, state) = create_test_bmc(
            &host_info(HardwareType::LenovoGB300Nvl),
            TestBmcConfig::default(),
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
            assert_eq!(system["SerialConsole"]["IPMI"]["ServiceEnabled"], false);
        }
    }
    /// The `Bios` resource path of the host's system.
    async fn bios_path(router: &Router) -> String {
        let systems = get_json(router, "/redfish/v1/Systems").await;
        for member in systems["Members"].as_array().unwrap() {
            let path = format!("{}/Bios", member["@odata.id"].as_str().unwrap());
            let response = router
                .clone()
                .oneshot(Request::builder().uri(&path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            if response.status() == StatusCode::OK {
                return path;
            }
        }
        panic!("no system serves a Bios resource");
    }

    #[tokio::test]
    async fn bios_actions_need_a_system_with_a_bios() {
        let (router, _) = create_test_bmc(
            &host_info(HardwareType::WiwynnGB200Nvl),
            TestBmcConfig::default(),
            "test-host-id".to_string(),
            false,
            MachineRouterOptions::default(),
        );
        let bios_system = bios_path(&router).await.replace("/Bios", "");
        let change_password = json!({
            "PasswordName": "AdminPassword",
            "OldPassword": "",
            "NewPassword": "first",
        });
        for (scenario, system, expected) in [
            ("a system with a BIOS", bios_system.as_str(), [204, 200]),
            (
                "a system without a BIOS",
                "/redfish/v1/Systems/HGX_Baseboard_0",
                [404, 404],
            ),
            (
                "an unknown system",
                "/redfish/v1/Systems/NoSuchSystem",
                [404, 404],
            ),
        ] {
            for (action, body, expected) in [
                ("ResetBios", json!({}), expected[0]),
                ("ChangePassword", change_password.clone(), expected[1]),
            ] {
                let response = router
                    .clone()
                    .oneshot(
                        Request::builder()
                            .method(Method::POST)
                            .uri(format!("{system}/Bios/Actions/Bios.{action}"))
                            .header(CONTENT_TYPE, "application/json")
                            .body(Body::from(body.to_string()))
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                assert_eq!(response.status().as_u16(), expected, "{scenario}: {action}");
            }
        }
    }
}

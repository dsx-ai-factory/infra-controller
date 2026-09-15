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
use std::sync::{Arc, Mutex, atomic};

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use serde_json::json;

use crate::bmc_state::BmcState;
use crate::json::{JsonExt, JsonPatch};
use crate::redfish::Builder;
use crate::{http, redfish};

pub(super) fn collection() -> redfish::Collection<'static> {
    redfish::Collection {
        odata_id: Cow::Borrowed("/redfish/v1/Managers"),
        odata_type: Cow::Borrowed("#ManagerCollection.ManagerCollection"),
        name: Cow::Borrowed("Manager"),
    }
}

fn resource<'a>(manager_id: &'a str) -> redfish::Resource<'a> {
    let odata_id = format!("/redfish/v1/Managers/{manager_id}");
    redfish::Resource {
        odata_id: Cow::Owned(odata_id),
        odata_type: Cow::Borrowed("#Manager.v1_12_0.Manager"),
        id: Cow::Borrowed(manager_id),
        name: Cow::Borrowed("Manager"),
    }
}

pub(super) fn reset_target(manager_id: &str) -> String {
    format!("{}/Actions/Manager.Reset", resource(manager_id).odata_id)
}

fn builder(resource: &redfish::Resource<'_>) -> ManagerBuilder {
    let reset_target = reset_target(&resource.id);
    ManagerBuilder {
        manager_id: resource.id.to_string(),
        reset_target,
        value: resource.json_patch(),
    }
}

struct ManagerBuilder {
    manager_id: String,
    reset_target: String,
    value: serde_json::Value,
}

impl Builder for ManagerBuilder {
    fn apply_patch(self, patch: serde_json::Value) -> Self {
        Self {
            value: self.value.patch(patch),
            manager_id: self.manager_id,
            reset_target: self.reset_target,
        }
    }
}

impl ManagerBuilder {
    fn ethernet_interfaces(self, collection: &redfish::Collection<'_>) -> Self {
        self.apply_patch(collection.nav_property("EthernetInterfaces"))
    }

    fn host_interfaces(self, collection: &redfish::Collection<'_>) -> Self {
        self.apply_patch(collection.nav_property("HostInterfaces"))
    }

    fn serial_interfaces(self, collection: &redfish::Collection<'_>) -> Self {
        self.apply_patch(collection.nav_property("SerialInterfaces"))
    }

    fn enable_reset_action(self) -> Self {
        let patch = json!({
            "Actions": {
                "#Manager.Reset": {
                    "target": &self.reset_target
                }
            }
        });
        self.apply_patch(patch)
    }

    fn log_services(self, collection: redfish::Collection<'_>) -> Self {
        self.apply_patch(collection.nav_property("LogServices"))
    }

    fn firmware_version(self, v: &str) -> Self {
        self.add_str_field("FirmwareVersion", v)
    }

    fn manager_type(self, v: &str) -> Self {
        self.add_str_field("ManagerType", v)
    }

    fn network_protocol(self, resource: redfish::Resource<'_>) -> Self {
        self.apply_patch(resource.nav_property("NetworkProtocol"))
    }

    fn oem(self, oem: &Oem) -> Self {
        match oem {
            Oem::Dell => self.apply_patch(json!({
                "Oem": {
                    "Dell": {
                        // DelliDRACCard is required by libredfish...
                        "DelliDRACCard": {
                            "@odata.context": "/redfish/v1/$metadata#DelliDRACCard.DelliDRACCard",
                            "@odata.id": "/redfish/v1/Managers/iDRAC.Embedded.1/Oem/Dell/DelliDRACCard/iDRAC.Embedded.1-1_0x23_IDRACinfo",
                            "@odata.type": "#DelliDRACCard.v1_1_0.DelliDRACCard",
                            "Description": "An instance of DelliDRACCard will have data specific to the Integrated Dell Remote Access Controller (iDRAC) in the managed system.",
                            "IPMIVersion": "2.0",
                            "Id": "iDRAC.Embedded.1-1_0x23_IDRACinfo",
                            "LastSystemInventoryTime": "2026-02-20T04:38:38+00:00",
                            "LastUpdateTime": "2026-03-06T04:44:21+00:00",
                            "Name": "DelliDRACCard",
                            "URLString": "https://10.217.157.137:443"
                        }
                    }
                }
            })),
            // bmc-explorer's HPE lockdown check reads
            // Oem.Hpe.VirtualNICEnabled; false is the locked-down state.
            Oem::Hpe => self.apply_patch(json!({
                "Oem": {
                    "Hpe": {
                        "@odata.context": "/redfish/v1/$metadata#HpeiLO.HpeiLO",
                        "@odata.type": "#HpeiLO.v2_11_0.HpeiLO",
                        "VirtualNICEnabled": false,
                    }
                }
            })),
            Oem::Supermicro => {
                let patch =
                    redfish::oem::supermicro::manager::manager_oem_patch(&self.manager_id);
                self.apply_patch(patch)
            }
        }
    }

    // TODO: we can use typed UUID here, but all these fields are
    // really not used it just requirements of libredfish model added
    // "just in case"...
    fn uuid(self, v: &str) -> Self {
        self.add_str_field("UUID", v)
    }

    fn date_time(self, v: DateTime<Utc>) -> Self {
        let current_time = v.format("%Y-%m-%dT%H:%M:%S+00:00").to_string();
        self.add_str_field("DateTime", &current_time)
    }

    fn status(self, status: redfish::resource::Status) -> Self {
        self.apply_patch(json!({"Status": status.into_json()}))
    }

    fn build(self) -> serde_json::Value {
        self.value
    }
}

pub(crate) fn add_routes(r: Router<BmcState>) -> Router<BmcState> {
    const MGR_ID: &str = "{manager_id}";
    const ETH_ID: &str = "{ethernet_id}";
    const HOST_IF_ID: &str = "{hostif_id}";
    r.route(&collection().odata_id, get(get_manager_collection))
        .route(
            &resource(MGR_ID).odata_id,
            get(get_manager).patch(patch_manager),
        )
        .route(
            &redfish::ethernet_interface::manager_collection(MGR_ID).odata_id,
            get(get_ethernet_interface_collection),
        )
        .route(
            &redfish::ethernet_interface::manager_resource(MGR_ID, ETH_ID).odata_id,
            get(get_ethernet_interface),
        )
        .route(
            &redfish::host_interface::manager_collection(MGR_ID).odata_id,
            get(get_host_interface_collection),
        )
        .route(
            &redfish::host_interface::manager_resource(MGR_ID, HOST_IF_ID).odata_id,
            get(get_host_interface).patch(patch_host_interface),
        )
        .route(
            &redfish::serial_interface::manager_collection(MGR_ID).odata_id,
            get(get_serial_interface_collection),
        )
        .route(
            &redfish::serial_interface::manager_resource(MGR_ID, HOST_IF_ID).odata_id,
            get(get_serial_interface),
        )
        .route(&reset_target(MGR_ID), post(post_reset_manager))
        .route(
            &redfish::manager_network_protocol::manager_resource(MGR_ID).odata_id,
            get(get_network_protocol).patch(patch_network_protocol),
        )
        .route(
            &redfish::log_service::manager_collection(MGR_ID).odata_id,
            get(get_log_services),
        )
}

#[derive(Clone, Copy)]
pub(crate) enum Oem {
    Dell,
    Hpe,
    Supermicro,
}

impl AsRef<Oem> for Oem {
    fn as_ref(&self) -> &Self {
        self
    }
}

pub(crate) struct Config {
    pub(crate) managers: Vec<SingleConfig>,
}

#[derive(Clone)]
pub(crate) struct SingleConfig {
    pub(crate) id: &'static str,
    pub(crate) eth_interfaces: Option<Vec<redfish::ethernet_interface::EthernetInterface>>,
    pub(crate) host_interfaces: Option<Vec<redfish::host_interface::HostInterface>>,
    pub(crate) serial_interfaces: Option<Vec<redfish::serial_interface::SerialInterface>>,
    pub(crate) firmware_version: Option<&'static str>,
    pub(crate) oem: Option<Oem>,
}

pub struct ManagerState {
    managers: Vec<SingleManagerState>,
}

impl ManagerState {
    pub(crate) fn new(config: &Config) -> Self {
        Self {
            managers: config
                .managers
                .iter()
                .map(SingleManagerState::new)
                .collect(),
        }
    }

    /// `@odata.id` of the BMC's own manager resource, the first configured one.
    pub(crate) fn primary_odata_id(&self) -> Option<String> {
        self.managers
            .first()
            .map(|manager| resource(manager.id).odata_id.into_owned())
    }

    pub(crate) fn find(&self, manager_id: &str) -> Option<&SingleManagerState> {
        self.managers.iter().find(|c| c.id == manager_id)
    }

    pub fn set_ipmi_endpoint(&self, port: Option<u16>) {
        for manager in &self.managers {
            manager.set_ipmi_endpoint(port);
        }
    }
}

pub(crate) struct SingleManagerState {
    id: &'static str,
    ipmi_enabled: Arc<atomic::AtomicBool>,
    ipmi_port: Mutex<Option<u16>>,
    ntp: Mutex<NtpState>,
    config: SingleConfig,
    // PATCHes applied to the manager resource (e.g. HPE lockdown flips
    // Oem.Hpe.VirtualNICEnabled); merged over the built JSON on GET.
    overrides: Arc<Mutex<serde_json::Value>>,
    // PATCHes applied to individual HostInterface resources. Lockdown flows
    // rely on InterfaceEnabled changing persistently across PATCH and GET.
    host_interface_overrides: Mutex<HashMap<String, serde_json::Value>>,
}

struct NtpState {
    protocol_enabled: bool,
    servers: Vec<String>,
}

impl Default for NtpState {
    fn default() -> Self {
        Self {
            protocol_enabled: true,
            servers: Vec::new(),
        }
    }
}

impl NtpState {
    fn apply_patch(&mut self, patch: &serde_json::Value) {
        if let Some(v) = patch
            .get("ProtocolEnabled")
            .and_then(serde_json::Value::as_bool)
        {
            self.protocol_enabled = v;
        }
        if let Some(servers) = patch
            .get("NTPServers")
            .and_then(serde_json::Value::as_array)
        {
            self.servers = servers
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(ToOwned::to_owned)
                .collect();
        }
    }
}

impl SingleManagerState {
    pub(crate) fn new(config: &SingleConfig) -> Self {
        Self {
            id: config.id,
            config: config.clone(),
            ipmi_enabled: Arc::new(false.into()),
            ipmi_port: Mutex::new(None),
            ntp: Mutex::new(NtpState::default()),
            overrides: Arc::new(Mutex::new(json!({}))),
            host_interface_overrides: Mutex::new(HashMap::new()),
        }
    }

    fn network_protocol(&self) -> serde_json::Value {
        let resource = redfish::manager_network_protocol::manager_resource(self.id);
        let ntp = self.ntp.lock().expect("mutex poisoned");
        redfish::manager_network_protocol::builder(&resource)
            .ipmi(
                self.ipmi_enabled.load(atomic::Ordering::Relaxed),
                *self.ipmi_port.lock().expect("mutex poisoned"),
            )
            .ntp(ntp.protocol_enabled, &ntp.servers)
            .build()
    }

    fn set_ipmi_endpoint(&self, port: Option<u16>) {
        *self.ipmi_port.lock().expect("mutex poisoned") = port;
        self.ipmi_enabled
            .store(port.is_some(), atomic::Ordering::Relaxed);
    }

    fn patch_network_protocol(&self, patch_request: serde_json::Value) {
        if let Some(v) = patch_request
            .get("IPMI")
            .and_then(|v| v.get("ProtocolEnabled"))
            .and_then(serde_json::Value::as_bool)
        {
            self.ipmi_enabled.store(v, atomic::Ordering::Relaxed)
        }
        if let Some(v) = patch_request.get("NTP") {
            self.ntp.lock().expect("mutex poisoned").apply_patch(v);
        }
    }

    fn host_interface(&self, iface_id: &str) -> Option<serde_json::Value> {
        let iface = self
            .config
            .host_interfaces
            .as_ref()?
            .iter()
            .find(|iface| iface.id == iface_id)?;
        let overrides = self
            .host_interface_overrides
            .lock()
            .expect("mutex poisoned");
        Some(
            iface.to_json().patch(
                overrides
                    .get(iface_id)
                    .cloned()
                    .unwrap_or_else(|| json!({})),
            ),
        )
    }

    fn patch_host_interface(&self, iface_id: &str, patch_request: serde_json::Value) -> bool {
        let Some(host_interfaces) = self.config.host_interfaces.as_ref() else {
            return false;
        };
        if !host_interfaces.iter().any(|iface| iface.id == iface_id) {
            return false;
        }

        let mut overrides = self
            .host_interface_overrides
            .lock()
            .expect("mutex poisoned");
        let current = overrides
            .entry(iface_id.to_string())
            .or_insert_with(|| json!({}));
        *current = current.clone().patch(patch_request);
        true
    }
}

async fn get_manager_collection(State(state): State<BmcState>) -> Response {
    collection()
        .with_members(
            &state
                .manager
                .managers
                .iter()
                .map(|manager| resource(manager.id).entity_ref())
                .collect::<Vec<_>>(),
        )
        .into_ok_response()
}

async fn get_manager(State(state): State<BmcState>, Path(manager_id): Path<String>) -> Response {
    let Some(this) = state.manager.find(&manager_id) else {
        return http::not_found();
    };

    builder(&resource(&manager_id))
        .manager_type("BMC")
        .network_protocol(redfish::manager_network_protocol::manager_resource(
            &manager_id,
        ))
        .maybe_with(
            ManagerBuilder::ethernet_interfaces,
            &this
                .config
                .eth_interfaces
                .as_ref()
                .map(|_| redfish::ethernet_interface::manager_collection(&manager_id)),
        )
        .maybe_with(
            ManagerBuilder::host_interfaces,
            &this
                .config
                .host_interfaces
                .as_ref()
                .map(|_| redfish::host_interface::manager_collection(&manager_id)),
        )
        .maybe_with(
            ManagerBuilder::serial_interfaces,
            &this
                .config
                .serial_interfaces
                .as_ref()
                .map(|_| redfish::serial_interface::manager_collection(&manager_id)),
        )
        .enable_reset_action()
        .log_services(redfish::log_service::manager_collection(&manager_id))
        .status(redfish::resource::Status::Ok)
        .uuid("3347314f-c0c6-5080-3410-00354c4c4544")
        .date_time(Utc::now())
        .maybe_with(ManagerBuilder::oem, &this.config.oem)
        .maybe_with(
            ManagerBuilder::firmware_version,
            &this.config.firmware_version,
        )
        .build()
        .patch(this.overrides.lock().expect("mutex poisoned").clone())
        .into_ok_response()
}

async fn patch_manager(
    State(state): State<BmcState>,
    Path(manager_id): Path<String>,
    Json(patch_request): Json<serde_json::Value>,
) -> Response {
    let Some(this) = state.manager.find(&manager_id) else {
        return http::not_found();
    };
    let mut overrides = this.overrides.lock().expect("mutex poisoned");
    *overrides = overrides.clone().patch(patch_request);
    http::ok_no_content()
}

async fn get_ethernet_interface_collection(
    State(state): State<BmcState>,
    Path(manager_id): Path<String>,
) -> Response {
    state
        .manager
        .find(&manager_id)
        .and_then(|manager| manager.config.eth_interfaces.as_ref())
        .map(|eth_interfaces| {
            let members = eth_interfaces
                .iter()
                .map(|eth| {
                    redfish::ethernet_interface::manager_resource(&manager_id, &eth.id).entity_ref()
                })
                .collect::<Vec<_>>();
            redfish::ethernet_interface::manager_collection(&manager_id)
                .with_members(&members)
                .into_ok_response()
        })
        .unwrap_or_else(http::not_found)
}

async fn get_ethernet_interface(
    State(state): State<BmcState>,
    Path((manager_id, eth_id)): Path<(String, String)>,
) -> Response {
    state
        .manager
        .find(&manager_id)
        .and_then(|manager| manager.config.eth_interfaces.as_ref())
        .and_then(|eth_interfaces| {
            eth_interfaces
                .iter()
                .find(|eth| eth.id == eth_id)
                .map(|eth| eth.to_json().into_ok_response())
        })
        .unwrap_or_else(http::not_found)
}

async fn get_host_interface_collection(
    State(state): State<BmcState>,
    Path(manager_id): Path<String>,
) -> Response {
    state
        .manager
        .find(&manager_id)
        .and_then(|manager| manager.config.host_interfaces.as_ref())
        .map(|host_interfaces| {
            let members = host_interfaces
                .iter()
                .map(|iface| {
                    redfish::host_interface::manager_resource(&manager_id, &iface.id).entity_ref()
                })
                .collect::<Vec<_>>();
            redfish::host_interface::manager_collection(&manager_id)
                .with_members(&members)
                .into_ok_response()
        })
        .unwrap_or_else(http::not_found)
}

async fn get_host_interface(
    State(state): State<BmcState>,
    Path((manager_id, iface_id)): Path<(String, String)>,
) -> Response {
    state
        .manager
        .find(&manager_id)
        .and_then(|manager| manager.host_interface(&iface_id))
        .map(JsonExt::into_ok_response)
        .unwrap_or_else(http::not_found)
}

async fn get_serial_interface_collection(
    State(state): State<BmcState>,
    Path(manager_id): Path<String>,
) -> Response {
    state
        .manager
        .find(&manager_id)
        .and_then(|manager| manager.config.serial_interfaces.as_ref())
        .map(|serial_interfaces| {
            let members = serial_interfaces
                .iter()
                .map(|interface| {
                    redfish::serial_interface::manager_resource(&manager_id, &interface.id)
                        .entity_ref()
                })
                .collect::<Vec<_>>();
            redfish::serial_interface::manager_collection(&manager_id)
                .with_members(&members)
                .into_ok_response()
        })
        .unwrap_or_else(http::not_found)
}

async fn get_serial_interface(
    State(state): State<BmcState>,
    Path((manager_id, interface_id)): Path<(String, String)>,
) -> Response {
    state
        .manager
        .find(&manager_id)
        .and_then(|manager| manager.config.serial_interfaces.as_ref())
        .and_then(|serial_interfaces| {
            serial_interfaces
                .iter()
                .find(|interface| interface.id == interface_id)
                .map(|interface| interface.to_json().into_ok_response())
        })
        .unwrap_or_else(http::not_found)
}

async fn patch_host_interface(
    State(state): State<BmcState>,
    Path((manager_id, iface_id)): Path<(String, String)>,
    Json(patch_request): Json<serde_json::Value>,
) -> Response {
    state
        .manager
        .find(&manager_id)
        .filter(|manager| manager.patch_host_interface(&iface_id, patch_request))
        .map(|_| http::ok_no_content())
        .unwrap_or_else(http::not_found)
}

async fn get_network_protocol(
    State(state): State<BmcState>,
    Path(manager_id): Path<String>,
) -> Response {
    let Some(this) = state.manager.find(&manager_id) else {
        return http::not_found();
    };
    this.network_protocol().into_ok_response()
}

async fn patch_network_protocol(
    State(state): State<BmcState>,
    Path(manager_id): Path<String>,
    Json(json): Json<serde_json::Value>,
) -> Response {
    let Some(this) = state.manager.find(&manager_id) else {
        return http::not_found();
    };
    this.patch_network_protocol(json);
    http::ok_no_content()
}

async fn post_reset_manager(
    State(state): State<BmcState>,
    Path(manager_id): Path<String>,
) -> Response {
    if state.manager.find(&manager_id).is_none() {
        return http::not_found();
    }
    // BMC self-reset: acknowledge, then go dark for the platform's
    // bmc_reset window (the middleware answers 503 until it expires).
    // The SERVER's power state is deliberately untouched — a BMC reset
    // interrupts management visibility, not the machine.
    // Logged, not announced: the reset closes every stream before a
    // subscriber could observe an Event, as when a real BMC goes dark.
    state.record_log(redfish::log_service::LogEntryDraft::manager_resetting(
        &resource(&manager_id).odata_id,
        "Manager.Reset",
    ));
    let offline_for = state.reset();
    tracing::info!(
        manager_id,
        ?offline_for,
        event_service = state.event_service.is_some(),
        "BMC self-reset"
    );
    json!({}).into_ok_response()
}

async fn get_log_services() -> Response {
    not_implemented()
}

fn not_implemented() -> Response {
    http::redfish_error(StatusCode::NOT_IMPLEMENTED, "not implemented")
}

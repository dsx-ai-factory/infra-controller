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
use chrono::Utc;
use serde::Deserialize;
use serde_json::json;

use crate::bmc_state::BmcState;
use crate::json::JsonExt;
use crate::redfish::ModelExt;
use crate::{ActionError, Callbacks, DeviceEffect, http, redfish};

#[cfg(test)]
#[path = "manager_reset_tests.rs"]
mod reset_defaults_tests;

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
        odata_type: Cow::Borrowed(redfish::schema::manager::Manager::ODATA_TYPE),
        id: Cow::Borrowed(manager_id),
        name: Cow::Borrowed("Manager"),
    }
}

pub(super) fn reset_target(manager_id: &str) -> String {
    format!("{}/Actions/Manager.Reset", resource(manager_id).odata_id)
}

fn reset_defaults_target(manager_id: &str) -> String {
    format!(
        "{}/Actions/Manager.ResetToDefaults",
        resource(manager_id).odata_id
    )
}

fn oem_patch(oem: &Oem, manager_id: &str) -> serde_json::Value {
    match oem {
        Oem::Dell => json!({
            "Oem": {
                "Dell": {
                    // Required by libredfish; nv-redfish bundles no DelliDRACCard CSDL.
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
        }),
        // bmc-explorer's HPE lockdown check reads
        // Oem.Hpe.VirtualNICEnabled; false is the locked-down state.
        Oem::Hpe => json!({
            "Oem": {
                "Hpe": redfish::oem::object(
                    "#HpeiLO.v2_11_0.HpeiLO",
                    super::schema::hpei_lo::HpeiLo::new().virtual_nic_enabled(false)
                ).patch(json!({"@odata.context": "/redfish/v1/$metadata#HpeiLO.HpeiLO"}))
            }
        }),
        Oem::Supermicro => redfish::oem::supermicro::manager::manager_oem_patch(manager_id),
    }
}

pub(crate) fn add_routes<C: Callbacks>(r: Router<BmcState<C>>) -> Router<BmcState<C>> {
    const MGR_ID: &str = "{manager_id}";
    const ETH_ID: &str = "{ethernet_id}";
    const HOST_IF_ID: &str = "{hostif_id}";
    r.route(&collection().odata_id, get(get_manager_collection::<C>))
        .route(
            &resource(MGR_ID).odata_id,
            get(get_manager::<C>).patch(patch_manager::<C>),
        )
        .route(
            &redfish::ethernet_interface::manager_collection(MGR_ID).odata_id,
            get(get_ethernet_interface_collection::<C>),
        )
        .route(
            &redfish::ethernet_interface::manager_resource(MGR_ID, ETH_ID).odata_id,
            get(get_ethernet_interface::<C>),
        )
        .route(
            &redfish::host_interface::manager_collection(MGR_ID).odata_id,
            get(get_host_interface_collection::<C>),
        )
        .route(
            &redfish::host_interface::manager_resource(MGR_ID, HOST_IF_ID).odata_id,
            get(get_host_interface::<C>).patch(patch_host_interface::<C>),
        )
        .route(
            &redfish::serial_interface::manager_collection(MGR_ID).odata_id,
            get(get_serial_interface_collection::<C>),
        )
        .route(
            &redfish::serial_interface::manager_resource(MGR_ID, HOST_IF_ID).odata_id,
            get(get_serial_interface::<C>),
        )
        .route(&reset_target(MGR_ID), post(post_reset_manager::<C>))
        .route(
            &reset_defaults_target(MGR_ID),
            post(post_reset_defaults::<C>),
        )
        .route(
            &redfish::manager_network_protocol::manager_resource(MGR_ID).odata_id,
            get(get_network_protocol::<C>).patch(patch_network_protocol::<C>),
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

#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct PersistedManagerState {
    overrides: serde_json::Value,
    host_interfaces: HashMap<String, serde_json::Value>,
}

pub struct ManagerState {
    managers: Vec<SingleManagerState>,
}

impl ManagerState {
    pub(crate) fn persisted(&self) -> std::collections::BTreeMap<String, PersistedManagerState> {
        self.managers
            .iter()
            .map(|manager| {
                (
                    manager.id.to_string(),
                    PersistedManagerState {
                        overrides: manager.overrides.lock().expect("mutex poisoned").clone(),
                        host_interfaces: manager
                            .host_interface_overrides
                            .lock()
                            .expect("mutex poisoned")
                            .clone(),
                    },
                )
            })
            .collect()
    }

    pub(crate) fn validate_persisted(
        &self,
        saved: &std::collections::BTreeMap<String, PersistedManagerState>,
    ) -> bool {
        saved.len() == self.managers.len()
            && saved.iter().all(|(id, state)| {
                self.find(id).is_some_and(|manager| {
                    state.overrides.is_object()
                        && state.host_interfaces.iter().all(|(iface, value)| {
                            value.is_object()
                                && manager.config.host_interfaces.as_ref().is_some_and(
                                    |interfaces| interfaces.iter().any(|i| i.id == iface.as_str()),
                                )
                        })
                })
            })
    }

    pub(crate) fn restore_persisted(
        &self,
        saved: &std::collections::BTreeMap<String, PersistedManagerState>,
    ) {
        for (id, state) in saved {
            let manager = self.find(id).expect("validated manager identity");
            *manager.overrides.lock().expect("mutex poisoned") = state.overrides.clone();
            *manager
                .host_interface_overrides
                .lock()
                .expect("mutex poisoned") = state.host_interfaces.clone();
        }
    }

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

    pub(crate) fn reset_to_defaults(&self) {
        for manager in &self.managers {
            *manager.overrides.lock().expect("mutex poisoned") = json!({});
            manager
                .host_interface_overrides
                .lock()
                .expect("mutex poisoned")
                .clear();
            *manager.ntp.lock().expect("mutex poisoned") = NtpState::default();
            // The configured simulator endpoint determines the factory IPMI default.
            manager.ipmi_enabled.store(
                manager.ipmi_port.lock().expect("mutex poisoned").is_some(),
                atomic::Ordering::Relaxed,
            );
        }
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
        use super::schema::manager_network_protocol::{
            ManagerNetworkProtocol, NtpProtocol, Protocol,
        };
        let port = *self.ipmi_port.lock().expect("mutex poisoned");
        ManagerNetworkProtocol::new(
            resource.odata_id.as_ref(),
            resource.id.as_ref(),
            resource.name.as_ref(),
        )
        .ipmi(Protocol {
            port: port.map(|p| Some(i64::from(p))),
            ..Protocol::new().protocol_enabled(self.ipmi_enabled.load(atomic::Ordering::Relaxed))
        })
        .ntp(
            NtpProtocol::new()
                .protocol_enabled(ntp.protocol_enabled)
                .ntp_servers(ntp.servers.iter().map(String::as_str)),
        )
        .into_json()
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

async fn get_manager_collection<C: Callbacks>(State(state): State<BmcState<C>>) -> Response {
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

async fn get_manager<C: Callbacks>(
    State(state): State<BmcState<C>>,
    Path(manager_id): Path<String>,
) -> Response {
    let Some(this) = state.manager.find(&manager_id) else {
        return http::not_found();
    };

    use super::schema::manager::{Manager, ManagerType};
    let resource = resource(&manager_id);
    let config = &this.config;
    let model = Manager {
        ethernet_interfaces: config.eth_interfaces.as_ref().map(|_| {
            redfish::ethernet_interface::manager_collection(&manager_id)
                .odata_id
                .into_owned()
                .into()
        }),
        host_interfaces: config.host_interfaces.as_ref().map(|_| {
            redfish::host_interface::manager_collection(&manager_id)
                .odata_id
                .into_owned()
                .into()
        }),
        serial_interfaces: config.serial_interfaces.as_ref().map(|_| {
            redfish::serial_interface::manager_collection(&manager_id)
                .odata_id
                .into_owned()
                .into()
        }),
        firmware_version: config.firmware_version.map(|v| Some(v.to_string())),
        ..Manager::new(
            resource.odata_id.as_ref(),
            resource.id.as_ref(),
            resource.name.as_ref(),
        )
    }
    .manager_type(ManagerType::Bmc)
    .network_protocol(
        redfish::manager_network_protocol::manager_resource(&manager_id)
            .odata_id
            .as_ref(),
    )
    .log_services(
        redfish::log_service::manager_collection(&manager_id)
            .odata_id
            .as_ref(),
    )
    .status(redfish::resource::Status::Ok.into())
    .uuid(
        "3347314f-c0c6-5080-3410-00354c4c4544"
            .parse()
            .expect("fixture manager UUID"),
    )
    .date_time(Utc::now().format("%Y-%m-%dT%H:%M:%S+00:00").to_string());
    // Bound actions are not emitted by the server schema generator yet.
    let mut document = model.into_json().patch(json!({
        "Actions": {
            "#Manager.Reset": {"target": reset_target(&manager_id)}
        }
    }));
    if state
        .callbacks
        .as_ref()
        .is_some_and(|callbacks| callbacks.supports_bmc_factory_reset())
    {
        document = document.patch(json!({"Actions": {
            "#Manager.ResetToDefaults": {
                "target": reset_defaults_target(&manager_id),
                "ResetType@Redfish.AllowableValues": ["ResetAll"],
                "ResetToDefaultsType@Redfish.AllowableValues": ["ResetAll"]
            }
        }}));
    }
    let document = if let Some(oem) = &config.oem {
        document.patch(oem_patch(oem, &manager_id))
    } else {
        document
    };
    document
        .patch(this.overrides.lock().expect("mutex poisoned").clone())
        .into_ok_response()
}

async fn patch_manager<C: Callbacks>(
    State(state): State<BmcState<C>>,
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

async fn get_ethernet_interface_collection<C: Callbacks>(
    State(state): State<BmcState<C>>,
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

async fn get_ethernet_interface<C: Callbacks>(
    State(state): State<BmcState<C>>,
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

async fn get_host_interface_collection<C: Callbacks>(
    State(state): State<BmcState<C>>,
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

async fn get_host_interface<C: Callbacks>(
    State(state): State<BmcState<C>>,
    Path((manager_id, iface_id)): Path<(String, String)>,
) -> Response {
    state
        .manager
        .find(&manager_id)
        .and_then(|manager| manager.host_interface(&iface_id))
        .map(JsonExt::into_ok_response)
        .unwrap_or_else(http::not_found)
}

async fn get_serial_interface_collection<C: Callbacks>(
    State(state): State<BmcState<C>>,
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

async fn get_serial_interface<C: Callbacks>(
    State(state): State<BmcState<C>>,
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

async fn patch_host_interface<C: Callbacks>(
    State(state): State<BmcState<C>>,
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

async fn get_network_protocol<C: Callbacks>(
    State(state): State<BmcState<C>>,
    Path(manager_id): Path<String>,
) -> Response {
    let Some(this) = state.manager.find(&manager_id) else {
        return http::not_found();
    };
    this.network_protocol().into_ok_response()
}

async fn patch_network_protocol<C: Callbacks>(
    State(state): State<BmcState<C>>,
    Path(manager_id): Path<String>,
    Json(json): Json<serde_json::Value>,
) -> Response {
    let Some(this) = state.manager.find(&manager_id) else {
        return http::not_found();
    };
    this.patch_network_protocol(json);
    http::ok_no_content()
}

async fn post_reset_manager<C: Callbacks>(
    State(state): State<BmcState<C>>,
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

#[derive(Deserialize)]
#[serde(untagged, deny_unknown_fields)]
enum ResetToDefaultsRequest {
    Standard {
        #[serde(rename = "ResetType")]
        value: ResetAll,
    },
    Defaults {
        #[serde(rename = "ResetToDefaultsType")]
        value: ResetAll,
    },
}

#[derive(Deserialize)]
enum ResetAll {
    ResetAll,
}

async fn post_reset_defaults<C: Callbacks>(
    State(state): State<BmcState<C>>,
    Path(manager_id): Path<String>,
    request: Result<Json<ResetToDefaultsRequest>, axum::extract::rejection::JsonRejection>,
) -> Response {
    if state.manager.find(&manager_id).is_none() {
        return http::not_found();
    }
    let Ok(Json(request)) = request else {
        return http::redfish_error(StatusCode::BAD_REQUEST, "invalid ResetAll request");
    };
    let ResetAll::ResetAll = match request {
        ResetToDefaultsRequest::Standard { value } | ResetToDefaultsRequest::Defaults { value } => {
            value
        }
    };
    let Some(callbacks) = &state.callbacks else {
        return http::redfish_error(StatusCode::BAD_REQUEST, "BMC reset backend is unavailable");
    };
    let update = state.factory_reset_update().await;
    match callbacks
        .device_effect(DeviceEffect::ResetBmcToDefaults(update))
        .await
    {
        Ok(()) => {
            state.record_log(redfish::log_service::LogEntryDraft::manager_resetting(
                &resource(&manager_id).odata_id,
                "Manager.ResetToDefaults",
            ));
            json!({}).into_ok_response()
        }
        Err(ActionError::BadRequest(error)) => {
            http::redfish_error(StatusCode::BAD_REQUEST, &error.to_string())
        }
        Err(ActionError::Internal(error)) => {
            tracing::error!(%error, "BMC factory reset failed");
            http::redfish_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "BMC factory reset failed",
            )
        }
    }
}

async fn get_log_services() -> Response {
    not_implemented()
}

fn not_implemented() -> Response {
    http::redfish_error(StatusCode::NOT_IMPLEMENTED, "not implemented")
}

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
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::{Json, Path, State};
use axum::response::Response;
use axum::routing::get;
use serde_json::json;

use crate::bmc_state::BmcState;
use crate::json::JsonExt;
use crate::redfish::ModelExt;
use crate::redfish::schema::kcs_interface::{KcsInterface, Privilege};
use crate::redfish::schema::sys_lockdown::SysLockdown;
use crate::{Callbacks, http, redfish};

#[derive(Clone)]
pub(crate) struct SupermicroState {
    kcs_interface: Arc<Mutex<serde_json::Value>>,
    sys_lockdown: Arc<Mutex<serde_json::Value>>,
}

impl Default for SupermicroState {
    fn default() -> Self {
        Self {
            kcs_interface: Arc::new(Mutex::new(json!({}))),
            sys_lockdown: Arc::new(Mutex::new(json!({}))),
        }
    }
}

impl SupermicroState {
    fn kcs_interface(&self, base: serde_json::Value) -> serde_json::Value {
        base.patch(self.kcs_interface.lock().expect("mutex poisoned").clone())
    }

    fn patch_kcs_interface(&self, patch: serde_json::Value) {
        let mut current = self.kcs_interface.lock().expect("mutex poisoned");
        *current = current.clone().patch(patch);
    }

    fn sys_lockdown(&self, base: serde_json::Value) -> serde_json::Value {
        base.patch(self.sys_lockdown.lock().expect("mutex poisoned").clone())
    }

    fn patch_sys_lockdown(&self, patch: serde_json::Value) {
        let mut current = self.sys_lockdown.lock().expect("mutex poisoned");
        *current = current.clone().patch(patch);
    }
}

fn kcs_interface_resource(manager_id: &str) -> redfish::Resource<'static> {
    redfish::Resource {
        odata_id: Cow::Owned(format!(
            "/redfish/v1/Managers/{manager_id}/Oem/Supermicro/KCSInterface"
        )),
        odata_type: Cow::Borrowed(KcsInterface::ODATA_TYPE),
        id: Cow::Borrowed("KCSInterface"),
        name: Cow::Borrowed("KCS Interface"),
    }
}

fn sys_lockdown_resource(manager_id: &str) -> redfish::Resource<'static> {
    redfish::Resource {
        odata_id: Cow::Owned(format!(
            "/redfish/v1/Managers/{manager_id}/Oem/Supermicro/SysLockdown"
        )),
        odata_type: Cow::Borrowed(SysLockdown::ODATA_TYPE),
        id: Cow::Borrowed("SysLockdown"),
        name: Cow::Borrowed("System Lockdown"),
    }
}

pub(in crate::redfish) fn manager_oem_patch(manager_id: &str) -> serde_json::Value {
    let model = crate::redfish::schema::smc_manager_extensions::Manager::new()
        .kcs_interface(kcs_interface_resource(manager_id).odata_id.as_ref())
        .sys_lockdown(sys_lockdown_resource(manager_id).odata_id.as_ref());
    json!({"Oem": {"Supermicro": model}})
}

pub(crate) fn add_routes<C: Callbacks>(r: Router<BmcState<C>>) -> Router<BmcState<C>> {
    r.route(
        "/redfish/v1/Managers/{manager_id}/Oem/Supermicro/KCSInterface",
        get(get_kcs_interface::<C>).patch(patch_kcs_interface::<C>),
    )
    .route(
        "/redfish/v1/Managers/{manager_id}/Oem/Supermicro/SysLockdown",
        get(get_sys_lockdown::<C>).patch(patch_sys_lockdown::<C>),
    )
}

async fn get_kcs_interface<C: Callbacks>(
    State(state): State<BmcState<C>>,
    Path(manager_id): Path<String>,
) -> Response {
    let redfish::oem::State::Supermicro(supermicro) = state.oem_state else {
        return http::not_found();
    };
    if state.manager.find(&manager_id).is_none() {
        return http::not_found();
    }
    let resource = kcs_interface_resource(&manager_id);
    let model = KcsInterface::new(
        resource.odata_id.as_ref(),
        resource.id.as_ref(),
        resource.name.as_ref(),
    )
    .privilege(Privilege::Administrator);
    supermicro
        .kcs_interface(model.into_json())
        .into_ok_response()
}

async fn patch_kcs_interface<C: Callbacks>(
    State(state): State<BmcState<C>>,
    Path(manager_id): Path<String>,
    Json(patch): Json<serde_json::Value>,
) -> Response {
    let redfish::oem::State::Supermicro(supermicro) = state.oem_state else {
        return http::not_found();
    };
    if state.manager.find(&manager_id).is_none() {
        return http::not_found();
    }
    supermicro.patch_kcs_interface(patch);
    http::ok_no_content()
}

async fn get_sys_lockdown<C: Callbacks>(
    State(state): State<BmcState<C>>,
    Path(manager_id): Path<String>,
) -> Response {
    let redfish::oem::State::Supermicro(supermicro) = state.oem_state else {
        return http::not_found();
    };
    if state.manager.find(&manager_id).is_none() {
        return http::not_found();
    }
    let resource = sys_lockdown_resource(&manager_id);
    let model = SysLockdown::new(
        resource.odata_id.as_ref(),
        resource.id.as_ref(),
        resource.name.as_ref(),
    )
    .sys_lockdown_enabled(false);
    supermicro
        .sys_lockdown(model.into_json())
        .into_ok_response()
}

async fn patch_sys_lockdown<C: Callbacks>(
    State(state): State<BmcState<C>>,
    Path(manager_id): Path<String>,
    Json(patch): Json<serde_json::Value>,
) -> Response {
    let redfish::oem::State::Supermicro(supermicro) = state.oem_state else {
        return http::not_found();
    };
    if state.manager.find(&manager_id).is_none() {
        return http::not_found();
    }
    supermicro.patch_sys_lockdown(patch);
    http::ok_no_content()
}

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

//! Stateful firmware update simulation for bmc-mock.
//!
//! ## Design notes
//!
//! ### Transport independence
//! All upload paths (Redfish SimpleUpdate, AMI multipart `UpdateService/upload`)
//! converge on `record_upload()`.  The staging and task lifecycle are identical
//! regardless of which transport carbide uses.
//!
//! ### Component identification
//! When Targets is explicit (SimpleUpdate), the component is derived from the
//! target path.  When it is absent (multipart), the next entry is peeked from
//! `pending_upgrades` — a deterministic ordered map injected by machine-a-tron.
//! This avoids ambiguity when multiple components are outdated.
//!
//! ### Task lifecycle (NICo-compatible)
//! The upload POST returns immediately with the task in `Running` state (202).
//! A background task transitions to `Completed` after a configurable delay (with
//! jitter to avoid correlated resets across many simulated hosts).  Task state
//! and the staged firmware entry are written in a single critical section to
//! eliminate the race window between `Completed` and staging.
//!
//! The background task is spawned as fire-and-forget (the `JoinHandle` is
//! dropped).  This is intentional for a mock BMC: tasks are short-lived (seconds),
//! the process exits when the simulation ends, and panics inside the task would
//! only leave a task permanently in `Running` state — visible to a poll loop but
//! harmless to the rest of the simulation.
//!
//! ### Activation event
//! All staged firmware is applied on `BmcEvent::PowerOn`.  This is an explicit
//! simulation simplification: real UEFI updates trigger a host restart while
//! real BMC updates trigger `Manager.Reset` (currently a no-op in bmc-mock).
//! Per-component activation events can be added in a follow-up.
//!
//! ### Non-destructive queue
//! `pending_upgrades` is an `IndexMap<component_id, target_version>` (ordered,
//! key-addressed).  Uploads `peek` the first entry without consuming it.  The
//! entry is only removed when `apply_staged_firmware` successfully stages the
//! version on PowerOn.  This makes upload retries safe: a failed or repeated
//! upload keeps the same queue entry available for the next attempt.
//!
//! ### Response format
//! Every upload response includes **both** the `Location` header (for Dell/DGX H100)
//! and a full Task JSON body (for GB200/GB300/Lenovo), satisfying all platform
//! contracts.
//!
//! ### Load isolation
//! All task and inventory state lives in `UpdateServiceState` behind `Arc`.
//! Each simulated BMC gets its own `Arc<UpdateServiceState>` (via `BmcState`),
//! so state is fully isolated per host.
//! Uploaded image bytes are streamed frame-by-frame and discarded without
//! buffering to avoid unbounded memory use.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::response::Response;
use axum::routing::{get, post};
use indexmap::IndexMap;
use serde::Deserialize;
use serde_json::json;

use crate::bmc_state::BmcState;
use crate::json::{JsonExt, JsonPatch};
use crate::redfish::Builder;
use crate::{http, redfish};

pub(crate) fn resource<'a>() -> redfish::Resource<'a> {
    redfish::Resource {
        odata_id: Cow::Borrowed("/redfish/v1/UpdateService"),
        odata_type: Cow::Borrowed("#UpdateService.v1_9_0.UpdateService"),
        id: Cow::Borrowed("UpdateService"),
        name: Cow::Borrowed("Update Service"),
    }
}

pub(crate) fn builder(resource: &redfish::Resource) -> UpdateServiceBuilder {
    UpdateServiceBuilder {
        value: resource.json_patch(),
    }
}

pub(crate) fn simple_update_target() -> String {
    format!("{}/Actions/UpdateService.SimpleUpdate", resource().odata_id)
}

/// AMI MegaRAC multipart upload endpoint (GenericAmi, DGX H100).
/// Also serves as the `MultipartHttpPushUri` advertised to GB200/GB300/Lenovo.
pub(crate) const MULTIPART_UPLOAD_PATH: &str = "/redfish/v1/UpdateService/upload";

pub(crate) fn add_routes(r: Router<BmcState>) -> Router<BmcState> {
    const FW_INVENTORY_ID: &str = "{fw_inventory_id}";
    r.route(&resource().odata_id, get(get_update_service))
        .route(&simple_update_target(), post(update_firmware_simple_update))
        .route(MULTIPART_UPLOAD_PATH, post(update_firmware_multipart))
        .route(
            &redfish::software_inventory::firmware_inventory_collection().odata_id,
            get(get_firmware_inventory_collection),
        )
        .route(
            &redfish::software_inventory::firmware_inventory_resource(FW_INVENTORY_ID).odata_id,
            get(get_firmware_inventory_resource),
        )
}

/// Default delay before a simulated firmware task transitions to Completed.
pub(crate) const DEFAULT_TASK_COMPLETION_DELAY: Duration = Duration::from_secs(2);

/// Default random jitter added to the completion delay.
pub(crate) const DEFAULT_TASK_COMPLETION_JITTER: Duration = Duration::from_secs(1);

pub(crate) struct UpdateServiceConfig {
    pub(crate) firmware_inventory: Vec<redfish::software_inventory::SoftwareInventory>,
    /// Ordered map of `component_id → target_version` representing the expected
    /// upgrade sequence.  machine-a-tron populates this from
    /// `desired_firmware_versions`.  Uploads without explicit Targets peek the
    /// first entry (non-destructively).  The entry is removed only when
    /// `apply_staged_firmware` successfully stages the version on PowerOn.
    ///
    /// Using `IndexMap` (ordered, key-addressed) instead of `VecDeque` so that
    /// upload retries cannot lose the pending entry: peek is non-destructive and
    /// `version_for_component` is a direct key lookup.
    pub(crate) pending_upgrades: IndexMap<String, String>,
    /// How long to wait before transitioning a task from `Running` to `Completed`.
    /// Add `task_completion_jitter` of random jitter to desynchronise resets
    /// across many simulated hosts in a load environment.
    pub(crate) task_completion_delay: Duration,
    pub(crate) task_completion_jitter: Duration,
    /// Which push URI to advertise in the UpdateService GET response.
    pub(crate) advertise_multipart_push_uri: bool,
    /// Inventory ID for the host BMC firmware entry.  `None` means this
    /// platform has no host firmware simulation (e.g. switches, power shelves).
    pub(crate) host_bmc_inventory_id: Option<String>,
    /// Inventory ID for the host UEFI/BIOS firmware entry.
    pub(crate) host_uefi_inventory_id: Option<String>,
}

impl Default for UpdateServiceConfig {
    fn default() -> Self {
        Self {
            firmware_inventory: Vec::new(),
            pending_upgrades: IndexMap::new(),
            task_completion_delay: DEFAULT_TASK_COMPLETION_DELAY,
            task_completion_jitter: DEFAULT_TASK_COMPLETION_JITTER,
            advertise_multipart_push_uri: true,
            host_bmc_inventory_id: None,
            host_uefi_inventory_id: None,
        }
    }
}

impl UpdateServiceConfig {
    pub(crate) fn apply_host_firmware_versions(
        &mut self,
        fw: &crate::machine_info::HostFirmwareVersions,
    ) {
        let mut upsert = |id: &str, version: &str| {
            if let Some(entry) = self.firmware_inventory.iter_mut().find(|e| e.id == id) {
                entry.set_version(version);
            } else {
                self.firmware_inventory.push(
                    redfish::software_inventory::builder(
                        &redfish::software_inventory::firmware_inventory_resource(id),
                    )
                    .version(version)
                    .build(),
                );
            }
        };
        if let (Some(id), Some(bmc)) = (&self.host_bmc_inventory_id, &fw.bmc) {
            upsert(id, bmc);
        }
        if let (Some(id), Some(uefi)) = (&self.host_uefi_inventory_id, &fw.uefi) {
            upsert(id, uefi);
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum TaskState {
    Running,
    Completed,
}

pub(crate) struct FirmwareTask {
    pub(crate) id: String,
    pub(crate) component_id: String,
    /// The target version stored on the task so the background completion
    /// closure can read it from the locked task map rather than capturing a
    /// separate clone.  Also used by the test-only `complete_all_tasks_for_test`
    /// helper.
    pub(crate) target_version: String,
    pub(crate) state: TaskState,
}

impl FirmwareTask {
    pub(crate) fn odata_id(&self) -> String {
        format!("/redfish/v1/TaskService/Tasks/{}", self.id)
    }

    pub(crate) fn to_json(&self) -> serde_json::Value {
        let (state_str, pct) = match self.state {
            TaskState::Running => ("Running", 0),
            TaskState::Completed => ("Completed", 100),
        };
        json!({
            "@odata.id": self.odata_id(),
            "@odata.type": "#Task.v1_4_3.Task",
            "Id": self.id,
            "PercentComplete": pct,
            "StartTime": "2024-01-30T09:00:52+00:00",
            "TaskMonitor": format!("{}/Monitor", self.odata_id()),
            "TaskState": state_str,
            // TaskStatus is always "OK" — "Warning" would signal a degraded
            // condition and cause carbide consumers to treat the upload as failed.
            "TaskStatus": "OK",
            "Messages": [{
                "MessageId": "Update.1.0.OperationTransitionedToJob",
                "Message": if self.state == TaskState::Completed {
                    "Firmware staged; version will be applied after the next power-cycle."
                } else {
                    "Firmware upload in progress."
                },
                "Severity": "OK"
            }]
        })
    }
}

pub struct UpdateServiceState {
    firmware_inventory: RwLock<HashMap<String, redfish::software_inventory::SoftwareInventory>>,
    /// Remaining expected upgrades, key=component_id, value=target_version.
    /// Peek gives the first entry without consuming; removal happens in
    /// `apply_staged_firmware` after the version is staged on PowerOn.
    pending_upgrades: RwLock<IndexMap<String, String>>,
    /// Staged (pending) firmware: component_id → target_version.
    /// Written atomically with the task Completed transition to avoid the race
    /// window where PowerOn fires between the two writes.
    staged_firmware: RwLock<HashMap<String, String>>,
    /// Live tasks keyed by task ID.  Pruned after `apply_staged_firmware` so the
    /// map does not grow without bound in long-running load tests.
    tasks: RwLock<HashMap<String, FirmwareTask>>,
    next_task_id: AtomicU64,
    task_completion_delay: Duration,
    task_completion_jitter: Duration,
    /// Which push URIs this platform advertises in the UpdateService GET response.
    pub(crate) advertise_multipart_push_uri: bool,
    /// Inventory ID for the host BMC firmware entry.  `None` means no host
    /// firmware simulation for this platform (e.g. switches, power shelves).
    pub host_bmc_inventory_id: Option<String>,
    /// Inventory ID for the host UEFI/BIOS firmware entry.
    pub host_uefi_inventory_id: Option<String>,
}

impl UpdateServiceState {
    pub(crate) fn from_config(config: UpdateServiceConfig) -> Self {
        let inventory = config
            .firmware_inventory
            .into_iter()
            .map(|sw| (sw.id.to_string(), sw))
            .collect();
        Self {
            firmware_inventory: RwLock::new(inventory),
            pending_upgrades: RwLock::new(config.pending_upgrades),
            staged_firmware: RwLock::new(HashMap::new()),
            tasks: RwLock::new(HashMap::new()),
            next_task_id: AtomicU64::new(1),
            task_completion_delay: config.task_completion_delay,
            task_completion_jitter: config.task_completion_jitter,
            advertise_multipart_push_uri: config.advertise_multipart_push_uri,
            host_bmc_inventory_id: config.host_bmc_inventory_id,
            host_uefi_inventory_id: config.host_uefi_inventory_id,
        }
    }

    pub fn find_firmware_inventory(&self, id: &str) -> Option<serde_json::Value> {
        self.firmware_inventory
            .read()
            .unwrap()
            .get(id)
            .map(|sw| sw.to_json())
    }

    /// Re-stage one component's upgrade target on a live mock. Only the
    /// pending target changes - active inventory and staged uploads are left
    /// alone. `Some(v)` stages `v` unless the component is already at (or an
    /// in-flight upload is already bringing it to) that version; `None` clears
    /// the target. The decision runs under the same tasks -> staged ->
    /// inventory -> pending lock order as `apply_staged_firmware`, so a
    /// concurrent apply either completes before or after the whole retarget.
    pub fn retarget_pending_upgrade(&self, component_id: &str, desired: Option<&str>) {
        let tasks = self.tasks.read().unwrap();
        let staged = self.staged_firmware.read().unwrap();
        let inventory = self.firmware_inventory.read().unwrap();
        let mut pending = self.pending_upgrades.write().unwrap();
        // The version this component ends up at once any staged or in-flight
        // upload applies: comparing against it (rather than the active
        // inventory alone) keeps a rollback target pending when an upload of
        // another version is still going to land.
        let effective_version = staged
            .get(component_id)
            .cloned()
            .or_else(|| {
                tasks
                    .values()
                    .filter(|t| t.component_id == component_id && !t.target_version.is_empty())
                    .map(|t| t.target_version.clone())
                    .next()
            })
            .or_else(|| {
                inventory
                    .get(component_id)
                    .and_then(|sw| sw.to_json()["Version"].as_str().map(str::to_owned))
            });
        match desired {
            Some(version) if effective_version.as_deref() != Some(version) => {
                pending.insert(component_id.to_string(), version.to_string());
            }
            _ => {
                pending.shift_remove(component_id);
            }
        }
    }

    pub(crate) fn all_firmware_inventory_ids(&self) -> Vec<String> {
        self.firmware_inventory
            .read()
            .unwrap()
            .keys()
            .cloned()
            .collect()
    }

    pub(crate) fn find_task(&self, id: &str) -> Option<serde_json::Value> {
        self.tasks.read().unwrap().get(id).map(|t| t.to_json())
    }

    /// Create a task in `Running` state for `component_id`.
    ///
    /// The task transitions to `Completed` after `task_completion_delay + jitter`
    /// via a background tokio task.  The task state and the staged firmware entry
    /// are written in the **same** critical section (single `tasks` write-lock
    /// acquisition) to eliminate the race window where `BmcEvent::PowerOn` fires
    /// between "task marked Completed" and "version inserted into staged_firmware".
    ///
    /// The `JoinHandle` is intentionally dropped (fire-and-forget); see the
    /// module-level doc comment for the rationale.
    fn record_upload(
        self: &std::sync::Arc<Self>,
        component_id: &str,
        target_version: String,
    ) -> (String, serde_json::Value) {
        let task_id = self
            .next_task_id
            .fetch_add(1, Ordering::Relaxed)
            .to_string();
        let task = FirmwareTask {
            id: task_id.clone(),
            component_id: component_id.to_string(),
            target_version: target_version.clone(),
            state: TaskState::Running,
        };
        let odata_id = task.odata_id();
        let running_json = task.to_json();
        self.tasks.write().unwrap().insert(task_id.clone(), task);

        if target_version.is_empty() {
            tracing::debug!(
                component_id,
                "firmware upload accepted but pending_upgrades queue was empty; \
                 no version will be staged — carbide will see no inventory change after PowerOn"
            );
        }

        let state = std::sync::Arc::clone(self);
        let delay = self.task_completion_delay;
        let jitter = self.task_completion_jitter;
        let component = component_id.to_string();
        // JoinHandle dropped intentionally — see module doc for rationale.
        drop(tokio::spawn(async move {
            // True random jitter to desynchronise resets across many simulated hosts.
            let jitter_ms = jitter.as_millis();
            let jitter_offset = if jitter_ms == 0 {
                0u64
            } else {
                rand::random::<u64>() % jitter_ms as u64
            };
            tokio::time::sleep(delay + Duration::from_millis(jitter_offset)).await;

            // Mark Completed and stage the version atomically under the tasks
            // write-lock.  Reading target_version from the task struct (rather than
            // capturing a separate clone) means apply_staged_firmware can never
            // observe a Completed task without a corresponding staged entry.
            let mut tasks = state.tasks.write().unwrap();
            if let Some(task) = tasks.get_mut(&task_id) {
                let version = task.target_version.clone();
                task.state = TaskState::Completed;
                if !version.is_empty() {
                    state
                        .staged_firmware
                        .write()
                        .unwrap()
                        .insert(component, version);
                }
            }
        }));

        (odata_id, running_json)
    }

    /// Peek the first pending upgrade without consuming it.
    ///
    /// Returns `None` when the queue is empty.  The caller uses the returned
    /// `(component_id, target_version)` to create a firmware task; the entry is
    /// only removed from the map after `apply_staged_firmware` confirms the
    /// version was staged successfully on PowerOn.  This makes upload retries
    /// safe: a failed or repeated upload keeps the same entry available.
    fn peek_pending(&self) -> Option<(String, String)> {
        self.pending_upgrades
            .read()
            .unwrap()
            .iter()
            .next()
            .map(|(k, v)| (k.clone(), v.clone()))
    }

    /// Look up the desired version for a component **without** consuming the
    /// queue entry.  Used by SimpleUpdate with explicit Targets so that a later
    /// multipart upload for a different component can still peek its own entry.
    fn version_for_component(&self, component_id: &str) -> String {
        self.pending_upgrades
            .read()
            .unwrap()
            .get(component_id)
            .cloned()
            .unwrap_or_default()
    }

    /// Force all Running tasks to Completed and stage their firmware versions
    /// immediately.  Only compiled in test builds; avoids wall-clock waits in
    /// HTTP-level tests (the async lifecycle is tested in the unit tests).
    #[cfg(test)]
    pub fn complete_all_tasks_for_test(&self) {
        let to_stage: Vec<(String, String)> = {
            let mut tasks = self.tasks.write().unwrap();
            tasks
                .values_mut()
                .filter(|t| t.state == TaskState::Running)
                .map(|t| {
                    t.state = TaskState::Completed;
                    (t.component_id.clone(), t.target_version.clone())
                })
                .collect()
        };
        let mut staged = self.staged_firmware.write().unwrap();
        for (component, version) in to_stage {
            if !version.is_empty() {
                staged.insert(component, version);
            }
        }
    }

    /// Apply staged firmware versions to the active inventory.
    /// Only fires for components that have completed tasks (the gate that prevents
    /// initial-boot power-on from applying staged versions prematurely).
    ///
    /// **Simulation simplification**: all staged versions are applied here
    /// regardless of component type.  In reality, UEFI updates require a host
    /// restart and BMC updates require `Manager.Reset`; those are currently
    /// no-ops in bmc-mock.  Per-component activation events can be added later.
    ///
    /// Completed tasks are pruned from the map after their firmware is applied
    /// so the map does not grow without bound in long-running load tests.
    /// Successfully applied components are also removed from `pending_upgrades`
    /// so a subsequent peek returns the next entry in the ordered map.
    pub(crate) fn apply_staged_firmware(&self) {
        // Collect component IDs of completed tasks and prune them from the map.
        let completed_components: Vec<String> = {
            let mut tasks = self.tasks.write().unwrap();
            let completed: Vec<String> = tasks
                .values()
                .filter(|t| t.state == TaskState::Completed)
                .map(|t| t.component_id.clone())
                .collect();
            tasks.retain(|_, t| t.state != TaskState::Completed);
            completed
        };

        if completed_components.is_empty() {
            return;
        }

        let mut staged = self.staged_firmware.write().unwrap();
        let mut inventory = self.firmware_inventory.write().unwrap();
        let mut pending = self.pending_upgrades.write().unwrap();

        for component_id in &completed_components {
            match staged.remove(component_id) {
                Some(target_version) => {
                    if let Some(entry) = inventory.get_mut(component_id) {
                        entry.set_version(&target_version);
                        // Only remove the pending entry when the applied
                        // version is still the target: a retarget that landed
                        // mid-upload must survive for the next upload pass.
                        if pending.get(component_id.as_str()) == Some(&target_version) {
                            pending.shift_remove(component_id);
                        }
                    } else {
                        tracing::warn!(
                            component_id,
                            target_version,
                            "staged firmware has no matching inventory entry; version lost"
                        );
                    }
                }
                None => {
                    tracing::debug!(
                        component_id,
                        "completed task had no staged version (upload had empty target)"
                    );
                }
            }
        }
    }
}

/// Drain a request body frame-by-frame without buffering to avoid memory
/// pressure when handling large firmware images.  Errors are silently ignored
/// — the mock does not inspect the uploaded content.
async fn discard_body(body: Body) {
    use http_body_util::BodyExt as _;
    let mut body = body;
    while body.frame().await.is_some() {}
}

#[derive(Deserialize, Default)]
struct SimpleUpdateRequest {
    #[serde(rename = "Targets", default)]
    targets: Vec<String>,
}

/// Advertise only the push URI(s) that this platform's BMC actually supports.
async fn get_update_service(State(state): State<BmcState>) -> Response {
    let us = &state.update_service_state;
    let mut b = builder(&resource())
        .firmware_inventory(&redfish::software_inventory::firmware_inventory_collection());
    if us.advertise_multipart_push_uri {
        b = b.multipart_http_push_uri(MULTIPART_UPLOAD_PATH);
    }
    b.build().into_ok_response()
}

/// Redfish SimpleUpdate (Dell iDRAC, BFB/DPU path).
async fn update_firmware_simple_update(
    State(state): State<BmcState>,
    body: Option<axum::Json<SimpleUpdateRequest>>,
) -> Response {
    let targets = body.map(|b| b.0.targets).unwrap_or_default();

    // Infer component from the first Redfish target path.
    // e.g. `/redfish/v1/UpdateService/FirmwareInventory/HostBMC_0` → `HostBMC_0`
    let (component_id, target_version) = if let Some(id) = targets
        .first()
        .and_then(|t| t.rsplit('/').next())
        .map(str::to_owned)
    {
        // Targets is explicit: look up the desired version WITHOUT consuming the
        // pending_upgrades map so a follow-up multipart upload for a different
        // component can still peek its own entry.
        let version = state.update_service_state.version_for_component(&id);
        (id, version)
    } else {
        // No Targets: peek from the ordered map (same path as multipart).
        state
            .update_service_state
            .peek_pending()
            .unwrap_or_else(|| ("unknown".to_string(), String::new()))
    };

    upload_response(&state.update_service_state, &component_id, target_version)
}

/// Multipart upload (AMI `UpdateService/upload`, GB200/GB300 `MultipartHttpPushUri`).
async fn update_firmware_multipart(
    State(state): State<BmcState>,
    body: axum::extract::Request,
) -> Response {
    discard_body(body.into_body()).await;
    let (component_id, target_version) = state
        .update_service_state
        .peek_pending()
        .unwrap_or_else(|| ("unknown".to_string(), String::new()));
    upload_response(&state.update_service_state, &component_id, target_version)
}

/// Shared upload response helper.
///
/// Returns 202 Accepted with:
/// - `Location` header — consumed by Dell/DGX H100 to retrieve the task ID.
/// - Full Task JSON body in `Running` state — consumed by GB200/raw HttpPush paths.
fn upload_response(
    update_state: &std::sync::Arc<UpdateServiceState>,
    component_id: &str,
    target_version: String,
) -> Response {
    let (task_odata_id, running_task_json) =
        update_state.record_upload(component_id, target_version);

    // Use into_response(ACCEPTED) + Location header.  into_response_with_location
    // sets 200; we override the status after insertion.
    let location = axum::http::HeaderValue::from_str(&task_odata_id).unwrap_or_else(|_| {
        axum::http::HeaderValue::from_static("/redfish/v1/TaskService/Tasks/0")
    });
    let mut response = running_task_json.into_ok_response_with_location(location);
    *response.status_mut() = axum::http::StatusCode::ACCEPTED;
    response
}

async fn get_firmware_inventory_collection(State(state): State<BmcState>) -> Response {
    let ids = state.update_service_state.all_firmware_inventory_ids();
    let members = ids
        .iter()
        .map(|id| redfish::software_inventory::firmware_inventory_resource(id).entity_ref())
        .collect::<Vec<_>>();
    redfish::software_inventory::firmware_inventory_collection()
        .with_members(&members)
        .into_ok_response()
}

async fn get_firmware_inventory_resource(
    State(state): State<BmcState>,
    Path(fw_inventory_id): Path<String>,
) -> Response {
    state
        .update_service_state
        .find_firmware_inventory(&fw_inventory_id)
        .map(|json| json.into_ok_response())
        .unwrap_or_else(http::not_found)
}

pub(crate) struct UpdateServiceBuilder {
    value: serde_json::Value,
}

impl Builder for UpdateServiceBuilder {
    fn apply_patch(self, patch: serde_json::Value) -> Self {
        Self {
            value: self.value.patch(patch),
        }
    }
}

impl UpdateServiceBuilder {
    pub(crate) fn build(self) -> serde_json::Value {
        self.value
    }

    pub(crate) fn firmware_inventory(self, v: &redfish::Collection<'_>) -> Self {
        self.apply_patch(v.nav_property("FirmwareInventory"))
    }

    /// Multipart HTTP push URI (AMI/GB200/GB300 consumers).
    pub(crate) fn multipart_http_push_uri(self, uri: &str) -> Self {
        self.apply_patch(json!({ "MultipartHttpPushUri": uri }))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::redfish::software_inventory;

    fn make_state(
        entries: &[(&'static str, &'static str)],
        pending: &[(&'static str, &'static str)],
    ) -> Arc<UpdateServiceState> {
        let inventory = entries
            .iter()
            .map(|(id, v)| {
                software_inventory::builder(&software_inventory::firmware_inventory_resource(id))
                    .version(v)
                    .build()
            })
            .collect();
        Arc::new(UpdateServiceState::from_config(UpdateServiceConfig {
            firmware_inventory: inventory,
            pending_upgrades: pending
                .iter()
                .map(|(c, v)| (c.to_string(), v.to_string()))
                .collect(),
            task_completion_delay: Duration::ZERO,
            task_completion_jitter: Duration::ZERO,
            ..Default::default()
        }))
    }

    fn pending_pairs(state: &UpdateServiceState) -> Vec<(String, String)> {
        let pending = state.pending_upgrades.read().unwrap();
        pending
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    #[test]
    fn retarget_stages_clears_and_scopes_pending_targets() {
        let state = make_state(
            &[("BMC_Firmware", "24.09"), ("UEFI", "1.0")],
            &[("BMC_Firmware", "24.09"), ("UEFI", "1.1")],
        );
        // Drift vs active: stages the new target; other components untouched.
        state.retarget_pending_upgrade("BMC_Firmware", Some("24.10"));
        assert_eq!(
            pending_pairs(&state),
            vec![
                ("BMC_Firmware".to_string(), "24.10".to_string()),
                ("UEFI".to_string(), "1.1".to_string()),
            ]
        );
        // Active inventory is never touched by a retarget.
        let active = state.find_firmware_inventory("BMC_Firmware").unwrap();
        assert_eq!(active["Version"], "24.09");
        // Already at target: the pending entry clears (no re-queue).
        state.retarget_pending_upgrade("BMC_Firmware", Some("24.09"));
        // None withdraws a target.
        state.retarget_pending_upgrade("UEFI", None);
        assert!(pending_pairs(&state).is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn retarget_to_active_keeps_target_while_upload_in_flight() {
        // Rollback race: active A, an upload of B in flight, desired flips
        // back to A. The pending A entry must survive - once B applies, the
        // next upload pass needs it to stage the rollback.
        let state = make_state(&[("BMC_Firmware", "A")], &[("BMC_Firmware", "B")]);
        state.record_upload("BMC_Firmware", "B".to_string());
        state.retarget_pending_upgrade("BMC_Firmware", Some("A"));
        assert_eq!(
            pending_pairs(&state),
            vec![("BMC_Firmware".to_string(), "A".to_string())]
        );
        // B still applies, then A remains the pending rollback target.
        tokio::time::sleep(Duration::from_millis(1)).await;
        tokio::task::yield_now().await;
        state.apply_staged_firmware();
        let active = state.find_firmware_inventory("BMC_Firmware").unwrap();
        assert_eq!(active["Version"], "B");
        assert_eq!(
            pending_pairs(&state),
            vec![("BMC_Firmware".to_string(), "A".to_string())]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn retarget_does_not_disturb_staged_uploads() {
        // An upload already staged for the old target stays staged: the
        // realistic sequence is old version applies on the next power cycle,
        // then the new drift is observed and upgraded in a later pass.
        let state = make_state(&[("BMC_Firmware", "24.09")], &[("BMC_Firmware", "24.10")]);
        state.record_upload("BMC_Firmware", "24.10".to_string());
        tokio::time::sleep(Duration::from_millis(1)).await;
        tokio::task::yield_now().await;
        state.retarget_pending_upgrade("BMC_Firmware", Some("24.11"));
        state.apply_staged_firmware();
        let active = state.find_firmware_inventory("BMC_Firmware").unwrap();
        assert_eq!(active["Version"], "24.10", "staged upload still applies");
        // 24.11 remains pending because the freshly-applied 24.10 != 24.11.
        assert_eq!(
            pending_pairs(&state),
            vec![("BMC_Firmware".to_string(), "24.11".to_string())]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn task_starts_running_then_completes() {
        let state = make_state(&[("HostBMC_0", "24.09.17")], &[("HostBMC_0", "24.10.00")]);
        let (task_path, running_json) = state.record_upload("HostBMC_0", "24.10.00".into());
        assert_eq!(running_json["TaskState"], "Running");
        assert_eq!(
            running_json["TaskStatus"], "OK",
            "Running tasks must emit OK not Warning"
        );
        assert_eq!(running_json["PercentComplete"], 0);

        // Yield to the runtime so the spawned task can run (delay=ZERO, no advance needed).
        tokio::task::yield_now().await;

        let task_id = task_path.rsplit('/').next().unwrap();
        let completed = state.find_task(task_id).unwrap();
        assert_eq!(completed["TaskState"], "Completed");
        assert_eq!(completed["TaskStatus"], "OK");
        assert_eq!(completed["PercentComplete"], 100);
    }

    #[tokio::test(start_paused = true)]
    async fn firmware_staged_only_after_task_completes() {
        let state = make_state(&[("HostBMC_0", "24.09.17")], &[("HostBMC_0", "24.10.00")]);
        state.record_upload("HostBMC_0", "24.10.00".into());

        // Still old immediately — task is Running.
        assert_eq!(
            state.find_firmware_inventory("HostBMC_0").unwrap()["Version"],
            "24.09.17"
        );
        state.apply_staged_firmware();
        assert_eq!(
            state.find_firmware_inventory("HostBMC_0").unwrap()["Version"],
            "24.09.17",
            "must not change until task Completed"
        );

        tokio::task::yield_now().await;

        state.apply_staged_firmware();
        assert_eq!(
            state.find_firmware_inventory("HostBMC_0").unwrap()["Version"],
            "24.10.00"
        );
    }

    #[tokio::test]
    async fn apply_staged_noop_without_completed_tasks() {
        let state = make_state(&[("HostBMC_0", "24.09.17")], &[("HostBMC_0", "24.10.00")]);
        state.apply_staged_firmware();
        assert_eq!(
            state.find_firmware_inventory("HostBMC_0").unwrap()["Version"],
            "24.09.17"
        );
    }

    /// PowerOn fires while the upload task is still Running (task not yet Completed).
    /// The version must not be applied; it must be applied on the *next* PowerOn
    /// after the task completes.
    #[tokio::test(start_paused = true)]
    async fn power_on_while_task_running_does_not_apply_version() {
        let state = make_state(&[("HostBMC_0", "24.09.17")], &[("HostBMC_0", "24.10.00")]);
        state.record_upload("HostBMC_0", "24.10.00".into());

        // PowerOn fires before the background task runs.
        state.apply_staged_firmware();
        assert_eq!(
            state.find_firmware_inventory("HostBMC_0").unwrap()["Version"],
            "24.09.17",
            "must not apply while task is Running"
        );

        tokio::task::yield_now().await;

        state.apply_staged_firmware();
        assert_eq!(
            state.find_firmware_inventory("HostBMC_0").unwrap()["Version"],
            "24.10.00"
        );
    }

    /// A second upload for the same component while the first task is still Running
    /// must overwrite the staged version (last-upload-wins).
    #[tokio::test(start_paused = true)]
    async fn double_upload_last_wins() {
        let state = make_state(&[("HostBMC_0", "24.09.17")], &[]);
        state.record_upload("HostBMC_0", "24.10.00".into());
        state.record_upload("HostBMC_0", "24.11.00".into());

        tokio::task::yield_now().await;

        state.apply_staged_firmware();
        assert_eq!(
            state.find_firmware_inventory("HostBMC_0").unwrap()["Version"],
            "24.11.00",
            "last upload version must win"
        );
    }

    /// Completed tasks are pruned from the map after apply_staged_firmware.
    #[tokio::test(start_paused = true)]
    async fn completed_tasks_pruned_after_apply() {
        let state = make_state(&[("HostBMC_0", "24.09.17")], &[]);
        state.record_upload("HostBMC_0", "24.10.00".into());
        tokio::task::yield_now().await;

        assert_eq!(state.tasks.read().unwrap().len(), 1);
        state.apply_staged_firmware();
        assert_eq!(
            state.tasks.read().unwrap().len(),
            0,
            "completed task must be pruned"
        );
    }

    #[test]
    fn queue_operations() {
        // peek_pending returns the first entry without consuming it.
        let state = make_state(
            &[("HostBMC_0", "24.09.17"), ("HostBIOS_0", "01.05.03")],
            &[("HostBMC_0", "24.10.00"), ("HostBIOS_0", "01.06.00")],
        );

        // Multiple peeks all return the same first entry (non-destructive).
        assert_eq!(
            state.peek_pending(),
            Some(("HostBMC_0".to_string(), "24.10.00".to_string())),
            "first peek"
        );
        assert_eq!(
            state.peek_pending(),
            Some(("HostBMC_0".to_string(), "24.10.00".to_string())),
            "second peek must return same entry (non-destructive)"
        );

        // version_for_component is a direct key lookup, does not consume.
        assert_eq!(state.version_for_component("HostBMC_0"), "24.10.00");
        assert_eq!(state.version_for_component("HostBIOS_0"), "01.06.00");
        assert_eq!(
            state.version_for_component("NonExistent"),
            "",
            "missing key → empty"
        );

        // After peeking, entry is still present.
        assert_eq!(
            state.peek_pending(),
            Some(("HostBMC_0".to_string(), "24.10.00".to_string())),
            "peek after version_for_component still returns first entry"
        );
    }

    /// After apply_staged_firmware removes an entry, peek returns the next one.
    #[tokio::test(start_paused = true)]
    async fn pending_entry_removed_after_apply() {
        let state = make_state(
            &[("HostBMC_0", "24.09.17"), ("HostBIOS_0", "01.05.03")],
            &[("HostBMC_0", "24.10.00"), ("HostBIOS_0", "01.06.00")],
        );

        // Upload BMC, complete it, then apply.
        state.record_upload("HostBMC_0", "24.10.00".into());
        tokio::task::yield_now().await;
        state.apply_staged_firmware();

        // BMC upgraded and removed from pending; BIOS is now first.
        assert_eq!(
            state.find_firmware_inventory("HostBMC_0").unwrap()["Version"],
            "24.10.00"
        );
        assert_eq!(
            state.peek_pending(),
            Some(("HostBIOS_0".to_string(), "01.06.00".to_string())),
            "after BMC removed, BIOS becomes first pending entry"
        );
    }

    #[test]
    fn apply_host_firmware_versions_adds_and_overrides() {
        let mut config = UpdateServiceConfig {
            host_bmc_inventory_id: Some("HostBMC_0".to_string()),
            host_uefi_inventory_id: Some("HostBIOS_0".to_string()),
            ..Default::default()
        };
        config.apply_host_firmware_versions(&crate::machine_info::HostFirmwareVersions {
            bmc: Some("24.09.17".into()),
            uefi: Some("01.05.03".into()),
        });
        assert_eq!(config.firmware_inventory.len(), 2);
        config.apply_host_firmware_versions(&crate::machine_info::HostFirmwareVersions {
            bmc: Some("24.10.00".into()),
            uefi: None,
        });
        assert_eq!(config.firmware_inventory.len(), 2);
        assert_eq!(
            config
                .firmware_inventory
                .iter()
                .find(|e| e.id == "HostBMC_0")
                .unwrap()
                .to_json()["Version"],
            "24.10.00"
        );
        assert_eq!(
            config
                .firmware_inventory
                .iter()
                .find(|e| e.id == "HostBIOS_0")
                .unwrap()
                .to_json()["Version"],
            "01.05.03"
        );
    }

    /// apply_host_firmware_versions is a no-op when inventory IDs are None.
    #[test]
    fn apply_host_firmware_versions_no_ids_is_noop() {
        let mut config = UpdateServiceConfig::default(); // host_bmc_inventory_id = None
        config.apply_host_firmware_versions(&crate::machine_info::HostFirmwareVersions {
            bmc: Some("24.09.17".into()),
            uefi: Some("01.05.03".into()),
        });
        assert!(
            config.firmware_inventory.is_empty(),
            "no IDs → no inventory entries added"
        );
    }

    use std::sync::Arc as StdArc;

    use axum::body::to_bytes;
    use axum::http::{Method, Request, StatusCode};
    use tower::ServiceExt;

    use crate::machine_info::HostFirmwareVersions;
    use crate::test_support::{NoopCallbacks, host_info};
    use crate::{HardwareType, MachineRouterOptions, machine_router};

    fn make_router(
        bmc_current: &str,
        bmc_desired: &str,
    ) -> (axum::Router, crate::bmc_state::BmcState) {
        make_router_with_uefi(bmc_current, bmc_desired, None, None)
    }

    fn make_router_with_uefi(
        bmc_current: &str,
        bmc_desired: &str,
        uefi_current: Option<&str>,
        uefi_desired: Option<&str>,
    ) -> (axum::Router, crate::bmc_state::BmcState) {
        let info = host_info(HardwareType::GenericAmi);
        let info = if let crate::MachineInfo::Host(mut h) = info {
            h.initial_host_firmware = Some(HostFirmwareVersions {
                bmc: Some(bmc_current.into()),
                uefi: uefi_current.map(Into::into),
            });
            h.desired_host_firmware = Some(HostFirmwareVersions {
                bmc: Some(bmc_desired.into()),
                uefi: uefi_desired.map(Into::into),
            });
            crate::MachineInfo::Host(h)
        } else {
            info
        };
        machine_router(
            &info,
            StdArc::new(NoopCallbacks),
            "test".into(),
            false,
            MachineRouterOptions::default(),
        )
    }

    async fn get_json(r: &axum::Router, path: &str) -> serde_json::Value {
        let resp = r
            .clone()
            .oneshot(
                Request::builder()
                    .uri(path)
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "GET {path}");
        serde_json::from_slice(&to_bytes(resp.into_body(), usize::MAX).await.unwrap()).unwrap()
    }

    async fn post_empty(r: &axum::Router, path: &str) -> axum::response::Response {
        r.clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(path)
                    .header("Content-Type", "application/json")
                    .body(axum::body::Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn update_service_advertises_multipart_push_uri_for_ami() {
        // GenericAmi only advertises MultipartHttpPushUri (not HttpPushUri).
        let (router, _) = make_router("24.09.17", "24.10.00");
        let svc = get_json(&router, "/redfish/v1/UpdateService").await;
        assert_eq!(svc["MultipartHttpPushUri"], MULTIPART_UPLOAD_PATH);
        assert!(
            svc["HttpPushUri"].is_null(),
            "GenericAmi must not advertise HttpPushUri"
        );
    }

    #[tokio::test]
    async fn upload_returns_202_with_location_and_running_task_body() {
        let (router, _) = make_router("24.09.17", "24.10.00");
        let resp = post_empty(&router, MULTIPART_UPLOAD_PATH).await;
        assert_eq!(resp.status(), StatusCode::ACCEPTED);
        assert!(
            resp.headers().contains_key("Location"),
            "Location header missing"
        );
        let body: serde_json::Value =
            serde_json::from_slice(&to_bytes(resp.into_body(), usize::MAX).await.unwrap()).unwrap();
        assert_eq!(body["TaskState"], "Running");
        assert_eq!(
            body["TaskStatus"], "OK",
            "Running task must not emit Warning"
        );
    }

    #[tokio::test]
    async fn full_upgrade_lifecycle_via_multipart() {
        let (router, bmc_state) = make_router("24.09.17", "24.10.00");

        let inv = get_json(
            &router,
            "/redfish/v1/UpdateService/FirmwareInventory/HostBMC_0",
        )
        .await;
        assert_eq!(inv["Version"], "24.09.17");

        post_empty(&router, MULTIPART_UPLOAD_PATH).await;

        // Still old — task is Running.
        let inv = get_json(
            &router,
            "/redfish/v1/UpdateService/FirmwareInventory/HostBMC_0",
        )
        .await;
        assert_eq!(
            inv["Version"], "24.09.17",
            "must stay old while task is Running"
        );

        // Force-complete the task (async lifecycle is tested in unit tests above).
        bmc_state.update_service_state.complete_all_tasks_for_test();

        bmc_state.on_event(&crate::bmc_state::BmcEvent::PowerOn);

        let inv = get_json(
            &router,
            "/redfish/v1/UpdateService/FirmwareInventory/HostBMC_0",
        )
        .await;
        assert_eq!(inv["Version"], "24.10.00");
    }

    #[tokio::test]
    async fn initial_power_on_does_not_apply_staged_firmware() {
        let (_, bmc_state) = make_router("24.09.17", "24.10.00");
        bmc_state.on_event(&crate::bmc_state::BmcEvent::PowerOn);
        let inv = bmc_state
            .update_service_state
            .find_firmware_inventory("HostBMC_0")
            .unwrap();
        assert_eq!(inv["Version"], "24.09.17");
    }

    /// SimpleUpdate with explicit Targets must not consume the pending_upgrades
    /// map — a follow-up multipart upload should still peek BMC from the map.
    /// The BIOS entry is never uploaded so it retains its initial version.
    #[tokio::test]
    async fn simple_update_with_targets_does_not_consume_queue() {
        let (router, bmc_state) =
            make_router_with_uefi("24.09.17", "24.10.00", Some("01.05.03"), Some("01.06.00"));

        // SimpleUpdate targeting BMC explicitly — must NOT consume queue.
        let resp = router
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(simple_update_target())
                    .header("Content-Type", "application/json")
                    .body(axum::body::Body::from(
                        r#"{"Targets":["/redfish/v1/UpdateService/FirmwareInventory/HostBMC_0"]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::ACCEPTED);

        // Multipart upload — peeks BMC from the map (SimpleUpdate did not consume it).
        let resp = post_empty(&router, MULTIPART_UPLOAD_PATH).await;
        assert_eq!(resp.status(), StatusCode::ACCEPTED);

        bmc_state.update_service_state.complete_all_tasks_for_test();
        bmc_state.on_event(&crate::bmc_state::BmcEvent::PowerOn);

        // BMC upgraded (both SimpleUpdate and multipart created tasks for it).
        let bmc = get_json(
            &router,
            "/redfish/v1/UpdateService/FirmwareInventory/HostBMC_0",
        )
        .await;
        assert_eq!(bmc["Version"], "24.10.00");

        // BIOS was never uploaded — must retain the initial version.
        let bios = get_json(
            &router,
            "/redfish/v1/UpdateService/FirmwareInventory/HostBIOS_0",
        )
        .await;
        assert_eq!(
            bios["Version"], "01.05.03",
            "BIOS was never uploaded; must be unchanged"
        );
    }
}

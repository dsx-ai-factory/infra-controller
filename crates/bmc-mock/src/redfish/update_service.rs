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
//! A BMC reset applies only completed host BMC firmware. Host power-on applies
//! all completed staged firmware, preserving the simulated cold-boot behavior.
//! UEFI firmware cannot activate on a BMC-only reset.
//!
//! ### Non-destructive queue
//! `pending_upgrades` is an `IndexMap<component_id, target_version>` (ordered,
//! key-addressed).  Uploads `peek` the first entry without consuming it.  The
//! entry is only removed when `apply_staged_firmware` successfully stages the
//! version on PowerOn.  This makes upload retries safe: a failed or repeated
//! upload keeps the same queue entry available for the next attempt.
//!
//! ### BlueField OS install (BFB)
//! On a BlueField BMC (`UpdateServiceConfig::dpu_os`), a SimpleUpdate that
//! targets `DPU_OS` installs a BFB instead of staging firmware. After the
//! transfer delay the BMC restarts the DPU, as the rshim push does on real
//! hardware. The install lands, and its task completes, on the DPU's next
//! power-on. Each change of the installed image advances a generation counter,
//! and `BootProgress` reports `OSRunning` only once the DPU has booted the
//! current generation.
//!
//! ### Task records
//! Task IDs are `{epoch}-{n}`, where `epoch` is random per BMC instance, so an
//! ID issued before a simulator restart never names a later task. A task that
//! finished and is no longer needed for staging moves to a bounded history
//! (`TASK_HISTORY_CAPACITY` per BMC, oldest out first). A lookup returns a live
//! or remembered task and 404 for any other ID; the mock never reports a task
//! it did not run. NICo polls a task to a terminal state before it starts the
//! next one on the same BMC, so the history outlives every in-repo poll loop.
//! Host firmware consumers already treat a 404 as "check the inventory
//! version", as they do on hardware that drops finished jobs.
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
use std::collections::{HashMap, VecDeque};
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
use crate::json::JsonExt;
use crate::redfish::ModelExt;
use crate::{Callbacks, http, redfish};

pub(crate) fn resource<'a>() -> redfish::Resource<'a> {
    redfish::Resource {
        odata_id: Cow::Borrowed("/redfish/v1/UpdateService"),
        odata_type: Cow::Borrowed(redfish::schema::update_service::UpdateService::ODATA_TYPE),
        id: Cow::Borrowed("UpdateService"),
        name: Cow::Borrowed("Update Service"),
    }
}

pub(crate) fn builder(
    resource: &redfish::Resource,
) -> redfish::schema::update_service::UpdateService {
    redfish::schema::update_service::UpdateService::new(
        resource.odata_id.as_ref(),
        resource.id.as_ref(),
        resource.name.as_ref(),
    )
}

pub(crate) fn simple_update_target() -> String {
    format!("{}/Actions/UpdateService.SimpleUpdate", resource().odata_id)
}

/// AMI MegaRAC multipart upload endpoint (GenericAmi, DGX H100).
/// Also serves as the `MultipartHttpPushUri` advertised to GB200/GB300/Lenovo.
pub(crate) const MULTIPART_UPLOAD_PATH: &str = "/redfish/v1/UpdateService/upload";

pub(crate) fn add_routes<C: Callbacks>(r: Router<BmcState<C>>) -> Router<BmcState<C>> {
    const FW_INVENTORY_ID: &str = "{fw_inventory_id}";
    r.route(
        &resource().odata_id,
        get(get_update_service::<C>).post(update_firmware_multipart::<C>),
    )
    .route(
        &simple_update_target(),
        post(update_firmware_simple_update::<C>),
    )
    .route(MULTIPART_UPLOAD_PATH, post(update_firmware_multipart::<C>))
    .route(
        &redfish::software_inventory::firmware_inventory_collection().odata_id,
        get(get_firmware_inventory_collection::<C>),
    )
    .route(
        &redfish::software_inventory::firmware_inventory_resource(FW_INVENTORY_ID).odata_id,
        get(get_firmware_inventory_resource::<C>),
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
    pub(crate) advertise_legacy_http_push_uri: bool,
    /// Inventory ID for the host BMC firmware entry.  `None` means this
    /// platform has no host firmware simulation (e.g. switches, power shelves).
    pub(crate) host_bmc_inventory_id: Option<String>,
    /// Inventory ID for the host UEFI/BIOS firmware entry.
    pub(crate) host_uefi_inventory_id: Option<String>,
    /// Whether this BMC manages a BlueField OS, so a SimpleUpdate targeting
    /// `DPU_OS` installs a BFB and the system reports `BootProgress`.
    pub(crate) dpu_os: bool,
}

impl Default for UpdateServiceConfig {
    fn default() -> Self {
        Self {
            firmware_inventory: Vec::new(),
            pending_upgrades: IndexMap::new(),
            task_completion_delay: DEFAULT_TASK_COMPLETION_DELAY,
            task_completion_jitter: DEFAULT_TASK_COMPLETION_JITTER,
            advertise_multipart_push_uri: true,
            advertise_legacy_http_push_uri: false,
            host_bmc_inventory_id: None,
            host_uefi_inventory_id: None,
            dpu_os: false,
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
    /// The operation failed; the payload is the task message.
    Exception(String),
}

impl TaskState {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Running => "Running",
            Self::Completed => "Completed",
            Self::Exception(_) => "Exception",
        }
    }
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
    /// Which host firmware component this task updates, resolved from
    /// `component_id` against the platform's inventory ids when the upload is
    /// recorded. It decides which event activates the staged version (a BMC
    /// reset for the BMC, the host's next power-on for the rest) and names
    /// that event in the task message.
    pub(crate) component: FirmwareComponent,
}

/// The host firmware component a task targets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FirmwareComponent {
    Bmc,
    Uefi,
    /// A BFB install on a BlueField. It completes on the DPU's power-on, not
    /// on a timer, and stages no firmware version.
    DpuOs,
    /// Any other inventory entry (a DPU component appended by the platform, or
    /// an id the platform does not recognise).
    Other,
}

impl FirmwareTask {
    pub(crate) fn odata_id(&self) -> String {
        format!("/redfish/v1/TaskService/Tasks/{}", self.id)
    }

    pub(crate) fn to_json(&self) -> serde_json::Value {
        let (pct, status) = match self.state {
            TaskState::Running => (0, "OK"),
            TaskState::Completed => (100, "OK"),
            TaskState::Exception(_) => (100, "Critical"),
        };
        json!({
            "@odata.id": self.odata_id(),
            "@odata.type": "#Task.v1_4_3.Task",
            "Id": self.id,
            "PercentComplete": pct,
            "StartTime": "2024-01-30T09:00:52+00:00",
            "TaskMonitor": format!("{}/Monitor", self.odata_id()),
            "TaskState": self.state.as_str(),
            // A running or completed task reports "OK" — "Warning" would signal
            // a degraded condition and cause carbide consumers to treat the
            // upload as failed.
            "TaskStatus": status,
            "Messages": [{
                "MessageId": "Update.1.0.OperationTransitionedToJob",
                "Message": match (&self.state, self.component) {
                    (TaskState::Exception(message), _) => message.as_str(),
                    (TaskState::Running, FirmwareComponent::DpuOs) => "BFB install in progress.",
                    (TaskState::Running, _) => "Firmware upload in progress.",
                    (TaskState::Completed, FirmwareComponent::DpuOs) => {
                        "BFB installed; the DPU restarted into the new image."
                    }
                    (TaskState::Completed, FirmwareComponent::Bmc) => {
                        "Firmware staged; version will be applied after the next BMC reset."
                    }
                    (TaskState::Completed, _) => {
                        "Firmware staged; version will be applied after the next host power-on."
                    }
                },
                "Severity": status
            }]
        })
    }
}

/// Finished tasks each BMC remembers for later lookups.
const TASK_HISTORY_CAPACITY: usize = 32;

/// Inventory ID that a BFB install targets on a BlueField BMC.
const DPU_OS_INVENTORY_ID: &str = "DPU_OS";

/// File name of the vanilla BFB that NICo installs to decommission a DPU.
const PRE_INGESTION_BFB: &str = "preingestion.bfb";

/// The OS image on a BlueField's disk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DpuOsImage {
    /// A NICo-managed image that runs the DPU agent: `forge.bfb`, a DPF
    /// bundle, or the agent image a netboot installs.
    Managed,
    /// The vanilla pre-ingestion BFB that decommissioning installs. It runs no
    /// NICo agent.
    PreIngestion,
}

impl DpuOsImage {
    /// Classify a SimpleUpdate `ImageURI` by its file name. Any BFB other than
    /// the pre-ingestion image is a managed image.
    fn from_image_uri(image_uri: &str) -> Self {
        if image_uri.rsplit('/').next() == Some(PRE_INGESTION_BFB) {
            Self::PreIngestion
        } else {
            Self::Managed
        }
    }
}

/// What a BlueField BMC knows about the DPU's disk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DpuOsStatus {
    /// The installed image, or `None` for an empty disk.
    pub image: Option<DpuOsImage>,
    /// Number of times the installed image has changed. A new value means a
    /// new install; consumers compare it with the last value they saw.
    pub generation: u64,
}

/// Why a BFB install was not accepted.
#[derive(Debug, PartialEq, Eq)]
enum BfbInstallRefused {
    /// This BMC manages no DPU OS.
    NoDpuOs,
    /// An earlier install is restarting the DPU and has not landed yet.
    RestartInProgress,
}

/// Read-only access to a BlueField's [`DpuOsStatus`] that does not keep the
/// BMC alive, so an embedder can observe the disk without a lifetime cycle.
#[derive(Clone, Debug)]
pub struct DpuOsStatusSource {
    update_service: std::sync::Weak<UpdateServiceState>,
}

impl DpuOsStatusSource {
    /// The current status, or `None` once the BMC is dropped or when it
    /// manages no DPU OS.
    pub fn status(&self) -> Option<DpuOsStatus> {
        self.update_service.upgrade()?.dpu_os_status()
    }
}

/// A BFB install that has not landed yet.
struct BfbInstall {
    task_id: String,
    image: DpuOsImage,
    /// Set once the transfer finished and the BMC restarted the DPU. Only
    /// then does the next power-on apply the image.
    rebooting: bool,
}

/// The last boot stage the BMC observed, for `BootProgress`.
struct BootRecord {
    last_state: &'static str,
    time: String,
    /// The image generation this boot started from.
    generation: u64,
}

#[derive(Default)]
struct DpuOs {
    image: Option<DpuOsImage>,
    generation: u64,
    install: Option<BfbInstall>,
    boot: Option<BootRecord>,
}

impl DpuOs {
    fn set_image(&mut self, image: Option<DpuOsImage>) {
        if self.image != image {
            self.image = image;
            self.generation += 1;
        }
    }

    fn record_boot(&mut self, last_state: &'static str) {
        self.boot = Some(BootRecord {
            last_state,
            time: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            generation: self.generation,
        });
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
    /// Live tasks keyed by task ID: running, or completed and not yet applied.
    /// Applied and failed tasks move to `finished`, so the map does not grow
    /// without bound in long-running load tests.
    tasks: RwLock<HashMap<String, FirmwareTask>>,
    /// Bounded history of finished tasks, oldest first.
    finished: RwLock<VecDeque<FirmwareTask>>,
    /// Random per instance; prefixes every task ID.
    task_id_epoch: u32,
    next_task_id: AtomicU64,
    task_completion_delay: Duration,
    task_completion_jitter: Duration,
    /// Which push URIs this platform advertises in the UpdateService GET response.
    pub(crate) advertise_multipart_push_uri: bool,
    pub(crate) advertise_legacy_http_push_uri: bool,
    /// Inventory ID for the host BMC firmware entry.  `None` means no host
    /// firmware simulation for this platform (e.g. switches, power shelves).
    pub host_bmc_inventory_id: Option<String>,
    /// Inventory ID for the host UEFI/BIOS firmware entry.
    pub host_uefi_inventory_id: Option<String>,
    /// BlueField OS state, or `None` for a BMC that manages no DPU OS. Lock
    /// order: `tasks`, then `dpu_os`, then `finished`.
    dpu_os: Option<std::sync::Mutex<DpuOs>>,
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
            finished: RwLock::new(VecDeque::new()),
            task_id_epoch: rand::random(),
            next_task_id: AtomicU64::new(1),
            task_completion_delay: config.task_completion_delay,
            task_completion_jitter: config.task_completion_jitter,
            advertise_multipart_push_uri: config.advertise_multipart_push_uri,
            advertise_legacy_http_push_uri: config.advertise_legacy_http_push_uri,
            host_bmc_inventory_id: config.host_bmc_inventory_id,
            host_uefi_inventory_id: config.host_uefi_inventory_id,
            dpu_os: config.dpu_os.then(Default::default),
        }
    }

    pub fn find_firmware_inventory(&self, id: &str) -> Option<serde_json::Value> {
        self.firmware_inventory
            .read()
            .unwrap()
            .get(id)
            .map(|sw| sw.to_json())
    }

    /// Re-stage one component's upgrade target on a live mock without touching
    /// the active inventory. `Some(v)` stages `v` unless the active version
    /// already equals it; otherwise the target and any staged upload for the
    /// component are dropped so a withdrawn version cannot land at PowerOn.
    pub fn retarget_pending_upgrade(&self, component_id: &str, desired: Option<&str>) {
        // Same lock order as apply_staged_firmware: staged -> inventory -> pending.
        let mut staged = self.staged_firmware.write().unwrap();
        let inventory = self.firmware_inventory.read().unwrap();
        let mut pending = self.pending_upgrades.write().unwrap();
        let active_version = inventory
            .get(component_id)
            .and_then(|sw| sw.to_json()["Version"].as_str().map(str::to_owned));
        match desired {
            Some(version) if active_version.as_deref() != Some(version) => {
                pending.insert(component_id.to_string(), version.to_string());
            }
            _ => {
                pending.shift_remove(component_id);
                staged.remove(component_id);
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

    /// Which host firmware component an inventory id names on this platform.
    fn component_kind(&self, component_id: &str) -> FirmwareComponent {
        if self.host_bmc_inventory_id.as_deref() == Some(component_id) {
            FirmwareComponent::Bmc
        } else if self.host_uefi_inventory_id.as_deref() == Some(component_id) {
            FirmwareComponent::Uefi
        } else {
            FirmwareComponent::Other
        }
    }

    /// A live or remembered task, or `None` for an ID this BMC instance never
    /// issued or has forgotten.
    pub(crate) fn find_task(&self, id: &str) -> Option<serde_json::Value> {
        if let Some(task) = self.tasks.read().unwrap().get(id) {
            return Some(task.to_json());
        }
        self.finished
            .read()
            .unwrap()
            .iter()
            .find(|task| task.id == id)
            .map(FirmwareTask::to_json)
    }

    pub(crate) fn task_states(&self) -> HashMap<String, String> {
        // Match the retirement lock order so live and retained tasks are one observation.
        let tasks = self.tasks.read().unwrap();
        let finished = self.finished.read().unwrap();
        tasks
            .values()
            .chain(finished.iter())
            .map(|task| (task.id.clone(), task.state.as_str().to_owned()))
            .collect()
    }

    /// Move finished tasks out of the live map into the bounded history.
    fn retire_tasks(&self, tasks: &mut HashMap<String, FirmwareTask>, ids: &[String]) {
        let mut finished = self.finished.write().unwrap();
        for task in ids.iter().filter_map(|id| tasks.remove(id)) {
            if finished.len() == TASK_HISTORY_CAPACITY {
                finished.pop_front();
            }
            finished.push_back(task);
        }
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
        let task_id = self.next_task_id();
        let task = FirmwareTask {
            id: task_id.clone(),
            component_id: component_id.to_string(),
            target_version: target_version.clone(),
            state: TaskState::Running,
            component: self.component_kind(component_id),
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
        let delay = self.completion_delay();
        let component = component_id.to_string();
        // JoinHandle dropped intentionally — see module doc for rationale.
        drop(tokio::spawn(async move {
            tokio::time::sleep(delay).await;

            // Mark Completed and stage the version atomically under the tasks
            // write-lock.  Reading target_version from the task struct (rather than
            // capturing a separate clone) means apply_staged_firmware can never
            // observe a Completed task without a corresponding staged entry.
            let mut tasks = state.tasks.write().unwrap();
            if let Some(task) = tasks.get_mut(&task_id) {
                let version = task.target_version.clone();
                task.state = TaskState::Completed;
                if !version.is_empty() {
                    let mut staged = state.staged_firmware.write().unwrap();
                    // A retarget that withdrew the target mid-upload leaves
                    // nothing to stage.
                    if state
                        .pending_upgrades
                        .read()
                        .unwrap()
                        .contains_key(&component)
                    {
                        staged.insert(component, version);
                    }
                }
            }
        }));

        (odata_id, running_json)
    }

    /// The configured completion delay plus true random jitter, which
    /// desynchronises resets across many simulated hosts.
    fn completion_delay(&self) -> Duration {
        let jitter_ms = self.task_completion_jitter.as_millis();
        let jitter_offset = if jitter_ms == 0 {
            0u64
        } else {
            rand::random::<u64>() % jitter_ms as u64
        };
        self.task_completion_delay + Duration::from_millis(jitter_offset)
    }

    fn next_task_id(&self) -> String {
        let n = self.next_task_id.fetch_add(1, Ordering::Relaxed);
        format!("{:08x}-{n}", self.task_id_epoch)
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
                .filter(|t| {
                    t.state == TaskState::Running && t.component != FirmwareComponent::DpuOs
                })
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
    /// Host power-on applies all staged components. A BMC-only reset uses
    /// `apply_staged_bmc_firmware` so it cannot activate UEFI firmware.
    ///
    /// Completed tasks move to the bounded history after their firmware is
    /// applied, so the live map does not grow without bound in long-running
    /// load tests and a late poll still reads the task.
    /// Successfully applied components are also removed from `pending_upgrades`
    /// so a subsequent peek returns the next entry in the ordered map.
    pub(crate) fn apply_staged_firmware(&self) {
        self.apply_staged_component(None);
    }

    /// Activate only completed host BMC firmware on a BMC reset.
    pub(crate) fn apply_staged_bmc_firmware(&self) {
        self.apply_staged_component(Some(FirmwareComponent::Bmc));
    }

    /// Apply the staged versions of completed tasks, all of them or only those
    /// for one component kind.
    fn apply_staged_component(&self, component: Option<FirmwareComponent>) {
        let applies = |task: &FirmwareTask| {
            task.state == TaskState::Completed
                && task.component != FirmwareComponent::DpuOs
                && component.is_none_or(|kind| task.component == kind)
        };
        // Collect component IDs of completed tasks and retire them to history.
        let completed_components: Vec<String> = {
            let mut tasks = self.tasks.write().unwrap();
            let (ids, completed): (Vec<String>, Vec<String>) = tasks
                .values()
                .filter(|t| applies(t))
                .map(|t| (t.id.clone(), t.component_id.clone()))
                .unzip();
            self.retire_tasks(&mut tasks, &ids);
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
                        // Keep a pending entry that a retarget changed mid-upload
                        // for the next upload pass.
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

impl UpdateServiceState {
    /// The BlueField disk image and its generation, or `None` for a BMC that
    /// manages no DPU OS.
    pub fn dpu_os_status(&self) -> Option<DpuOsStatus> {
        let os = self.dpu_os.as_ref()?.lock().unwrap();
        Some(DpuOsStatus {
            image: os.image,
            generation: os.generation,
        })
    }

    /// A weak, read-only source of [`DpuOsStatus`] for this BMC.
    pub fn dpu_os_status_source(self: &std::sync::Arc<Self>) -> DpuOsStatusSource {
        DpuOsStatusSource {
            update_service: std::sync::Arc::downgrade(self),
        }
    }

    /// Record an image that was installed without a BFB task, such as the
    /// agent image a netboot writes, or the disk contents an embedder restores
    /// at startup. Advances the generation only when the image changes.
    /// Returns the resulting status, or `None` for a BMC with no DPU OS.
    pub fn record_dpu_os_image(&self, image: Option<DpuOsImage>) -> Option<DpuOsStatus> {
        let mut os = self.dpu_os.as_ref()?.lock().unwrap();
        os.set_image(image);
        Some(DpuOsStatus {
            image: os.image,
            generation: os.generation,
        })
    }

    /// `BootProgress` for the BlueField system, or `None` when this BMC
    /// manages no DPU OS or has not seen a boot yet. A DPU that is not powered
    /// on reports `None` as its last state, and `OSRunning` holds only for the
    /// boot of the current image generation.
    pub(crate) fn dpu_boot_progress(&self, powered_on: bool) -> Option<serde_json::Value> {
        let os = self.dpu_os.as_ref()?.lock().unwrap();
        let boot = os.boot.as_ref()?;
        if !powered_on {
            return Some(json!({ "LastState": "None" }));
        }
        let last_state = if boot.last_state == "OSRunning" && boot.generation != os.generation {
            "OSBootStarted"
        } else {
            boot.last_state
        };
        Some(json!({
            "LastState": last_state,
            "LastStateTime": boot.time,
        }))
    }

    /// Accept a BFB install and return its task. An install still in transfer
    /// is superseded and its task fails. Once an install has begun restarting
    /// the DPU it owns the DPU until it lands or fails, and a new install is
    /// refused, as a BMC refuses an update while one is in progress. After the
    /// transfer delay the BMC restarts the DPU through `callbacks`; the image
    /// lands on the next power-on (see `on_dpu_power_on`).
    fn start_bfb_install<C: Callbacks>(
        self: &std::sync::Arc<Self>,
        image: DpuOsImage,
        callbacks: Option<std::sync::Arc<C>>,
    ) -> Result<(String, serde_json::Value), BfbInstallRefused> {
        let dpu_os = self.dpu_os.as_ref().ok_or(BfbInstallRefused::NoDpuOs)?;
        let task_id = self.next_task_id();
        let task = FirmwareTask {
            id: task_id.clone(),
            component_id: DPU_OS_INVENTORY_ID.to_string(),
            target_version: String::new(),
            state: TaskState::Running,
            component: FirmwareComponent::DpuOs,
        };
        let response = (task.odata_id(), task.to_json());
        {
            let mut tasks = self.tasks.write().unwrap();
            let mut os = dpu_os.lock().unwrap();
            // The restart decision and this check share the `dpu_os` lock, so a
            // restart that has been admitted can never be for a replaced install.
            if os.install.as_ref().is_some_and(|install| install.rebooting) {
                return Err(BfbInstallRefused::RestartInProgress);
            }
            let previous = os.install.replace(BfbInstall {
                task_id: task_id.clone(),
                image,
                rebooting: false,
            });
            if let Some(previous) = previous
                && let Some(task) = tasks.get_mut(&previous.task_id)
            {
                task.state = TaskState::Exception("Superseded by a newer BFB install.".to_string());
                self.retire_tasks(&mut tasks, &[previous.task_id]);
            }
            tasks.insert(task_id.clone(), task);
        }

        let state = std::sync::Arc::clone(self);
        let delay = self.completion_delay();
        // JoinHandle dropped intentionally — see module doc for rationale.
        drop(tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            if !state.begin_bfb_reboot(&task_id) {
                return;
            }
            let restart = match callbacks {
                Some(callbacks) => {
                    callbacks
                        .computer_system_reset(crate::ResourceResetType::ForceRestart)
                        .await
                }
                None => Err(crate::ActionError::Internal(eyre::eyre!(
                    "BMC has no DPU power control"
                ))),
            };
            if let Err(error) = restart {
                state.fail_bfb_install(&task_id, &format!("DPU restart failed: {error}"));
            }
        }));
        Ok(response)
    }

    /// Mark the transfer of `task_id` finished and admit its DPU restart, so
    /// the next power-on applies it. Returns false when a newer install
    /// superseded it. From here until the install lands or fails, new installs
    /// are refused.
    fn begin_bfb_reboot(&self, task_id: &str) -> bool {
        let Some(dpu_os) = &self.dpu_os else {
            return false;
        };
        let mut os = dpu_os.lock().unwrap();
        match &mut os.install {
            Some(install) if install.task_id == task_id => {
                install.rebooting = true;
                true
            }
            _ => false,
        }
    }

    fn fail_bfb_install(&self, task_id: &str, message: &str) {
        let Some(dpu_os) = &self.dpu_os else {
            return;
        };
        let mut tasks = self.tasks.write().unwrap();
        let mut os = dpu_os.lock().unwrap();
        if os
            .install
            .as_ref()
            .is_some_and(|install| install.task_id == task_id)
        {
            os.install = None;
        }
        if let Some(task) = tasks.get_mut(task_id)
            && task.state == TaskState::Running
        {
            task.state = TaskState::Exception(message.to_string());
            self.retire_tasks(&mut tasks, &[task_id.to_string()]);
        }
    }

    /// The DPU powered on. An install whose restart was issued lands now: the
    /// image changes, the generation advances and its task completes. Every
    /// power-on starts a new boot, so `BootProgress` leaves `OSRunning`.
    pub(crate) fn on_dpu_power_on(&self) {
        let Some(dpu_os) = &self.dpu_os else {
            return;
        };
        let mut tasks = self.tasks.write().unwrap();
        let mut os = dpu_os.lock().unwrap();
        if os.install.as_ref().is_some_and(|install| install.rebooting)
            && let Some(install) = os.install.take()
        {
            os.image = Some(install.image);
            os.generation += 1;
            if let Some(task) = tasks.get_mut(&install.task_id) {
                task.state = TaskState::Completed;
                self.retire_tasks(&mut tasks, &[install.task_id]);
            }
        }
        os.record_boot("OSBootStarted");
    }

    /// The DPU finished booting. With an image on disk, its OS is now running.
    pub(crate) fn on_dpu_boot_completed(&self) {
        let Some(dpu_os) = &self.dpu_os else {
            return;
        };
        let mut os = dpu_os.lock().unwrap();
        if os.image.is_some() {
            os.record_boot("OSRunning");
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
    #[serde(rename = "ImageURI")]
    image_uri: Option<String>,
    #[serde(rename = "Targets", default)]
    targets: Vec<String>,
}

/// Advertise only the push URI(s) that this platform's BMC actually supports.
async fn get_update_service<C: Callbacks>(State(state): State<BmcState<C>>) -> Response {
    let us = &state.update_service_state;
    let mut b = builder(&resource()).firmware_inventory(
        redfish::software_inventory::firmware_inventory_collection()
            .odata_id
            .as_ref(),
    );
    if us.advertise_multipart_push_uri {
        b = b.multipart_http_push_uri(MULTIPART_UPLOAD_PATH);
    }
    if us.advertise_legacy_http_push_uri {
        b = b.http_push_uri(resource().odata_id.as_ref());
    }
    b.into_json().into_ok_response()
}

/// Redfish SimpleUpdate (Dell iDRAC, BFB/DPU path).
async fn update_firmware_simple_update<C: Callbacks>(
    State(state): State<BmcState<C>>,
    body: Option<axum::Json<SimpleUpdateRequest>>,
) -> Response {
    let SimpleUpdateRequest { image_uri, targets } = body.map(|b| b.0).unwrap_or_default();

    // Infer component from the first Redfish target path.
    // e.g. `/redfish/v1/UpdateService/FirmwareInventory/HostBMC_0` → `HostBMC_0`
    let (component_id, target_version) = if let Some(id) = targets
        .first()
        .and_then(|t| t.rsplit('/').next())
        .map(str::to_owned)
    {
        if id == DPU_OS_INVENTORY_ID && state.update_service_state.dpu_os.is_some() {
            let Some(image_uri) = image_uri else {
                return http::bad_request("ImageURI is required to install a BFB");
            };
            let image = DpuOsImage::from_image_uri(&image_uri);
            return match state
                .update_service_state
                .start_bfb_install(image, state.callbacks.clone())
            {
                Ok((task_odata_id, task_json)) => accepted_task_response(&task_odata_id, task_json),
                Err(BfbInstallRefused::NoDpuOs) => http::not_found(),
                // bmcweb refuses an update while another is in progress this way.
                Err(BfbInstallRefused::RestartInProgress) => http::registry_error(
                    axum::http::StatusCode::SERVICE_UNAVAILABLE,
                    "Base.1.13.0.ServiceTemporarilyUnavailable",
                    "The service is temporarily unavailable.  Retry in 30 seconds.",
                    &["30"],
                    "Wait for the indicated retry duration and retry the operation.",
                ),
            };
        }
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
async fn update_firmware_multipart<C: Callbacks>(
    State(state): State<BmcState<C>>,
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
    accepted_task_response(&task_odata_id, running_task_json)
}

/// 202 Accepted with the task's `Location` and its JSON body.
fn accepted_task_response(task_odata_id: &str, task_json: serde_json::Value) -> Response {
    // Use into_response(ACCEPTED) + Location header.  into_response_with_location
    // sets 200; we override the status after insertion.
    let location = axum::http::HeaderValue::from_str(task_odata_id).unwrap_or_else(|_| {
        axum::http::HeaderValue::from_static("/redfish/v1/TaskService/Tasks/0")
    });
    let mut response = task_json.into_ok_response_with_location(location);
    *response.status_mut() = axum::http::StatusCode::ACCEPTED;
    response
}

async fn get_firmware_inventory_collection<C: Callbacks>(
    State(state): State<BmcState<C>>,
) -> Response {
    let ids = state.update_service_state.all_firmware_inventory_ids();
    let members = ids
        .iter()
        .map(|id| redfish::software_inventory::firmware_inventory_resource(id).entity_ref())
        .collect::<Vec<_>>();
    redfish::software_inventory::firmware_inventory_collection()
        .with_members(&members)
        .into_ok_response()
}

async fn get_firmware_inventory_resource<C: Callbacks>(
    State(state): State<BmcState<C>>,
    Path(fw_inventory_id): Path<String>,
) -> Response {
    state
        .update_service_state
        .find_firmware_inventory(&fw_inventory_id)
        .map(|json| json.into_ok_response())
        .unwrap_or_else(http::not_found)
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
                    .version(*v)
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

    /// A staged upload for the old target still applies when the target moves
    /// forward; only withdrawing the target drops it.
    #[tokio::test(start_paused = true)]
    async fn retarget_drops_staged_uploads_only_when_the_target_is_withdrawn() {
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
        // Rolling back to the active version withdraws the pending entry and the staged upload.
        state.record_upload("BMC_Firmware", "24.11".to_string());
        tokio::time::sleep(Duration::from_millis(1)).await;
        tokio::task::yield_now().await;
        state.retarget_pending_upgrade("BMC_Firmware", Some("24.10"));
        state.apply_staged_firmware();
        let active = state.find_firmware_inventory("BMC_Firmware").unwrap();
        assert_eq!(
            active["Version"], "24.10",
            "withdrawn upload does not apply"
        );
        assert!(pending_pairs(&state).is_empty());
        // Withdrawn while the upload task is still Running: completion stages nothing.
        state.retarget_pending_upgrade("BMC_Firmware", Some("24.11"));
        state.record_upload("BMC_Firmware", "24.11".to_string());
        state.retarget_pending_upgrade("BMC_Firmware", Some("24.10"));
        tokio::time::sleep(Duration::from_millis(1)).await;
        tokio::task::yield_now().await;
        state.apply_staged_firmware();
        let active = state.find_firmware_inventory("BMC_Firmware").unwrap();
        assert_eq!(
            active["Version"], "24.10",
            "in-flight withdrawn upload does not apply"
        );
        assert!(pending_pairs(&state).is_empty());
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
        let task_id = task_path.rsplit('/').next().unwrap();
        assert_eq!(state.task_states()[task_id], "Running");

        // Yield to the runtime so the spawned task can run (delay=ZERO, no advance needed).
        tokio::task::yield_now().await;

        let completed = state.find_task(task_id).unwrap();
        assert_eq!(completed["TaskState"], "Completed");
        assert_eq!(completed["TaskStatus"], "OK");
        assert_eq!(completed["PercentComplete"], 100);
        assert_eq!(state.task_states()[task_id], "Completed");
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

    /// `task_completion_delay` is the whole time the task reports `Running`:
    /// still Running just before it, Completed right after, with no jitter
    /// when jitter is zero (the router caps jitter at the delay, so a zero
    /// delay from `firmware_upgrade = "0s"` completes immediately).
    #[tokio::test(start_paused = true)]
    async fn task_completes_after_the_configured_delay() {
        let state = Arc::new(UpdateServiceState::from_config(UpdateServiceConfig {
            firmware_inventory: vec![
                software_inventory::builder(&software_inventory::firmware_inventory_resource(
                    "HostBMC_0",
                ))
                .version("24.09.17")
                .build(),
            ],
            pending_upgrades: [("HostBMC_0".to_string(), "24.10.00".to_string())]
                .into_iter()
                .collect(),
            task_completion_delay: Duration::from_secs(600),
            task_completion_jitter: Duration::ZERO,
            ..Default::default()
        }));
        // record_upload returns the task's @odata.id and its JSON; the task is
        // looked up by the "Id" field.
        let (_, running) = state.record_upload("HostBMC_0", "24.10.00".into());
        let task_id = running["Id"].as_str().unwrap().to_string();
        let task_state =
            |s: &Arc<UpdateServiceState>| s.find_task(&task_id).unwrap()["TaskState"].clone();
        // Let the spawned completion task run up to its sleep, so the sleep is
        // registered at t=0 before the clock is advanced.
        tokio::task::yield_now().await;
        assert_eq!(task_state(&state), "Running", "right after the upload");

        tokio::time::advance(Duration::from_secs(599)).await;
        tokio::task::yield_now().await;
        assert_eq!(task_state(&state), "Running", "one second before the delay");

        tokio::time::advance(Duration::from_secs(2)).await;
        tokio::task::yield_now().await;
        assert_eq!(task_state(&state), "Completed", "once the delay has passed");
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
        // Completion only stages while the component is still pending.
        let state = make_state(&[("HostBMC_0", "24.09.17")], &[("HostBMC_0", "24.11.00")]);
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

    /// Applied tasks leave the live map but stay readable from the history,
    /// which keeps only the newest `TASK_HISTORY_CAPACITY` tasks.
    #[tokio::test(start_paused = true)]
    async fn applied_tasks_move_to_a_bounded_history() {
        let state = make_state(&[("HostBMC_0", "24.09.17")], &[]);
        let (first, _) = state.record_upload("HostBMC_0", "24.10.00".into());
        let first = first.rsplit('/').next().unwrap().to_owned();
        tokio::task::yield_now().await;

        assert_eq!(state.tasks.read().unwrap().len(), 1);
        state.apply_staged_firmware();
        assert_eq!(
            state.tasks.read().unwrap().len(),
            0,
            "applied task leaves the live map"
        );
        let task = state
            .find_task(&first)
            .expect("applied task stays readable");
        assert_eq!(task["TaskState"], "Completed");
        assert_eq!(task["Id"], first.as_str());
        assert_eq!(state.task_states()[&first], "Completed");

        for _ in 0..TASK_HISTORY_CAPACITY {
            state.record_upload("HostBMC_0", "24.10.00".into());
            tokio::task::yield_now().await;
            state.apply_staged_firmware();
        }
        assert_eq!(state.finished.read().unwrap().len(), TASK_HISTORY_CAPACITY);
        assert_eq!(state.find_task(&first), None, "oldest record is evicted");
        assert!(!state.task_states().contains_key(&first));
        assert_eq!(state.task_states().len(), TASK_HISTORY_CAPACITY);
    }

    /// Task IDs carry a per-instance epoch, so an ID issued before a restart
    /// does not name a task of the new instance.
    #[test]
    fn task_ids_do_not_alias_across_instances() {
        let before = make_state(&[], &[]);
        let after = make_state(&[], &[]);
        assert_ne!(before.task_id_epoch, after.task_id_epoch);
        let id_before = before.next_task_id();
        let id_after = after.next_task_id();
        assert_ne!(id_before, id_after);
        assert!(id_before.ends_with("-1") && id_after.ends_with("-1"));
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

    use axum::body::to_bytes;
    use axum::http::{Method, Request, StatusCode};
    use tower::ServiceExt;

    use crate::machine_info::HostFirmwareVersions;
    use crate::test_support::{TestBmcConfig, create_test_bmc, host_info};
    use crate::{HardwareType, MachineRouterOptions};

    fn make_router(
        bmc_current: &str,
        bmc_desired: &str,
    ) -> (
        axum::Router,
        crate::bmc_state::BmcState<crate::test_support::TestCallbacks>,
    ) {
        make_router_with_uefi(bmc_current, bmc_desired, None, None)
    }

    fn make_router_with_uefi(
        bmc_current: &str,
        bmc_desired: &str,
        uefi_current: Option<&str>,
        uefi_desired: Option<&str>,
    ) -> (
        axum::Router,
        crate::bmc_state::BmcState<crate::test_support::TestCallbacks>,
    ) {
        make_platform_router(
            HardwareType::GenericAmi,
            bmc_current,
            bmc_desired,
            uefi_current,
            uefi_desired,
        )
    }

    fn make_platform_router(
        hw_type: HardwareType,
        bmc_current: &str,
        bmc_desired: &str,
        uefi_current: Option<&str>,
        uefi_desired: Option<&str>,
    ) -> (
        axum::Router,
        crate::bmc_state::BmcState<crate::test_support::TestCallbacks>,
    ) {
        let info = host_info(hw_type);
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
        create_test_bmc(
            &info,
            TestBmcConfig::default(),
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
        let (router, state) = make_router("24.09.17", "24.10.00");
        let resp = post_empty(&router, MULTIPART_UPLOAD_PATH).await;
        assert_eq!(resp.status(), StatusCode::ACCEPTED);
        assert!(
            resp.headers().contains_key("Location"),
            "Location header missing"
        );
        let body: serde_json::Value =
            serde_json::from_slice(&to_bytes(resp.into_body(), usize::MAX).await.unwrap()).unwrap();
        assert_eq!(body["TaskState"], "Running");
        let snapshot = state.snapshot_source().effect_snapshot().unwrap();
        assert_eq!(snapshot.tasks[body["Id"].as_str().unwrap()], "Running");
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
    async fn gb200_upgrades_bmc_and_uefi_independently() {
        let (hw_type, bmc_id, uefi_id) = (HardwareType::WiwynnGB200Nvl, "FW_BMC_0", "HGX_FW_CPU_0");
        for multipart in [false, true] {
            let (router, state) = make_platform_router(
                hw_type,
                "bmc-old",
                "bmc-new",
                Some("uefi-old"),
                Some("uefi-new"),
            );
            let bmc_path = format!("/redfish/v1/UpdateService/FirmwareInventory/{bmc_id}");
            let uefi_path = format!("/redfish/v1/UpdateService/FirmwareInventory/{uefi_id}");
            let service = get_json(&router, "/redfish/v1/UpdateService").await;
            assert_eq!(service["MultipartHttpPushUri"], MULTIPART_UPLOAD_PATH);
            state.on_event(&crate::bmc_state::BmcEvent::PowerOn);
            assert_eq!(get_json(&router, &bmc_path).await["Version"], "bmc-old");
            assert_eq!(get_json(&router, &uefi_path).await["Version"], "uefi-old");

            for (path, old, new) in [
                (&bmc_path, "bmc-old", "bmc-new"),
                (&uefi_path, "uefi-old", "uefi-new"),
            ] {
                let response = if multipart {
                    post_empty(&router, MULTIPART_UPLOAD_PATH).await
                } else {
                    router
                        .clone()
                        .oneshot(
                            Request::builder()
                                .method(Method::POST)
                                .uri(simple_update_target())
                                .header("Content-Type", "application/json")
                                .body(axum::body::Body::from(
                                    serde_json::json!({"Targets": [path]}).to_string(),
                                ))
                                .unwrap(),
                        )
                        .await
                        .unwrap()
                };
                assert_eq!(response.status(), StatusCode::ACCEPTED, "{hw_type:?}");
                let task_path = response.headers()["Location"].to_str().unwrap().to_owned();
                assert_eq!(get_json(&router, &task_path).await["TaskState"], "Running");
                let reset = "/redfish/v1/Managers/BMC_0/Actions/Manager.Reset";
                assert_eq!(post_empty(&router, reset).await.status(), StatusCode::OK);
                assert_eq!(get_json(&router, path).await["Version"], old);
                state.update_service_state.complete_all_tasks_for_test();
                assert_eq!(
                    get_json(&router, &task_path).await["TaskState"],
                    "Completed"
                );
                let event = if path == &bmc_path {
                    "BMC reset"
                } else {
                    "host power-on"
                };
                assert_eq!(
                    get_json(&router, &task_path).await["Messages"][0]["Message"],
                    format!("Firmware staged; version will be applied after the next {event}."),
                    "{hw_type:?} {path}"
                );
                assert_eq!(get_json(&router, path).await["Version"], old);
                assert_eq!(post_empty(&router, reset).await.status(), StatusCode::OK);
                if path == &uefi_path {
                    assert_eq!(get_json(&router, path).await["Version"], old);
                    state.on_event(&crate::bmc_state::BmcEvent::PowerOn);
                }
                assert_eq!(get_json(&router, path).await["Version"], new);
                // A poll after activation still reads this task's own record.
                let task = get_json(&router, &task_path).await;
                assert_eq!(task["TaskState"], "Completed");
                assert_eq!(
                    task["Messages"][0]["Message"],
                    format!("Firmware staged; version will be applied after the next {event}.")
                );
                if path == &bmc_path {
                    assert_eq!(get_json(&router, &uefi_path).await["Version"], "uefi-old");
                }
            }
            assert_eq!(get_json(&router, &bmc_path).await["Version"], "bmc-new");
            assert_eq!(get_json(&router, &uefi_path).await["Version"], "uefi-new");
        }
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

    const PRE_INGESTION_URI: &str =
        "carbide-pxe.forge//public/blobs/internal/aarch64/preingestion.bfb";
    const MANAGED_URI: &str = "carbide-pxe.forge//public/blobs/internal/aarch64/forge.bfb";

    fn dpu_os_state(delay: Duration) -> Arc<UpdateServiceState> {
        Arc::new(UpdateServiceState::from_config(UpdateServiceConfig {
            task_completion_delay: delay,
            task_completion_jitter: Duration::ZERO,
            dpu_os: true,
            ..Default::default()
        }))
    }

    fn task_state(state: &UpdateServiceState, task_path: &str) -> serde_json::Value {
        state
            .find_task(task_path.rsplit('/').next().unwrap())
            .unwrap()["TaskState"]
            .clone()
    }

    #[test]
    fn bfb_image_kind_follows_the_image_file_name() {
        for (image_uri, expected) in [
            (PRE_INGESTION_URI, DpuOsImage::PreIngestion),
            (MANAGED_URI, DpuOsImage::Managed),
            (
                "http://dpf-bfb-registry/bfb/dpf-operator-system-bf-bundle.bfb",
                DpuOsImage::Managed,
            ),
        ] {
            assert_eq!(
                DpuOsImage::from_image_uri(image_uri),
                expected,
                "{image_uri}"
            );
        }
    }

    /// A power-on during the transfer is not the install's restart, so the
    /// image stays and the task keeps running.
    #[tokio::test(start_paused = true)]
    async fn bfb_install_ignores_power_on_before_its_restart() {
        let state = dpu_os_state(Duration::from_secs(600));
        let (task_path, _) = state
            .start_bfb_install::<crate::test_support::TestCallbacks>(DpuOsImage::PreIngestion, None)
            .unwrap();
        tokio::task::yield_now().await;
        state.on_dpu_power_on();
        assert_eq!(task_state(&state, &task_path), "Running");
        assert_eq!(
            state.dpu_os_status(),
            Some(DpuOsStatus {
                image: None,
                generation: 0
            })
        );
    }

    /// Without DPU power control the BMC cannot restart the DPU, so the task
    /// fails and no later power-on installs the image.
    #[tokio::test(start_paused = true)]
    async fn bfb_install_fails_when_the_dpu_cannot_restart() {
        let state = dpu_os_state(Duration::ZERO);
        let (task_path, _) = state
            .start_bfb_install::<crate::test_support::TestCallbacks>(DpuOsImage::PreIngestion, None)
            .unwrap();
        tokio::time::sleep(Duration::from_millis(1)).await;
        let task = state
            .find_task(task_path.rsplit('/').next().unwrap())
            .unwrap();
        assert_eq!(task["TaskState"], "Exception");
        assert_eq!(task["TaskStatus"], "Critical");
        assert_eq!(
            state.task_states()[task_path.rsplit('/').next().unwrap()],
            "Exception"
        );
        state.on_dpu_power_on();
        assert_eq!(state.dpu_os_status().unwrap().image, None);
    }

    /// A retried install replaces the open one: the old task fails and only
    /// the new one lands.
    #[tokio::test(start_paused = true)]
    async fn newer_bfb_install_supersedes_the_open_one() {
        let state = dpu_os_state(Duration::from_secs(600));
        let (first, _) = state
            .start_bfb_install::<crate::test_support::TestCallbacks>(DpuOsImage::PreIngestion, None)
            .unwrap();
        let (second, _) = state
            .start_bfb_install::<crate::test_support::TestCallbacks>(DpuOsImage::Managed, None)
            .unwrap();
        assert_eq!(task_state(&state, &first), "Exception");
        assert_eq!(task_state(&state, &second), "Running");
        // The first install's timer fires first but no longer owns the install.
        let first_id = first.rsplit('/').next().unwrap();
        assert!(!state.begin_bfb_reboot(first_id));
        assert!(state.begin_bfb_reboot(second.rsplit('/').next().unwrap()));
        state.on_dpu_power_on();
        assert_eq!(task_state(&state, &second), "Completed");
        assert_eq!(
            state.dpu_os_status(),
            Some(DpuOsStatus {
                image: Some(DpuOsImage::Managed),
                generation: 1
            })
        );
    }

    /// Power control that parks each restart until the test releases it, so
    /// a request can arrive after the restart was admitted but before the
    /// backend received it.
    #[derive(Default)]
    struct GatedRestart {
        entered: tokio::sync::Notify,
        release: tokio::sync::Notify,
        restarts: std::sync::atomic::AtomicUsize,
    }

    impl Callbacks for GatedRestart {
        async fn computer_system_reset(
            &self,
            _reset_type: crate::ResourceResetType,
        ) -> Result<(), crate::ActionError> {
            self.entered.notify_one();
            self.release.notified().await;
            self.restarts.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        fn state_refresh_indication(&self) {}
    }

    /// Once an install's restart is admitted, a replacement is refused, so
    /// the only restart the DPU receives belongs to the install that lands.
    #[tokio::test]
    async fn replacement_during_restart_dispatch_is_refused() {
        let state = dpu_os_state(Duration::ZERO);
        let power = Arc::new(GatedRestart::default());
        let (first, _) = state
            .start_bfb_install(DpuOsImage::PreIngestion, Some(power.clone()))
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), power.entered.notified())
            .await
            .expect("restart dispatch started");

        assert_eq!(
            state.start_bfb_install(DpuOsImage::Managed, Some(power.clone())),
            Err(BfbInstallRefused::RestartInProgress)
        );
        power.release.notify_one();
        tokio::time::timeout(Duration::from_secs(5), async {
            while power.restarts.load(Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("restart dispatched");
        assert_eq!(
            state.tasks.read().unwrap().len(),
            1,
            "the refused install has no task"
        );
        state.on_dpu_power_on();
        assert_eq!(task_state(&state, &first), "Completed");
        assert_eq!(
            state.dpu_os_status(),
            Some(DpuOsStatus {
                image: Some(DpuOsImage::PreIngestion),
                generation: 1
            })
        );
        // The landed install no longer owns the DPU.
        assert!(
            state
                .start_bfb_install(DpuOsImage::Managed, Some(power.clone()))
                .is_ok()
        );
        assert_eq!(power.restarts.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn dpu_os_status_source_reads_without_keeping_the_bmc_alive() {
        let state = dpu_os_state(Duration::ZERO);
        let source = state.dpu_os_status_source();
        state.record_dpu_os_image(Some(DpuOsImage::PreIngestion));
        assert_eq!(
            source.status(),
            Some(DpuOsStatus {
                image: Some(DpuOsImage::PreIngestion),
                generation: 1
            })
        );
        drop(state);
        assert_eq!(source.status(), None);
    }

    #[test]
    fn boot_progress_reports_running_only_for_the_booted_generation() {
        let state = dpu_os_state(Duration::ZERO);
        assert_eq!(state.dpu_boot_progress(true), None, "no boot observed yet");
        state.record_dpu_os_image(Some(DpuOsImage::Managed));
        state.on_dpu_power_on();
        assert_eq!(
            state.dpu_boot_progress(true).unwrap()["LastState"],
            "OSBootStarted"
        );
        state.on_dpu_boot_completed();
        assert_eq!(
            state.dpu_boot_progress(true).unwrap()["LastState"],
            "OSRunning"
        );
        assert_eq!(state.dpu_boot_progress(false).unwrap()["LastState"], "None");
        // An image change without a reboot leaves the running OS stale.
        state.record_dpu_os_image(Some(DpuOsImage::PreIngestion));
        assert_eq!(
            state.dpu_boot_progress(true).unwrap()["LastState"],
            "OSBootStarted"
        );
        // A boot with an empty disk never reaches a running OS.
        state.record_dpu_os_image(None);
        state.on_dpu_power_on();
        state.on_dpu_boot_completed();
        assert_eq!(
            state.dpu_boot_progress(true).unwrap()["LastState"],
            "OSBootStarted"
        );
    }

    fn bluefield3_dpu_bmc() -> (
        axum::Router,
        crate::bmc_state::BmcState<crate::test_support::TestCallbacks>,
    ) {
        let machine_info = {
            let mut pool = crate::test_support::TEST_MAC_POOL.lock().unwrap();
            crate::MachineInfo::Dpu(crate::DpuMachineInfo::new(
                HardwareType::DellPowerEdgeR750,
                &mut pool,
                crate::DpuSettings::default(),
            ))
        };
        create_test_bmc(
            &machine_info,
            TestBmcConfig::default(),
            "test-dpu".into(),
            false,
            MachineRouterOptions {
                firmware_upgrade_duration: Some(Duration::ZERO),
                ..Default::default()
            },
        )
    }

    /// Drive a BFB install through the Redfish routes: the task completes only
    /// after the DPU restarts into the new image, and `BootProgress` reaches
    /// `OSRunning` only once that image has booted. A later managed install
    /// brings the managed image back.
    #[tokio::test]
    async fn bfb_install_lands_on_restart_and_boot_progress_follows() {
        let (router, state) = bluefield3_dpu_bmc();
        let callbacks = state.callbacks.clone().unwrap();
        let system_id = state.system_state.primary_system_odata_id().unwrap();
        state
            .update_service_state
            .record_dpu_os_image(Some(DpuOsImage::Managed));
        state.on_event(&crate::bmc_state::BmcEvent::BootCompleted);
        assert_eq!(
            get_json(&router, &system_id).await["BootProgress"]["LastState"],
            "OSRunning"
        );

        for (round, (image_uri, image)) in [
            (PRE_INGESTION_URI, DpuOsImage::PreIngestion),
            (MANAGED_URI, DpuOsImage::Managed),
        ]
        .into_iter()
        .enumerate()
        {
            let generation = state
                .update_service_state
                .dpu_os_status()
                .unwrap()
                .generation;
            let response = router
                .clone()
                .oneshot(
                    Request::builder()
                        .method(Method::POST)
                        .uri(simple_update_target())
                        .header("Content-Type", "application/json")
                        .body(Body::from(
                            json!({
                                "ImageURI": image_uri,
                                "Targets": ["redfish/v1/UpdateService/FirmwareInventory/DPU_OS"],
                                "TransferProtocol": "HTTP",
                            })
                            .to_string(),
                        ))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::ACCEPTED);
            let task_path = response.headers()["Location"].to_str().unwrap().to_owned();
            let task: serde_json::Value =
                serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                    .unwrap();
            assert_eq!(task["TaskState"], "Running");
            assert_eq!(task["@odata.id"], task_path);
            callbacks.wait_for_command_count(round + 1).await;
            assert_eq!(
                callbacks.commands.lock().unwrap()[round],
                crate::ResourceResetType::ForceRestart
            );
            assert_eq!(get_json(&router, &task_path).await["TaskState"], "Running");
            state.on_event(&crate::bmc_state::BmcEvent::PowerOn);
            let completed = get_json(&router, &task_path).await;
            assert_eq!(completed["TaskState"], "Completed");
            assert_eq!(
                state.update_service_state.dpu_os_status(),
                Some(DpuOsStatus {
                    image: Some(image),
                    generation: generation + 1
                })
            );
            assert_eq!(
                get_json(&router, &system_id).await["BootProgress"]["LastState"],
                "OSBootStarted"
            );
            state.on_event(&crate::bmc_state::BmcEvent::BootCompleted);
            assert_eq!(
                get_json(&router, &system_id).await["BootProgress"]["LastState"],
                "OSRunning"
            );
            assert_eq!(
                completed["Messages"][0]["Message"],
                "BFB installed; the DPU restarted into the new image."
            );
        }
    }

    /// A SimpleUpdate without an image cannot install a BFB.
    #[tokio::test]
    async fn bfb_install_requires_an_image_uri() {
        let (router, _) = bluefield3_dpu_bmc();
        let response = router
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(simple_update_target())
                    .header("Content-Type", "application/json")
                    .body(axum::body::Body::from(
                        r#"{"Targets":["/redfish/v1/UpdateService/FirmwareInventory/DPU_OS"]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}

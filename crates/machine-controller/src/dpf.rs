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

//! DPF SDK trait abstraction for testability.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use carbide_dpf::types::{
    DetachedDpuServiceDefinition, DpuServiceObservation, DpuServiceVersion, HostDpfSnapshot,
    ServiceTemplateVersion,
};
use carbide_dpf::{
    BmcPasswordProvider, DPU_ENABLED_NODE_LABEL, DpfError, DpfSdk, DpuDeploymentType,
    DpuDeviceInfo, DpuNodeInfo, DpuPhase, DpuWatcher, KubeRepository, ResourceLabeler,
    node_id_from_dpu_node_cr_name,
};
use carbide_uuid::machine::{DpuMachineId, HostMachineId};
use model::dpa_interface::DpaInterface;
use model::dpu_machine_update::OutdatedDpfDpu;
use model::machine::{Machine, ManagedHostStateSnapshot};
use model::machine_pending_action::{MachinePendingAction, MachinePendingActionKind};
use sqlx::PgPool;
use state_controller::controller::Enqueuer;
use tokio::task::JoinSet;

use crate::handler::is_bf4_dmi_product;
use crate::io::MachineStateControllerIO;

/// Label key used by [`CarbideDPFLabeler`] to stamp the carbide `MachineId` of
/// the DPU onto its DPUDevice. Propagates to the DPU CR via DPF.
const DPU_MACHINE_ID_LABEL: &str = "carbide.nvidia.com/dpu-machine-id";

/// Label key used by [`CarbideDPFLabeler`] to mark a DPU device as
/// carbide-controlled. Propagates to the DPU CR.
const CONTROLLED_DEVICE_LABEL: &str = "carbide.nvidia.com/controlled.device";

/// Label populated with the host BMC address on both DPUDevice and DPUNode resources.
pub const HOST_BMC_IP_LABEL: &str = "carbide.nvidia.com/host-bmc-ip";

/// Trait for DPF SDK operations used by Carbide.
///
/// The DPF operator owns provisioning; Carbide declares setup (deployment, devices, node),
/// reacts to watcher callbacks, and performs reprovision/force-delete.
///
/// Reboot handling is managed via the watcher's `on_reboot_required` callback.
#[cfg_attr(feature = "test-support", mockall::automock)]
#[async_trait]
pub trait DpfOperations: Send + Sync + std::fmt::Debug {
    /// Register a DPU device.
    async fn register_dpu_device<'a>(
        &self,
        info: DpuDeviceInfo,
        astra_nics: Option<Vec<&'a DpaInterface>>,
    ) -> Result<(), DpfError>;

    /// Register a DPU node.
    async fn register_dpu_node(&self, info: DpuNodeInfo) -> Result<(), DpfError>;

    /// Release the maintenance hold on a DPU node.
    async fn release_maintenance_hold(&self, node_name: &str) -> Result<(), DpfError>;

    /// Reprovision a DPU (delete DPU CR; operator creates a new one that waits on node effect).
    async fn reprovision_dpu(&self, dpu_device_name: &str, node_name: &str)
    -> Result<(), DpfError>;

    /// Force delete a host and all its DPU resources.
    async fn force_delete_host(
        &self,
        node_id: &str,
        dpu_device_names: &[String],
    ) -> Result<(), DpfError>;

    /// Get the current phase of a DPU (for status reporting).
    async fn get_dpu_phase(
        &self,
        dpu_device_name: &str,
        node_name: &str,
    ) -> Result<DpuPhase, DpfError>;

    /// Read every requested DPU phase only when one deployment owns the full
    /// set and each Ready DPU matches its flavor and provisioning source.
    async fn get_dpu_phases_for_deployment_type(
        &self,
        dpu_device_names: &[String],
        node_name: &str,
        deployment_type: DpuDeploymentType,
    ) -> Result<Option<BTreeMap<String, DpuPhase>>, DpfError>;

    /// Delete source deployment DPU CRs while preserving target replacements.
    async fn delete_source_dpus_for_deployment_migration(
        &self,
        dpu_device_names: &[String],
        node_name: &str,
        source_deployment_type: DpuDeploymentType,
        target_deployment_type: DpuDeploymentType,
    ) -> Result<(), DpfError>;

    /// Check if a DPU node is waiting for external reboot.
    async fn is_reboot_required(&self, node_name: &str) -> Result<bool, DpfError>;

    /// Mark DPU node as rebooted (clear the external reboot required annotation).
    async fn reboot_complete(&self, node_name: &str) -> Result<(), DpfError>;

    /// Resolve a DPU's base hardware deployment class (BF3 or BF4).
    ///
    /// The state handler separately refines BF3 from the host rack and DPU
    /// profile. This returns `Err` when the DMI product name is absent.
    fn deployment_type_for_dpu(
        &self,
        dpu: &Machine,
        astra_nics: bool,
    ) -> Result<DpuDeploymentType, DpfError>;

    /// Check that a DPUNode's labels match the current expected labels.
    /// Returns `false` when the node exists but has stale labels.
    async fn verify_node_labels(
        &self,
        node_name: &str,
        deployment_type: DpuDeploymentType,
    ) -> Result<bool, DpfError>;

    /// Atomically moves a DPUNode from one deployment selector to another.
    ///
    /// A completed transfer does nothing, while a node matching neither selector
    /// is rejected.
    async fn transfer_dpu_node_deployment_labels(
        &self,
        node_name: &str,
        source_deployment_type: DpuDeploymentType,
        target_deployment_type: DpuDeploymentType,
    ) -> Result<(), DpfError>;

    /// Curated snapshot of all DPF CRs related to one host (DPUNode +
    /// DPUDevices + DPUs). `node_name` is the full DPUNode CR name.
    async fn snapshot_host(&self, node_name: &str) -> Result<HostDpfSnapshot, DpfError>;

    /// List helm-chart versions declared on each live `DPUServiceTemplate`
    /// CR — used for comparing config vs deployed state.
    async fn list_service_template_versions(&self)
    -> Result<Vec<ServiceTemplateVersion>, DpfError>;

    /// Get service versions for a DPU from its owning DPUDeployment.
    ///
    /// Looks up the DPU CR by its full node-device resource name, follows the
    /// `svc.dpu.nvidia.com/owned-by-dpudeployment` label to find the owning
    /// DPUDeployment, and resolves each service's version from its
    /// DPUServiceTemplate. Used to populate `agent_reported_inventory` once
    /// the DPU reaches `DpuPhase::Ready`.
    async fn get_service_versions_for_dpu(
        &self,
        dpu_name: &str,
    ) -> Result<Vec<DpuServiceVersion>, DpfError>;

    /// Return DPUs whose installed BFB or `spec.dpuFlavor` does not match
    /// the namespace's ready DPUDeployment, mapped back to carbide
    /// `MachineId` via the `carbide.nvidia.com/dpu-machine-id` label. The
    /// expected BFB and flavor are read from the live DPUDeployment, not
    /// from carbide config — see [`DpfSdk::find_outdated_dpus_dpf`] for
    /// details.
    async fn find_outdated_dpus_dpf(&self) -> Result<Vec<OutdatedDpfDpu>, DpfError>;

    /// Whether one DPU still differs from its owning DPUDeployment, i.e. a
    /// reprovision is still coming for it.
    ///
    /// Reports `true` for a DPU it cannot evaluate, so callers gated on "this
    /// DPU is already up to date" fail closed.
    async fn is_dpu_outdated(&self, dpu_name: &str) -> Result<bool, DpfError>;

    /// Create a direct, detached DPUService for an extension service.
    async fn create_dpu_service(
        &self,
        service: &DetachedDpuServiceDefinition,
    ) -> Result<DpuServiceObservation, DpfError>;

    /// Get any observed DPUService by name for an extension service.
    async fn get_dpu_service(
        &self,
        service_name: &str,
    ) -> Result<Option<DpuServiceObservation>, DpfError>;

    /// Merge-patch a detached DPUService for an extension service.
    async fn patch_dpu_service(
        &self,
        service_name: &str,
        patch: serde_json::Value,
    ) -> Result<(), DpfError>;

    /// Delete a detached DPUService for an extension service.
    async fn delete_dpu_service(&self, service_name: &str) -> Result<(), DpfError>;

    /// Merge changes into a DPUDevice's DPU-cluster node labels.
    async fn merge_dpu_device_node_labels(
        &self,
        dpu_device_name: &str,
        changes: BTreeMap<String, Option<String>>,
    ) -> Result<(), DpfError>;

    /// Returns the DPU-cluster node labels on one DPUDevice CR.
    async fn get_dpu_device_node_labels(
        &self,
        dpu_device_name: &str,
    ) -> Result<BTreeMap<String, String>, DpfError>;
}

/// Check whether the DPUNode and DPUDevice CRs are missing for the given host.
/// Registration is all-or-nothing: `create_and_register_dpudevices_and_dpunode`
/// creates every DPUDevice followed by the DPUNode in one pass, and we never
/// delete a subset. A partial CR set (node without all devices, or devices
/// without a node) therefore indicates external tampering or a half-completed
/// force-delete and requires operator intervention -- it is reported as
/// `InvalidState` rather than silently re-registered.
/// - `Ok(true)`  : neither the node nor any devices exist -- safe to register.
/// - `Ok(false)` : node exists and device count matches DPU count -- nothing to do.
/// - `Err`       : host has no DPF id, OR the CR set is partial/mismatched.
pub async fn dpf_dpudevices_and_dpunode_crs_noexist(
    managed_host_state: &ManagedHostStateSnapshot,
    dpf_sdk: &dyn DpfOperations,
) -> Result<bool, DpfError> {
    let managed_host = &managed_host_state.host_snapshot;
    let Some(dpf_id) = managed_host.dpf_id() else {
        return Err(DpfError::InvalidState(format!(
            "Host {} is missing a DPF id",
            managed_host.id
        )));
    };

    let dpu_count = managed_host_state.dpu_snapshots.len();

    let node_name = carbide_dpf::dpu_node_cr_name(&dpf_id);
    let dpf_sdk_host_snapshot = dpf_sdk.snapshot_host(&node_name).await?;
    let dpunode_cr_exists = dpf_sdk_host_snapshot.dpu_node.is_some();
    let dpfdevice_cr_count = dpf_sdk_host_snapshot.dpu_devices.len();

    if !dpunode_cr_exists && dpfdevice_cr_count == 0 {
        return Ok(true);
    }

    if dpunode_cr_exists && dpfdevice_cr_count == dpu_count {
        return Ok(false);
    }

    Err(DpfError::InvalidState(format!(
        "Host {} has inconsistent DPF CRs for {} DPU(s): dpu_node_present={}, dpu_device_count={}",
        managed_host.id, dpu_count, dpunode_cr_exists, dpfdevice_cr_count,
    )))
}

/// Applies carbide-specific labels to DPF resources.
///
/// Label inheritance in DPF:
/// - DPUDevice labels propagate to the DPU CR created by the operator.
/// - DPUNode static labels (`node_labels`) are used by DPUDeployment's
///   `dpuNodeSelector` to match nodes, and also propagate to DPU CRs.
/// - DPUNode contextual labels (`node_context_labels`) are only set at
///   creation and propagate to DPU CRs, but are not part of selectors.
pub struct CarbideDPFLabeler {
    node_label_key: String,
    /// Per-deployment-type node selector labels: DpuDeploymentType → labels.
    /// Populated for each configured deployment so that [`build_deployment`]
    /// can look up the correct `dpuNodeSelector.matchLabels` by type.
    deployment_type_labels: BTreeMap<DpuDeploymentType, BTreeMap<String, String>>,
}

impl CarbideDPFLabeler {
    pub fn new(node_label_key: String) -> Self {
        Self {
            node_label_key,
            deployment_type_labels: BTreeMap::new(),
        }
    }

    /// Register per-deployment-type node selector labels. Call once per configured
    /// DPUDeployment before passing the labeler to the DPF SDK builder.
    pub fn with_deployment_type_labels(
        mut self,
        deployment_type_labels: BTreeMap<DpuDeploymentType, BTreeMap<String, String>>,
    ) -> Self {
        self.deployment_type_labels = deployment_type_labels;
        self
    }
}

impl ResourceLabeler for CarbideDPFLabeler {
    fn device_labels(&self, info: &DpuDeviceInfo) -> BTreeMap<String, String> {
        BTreeMap::from([
            (CONTROLLED_DEVICE_LABEL.to_string(), "true".to_string()),
            (HOST_BMC_IP_LABEL.to_string(), info.host_bmc_ip.to_string()),
            (
                "carbide.nvidia.com/is-primary-dpu".to_string(),
                info.is_primary.to_string(),
            ),
            (
                DPU_MACHINE_ID_LABEL.to_string(),
                info.dpu_machine_id.clone(),
            ),
        ])
    }

    fn node_labels(&self) -> BTreeMap<String, String> {
        BTreeMap::from([
            (self.node_label_key.clone(), "true".to_string()),
            (DPU_ENABLED_NODE_LABEL.to_string(), "true".to_string()),
        ])
    }

    fn node_labels_for_deployment_type(
        &self,
        deployment_type: DpuDeploymentType,
    ) -> Result<BTreeMap<String, String>, DpfError> {
        self.deployment_type_labels
            .get(&deployment_type)
            .cloned()
            .ok_or_else(|| {
                DpfError::ConfigError(format!(
                    "no DPUDeployment configured for {deployment_type:?}",
                ))
            })
    }

    fn node_context_labels(&self, info: &DpuNodeInfo) -> BTreeMap<String, String> {
        BTreeMap::from([(HOST_BMC_IP_LABEL.to_string(), info.host_bmc_ip.to_string())])
    }

    fn dpu_label_selector(&self) -> Option<String> {
        Some(format!("{CONTROLLED_DEVICE_LABEL}=true"))
    }
}

/// BMC password provider backed by the Carbide credential manager.
///
/// DPF needs a single site-wide BMC password (it has no per-device MAC at this
/// layer), so this is one of the few legitimate site-wide credential consumers.
/// It resolves the *current* site-wide version from
/// `sitewide_credential_rotation.target_version` rather than reading a fixed
/// unversioned path, so after a rotation it hands DPF the version the fleet has
/// moved to.
pub struct CarbideBmcPasswordProvider {
    credential_reader: Arc<dyn carbide_secrets::credentials::CredentialReader>,
    db_pool: sqlx::PgPool,
}

impl CarbideBmcPasswordProvider {
    pub fn new(
        credential_reader: Arc<dyn carbide_secrets::credentials::CredentialReader>,
        db_pool: sqlx::PgPool,
    ) -> Self {
        Self {
            credential_reader,
            db_pool,
        }
    }

    /// Resolve the live site-wide BMC root version from the rotation table.
    /// A `target_version` of 0 (no rotation yet) maps to the legacy unversioned
    /// path via [`BmcCredentialType::site_wide_root`]. A *missing* row is a
    /// broken/unmigrated database -- the backfill seeds a row at 0 for every
    /// active type -- and is surfaced as an error rather than silently assuming
    /// 0, matching the rest of the rotation code.
    async fn current_sitewide_bmc_version(&self) -> Result<u32, DpfError> {
        // Single read; needs no transaction.
        let mut conn = self.db_pool.acquire().await.map_err(|e| {
            DpfError::InvalidState(format!(
                "Failed to acquire db connection for BMC rotation target: {e}"
            ))
        })?;
        let target_version = db::credential_rotation::current_target_version(
            &mut conn,
            db::credential_rotation::CredentialRotationType::Bmc,
        )
        .await
        .map_err(|e| DpfError::InvalidState(format!("Failed to read BMC rotation target: {e}")))?
        .ok_or_else(|| {
            DpfError::InvalidState(
                "No site-wide BMC rotation target row exists; the backfill migration seeds one \
                 for every active credential type, so a missing row indicates a broken or \
                 unmigrated database"
                    .to_string(),
            )
        })?;
        // The column is constrained non-negative, so a failed conversion means a
        // corrupt value, not "no rotation" -- surface it rather than masking it as
        // the legacy v0 path.
        u32::try_from(target_version).map_err(|_| {
            DpfError::InvalidState(format!(
                "site-wide BMC rotation target version {target_version} is negative; the column \
                 is constrained non-negative, so this indicates a corrupt database"
            ))
        })
    }
}

#[async_trait]
impl BmcPasswordProvider for CarbideBmcPasswordProvider {
    async fn get_bmc_password(&self) -> Result<String, DpfError> {
        use carbide_secrets::credentials::{BmcCredentialType, CredentialKey, Credentials};
        let version = self.current_sitewide_bmc_version().await?;
        let key = CredentialKey::BmcCredentials {
            credential_type: BmcCredentialType::site_wide_root(version),
        };
        match self.credential_reader.get_credentials(&key).await {
            Ok(Some(Credentials::UsernamePassword { password, .. })) => Ok(password),
            Ok(_) => Err(DpfError::InvalidState(
                "Site wide BMC root credentials not set".into(),
            )),
            Err(e) => Err(DpfError::InvalidState(format!(
                "Failed to read BMC credentials: {e}"
            ))),
        }
    }
}

/// DPF SDK operations implementation that wraps the real DPF SDK.
pub struct DpfSdkOps {
    sdk: Arc<DpfSdk<KubeRepository, CarbideDPFLabeler>>,
    _watcher: DpuWatcher,
}

impl DpfSdkOps {
    /// Create a new DpfSdkOps using the DPF SDK and sets up watcher callbacks to trigger carbide state handling.
    pub fn new(
        sdk: Arc<DpfSdk<KubeRepository, CarbideDPFLabeler>>,
        db_pool: PgPool,
        join_set: &mut JoinSet<()>,
    ) -> std::io::Result<Self> {
        let watcher = sdk
            .watcher()
            .on_dpu_event(|event| async move {
                tracing::debug!(
                    dpu_name = %event.dpu_name,
                    device_name = %event.device_name,
                    node = %event.node_name,
                    phase = ?event.phase,
                    "DPF DPU event"
                );
                Ok(())
            })
            .on_reboot_required({
                let db_pool = db_pool.clone();
                move |event| {
                    let db_pool = db_pool.clone();
                    async move {
                        tracing::info!(
                            node = %event.node_name,
                            host_bmc_ip_address = %event.host_bmc_ip,
                            "DPF reboot required"
                        );
                        enqueue_host(&db_pool, &event.node_name, "reboot", None).await
                    }
                }
            })
            .on_dpu_ready({
                let db_pool = db_pool.clone();
                move |event| {
                    let db_pool = db_pool.clone();
                    async move {
                        tracing::info!(
                            dpu_name = %event.dpu_name,
                            device_name = %event.device_name,
                            node = %event.node_name,
                            "DPF DPU ready"
                        );
                        enqueue_host(&db_pool, &event.node_name, "ready", None).await
                    }
                }
            })
            .on_maintenance_needed({
                let db_pool = db_pool.clone();
                move |event| {
                    let db_pool = db_pool.clone();
                    async move {
                        tracing::info!(
                            node = %event.node_name,
                            "DPF maintenance needed (NodeEffect phase)"
                        );
                        // A DPUService change is one of the things that drives a
                        // DPU here, and rolling it out needs carbide to release
                        // the hold once the DPU is confirmed up to date.
                        enqueue_host(
                            &db_pool,
                            &event.node_name,
                            "maintenance",
                            Some(MachinePendingActionKind::DpuServiceSync),
                        )
                        .await
                    }
                }
            })
            .on_error({
                move |event| {
                    let db_pool = db_pool.clone();
                    async move {
                        tracing::error!(
                            dpu_name = %event.dpu_name,
                            device_name = %event.device_name,
                            node = %event.node_name,
                            "DPF DPU entered error phase"
                        );
                        enqueue_host(&db_pool, &event.node_name, "error", None).await
                    }
                }
            })
            .with_join_set(join_set)
            .start()?;

        Ok(Self {
            sdk,
            _watcher: watcher,
        })
    }
}

/// Records that a host owes `kind`, returning the stored action.
async fn record_pending_action(
    db_pool: &PgPool,
    host_machine_id: &HostMachineId,
    kind: MachinePendingActionKind,
) -> Result<MachinePendingAction, DpfError> {
    let mut conn = db_pool.acquire().await.map_err(|e| {
        DpfError::InvalidState(format!("Failed to acquire database connection: {e}"))
    })?;
    db::machine_pending_action::request(&mut conn, host_machine_id, kind)
        .await
        .map_err(|e| {
            DpfError::InvalidState(format!(
                "DB error recording pending action for machine {host_machine_id}: {e}"
            ))
        })
}

/// Look up a host by DPUNode CR name and enqueue it for state handling.
/// CR name format: `node-{dpf_id}`, where `dpf_id` is the host's BMC MAC
/// address with colons replaced by hyphens.
///
/// `pending_action` records durable work the host owes before it is enqueued.
/// The queue itself cannot carry that signal: it stores only an object id and
/// coalesces duplicates, so this enqueue is indistinguishable from the periodic
/// sweep's and is dropped outright if the sweep queued the host first.
async fn enqueue_host(
    db_pool: &PgPool,
    node_name: &str,
    reason: &str,
    pending_action: Option<MachinePendingActionKind>,
) -> Result<(), DpfError> {
    let bmc_mac_id = node_id_from_dpu_node_cr_name(node_name);
    let bmc_mac: mac_address::MacAddress = bmc_mac_id
        .replace('-', ":")
        .parse()
        .map_err(|e| DpfError::InvalidState(format!("Invalid BMC MAC in node name: {e}")))?;

    let host_machine_id = {
        let mut conn = db_pool.acquire().await.map_err(|e| {
            DpfError::InvalidState(format!("Failed to acquire database connection: {e}"))
        })?;
        db::machine_topology::find_machine_id_by_bmc_mac(&mut conn, bmc_mac)
            .await
            .map_err(|e| {
                DpfError::InvalidState(format!("DB error looking up host by BMC MAC: {e}"))
            })?
    };

    let Some(host_machine_id) = host_machine_id else {
        tracing::warn!(node = %node_name, bmc_mac_address = %bmc_mac, reason, "Could not find host for DPF node");
        return Ok(());
    };
    let host_machine_id = HostMachineId::try_from(host_machine_id).map_err(|error| {
        DpfError::InvalidState(format!("BMC MAC resolved to a non-host machine: {error}"))
    })?;

    // Written before the enqueue so a processor cannot reach the host's handler
    // while the marker is still missing.
    //
    // A failure here must not suppress the enqueue: this callback's wake-up is
    // what the DPF provisioning handler relies on to release a node's hold, and
    // that is older and more important than the marker. The error is carried
    // past the enqueue and returned instead, so the watcher retries this event
    // (~30s) rather than leaving the marker to the next hourly resync.
    let marker_error = match pending_action {
        Some(kind) => match record_pending_action(db_pool, &host_machine_id, kind).await {
            Ok(action) => {
                tracing::info!(
                    node = %node_name,
                    machine_id = %host_machine_id,
                    requested_at = %action.requested_at,
                    "Recorded pending DPF action for host"
                );
                None
            }
            Err(error) => {
                tracing::warn!(
                    node = %node_name,
                    machine_id = %host_machine_id,
                    %error,
                    "Could not record pending DPF action; enqueueing the host before retrying"
                );
                Some(error)
            }
        },
        None => None,
    };

    Enqueuer::<MachineStateControllerIO>::new(db_pool.clone())
        .enqueue_object(&host_machine_id)
        .await
        .map_err(|e| {
            DpfError::InvalidState(format!("Failed to enqueue machine {host_machine_id}: {e}"))
        })?;

    tracing::info!(node = %node_name, machine_id = %host_machine_id, reason, "Enqueued host for DPF state handling");

    match marker_error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

impl std::fmt::Debug for DpfSdkOps {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DpfSdkOps").finish()
    }
}

/// Delegates everything to the underlying DPF SDK.
#[async_trait]
impl DpfOperations for DpfSdkOps {
    async fn register_dpu_device<'a>(
        &self,
        info: DpuDeviceInfo,
        astra_nics: Option<Vec<&'a DpaInterface>>,
    ) -> Result<(), DpfError> {
        self.sdk.register_dpu_device(info, astra_nics).await
    }

    async fn register_dpu_node(&self, info: DpuNodeInfo) -> Result<(), DpfError> {
        self.sdk.register_dpu_node(info).await
    }

    async fn release_maintenance_hold(&self, node_name: &str) -> Result<(), DpfError> {
        self.sdk.release_maintenance_hold(node_name).await
    }

    async fn force_delete_host(
        &self,
        node_id: &str,
        dpu_device_names: &[String],
    ) -> Result<(), DpfError> {
        self.sdk.force_delete_host(node_id, dpu_device_names).await
    }

    async fn reprovision_dpu(
        &self,
        dpu_device_name: &str,
        node_name: &str,
    ) -> Result<(), DpfError> {
        self.sdk.reprovision_dpu(dpu_device_name, node_name).await
    }

    async fn get_dpu_phase(
        &self,
        dpu_device_name: &str,
        node_name: &str,
    ) -> Result<DpuPhase, DpfError> {
        self.sdk.get_dpu_phase(dpu_device_name, node_name).await
    }

    async fn get_dpu_phases_for_deployment_type(
        &self,
        dpu_device_names: &[String],
        node_name: &str,
        deployment_type: DpuDeploymentType,
    ) -> Result<Option<BTreeMap<String, DpuPhase>>, DpfError> {
        self.sdk
            .get_dpu_phases_for_deployment_type(dpu_device_names, node_name, deployment_type)
            .await
    }

    async fn delete_source_dpus_for_deployment_migration(
        &self,
        dpu_device_names: &[String],
        node_name: &str,
        source_deployment_type: DpuDeploymentType,
        target_deployment_type: DpuDeploymentType,
    ) -> Result<(), DpfError> {
        self.sdk
            .delete_source_dpus_for_deployment_migration(
                dpu_device_names,
                node_name,
                source_deployment_type,
                target_deployment_type,
            )
            .await
    }

    async fn is_reboot_required(&self, node_name: &str) -> Result<bool, DpfError> {
        self.sdk.is_reboot_required(node_name).await
    }

    async fn reboot_complete(&self, node_name: &str) -> Result<(), DpfError> {
        self.sdk.reboot_complete(node_name).await
    }

    fn deployment_type_for_dpu(
        &self,
        dpu: &Machine,
        astra_nics: bool,
    ) -> Result<DpuDeploymentType, DpfError> {
        let product_name = dpu
            .status
            .hardware_info
            .as_ref()
            .and_then(|hw| hw.dmi_data.as_ref())
            .map(|d| d.product_name.trim())
            .filter(|s| !s.is_empty());

        let Some(product_name) = product_name else {
            return Err(DpfError::InvalidState(format!(
                "cannot determine DPU deployment type for machine {}: product name is absent",
                dpu.id,
            )));
        };

        // Only a BF3 or BF4 DPU can reach here.
        let deployment_type = if is_bf4_dmi_product(product_name) {
            if astra_nics {
                DpuDeploymentType::Bf4Astra
            } else {
                DpuDeploymentType::Bf4Generic
            }
        } else {
            DpuDeploymentType::Bf3
        };

        tracing::info!(
            machine_id = %dpu.id,
            product_name,
            astra_nics,
            ?deployment_type,
            "selected base DPF deployment type for DPU"
        );

        Ok(deployment_type)
    }

    async fn verify_node_labels(
        &self,
        node_name: &str,
        deployment_type: DpuDeploymentType,
    ) -> Result<bool, DpfError> {
        self.sdk
            .verify_node_labels(node_name, deployment_type)
            .await
    }

    async fn transfer_dpu_node_deployment_labels(
        &self,
        node_name: &str,
        source_deployment_type: DpuDeploymentType,
        target_deployment_type: DpuDeploymentType,
    ) -> Result<(), DpfError> {
        self.sdk
            .transfer_dpu_node_deployment_labels(
                node_name,
                source_deployment_type,
                target_deployment_type,
            )
            .await
    }

    async fn snapshot_host(&self, node_name: &str) -> Result<HostDpfSnapshot, DpfError> {
        self.sdk.snapshot_host(node_name).await
    }

    async fn is_dpu_outdated(&self, dpu_name: &str) -> Result<bool, DpfError> {
        self.sdk.is_dpu_outdated(dpu_name).await
    }

    async fn list_service_template_versions(
        &self,
    ) -> Result<Vec<ServiceTemplateVersion>, DpfError> {
        self.sdk.list_service_template_versions().await
    }

    async fn get_service_versions_for_dpu(
        &self,
        dpu_name: &str,
    ) -> Result<Vec<DpuServiceVersion>, DpfError> {
        self.sdk.get_service_versions_for_dpu(dpu_name).await
    }

    async fn find_outdated_dpus_dpf(&self) -> Result<Vec<OutdatedDpfDpu>, DpfError> {
        let label_selector = format!("{CONTROLLED_DEVICE_LABEL}=true");
        let mismatches = self
            .sdk
            .find_outdated_dpus_dpf(Some(label_selector.as_str()))
            .await?;

        let mut out = Vec::with_capacity(mismatches.len());
        for m in mismatches {
            let Some(machine_id_str) = m.dpu_labels.get(DPU_MACHINE_ID_LABEL) else {
                tracing::warn!(
                    dpu_name = %m.dpu_cr_name,
                    label = DPU_MACHINE_ID_LABEL,
                    "Outdated DPU missing label; skipping"
                );
                continue;
            };
            let dpu_machine_id: DpuMachineId = match machine_id_str.parse() {
                Ok(id) => id,
                Err(e) => {
                    tracing::warn!(
                        dpu_name = %m.dpu_cr_name,
                        label_value = %machine_id_str,
                        label = DPU_MACHINE_ID_LABEL,
                        error = %e,
                        "Outdated DPU has invalid label; skipping"
                    );
                    continue;
                }
            };
            out.push(OutdatedDpfDpu {
                dpu_machine_id,
                target_source: m.target_source,
            });
        }
        Ok(out)
    }

    async fn create_dpu_service(
        &self,
        service: &DetachedDpuServiceDefinition,
    ) -> Result<DpuServiceObservation, DpfError> {
        self.sdk.create_dpu_service(service).await
    }

    async fn get_dpu_service(
        &self,
        service_name: &str,
    ) -> Result<Option<DpuServiceObservation>, DpfError> {
        self.sdk.get_dpu_service(service_name).await
    }

    async fn patch_dpu_service(
        &self,
        service_name: &str,
        patch: serde_json::Value,
    ) -> Result<(), DpfError> {
        self.sdk.patch_dpu_service(service_name, patch).await
    }

    async fn delete_dpu_service(&self, service_name: &str) -> Result<(), DpfError> {
        self.sdk.delete_dpu_service(service_name).await
    }

    async fn merge_dpu_device_node_labels(
        &self,
        dpu_device_name: &str,
        changes: BTreeMap<String, Option<String>>,
    ) -> Result<(), DpfError> {
        self.sdk
            .merge_dpu_device_node_labels(dpu_device_name, changes)
            .await
    }

    async fn get_dpu_device_node_labels(
        &self,
        dpu_device_name: &str,
    ) -> Result<BTreeMap<String, String>, DpfError> {
        self.sdk.get_dpu_device_node_labels(dpu_device_name).await
    }
}

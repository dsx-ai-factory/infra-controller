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

//! Reconciles registered BF4 networking before HostInit and verifies explicit or registered targets before boot work.

use std::collections::HashSet;

use carbide_uuid::machine::{DpuMachineId, HostMachineId, MachineId};
use mac_address::MacAddress;
use model::machine::{LoadSnapshotOptions, ManagedHostState, ManagedHostStateSnapshot};
use model::machine_boot_interface::{
    BootInterfaceSelectionSource, MachineBootInterface, MachineBootInterfaceTarget,
    canonical_redfish_boot_interface_id,
};
use model::site_explorer::{
    EndpointExplorationReport, HostBootInterfaceDpu, HostPrimaryInterfaceError,
    HostPrimaryInterfaceSelection,
};
use sqlx::{PgConnection, PgPool};

use crate::{DatabaseError, DatabaseResult};

#[cfg(test)]
mod tests;

/// Describes the exact persisted fact still preventing BF4 boot work.
#[derive(Debug, thiserror::Error)]
pub enum HostBootInterfaceNotReady {
    /// A rejected DPU host-PF identity report still blocks new boot work.
    #[error("DPU {0} has a blocking registered host-PF identity drift alert")]
    DpuHostPfIdentityDrift(DpuMachineId),
    /// Registered inventory or current host Redfish evidence cannot select a target.
    #[error(transparent)]
    Selection(#[from] HostPrimaryInterfaceError),
    /// The host does not have the selected MAC as its only primary interface.
    #[error("selected MAC {0} is not the host's sole primary interface")]
    Primary(MacAddress),
    /// A target link is missing, attached to the wrong DPU, or duplicated.
    #[error("registered host-PF link for DPU {0} is not uniquely associated")]
    DpuAssociation(DpuMachineId),
    /// The selected interface does not have the current Redfish interface ID.
    #[error("selected MAC {0} lacks its current redfish boot-interface ID")]
    InterfaceId(MacAddress),
    /// Durable desired target and its selected MAC/ID disagree.
    #[error("desired boot target does not match the registered selection")]
    DesiredTarget,
    /// Durable selection metadata does not describe the current decision.
    #[error("boot selection source or decision time is inconsistent")]
    SelectionMetadata,
    /// A preserved automatic target disagrees with complete registered and Redfish evidence.
    #[error(
        "preserved automatic boot target disagrees with registered BF4 selection; use MachineSetup or SetDpuFirstBootOrder with an explicit MAC, or reinitialize the host to reconcile BF4 networking"
    )]
    PreservedAutomaticTarget,
}

/// Keeps one current host snapshot beside the Redfish report and registered candidates used to select its boot NIC.
struct HostBootInterfaceFacts {
    snapshot: ManagedHostStateSnapshot,
    report: Option<EndpointExplorationReport>,
    dpus: Vec<HostBootInterfaceDpu>,
    declared_primary: Option<MacAddress>,
    pending_declared_primary: Option<MachineBootInterface>,
}

impl HostBootInterfaceFacts {
    /// Rejects a paired DPU's dedicated identity alert so retained inventory cannot authorize new boot work after rejected drift.
    fn check_host_pf_drift(&self) -> Result<(), HostBootInterfaceNotReady> {
        // Read the dedicated report directly because DPU-agent network health and operator boot intent cannot clear rejected identity drift.
        if let Some(dpu) = self.snapshot.dpu_snapshots.iter().find(|dpu| {
            dpu.health_reports
                .merges
                .get("dpu-host-pf")
                .is_some_and(|report| {
                    report.has_classification(
                        &health_report::HealthAlertClassification::prevent_host_state_changes(),
                    )
                })
        }) {
            return Err(HostBootInterfaceNotReady::DpuHostPfIdentityDrift(dpu.id));
        }
        Ok(())
    }

    /// Selects the networking primary from registered inventory and the ExpectedMachine declaration independently of operator boot intent.
    fn selection(
        &self,
        controller: u8,
    ) -> Result<HostPrimaryInterfaceSelection, HostPrimaryInterfaceError> {
        let report = self
            .report
            .as_ref()
            .ok_or(HostPrimaryInterfaceError::MissingHostSystem)?;
        report.select_host_primary_interface_from_dpus(
            &self.dpus,
            self.declared_primary,
            controller,
        )
    }
}

/// Loads registered DPU inventory and the latest host report without depending on a remembered readiness flag.
async fn load_facts(
    txn: &mut PgConnection,
    host_machine_id: &HostMachineId,
    controller: u8,
) -> DatabaseResult<Option<HostBootInterfaceFacts>> {
    let Some(snapshot) = crate::managed_host::load_snapshot(
        txn,
        &(*host_machine_id).into(),
        LoadSnapshotOptions::default(),
    )
    .await?
    else {
        return Ok(None);
    };
    load_snapshot_facts(txn, snapshot, controller).await
}

/// Loads Redfish evidence and registered candidates beside one supplied snapshot so networking and boot readiness use the same host interfaces and DPU reports.
async fn load_snapshot_facts(
    txn: &mut PgConnection,
    snapshot: ManagedHostStateSnapshot,
    controller: u8,
) -> DatabaseResult<Option<HostBootInterfaceFacts>> {
    // Build candidates from the supplied snapshot so boot readiness uses the DPU reports accepted by the network check.
    let mut paired_ids = HashSet::new();
    let dpus = snapshot
        .dpu_snapshots
        .iter()
        .filter(|dpu| paired_ids.insert(dpu.id))
        .map(|dpu| {
            let hardware = dpu
                .status
                .hardware_info
                .as_ref()
                .and_then(|info| info.dpu_info.as_ref())
                .cloned()
                .unwrap_or_default();
            // Factory identity remains a legacy candidate only; BF4 selection reads registered host_pfs.
            let provisional_mac_address = hardware.factory_mac_address.parse().ok();
            HostBootInterfaceDpu {
                dpu_machine_id: dpu.id,
                hardware,
                provisional_mac_address,
            }
        })
        .collect::<Vec<_>>();
    if !dpus
        .iter()
        .any(|dpu| dpu.hardware.required_host_pf(controller).is_some())
    {
        return Ok(None);
    }
    // Read current host Redfish evidence and the declared primary in the caller's transaction.
    let report = if let Some(ip) = snapshot.host_snapshot.status.bmc_info.ip {
        crate::explored_endpoints::find_by_ips(&mut *txn, vec![ip])
            .await?
            .into_iter()
            .next()
            .map(|endpoint| endpoint.report)
    } else {
        None
    };
    let declared_primary = if let Some(mac) = snapshot.host_snapshot.status.bmc_info.mac {
        crate::expected_machine::find_by_bmc_mac_address(&mut *txn, mac)
            .await?
            .and_then(|expected| expected.data.declared_primary_mac())
    } else {
        None
    };
    // Require a prediction without a real row so a declaration cannot bypass existing interface ownership.
    let pending_declared_primary = if let Some(mac) = declared_primary
        && crate::machine_interface::find_by_mac_address(&mut *txn, mac)
            .await?
            .is_empty()
    {
        crate::predicted_machine_interface::find_by_mac_address(&mut *txn, mac)
            .await?
            .and_then(|prediction| {
                let target = MachineBootInterface {
                    mac_address: mac,
                    interface_id: report.as_ref()?.find_interface_id_for_mac(mac)?.to_owned(),
                };
                crate::predicted_machine_interface::matches_primary_host_boot_interface(
                    &prediction,
                    &snapshot.host_snapshot.id,
                    &target,
                )
                .then_some(target)
            })
    } else {
        None
    };
    Ok(Some(HostBootInterfaceFacts {
        snapshot,
        report,
        dpus,
        declared_primary,
        pending_declared_primary,
    }))
}

/// Reconciles exact BF4 host links and explicitly replaces provisional desired targets only before HostInit.
///
/// This operation preserves Ready hosts, every persisted instance allocation, and operator-selected MACs.
/// A unique matching interface ID may enrich an Operator MAC-only target without changing networking.
/// Callers must begin a fresh transaction and keep it open through commit or rollback. An error
/// requires rollback; ordinary unresolved selection returns without changing persisted networking.
/// Returns true when BF4 policy applies, including when repair is skipped or unresolved.
pub async fn reconcile_pre_host_init(
    txn: &mut PgConnection,
    host_machine_id: &HostMachineId,
    controller: u8,
    admin_fnn_enabled: bool,
    retained_window: Option<chrono::Duration>,
) -> DatabaseResult<bool> {
    let Some(initial) = load_facts(txn, host_machine_id, controller).await? else {
        return Ok(false);
    };
    // Ready hosts migrate only after an operator explicitly returns them to initialization.
    if !matches!(
        initial.snapshot.managed_state,
        ManagedHostState::DpuDiscoveringState { .. }
            | ManagedHostState::DPUInit { .. }
            | ManagedHostState::ConfigureAstra { .. }
    ) {
        tracing::debug!(%host_machine_id, machine_state = ?initial.snapshot.managed_state,
            reason = "outside_pre_host_init", "Registered host-PF repair skipped");
        return Ok(true);
    }

    // Avoid taking site-wide admin locks for automatic repair while immutable registration or host Redfish evidence is still missing.
    // Operator ID enrichment needs only the explicitly selected MAC, so missing automatic evidence must not block it.
    if !initial
        .snapshot
        .host_snapshot
        .config
        .boot_interface_selection
        .is_some_and(|selection| selection.source == BootInterfaceSelectionSource::Operator)
        && let Err(reason) = initial.selection(controller)
    {
        tracing::debug!(%host_machine_id, pci_controller_id = controller, pf_id = 0,
            dpu_count = initial.dpus.len(), %reason,
            "Waiting for registered BF4 boot-interface evidence");
        return Ok(true);
    }

    // Preserve segment-before-row ordering, then serialize the allocation check with instance creation.
    crate::machine_interface::lock_all_admin_segments(txn).await?;
    crate::machine_interface::lock_host_boot_interface_rows(txn, host_machine_id).await?;
    let mut locked_ids = initial
        .dpus
        .iter()
        .map(|dpu| MachineId::from(dpu.dpu_machine_id))
        .chain(std::iter::once((*host_machine_id).into()))
        .collect::<Vec<_>>();
    locked_ids.sort();
    locked_ids.dedup();
    let query = "SELECT id FROM machines WHERE id = ANY($1) ORDER BY id FOR UPDATE NOWAIT";
    let _: Vec<MachineId> = sqlx::query_scalar(query)
        .bind(
            locked_ids
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
        )
        .fetch_all(&mut *txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;

    // Re-read after locking; a committed Instance blocks repair even if controller_state still says Ready or DPUInit.
    let Some(facts) = load_facts(txn, host_machine_id, controller).await? else {
        return Ok(false);
    };
    if crate::instance::find_id_by_machine_id(&mut *txn, &(*host_machine_id).into())
        .await?
        .is_some()
    {
        tracing::warn!(%host_machine_id, reason = "instance_allocation_exists", "Registered host-PF repair skipped");
        return Ok(true);
    }
    if !matches!(
        facts.snapshot.managed_state,
        ManagedHostState::DpuDiscoveringState { .. }
            | ManagedHostState::DPUInit { .. }
            | ManagedHostState::ConfigureAstra { .. }
    ) {
        tracing::debug!(%host_machine_id, reason = "host_state_changed", "Registered host-PF repair skipped");
        return Ok(true);
    }
    if facts
        .dpus
        .iter()
        .any(|dpu| !locked_ids.contains(&dpu.dpu_machine_id.into()))
    {
        return Err(DatabaseError::FailedPrecondition(format!(
            "paired DPU set changed during host-PF repair for {host_machine_id}; retry"
        )));
    }
    if facts
        .snapshot
        .host_snapshot
        .config
        .boot_interface_selection
        .is_some_and(|selection| selection.source == BootInterfaceSelectionSource::Operator)
    {
        // Enrich only the selected MacOnly target so learning its ID preserves Operator selection and networking.
        if let Some(MachineBootInterfaceTarget::MacOnly(mac_address)) = facts
            .snapshot
            .host_snapshot
            .config
            .desired_boot_interface
            .as_ref()
            .map(|desired| &desired.value)
        {
            let mut interface_ids = facts
                .snapshot
                .host_snapshot
                .status
                .interfaces
                .iter()
                .filter(|interface| {
                    interface.interface_type != model::machine_interface::InterfaceType::Bmc
                        && interface.mac_address == *mac_address
                })
                .filter_map(|interface| interface.boot_interface_id.as_deref())
                .filter_map(canonical_redfish_boot_interface_id)
                .map(str::to_owned)
                .collect::<HashSet<_>>();
            // Use predicted IDs only when owned IDs are missing so conflicting owned IDs remain unresolved.
            if interface_ids.is_empty() {
                interface_ids.extend(
                    crate::predicted_machine_interface::find_by_machine_id(txn, host_machine_id)
                        .await?
                        .into_iter()
                        .filter(|prediction| prediction.mac_address == *mac_address)
                        .filter_map(|prediction| prediction.boot_interface_id)
                        .filter_map(|interface_id| {
                            canonical_redfish_boot_interface_id(&interface_id).map(str::to_owned)
                        }),
                );
            }
            if interface_ids.len() == 1
                && let Some(interface_id) = interface_ids.into_iter().next()
            {
                crate::machine_desired_boot_interface::enrich_interface_id(
                    txn,
                    host_machine_id,
                    *mac_address,
                    &interface_id,
                )
                .await?;
            }
        }
        tracing::debug!(%host_machine_id, reason = "operator_selection", "Registered host-PF network repair skipped");
        return Ok(true);
    }

    let selected = match facts.selection(controller) {
        Ok(selected) => selected,
        Err(reason) => {
            tracing::debug!(%host_machine_id, pci_controller_id = controller, pf_id = 0,
                %reason, "Waiting for registered BF4 boot-interface evidence");
            return Ok(true);
        }
    };
    let report = facts.report.as_ref().ok_or_else(|| {
        DatabaseError::FailedPrecondition(
            "host Redfish report disappeared during boot selection".to_string(),
        )
    })?;
    let interface_id = report
        .find_interface_id_for_mac(selected.mac_address)
        .ok_or_else(|| {
            DatabaseError::FailedPrecondition(format!(
                "selected MAC {} has no current Redfish interface ID",
                selected.mac_address
            ))
        })?;
    let target = MachineBootInterface {
        mac_address: selected.mac_address,
        interface_id: interface_id.to_owned(),
    };

    let mut targets = Vec::new();
    for dpu in &facts.dpus {
        if dpu.hardware.required_host_pf(controller).is_none() {
            continue;
        }
        match dpu.host_mac_address(controller) {
            Ok(Some(mac)) => targets.push((dpu.dpu_machine_id, mac)),
            // A declared boot NIC does not impose an independent DPU link-inventory readiness policy.
            Err(reason) if selected.source == BootInterfaceSelectionSource::ExpectedMachine => {
                tracing::debug!(%host_machine_id, dpu_machine_id = %dpu.dpu_machine_id,
                    %reason, "DPU link inventory unresolved while using a declared boot NIC");
            }
            Err(source) => {
                return Err(DatabaseError::DpuHostPf {
                    dpu_machine_id: dpu.dpu_machine_id,
                    source,
                });
            }
            Ok(None) => {}
        }
    }
    crate::machine_interface::reconcile_registered_host_interfaces(
        txn,
        host_machine_id,
        &targets,
        &target,
        selected.source,
        admin_fnn_enabled,
        retained_window,
    )
    .await?;
    for (_, mac) in &targets {
        if let Some(interface_id) = report.find_interface_id_for_mac(*mac) {
            crate::machine_interface::set_boot_interface_id(*mac, interface_id, txn).await?;
        }
    }

    // Publish target and truthful decision metadata atomically with the exact associations and DHCP ownership.
    crate::machine_desired_boot_interface::set(
        txn,
        host_machine_id,
        &MachineBootInterfaceTarget::Pair(target),
        selected.source,
    )
    .await?;
    Ok(true)
}

/// Pages initializing hosts so late registration repairs completed provisional targets even without fresh exploration.
pub async fn find_initializing_host_ids(
    pool: &PgPool,
    after: Option<&HostMachineId>,
    limit: i64,
) -> DatabaseResult<Vec<HostMachineId>> {
    let query = "SELECT id FROM machines WHERE NOT starts_with(id, $1)
        AND controller_state->>'state' IN ('dpudiscoveringstate', 'dpuinit', 'configureastra')
        AND ($2::text IS NULL OR id > $2) ORDER BY id LIMIT $3";
    sqlx::query_scalar(query)
        .bind(carbide_uuid::machine::MachineType::Dpu.id_prefix())
        .bind(after.map(ToString::to_string))
        .bind(limit)
        .fetch_all(pool)
        .await
        .map_err(|error| DatabaseError::query(query, error))
}

/// Recomputes automatic BF4 networking readiness or accepts explicit Operator boot intent before HostInit.
///
/// A blocking registered host-PF identity drift alert prevents readiness for every target source.
pub async fn not_ready_reason(
    pool: &PgPool,
    host_machine_id: &HostMachineId,
    controller: u8,
) -> DatabaseResult<Option<HostBootInterfaceNotReady>> {
    let mut txn = crate::Transaction::begin(pool).await?;
    let facts = load_facts(txn.as_pgconn(), host_machine_id, controller).await?;
    let reason = facts
        .as_ref()
        .and_then(|facts| check_ready(facts, controller).err());
    txn.commit().await?;
    Ok(reason)
}

/// Checks BF4 boot evidence against the supplied snapshot so HostInit validates the same host interfaces and DPU reports as its network check.
///
/// Callers that require stable host networking must acquire `lock_host_init_network_config`
/// before loading the snapshot and retain that transaction through controller-state publication.
pub async fn not_ready_reason_for_snapshot(
    txn: &mut PgConnection,
    snapshot: ManagedHostStateSnapshot,
    controller: u8,
) -> DatabaseResult<Option<HostBootInterfaceNotReady>> {
    // Keep boot evidence beside the exact DPU observations accepted by the caller.
    let facts = load_snapshot_facts(txn, snapshot, controller).await?;
    Ok(facts
        .as_ref()
        .and_then(|facts| check_ready(facts, controller).err()))
}

/// Verifies registered networking and durable automatic intent before HostInit while honoring independent operator boot intent.
///
/// Rejected host-PF identity drift blocks readiness before any target source can authorize boot work.
fn check_ready(
    facts: &HostBootInterfaceFacts,
    controller: u8,
) -> Result<(), HostBootInterfaceNotReady> {
    facts.check_host_pf_drift()?;
    // Operator boot intent is independent of networking primary selection, including SetPrimaryInterface choices.
    if facts
        .snapshot
        .host_snapshot
        .config
        .boot_interface_selection
        .is_some_and(|selection| selection.source == BootInterfaceSelectionSource::Operator)
    {
        return Ok(());
    }
    let selected = facts.selection(controller)?;
    let report = facts
        .report
        .as_ref()
        .ok_or(HostPrimaryInterfaceError::MissingHostSystem)?;
    let interface_id = report
        .find_interface_id_for_mac(selected.mac_address)
        .ok_or(HostPrimaryInterfaceError::MissingInterfaceId(
            selected.mac_address,
        ))?;
    let interfaces = &facts.snapshot.host_snapshot.status.interfaces;
    let primary = interfaces
        .iter()
        .filter(|interface| interface.primary_interface)
        .collect::<Vec<_>>();
    let pending_declared_primary = selected.source == BootInterfaceSelectionSource::ExpectedMachine
        && facts
            .pending_declared_primary
            .as_ref()
            .is_some_and(|target| {
                target.mac_address == selected.mac_address && target.interface_id == interface_id
            });
    // Keep the interim networking primary until DHCP promotes the exact declared prediction.
    if !pending_declared_primary
        && (primary.len() != 1 || primary[0].mac_address != selected.mac_address)
    {
        return Err(HostBootInterfaceNotReady::Primary(selected.mac_address));
    }
    if selected.source == BootInterfaceSelectionSource::RedfishUefiPci {
        for dpu in &facts.dpus {
            let mac = dpu.host_mac_address(controller).map_err(|source| {
                HostPrimaryInterfaceError::DpuInventory {
                    dpu_machine_id: dpu.dpu_machine_id,
                    source,
                }
            })?;
            let links = interfaces
                .iter()
                .filter(|interface| interface.attached_dpu_machine_id == Some(dpu.dpu_machine_id))
                .collect::<Vec<_>>();
            if links.len() != 1 || Some(links[0].mac_address) != mac {
                return Err(HostBootInterfaceNotReady::DpuAssociation(
                    dpu.dpu_machine_id,
                ));
            }
        }
    }
    if !pending_declared_primary && primary[0].boot_interface_id.as_deref() != Some(interface_id) {
        return Err(HostBootInterfaceNotReady::InterfaceId(selected.mac_address));
    }
    let expected = MachineBootInterfaceTarget::Pair(MachineBootInterface {
        mac_address: selected.mac_address,
        interface_id: interface_id.to_owned(),
    });
    if facts
        .snapshot
        .host_snapshot
        .config
        .desired_boot_interface
        .as_ref()
        .is_none_or(|target| target.value != expected)
    {
        return Err(HostBootInterfaceNotReady::DesiredTarget);
    }
    if facts
        .snapshot
        .host_snapshot
        .config
        .boot_interface_selection
        .is_none_or(|selection| {
            selection.source != selected.source || selection.updated_at.is_none()
        })
    {
        return Err(HostBootInterfaceNotReady::SelectionMetadata);
    }
    Ok(())
}

/// Checks the actual target before a new BF4 boot-order request without migrating an existing host's networking or selection history.
///
/// A blocking registered identity drift alert prevents new requests, including Operator targets.
/// Operator intent may differ from the networking primary. Outside pre-HostInit repair states,
/// automatic intent must still agree with complete registered and Redfish evidence, but legacy
/// source metadata and existing network associations do not prevent reapplying that same target.
/// Callers must pass the captured or resolved target they will submit to Redfish.
pub async fn boot_order_not_ready_reason(
    pool: &PgPool,
    host_machine_id: &HostMachineId,
    controller: u8,
    target: &MachineBootInterfaceTarget,
) -> DatabaseResult<Option<HostBootInterfaceNotReady>> {
    // Reload the current selection authority rather than treating an in-flight captured target as operator intent.
    let mut txn = crate::Transaction::begin(pool).await?;
    let facts = load_facts(txn.as_pgconn(), host_machine_id, controller).await?;
    let reason = facts
        .as_ref()
        .and_then(|facts| check_boot_order_ready(facts, controller, target).err());
    txn.commit().await?;
    Ok(reason)
}

/// Verifies explicit boot authority or current registered target evidence while preserving legacy hosts until explicit reinitialization.
fn check_boot_order_ready(
    facts: &HostBootInterfaceFacts,
    controller: u8,
    target: &MachineBootInterfaceTarget,
) -> Result<(), HostBootInterfaceNotReady> {
    facts.check_host_pf_drift()?;
    let config = &facts.snapshot.host_snapshot.config;
    // An explicit MAC is allowed to select a different boot NIC without moving addresses or the networking primary.
    if config
        .boot_interface_selection
        .is_some_and(|selection| selection.source == BootInterfaceSelectionSource::Operator)
    {
        return if config
            .desired_boot_interface
            .as_ref()
            .is_some_and(|desired| {
                desired.value.mac_address() == target.mac_address()
                    && desired
                        .value
                        .interface_id()
                        .is_none_or(|interface_id| target.interface_id() == Some(interface_id))
            }) {
            Ok(())
        } else {
            Err(HostBootInterfaceNotReady::DesiredTarget)
        };
    }

    // Initial ingestion must publish the full networking and automatic-target proof before boot work can begin.
    if matches!(
        facts.snapshot.managed_state,
        ManagedHostState::DpuDiscoveringState { .. }
            | ManagedHostState::DPUInit { .. }
            | ManagedHostState::ConfigureAstra { .. }
    ) {
        check_ready(facts, controller)?;
        return if config
            .desired_boot_interface
            .as_ref()
            .is_some_and(|desired| &desired.value == target)
        {
            Ok(())
        } else {
            Err(HostBootInterfaceNotReady::DesiredTarget)
        };
    }

    // Existing lifecycles may reapply a registered target without rewriting truthful legacy source metadata or active networking.
    let selected = facts.selection(controller)?;
    let interface_id = facts
        .report
        .as_ref()
        .and_then(|report| report.find_interface_id_for_mac(selected.mac_address))
        .ok_or(HostPrimaryInterfaceError::MissingInterfaceId(
            selected.mac_address,
        ))?;
    if target.mac_address() != selected.mac_address
        || target
            .interface_id()
            .is_some_and(|existing| existing != interface_id)
    {
        return Err(HostBootInterfaceNotReady::PreservedAutomaticTarget);
    }
    Ok(())
}

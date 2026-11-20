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

use carbide_uuid::machine::{DpuMachineId, HostMachineId};
use config_version::ConfigVersion;
use model::machine::{DpuInitState, ManagedHostState};
use model::machine_boot_interface::{
    BootInterfaceSelectionAuthority, BootInterfaceSelectionSource, MachineBootInterface,
    MachineBootInterfaceTarget,
};

use super::*;

/// Associates one DPU with its registered host-visible MAC without changing its factory identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisteredHostPf {
    /// DPU to associate with this host-visible interface.
    pub dpu_id: DpuMachineId,
    /// MAC used to create or adopt the DPU-attached Admin interface.
    pub mac_address: MacAddress,
    /// Optional Redfish boot interface ID to apply to this MAC.
    pub boot_interface_id: Option<String>,
}

/// Keeps network-primary selection separate from automatic boot intent during initial host repair.
#[derive(Debug, PartialEq, Eq)]
pub struct HostPfInterfacePlan {
    /// Host-visible interface assignments for the paired DPUs.
    pub links: Vec<RegisteredHostPf>,
    /// MAC whose owned interface becomes the networking primary.
    pub network_primary_mac: MacAddress,
    /// Mechanism selecting the networking primary, which can differ from the boot target's source.
    pub network_selection_source: BootInterfaceSelectionSource,
    /// Proposed boot target; automatic plans preserve explicit intent, while current declarations may update ExpectedMachine selections.
    pub boot_target: MachineBootInterfaceTarget,
    /// Mechanism selecting the boot target independently of networking-primary provenance.
    pub boot_selection_source: BootInterfaceSelectionSource,
    /// Carries the current report's complete canonical Pair for the stored desired MAC so publication detects ID changes independently of the proposed boot target.
    pub reported_desired_boot_interface: Option<MachineBootInterface>,
}

/// Locks admin segments, affected interfaces, predictions and addresses before the machine group, then rejects stale initialization or any persisted instance.
///
/// Returns the locked interface IDs on successful admission. Interface contention returns None and aborts the transaction.
/// Stale or duplicate snapshots also return None. FailedPrecondition only reports persisted DPU states that disagree with unchanged attached DPUs.
/// Callers must acquire admin-lock admission before opening the transaction and roll back on None or error.
/// Include every MAC the plan may adopt or predict; repair must not acquire additional existing interface rows.
pub async fn lock_host_pf_initialization(
    txn: &mut PgConnection,
    host_id: &HostMachineId,
    expected_state_version: ConfigVersion,
    expected_dpus: &[DpuMachineId],
    target_macs: &[MacAddress],
) -> DatabaseResult<Option<Vec<MachineInterfaceId>>> {
    // Follow allocator order before taking any host or DPU machine row.
    lock_all_admin_segments(txn).await?;
    let mut machine_ids = expected_dpus
        .iter()
        .map(|id| id.to_machine_id())
        .collect_vec();
    machine_ids.push(host_id.to_machine_id());
    // Legacy rediscovery locks OOB before its host link, so never wait while holding another interface.
    let query = "SELECT id FROM machine_interfaces
                 WHERE machine_id = ANY($1) OR attached_dpu_machine_id = ANY($1)
                    OR mac_address = ANY($2)
                 ORDER BY id FOR UPDATE NOWAIT";
    let interface_ids: Vec<MachineInterfaceId> = match sqlx::query_scalar(query)
        .bind(&machine_ids)
        .bind(target_macs)
        .fetch_all(&mut *txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))
    {
        Ok(ids) => ids,
        // The caller rolls back before retrying, releasing earlier interface and segment locks.
        Err(error) if error.is_lock_not_available() => return Ok(None),
        Err(error) => return Err(error),
    };
    let query = "SELECT id FROM predicted_machine_interfaces
                 WHERE machine_id = $1 OR mac_address = ANY($2) ORDER BY id FOR UPDATE";
    sqlx::query(query)
        .bind(host_id)
        .bind(target_macs)
        .fetch_all(&mut *txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;
    lock_admin_interface_addresses(txn, &interface_ids).await?;
    if !crate::machine::try_lock_group_for_update(txn, host_id.as_machine_id()).await? {
        return Ok(None);
    }

    // Allocation takes the host lock before inserting an instance, including deleted instances.
    let query = "SELECT controller_state_version, controller_state FROM machines WHERE id = $1";
    let current: Option<(ConfigVersion, sqlx::types::Json<ManagedHostState>)> =
        sqlx::query_as(query)
            .bind(host_id)
            .fetch_optional(&mut *txn)
            .await
            .map_err(|error| DatabaseError::query(query, error))?;
    let Some((version, sqlx::types::Json(ManagedHostState::DPUInit { dpu_states }))) = current
    else {
        return Ok(None);
    };
    if version != expected_state_version
        || !dpu_states
            .states
            .values()
            .all(|state| matches!(state, DpuInitState::WaitingForNetworkConfig))
        || crate::instance::find_id_by_machine_id(&mut *txn, host_id.as_machine_id())
            .await?
            .is_some()
    {
        return Ok(None);
    }
    let query = "SELECT DISTINCT attached_dpu_machine_id FROM machine_interfaces
                 WHERE machine_id = $1 AND interface_type != 'Bmc'
                   AND attached_dpu_machine_id IS NOT NULL ORDER BY attached_dpu_machine_id";
    let attached: Vec<DpuMachineId> = sqlx::query_scalar(query)
        .bind(host_id)
        .fetch_all(txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;
    let mut expected = expected_dpus.to_vec();
    expected.sort();
    expected.dedup();
    // Reload changed attachments or duplicate snapshot DPUs before diagnosing persisted DPU states.
    if expected.is_empty() || attached != expected || expected.len() != expected_dpus.len() {
        return Ok(None);
    }
    // Report the stored mismatch separately so callers can use periodic retry for unchanged attachments.
    if dpu_states.states.len() != expected.len()
        || !dpu_states.states.keys().all(|id| expected.contains(id))
    {
        return Err(DatabaseError::FailedPrecondition(
            "BF4 host initialization DPU states do not match attached DPUs".to_string(),
        ));
    }
    Ok(Some(interface_ids))
}

/// Applies registered PF links, primary selection, admin addresses and automatic boot intent in the caller's initialization transaction.
///
/// Callers must retain successful lock_host_pf_initialization locks, pass its returned interface IDs, recompute the plan from current facts, and commit or roll back the entire repair.
/// Existing static allocations remain attached to their original MACs. Automatic plans preserve Operator and ExpectedMachine boot intent; a current declaration may replace an ExpectedMachine selection.
/// Callers must supply link IDs from the current host report or current same-MAC desired target, and boot-target IDs from the current host report.
/// Callers must supply reported_desired_boot_interface as a complete canonical Pair from that report for the stored desired MAC, without falling back to stored IDs.
/// Returns TryAgain when an existing interface was not admitted, requiring a fresh admission before repair.
pub async fn repair_host_pf_interfaces(
    txn: &mut PgConnection,
    host_id: &HostMachineId,
    plan: &HostPfInterfacePlan,
    admitted_interface_ids: &[MachineInterfaceId],
    retained_window: Option<chrono::Duration>,
) -> DatabaseResult<bool> {
    let host_machine_id = host_id.to_machine_id();
    let mut by_machine = find_by_machine_ids(txn, &[*host_id]).await?;
    let original = by_machine.remove(host_id).unwrap_or_default();
    // Capture the networking decision before demotion clears the current primary interface row.
    let query = "SELECT primary_selection_source, primary_selection_updated_at
                 FROM machine_interfaces WHERE machine_id = $1 AND primary_interface AND interface_type != 'Bmc'";
    let previous_selection: Option<(Option<BootInterfaceSelectionSource>, Option<DateTime<Utc>>)> =
        sqlx::query_as(query)
            .bind(host_id)
            .fetch_optional(&mut *txn)
            .await
            .map_err(|error| DatabaseError::query(query, error))?;
    // Resolve explicit boot authority before preparing an automatic boot-only candidate.
    let query = "SELECT selection_source FROM machine_boot_interfaces WHERE machine_id = $1";
    let selection_source: Option<BootInterfaceSelectionSource> = sqlx::query_scalar(query)
        .bind(host_id)
        .fetch_optional(&mut *txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;
    let mut seen_macs = HashSet::new();
    let mut seen_dpus = HashSet::new();

    // Validate every existing assignment before creating or adopting any row.
    for link in &plan.links {
        if !seen_macs.insert(link.mac_address) || !seen_dpus.insert(link.dpu_id) {
            return Err(DatabaseError::FailedPrecondition(
                "duplicate registered host PF assignment".to_string(),
            ));
        }
        let rows = find_by_mac_address(&mut *txn, link.mac_address).await?;
        // A newly visible reservation must join the allocator lock order on a fresh attempt.
        if rows
            .iter()
            .any(|row| !admitted_interface_ids.contains(&row.id))
        {
            return Err(DatabaseError::TryAgain);
        }
        if rows.len() > 1
            || rows.iter().any(|row| {
                row.machine_id.is_some_and(|id| id != host_machine_id)
                    || row
                        .attached_dpu_machine_id
                        .is_some_and(|id| id != link.dpu_id)
                    || row.switch_id.is_some()
                    || row.power_shelf_id.is_some()
                    || row.interface_type == InterfaceType::Bmc
                    || row.network_segment_type != Some(NetworkSegmentType::Admin)
            })
        {
            return Err(DatabaseError::FailedPrecondition(format!(
                "registered host PF {} for DPU {} has conflicting ownership or network segment",
                link.mac_address, link.dpu_id,
            )));
        }
        if let Some(prediction) =
            crate::predicted_machine_interface::find_by_mac_address(txn, link.mac_address).await?
            && prediction.machine_id != host_machine_id
        {
            return Err(DatabaseError::FailedPrecondition(format!(
                "host PF {} is predicted for another machine",
                link.mac_address
            )));
        }
    }
    // Only a current declaration may replace ExpectedMachine intent; Operator selection retains precedence over declarations.
    let preserve_boot_selection = selection_source == Some(BootInterfaceSelectionSource::Operator)
        || selection_source == Some(BootInterfaceSelectionSource::ExpectedMachine)
            && plan.boot_selection_source != BootInterfaceSelectionSource::ExpectedMachine;
    let boot_mac = plan.boot_target.mac_address();
    let boot_overridden = preserve_boot_selection && !seen_macs.contains(&boot_mac);
    let boot_rows = if boot_overridden {
        // Enrich only an already-owned data interface when explicit boot intent overrides this candidate.
        original
            .iter()
            .filter(|row| row.mac_address == boot_mac && row.interface_type != InterfaceType::Bmc)
            .cloned()
            .collect_vec()
    } else {
        // Required PFs and active boot targets retain full ownership validation before mutation.
        find_by_mac_address(&mut *txn, boot_mac).await?
    };
    // Separate boot-NIC adoption needs the same pre-machine interface and address locks.
    if boot_rows
        .iter()
        .any(|row| !admitted_interface_ids.contains(&row.id))
    {
        return Err(DatabaseError::TryAgain);
    }
    if boot_rows.len() > 1
        || boot_rows.iter().any(|row| {
            row.machine_id.is_some_and(|id| id != host_machine_id)
                || row.switch_id.is_some()
                || row.power_shelf_id.is_some()
                || row.interface_type == InterfaceType::Bmc
        })
    {
        return Err(DatabaseError::FailedPrecondition(format!(
            "boot MAC {boot_mac} has conflicting ownership"
        )));
    }
    let boot_prediction = if boot_overridden {
        None
    } else {
        crate::predicted_machine_interface::find_by_mac_address(txn, boot_mac).await?
    };
    if boot_prediction
        .as_ref()
        .is_some_and(|row| row.machine_id != host_machine_id)
    {
        return Err(DatabaseError::FailedPrecondition(format!(
            "boot MAC {boot_mac} is predicted for another machine"
        )));
    }

    let segments = load_admin_segments(txn).await?;
    let mut changed = false;
    let mut network_changed = false;
    let mut corrected_ids = Vec::new();
    for link in &plan.links {
        let mut rows = find_by_mac_address(&mut *txn, link.mac_address).await?;
        // An absent MAC can appear after validation; retry admission instead of locking it after the machine group.
        if rows
            .iter()
            .any(|row| !admitted_interface_ids.contains(&row.id))
        {
            return Err(DatabaseError::TryAgain);
        }
        let row = if let Some(row) = rows.pop() {
            row
        } else {
            // Reuse this DPU's bootstrap segment so DHCP can move without requiring spare addresses.
            let segment_id = original
                .iter()
                .find(|row| {
                    row.attached_dpu_machine_id == Some(link.dpu_id)
                        && row.network_segment_type == Some(NetworkSegmentType::Admin)
                })
                .map(|row| row.segment_id)
                .ok_or_else(|| {
                    DatabaseError::FailedPrecondition(format!(
                        "DPU {} has no provisional admin link",
                        link.dpu_id
                    ))
                })?;
            let segment = segments
                .iter()
                .find(|segment| segment.id == segment_id)
                .ok_or(DatabaseError::AdminNetworkNotConfigured)?;
            changed = true;
            network_changed = true;
            create_without_addresses(
                txn,
                segment,
                &link.mac_address,
                false,
                InterfaceType::Data,
                retained_window,
            )
            .await?
        };
        if row.machine_id != Some(host_machine_id) {
            // Demote an anonymous leased primary before adoption to preserve the host's unique primary.
            if row.primary_interface {
                set_primary_interface(&row.id, false, txn).await?;
            }
            associate_interface_with_machine(
                &row.id,
                MachineInterfaceAssociation::Machine(host_machine_id),
                txn,
            )
            .await?;
            changed = true;
            network_changed = true;
        }
        if row.attached_dpu_machine_id != Some(link.dpu_id) {
            associate_interface_with_dpu_machine(&row.id, link.dpu_id.as_machine_id(), txn).await?;
            changed = true;
            network_changed = true;
        }
        if let Some(id) = &link.boot_interface_id
            && row.boot_interface_id.as_ref() != Some(id)
        {
            // Restrict metadata writes to the admitted row; another segment can acquire the same MAC concurrently.
            let query = "UPDATE machine_interfaces SET boot_interface_id = $2 WHERE id = $1";
            sqlx::query(query)
                .bind(row.id)
                .bind(id)
                .execute(&mut *txn)
                .await
                .map_err(|error| DatabaseError::query(query, error))?;
            changed = true;
        }
        corrected_ids.push(row.id);
    }

    // Keep integrated-NIC boot plans as secondary predictions so their first DHCP cannot displace DPU networking.
    if plan.boot_selection_source == BootInterfaceSelectionSource::ExpectedMachine
        && !plan.links.iter().any(|link| link.mac_address == boot_mac)
    {
        if let Some(row) = boot_rows.first() {
            // Capture only this NIC's report-resolved ID so initial boot can use it while a DPU remains primary.
            if let Some(id) = plan.boot_target.interface_id()
                && row.boot_interface_id.as_deref() != Some(id)
            {
                // Update only the admitted row because another segment may contain the same MAC.
                let query = "UPDATE machine_interfaces SET boot_interface_id = $2 WHERE id = $1";
                sqlx::query(query)
                    .bind(row.id)
                    .bind(id)
                    .execute(&mut *txn)
                    .await
                    .map_err(|error| DatabaseError::query(query, error))?;
                changed = true;
            }
            if row.machine_id.is_none() {
                if row.primary_interface {
                    set_primary_interface(&row.id, false, txn).await?;
                }
                associate_interface_with_machine(
                    &row.id,
                    MachineInterfaceAssociation::Machine(host_machine_id),
                    txn,
                )
                .await?;
                changed = true;
                network_changed = true;
            }
        } else if !boot_overridden && boot_prediction.is_none() {
            crate::predicted_machine_interface::create(
                NewPredictedMachineInterface {
                    machine_id: &host_machine_id,
                    mac_address: boot_mac,
                    expected_network_segment_type: NetworkSegmentType::HostInband,
                    boot_interface_id: plan.boot_target.interface_id(),
                    primary_interface: false,
                },
                txn,
            )
            .await?;
            changed = true;
        }
    }

    let mut by_machine = find_by_machine_ids(txn, &[*host_id]).await?;
    let interfaces = by_machine.remove(host_id).unwrap_or_default();
    let primary = interfaces
        .iter()
        .find(|row| row.mac_address == plan.network_primary_mac)
        .ok_or_else(|| {
            DatabaseError::FailedPrecondition(
                "planned network primary has no owned interface".to_string(),
            )
        })?;
    if !primary.primary_interface
        || interfaces
            .iter()
            .any(|row| row.primary_interface && row.id != primary.id)
    {
        demote_primary_interfaces_for_machine(&host_machine_id, txn).await?;
        set_primary_interface(&primary.id, true, txn).await?;
        changed = true;
        network_changed = true;
    }
    // A corrected MAC on the same selected DPU retains its original networking
    // decision time when the selection source is unchanged.
    let primary_unchanged = original.iter().any(|row| {
        row.primary_interface
            && (row.id == primary.id
                || row.attached_dpu_machine_id.is_some()
                    && row.attached_dpu_machine_id == primary.attached_dpu_machine_id)
    });
    if let Some((Some(source), updated_at)) = previous_selection
        .filter(|(source, _)| primary_unchanged && *source == Some(plan.network_selection_source))
    {
        // Transfer the original decision when a corrected PF row still represents the same selected DPU.
        let query = "UPDATE machine_interfaces SET primary_selection_source = $2, primary_selection_updated_at = $3
                     WHERE id = $1 AND (primary_selection_source, primary_selection_updated_at) IS DISTINCT FROM ($2, $3)";
        changed |= sqlx::query(query)
            .bind(primary.id)
            .bind(source)
            .bind(updated_at)
            .execute(&mut *txn)
            .await
            .map_err(|error| DatabaseError::query(query, error))?
            .rows_affected()
            != 0;
    } else {
        // A different NIC, DPU or source establishes a new networking decision.
        changed |= set_primary_interface_selection(
            txn,
            host_id,
            plan.network_selection_source,
            !primary_unchanged,
        )
        .await?;
    }
    let predictions =
        crate::predicted_machine_interface::find_by_machine_id(txn, &host_machine_id).await?;
    // Networking PFs now have owned rows; remaining predictions carry boot identity without selecting networking.
    for prediction in &predictions {
        if prediction.primary_interface {
            crate::predicted_machine_interface::set_primary_interface(prediction.id, false, txn)
                .await?;
            changed = true;
        }
    }

    // Move DHCP and retire dormant allocations while stale rows still identify their DPU-backed role.
    let admin_before = find_host_admin_interfaces(txn, &host_machine_id)
        .await?
        .into_iter()
        .map(|row| {
            (
                row.id,
                row.hostname,
                row.domain_id,
                row.addresses
                    .into_iter()
                    .map(|address| (address.address, address.allocation_type))
                    .collect_vec(),
            )
        })
        .collect_vec();
    if reconcile_admin_addresses_for_host(txn, &host_machine_id).await? {
        changed = true;
        network_changed = true;
    }
    let admin_after = find_host_admin_interfaces(txn, &host_machine_id)
        .await?
        .into_iter()
        .map(|row| {
            (
                row.id,
                row.hostname,
                row.domain_id,
                row.addresses
                    .into_iter()
                    .map(|address| (address.address, address.allocation_type))
                    .collect_vec(),
            )
        })
        .collect_vec();
    changed |= admin_before != admin_after;
    let stale = original
        .iter()
        .filter(|row| {
            row.attached_dpu_machine_id
                .is_some_and(|id| seen_dpus.contains(&id))
                && !corrected_ids.contains(&row.id)
        })
        .map(|row| row.id)
        .collect_vec();
    if !stale.is_empty() {
        let query =
            "UPDATE machine_interfaces SET attached_dpu_machine_id = NULL WHERE id = ANY($1)";
        sqlx::query(query)
            .bind(&stale)
            .execute(&mut *txn)
            .await
            .map_err(|error| DatabaseError::query(query, error))?;
        changed = true;
        network_changed = true;
    }

    // Publish current declarations and automatic choices without replacing protected explicit intent.
    if !preserve_boot_selection {
        // Enrich unchanged MAC-only selections before set so learning their Redfish ID retains current verification.
        if selection_source == Some(plan.boot_selection_source)
            && let MachineBootInterfaceTarget::Pair(pair) = &plan.boot_target
            && crate::machine_desired_boot_interface::get(&mut *txn, host_id)
                .await?
                .is_some_and(|desired| {
                    desired.value == MachineBootInterfaceTarget::MacOnly(pair.mac_address)
                })
        {
            crate::machine_desired_boot_interface::enrich_interface_id(
                txn,
                host_id,
                pair.mac_address,
                &pair.interface_id,
            )
            .await?;
            changed = true;
        }
        let result = crate::machine_desired_boot_interface::set(
            txn,
            host_id,
            &plan.boot_target,
            plan.boot_selection_source,
        )
        .await?;
        changed |= result.desired_changed || selection_source != Some(plan.boot_selection_source);
    }
    // Publish applicable same-MAC IDs before HostInit so boot resolution can use the desired target directly.
    let desired = crate::machine_desired_boot_interface::get(&mut *txn, host_id).await?;
    // Apply report evidence only while its MAC remains selected; automatic repair may have published another target.
    let reported_desired = plan
        .reported_desired_boot_interface
        .as_ref()
        .filter(|reported| {
            desired
                .as_ref()
                .is_some_and(|desired| desired.value.mac_address() == reported.mac_address)
        });
    if let Some(reported) = reported_desired {
        // Refresh owned data rows by ID so another segment's foreign same-MAC row cannot gain this host's evidence.
        for row in interfaces.iter().filter(|row| {
            row.mac_address == reported.mac_address
                && row.interface_type != InterfaceType::Bmc
                && row.boot_interface_id.as_deref() != Some(reported.interface_id.as_str())
        }) {
            // Retry admission instead of acquiring a newly visible interface after the machine group.
            if !admitted_interface_ids.contains(&row.id) {
                return Err(DatabaseError::TryAgain);
            }
            let query = "UPDATE machine_interfaces SET boot_interface_id = $2 WHERE id = $1";
            sqlx::query(query)
                .bind(row.id)
                .bind(&reported.interface_id)
                .execute(&mut *txn)
                .await
                .map_err(|error| DatabaseError::query(query, error))?;
            changed = true;
        }
    }
    // Preserve MAC-only verification while preferring the same report used for owned-ID refresh.
    if let Some(desired) = &desired
        && let MachineBootInterfaceTarget::MacOnly(mac_address) = &desired.value
        && let Some(pair) = reported_desired
            .cloned()
            .or_else(|| {
                interfaces
                    .iter()
                    .find(|row| row.mac_address == *mac_address)
                    .and_then(|row| row.boot_interface())
            })
            .or_else(|| {
                predictions
                    .iter()
                    .find(|prediction| prediction.mac_address == *mac_address)
                    .and_then(|prediction| prediction.boot_interface())
            })
    {
        crate::machine_desired_boot_interface::enrich_interface_id(
            txn,
            host_id,
            *mac_address,
            &pair.interface_id,
        )
        .await?;
        changed = true;
    }
    // Refresh complete Pairs from same-MAC report evidence without reselecting the NIC or its authority.
    if let Some(desired) = &desired
        && let MachineBootInterfaceTarget::Pair(pair) = &desired.value
        && let Some(reported) =
            reported_desired.filter(|reported| reported.interface_id != pair.interface_id)
    {
        crate::machine_desired_boot_interface::force_reconcile(
            txn,
            host_id,
            &MachineBootInterfaceTarget::Pair(reported.clone()),
            BootInterfaceSelectionAuthority::Existing,
        )
        .await?;
        changed = true;
    }
    if network_changed {
        bump_network_config_version(txn, &host_machine_id).await?;
    }
    Ok(changed)
}

#[cfg(test)]
mod tests;

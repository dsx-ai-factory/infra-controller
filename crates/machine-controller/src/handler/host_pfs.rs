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

use std::collections::HashSet;
use std::sync::Arc;

use db::machine_interface::{HostPfInterfacePlan, RegisteredHostPf};
use model::machine_boot_interface::{
    BootInterfaceSelectionSource, MachineBootInterfaceTarget, canonical_redfish_boot_interface_id,
};
use model::site_explorer::dpu_ordering::{
    ReportSelectionOrdering, apply_report_selection, assign_host_chassis_ids,
    sort_dpus_by_redfish_chassis_id, sort_dpus_by_redfish_serial,
};
use model::site_explorer::{ExploredDpu, HostPrimaryInterfaceSelection};

use super::*;

/// Distinguishes legacy hosts from missing BF4 evidence without publishing a provisional selection.
enum HostPfPlan {
    Legacy,
    Waiting(String),
    Ready(HostPfInterfacePlan),
}

impl DpuMachineStateHandler {
    /// Settles initial BF4 PF links atomically, reloads after repairs, and admits HostInit only under fresh network and health locks.
    ///
    /// Callers must dispatch only synchronized WaitingForNetworkConfig states; later HostInit and allocated hosts are never repaired here.
    pub(super) async fn finalize_registered_host_pfs(
        &self,
        state: &ManagedHostStateSnapshot,
        ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    ) -> Result<Option<StateHandlerOutcome<ManagedHostState>>, StateHandlerError> {
        let host_id = state.host_snapshot.id;
        let controller = ctx.services.site_config.dpu_host_pf_pci_controller_id;

        // Read candidate MACs before admission so the repair can lock every affected existing row.
        let mut read = ctx.services.db_pool.begin().await?;
        let initial = host_pf_interface_plan(&mut read, state, controller).await?;
        read.commit().await?;
        let plan = match initial {
            HostPfPlan::Legacy => return Ok(None),
            HostPfPlan::Waiting(reason) => return Ok(Some(StateHandlerOutcome::wait(reason))),
            HostPfPlan::Ready(plan) => plan,
        };
        let dpu_ids = state.dpu_snapshots.iter().map(|dpu| dpu.id).collect_vec();
        let mut target_macs = plan.links.iter().map(|link| link.mac_address).collect_vec();
        target_macs.push(plan.boot_target.mac_address());

        // Take allocator admission before opening the transaction and machine locks last.
        let admission = db::machine_interface::admin_lock_admission().await;
        let mut txn = ctx.services.db_pool.begin().await?;
        if !db::machine_interface::lock_host_pf_initialization(
            &mut txn,
            &host_id,
            state.host_snapshot.state.version,
            &dpu_ids,
            &target_macs,
        )
        .await?
        {
            txn.rollback().await?;
            return Ok(Some(StateHandlerOutcome::wait_and_requeue(
                "BF4 host initialization changed or is busy; reload".to_string(),
            )));
        }
        // Controller polling omits history; load it for the locked previously-working primary decision.
        let Some(fresh) = db::managed_host::load_snapshot(
            &mut *txn,
            host_id.as_machine_id(),
            LoadSnapshotOptions {
                include_history: true,
                ..Default::default()
            },
        )
        .await?
        else {
            txn.rollback().await?;
            return Ok(Some(StateHandlerOutcome::wait_and_requeue(
                "Host disappeared during PF initialization".to_string(),
            )));
        };
        let plan = match host_pf_interface_plan(&mut txn, &fresh, controller).await? {
            HostPfPlan::Ready(plan) => plan,
            HostPfPlan::Waiting(reason) => {
                txn.rollback().await?;
                return Ok(Some(StateHandlerOutcome::wait(reason)));
            }
            HostPfPlan::Legacy => {
                txn.rollback().await?;
                return Ok(Some(StateHandlerOutcome::wait_and_requeue(
                    "DPU classification changed; reload".to_string(),
                )));
            }
        };
        if plan
            .links
            .iter()
            .map(|link| link.mac_address)
            .chain([plan.boot_target.mac_address()])
            .any(|mac| !target_macs.contains(&mac))
        {
            txn.rollback().await?;
            return Ok(Some(StateHandlerOutcome::wait_and_requeue(
                "Host PF candidates changed before repair; reload".to_string(),
            )));
        }
        let changed = db::machine_interface::repair_host_pf_interfaces(
            &mut txn,
            &host_id,
            &plan,
            ctx.services.site_config.retained_boot_interface_window,
        )
        .await?;
        txn.commit().await?;
        drop(admission);
        if changed {
            return Ok(Some(StateHandlerOutcome::wait_and_requeue("Registered BF4 host PF interfaces finalized; reload before checking network acknowledgment".to_string())));
        }

        // Fence publication separately from repair, then read reports while all selected machine rows remain locked.
        let mut txn = ctx.services.db_pool.begin().await?;
        if !db::machine::lock_host_init_network_config(
            &mut txn,
            &host_id,
            fresh.host_snapshot.state.version,
            fresh.host_snapshot.network_config.version,
            &dpu_ids,
        )
        .await?
        {
            txn.rollback().await?;
            return Ok(Some(StateHandlerOutcome::wait_and_requeue(
                "Host state, network config, or DPU attachments changed; reload".to_string(),
            )));
        }
        let Some(current) = db::managed_host::load_snapshot(
            &mut *txn,
            host_id.as_machine_id(),
            LoadSnapshotOptions::default(),
        )
        .await?
        else {
            txn.rollback().await?;
            return Ok(Some(StateHandlerOutcome::wait_and_requeue(
                "Host disappeared before HostInit publication".to_string(),
            )));
        };
        if let Some((dpu, reason)) = current.dpu_snapshots.iter().find_map(|dpu| {
            dpu_network_not_ready_reason(dpu, current.host_snapshot.network_config.version)
                .map(|reason| (dpu, reason))
        }) {
            // Release every database lock before performing a possible external reboot.
            txn.rollback().await?;
            let reboot_status = Some(
                trigger_reboot_if_needed(dpu, &current, None, &self.reachability_params, ctx)
                    .await
                    .map_err(|error| dpu_network_not_ready_error(error, &dpu.id, reason))?,
            );
            return Ok(Some(StateHandlerOutcome::wait(
                dpu_network_not_ready_wait_message(&dpu.id, reason, &reboot_status),
            )));
        }
        Ok(Some(
            StateHandlerOutcome::transition(ManagedHostState::HostInit {
                machine_state: MachineState::EnableIpmiOverLan,
            })
            .with_txn(txn),
        ))
    }
}

/// Builds initial selection from each DPU's authenticated PF facts and the same report, chassis and serial rules used by Site Explorer.
///
/// Reconstructs inventory encounter order before stable sorting because persisted explored-DPU lists have already been selection-ordered and omit their reports.
async fn host_pf_interface_plan(
    txn: &mut PgConnection,
    state: &ManagedHostStateSnapshot,
    controller: u8,
) -> Result<HostPfPlan, StateHandlerError> {
    let dpu_ids = state.dpu_snapshots.iter().map(|dpu| dpu.id).collect_vec();
    let topologies = db::machine_topology::find_latest_by_machine_ids(txn, &dpu_ids).await?;
    let mut bf4_ids = HashSet::new();
    for dpu in &state.dpu_snapshots {
        let known_bf4 = topologies.get(&dpu.id).is_some_and(|topology| {
            topology.topology.discovery_data.info.is_bf4_dpu()
                || topology
                    .dpu_host_pf_inventory
                    .as_ref()
                    .is_some_and(|inventory| inventory.is_bf4)
        }) || dpu
            .status
            .hardware_info
            .as_ref()
            .is_some_and(|info| info.is_bf4_dpu());
        if known_bf4 {
            bf4_ids.insert(dpu.id);
        } else {
            // DPF's persisted ingestion marker requires its existing DMI evidence instead of silently assuming BF3.
            match is_dpf_managed_bf4(state, dpu) {
                Ok(true) => {
                    bf4_ids.insert(dpu.id);
                }
                Ok(false) => {}
                Err(error) => {
                    return Ok(HostPfPlan::Waiting(format!(
                        "Waiting for DPU {} platform classification: {error}",
                        dpu.id
                    )));
                }
            }
        }
    }
    if bf4_ids.is_empty() {
        return Ok(HostPfPlan::Legacy);
    }

    // Resolve every paired BF4 independently; a different DPU's facts cannot satisfy a missing mapping.
    let mut links = Vec::with_capacity(dpu_ids.len());
    for dpu in &state.dpu_snapshots {
        let mac_address = if bf4_ids.contains(&dpu.id) {
            topologies
                .get(&dpu.id)
                .and_then(|topology| topology.dpu_host_pf_inventory.as_ref())
                .and_then(|inventory| {
                    inventory
                        .host_pfs
                        .iter()
                        .find(|pf| pf.pci_controller_id == controller && pf.pf_id == 0)
                })
                .map(|pf| pf.mac_address)
        } else {
            // Retain BF2/BF3 factory-MAC semantics in mixed hosts.
            dpu.status
                .hardware_info
                .as_ref()
                .and_then(|info| info.factory_mac_address().ok())
        };
        let Some(mac_address) = mac_address else {
            return Ok(HostPfPlan::Waiting(format!(
                "Waiting for registered host PF c{controller}pf0 for DPU {}",
                dpu.id
            )));
        };
        links.push(RegisteredHostPf {
            dpu_id: dpu.id,
            mac_address,
            boot_interface_id: None,
        });
    }

    let Some(host_ip) = state.host_snapshot.status.bmc_info.ip else {
        return Ok(HostPfPlan::Waiting(
            "Waiting for host BMC address to resolve host PF ordering".to_string(),
        ));
    };
    let mut ips = vec![host_ip];
    for dpu in &state.dpu_snapshots {
        let Some(ip) = dpu.status.bmc_info.ip else {
            return Ok(HostPfPlan::Waiting(format!(
                "Waiting for DPU {} BMC address to resolve host PF ordering",
                dpu.id
            )));
        };
        ips.push(ip);
    }
    let endpoints = db::explored_endpoints::find_by_ips(&mut *txn, ips).await?;
    let Some(host) = endpoints
        .iter()
        .find(|endpoint| endpoint.address == host_ip)
    else {
        return Ok(HostPfPlan::Waiting(
            "Waiting for host exploration to resolve registered host PF ordering".to_string(),
        ));
    };
    let expected = if let Some(mac) = state.host_snapshot.status.bmc_info.mac {
        db::expected_machine::find_by_bmc_mac_address(&mut *txn, mac).await?
    } else {
        None
    };
    let declared_primary = expected
        .as_ref()
        .and_then(|machine| machine.data.declared_primary_mac());
    let inventory = host.report.devices_with_unique_serials();
    let mut serials = inventory
        .iter()
        .filter_map(|device| device.serial_number())
        .collect_vec();
    if let Some(expected) = &expected {
        for serial in &expected.data.fallback_dpu_serial_numbers {
            let serial = serial.trim();
            if !serial.is_empty() && !serials.contains(&serial) {
                serials.push(serial);
            }
        }
    }
    let mut candidates = Vec::with_capacity(links.len());
    let mut positions = HashSet::new();
    for (dpu, link) in state.dpu_snapshots.iter().zip(&mut links) {
        let endpoint = endpoints
            .iter()
            .find(|endpoint| Some(endpoint.address) == dpu.status.bmc_info.ip);
        let Some(endpoint) = endpoint.filter(|endpoint| !endpoint.report.systems.is_empty()) else {
            return Ok(HostPfPlan::Waiting(format!(
                "Waiting for DPU {} exploration to reconstruct discovery order",
                dpu.id
            )));
        };
        let Some(position) = endpoint
            .report
            .dpu_pairing_serial_number()
            .and_then(|serial| serials.iter().position(|candidate| *candidate == serial))
            .filter(|position| positions.insert(*position))
        else {
            return Ok(HostPfPlan::Waiting(format!(
                "Waiting for unambiguous host inventory or configured pairing serial for DPU {}",
                dpu.id
            )));
        };
        link.boot_interface_id = host
            .report
            .find_interface_id_for_mac(link.mac_address)
            .and_then(canonical_redfish_boot_interface_id)
            .map(str::to_string)
            .or_else(|| {
                state
                    .host_snapshot
                    .config
                    .desired_boot_interface
                    .as_ref()
                    .filter(|desired| desired.value.mac_address() == link.mac_address)
                    .and_then(|desired| desired.value.interface_id())
                    .map(str::to_string)
            });
        candidates.push((
            position,
            ExploredDpu {
                bmc_ip: endpoint.address,
                host_pf_mac_address: Some(link.mac_address),
                host_chassis_id: None,
                report: Arc::new(endpoint.report.clone()),
            },
        ));
    }
    candidates.sort_by_key(|(position, _)| *position);
    let mut candidates = candidates.into_iter().map(|(_, dpu)| dpu).collect_vec();
    assign_host_chassis_ids(&host.report, &mut candidates);
    let mut selection = host
        .report
        .select_host_primary_interface(&candidates, declared_primary);
    let report_ordered = selection.is_some_and(|selection| {
        apply_report_selection(&mut candidates, selection.mac_address)
            == ReportSelectionOrdering::Applied
    });
    let mut network_selection_source = selection.map(|selection| selection.source);
    if !report_ordered {
        // A declared integrated NIC keeps boot precedence while ordinary fallbacks still order its DPUs.
        let fallback = if candidates.len() > 1 {
            sort_dpus_by_redfish_chassis_id(&mut candidates).ok()
        } else {
            None
        }
        .or_else(|| sort_dpus_by_redfish_serial(&mut candidates));
        network_selection_source = fallback.map(|selection| selection.source);
        if selection.is_none_or(|selection| {
            selection.source != BootInterfaceSelectionSource::ExpectedMachine
        }) {
            selection = fallback;
        }
    }
    let Some(mut selection) = selection else {
        return Ok(HostPfPlan::Waiting(
            "Waiting for a usable registered host PF boot candidate".to_string(),
        ));
    };
    let Some(mut network_primary_mac) = candidates.first().and_then(|dpu| dpu.host_pf_mac_address)
    else {
        return Ok(HostPfPlan::Waiting(
            "Waiting for a registered network-primary DPU".to_string(),
        ));
    };

    // Preserve working networking and explicit primary choices independently of boot-only Operator requests.
    let prior_source = state
        .host_snapshot
        .config
        .primary_interface_selection
        .map(|selection| selection.source);
    // Validation completion also records failed and skipped runs; only retained lifecycle history qualifies prior operation.
    let previously_working = state.host_snapshot.history.iter().any(|record| {
        serde_json::from_str::<ManagedHostState>(&record.state).is_ok_and(|state| {
            matches!(
                state,
                ManagedHostState::Ready
                    | ManagedHostState::Assigned { .. }
                    | ManagedHostState::BootConfiguring { .. }
            )
        })
    });
    if previously_working || prior_source == Some(BootInterfaceSelectionSource::Operator) {
        if let Some(primary_dpu) = state.host_snapshot.primary_attached_dpu_machine_id()
            && let Some(link) = links.iter().find(|link| link.dpu_id == primary_dpu)
        {
            network_primary_mac = link.mac_address;
            network_selection_source = prior_source;
            if selection.source != BootInterfaceSelectionSource::ExpectedMachine {
                selection = HostPrimaryInterfaceSelection {
                    mac_address: link.mac_address,
                    source: state
                        .host_snapshot
                        .config
                        .boot_interface_selection
                        .map(|selection| selection.source)
                        .filter(|source| *source != BootInterfaceSelectionSource::Operator)
                        .unwrap_or(BootInterfaceSelectionSource::LegacyUnknown),
                };
            }
        } else if prior_source == Some(BootInterfaceSelectionSource::Operator)
            && let Some(primary) = state
                .host_snapshot
                .status
                .interfaces
                .iter()
                .find(|row| row.primary_interface && row.attached_dpu_machine_id.is_none())
        {
            network_primary_mac = primary.mac_address;
            network_selection_source = prior_source;
        }
    }
    let interface_id = host
        .report
        .find_interface_id_for_mac(selection.mac_address)
        .and_then(canonical_redfish_boot_interface_id)
        .map(str::to_string);
    let boot_target =
        MachineBootInterfaceTarget::from_parts(Some(selection.mac_address), interface_id)
            .ok_or_else(|| {
                StateHandlerError::InvalidState("host PF selection has no MAC".to_string())
            })?;
    Ok(HostPfPlan::Ready(HostPfInterfacePlan {
        links,
        network_primary_mac,
        network_selection_source: network_selection_source
            .unwrap_or(BootInterfaceSelectionSource::LegacyUnknown),
        boot_target,
        boot_selection_source: selection.source,
    }))
}

#[cfg(test)]
mod tests;

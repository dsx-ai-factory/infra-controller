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

use carbide_uuid::machine::{
    DpuMachineId, MachineIdSubtypeTrait, MachineInterfaceId, StableHostMachineId,
};
use model::hardware_info::HardwareInfo;
use model::machine::machine_search_config::MachineSearchConfig;
use model::machine::{Machine, MachineInterfaceSnapshot, MachineState, ManagedHostState};
use model::machine_boot_interface::{
    BootInterfaceSelectionSource, MachineBootInterface, MachineBootInterfaceTarget,
    canonical_redfish_boot_interface_id,
};
use model::network_segment::NetworkSegmentType;

use crate::api::Api;
use crate::handlers::machine_discovery::scout_pci;
use crate::{CarbideError, CarbideResult};

/// Identifies a host interface directly or through its attached DPU.
#[derive(Clone, Copy)]
pub(super) enum PrimaryInterfaceSelector {
    /// Selects the host interface with this ID.
    Interface(MachineInterfaceId),
    /// Selects the host interface attached to this DPU.
    Dpu(DpuMachineId),
}

/// Describes work the caller must perform after the primary interface update commits.
pub(super) struct PrimaryInterfaceUpdate {
    /// Whether the machine controller should be woken to reconcile the target.
    pub(super) reconciliation_needed: bool,
}

/// Returns whether scout may replace a selection recorded as `RedfishChassisId` or
/// `RedfishSerialNumber`.
///
/// `ExpectedMachine`, `Operator`, `RedfishUefiPci`, `ScoutReportPci`, and `LegacyUnknown` take
/// precedence and remain unchanged.
fn scout_may_replace_source(source: BootInterfaceSelectionSource) -> bool {
    match source {
        BootInterfaceSelectionSource::RedfishChassisId
        | BootInterfaceSelectionSource::RedfishSerialNumber => true,
        BootInterfaceSelectionSource::ExpectedMachine
        | BootInterfaceSelectionSource::Operator
        | BootInterfaceSelectionSource::RedfishUefiPci
        | BootInterfaceSelectionSource::ScoutReportPci
        | BootInterfaceSelectionSource::LegacyUnknown => false,
    }
}

/// Returns whether the machine state permits automatic scout reconciliation.
fn scout_state_is_eligible(state: &ManagedHostState) -> bool {
    matches!(
        state,
        ManagedHostState::HostInit {
            machine_state: MachineState::Discovered { .. }
        } | ManagedHostState::Ready
    )
}

/// Returns whether scout may reconcile the current primary interface records.
///
/// scout may select an interface when no primary is present. It may also change exactly one
/// primary whose `attached_dpu_machine_id` identifies a DPU. Multiple primary rows require
/// operator intervention. A primary with no `attached_dpu_machine_id` may be an intended
/// integrated NIC and remains unchanged.
fn scout_may_replace_current_primary(interfaces: &[MachineInterfaceSnapshot]) -> bool {
    let mut primaries = interfaces
        .iter()
        .filter(|interface| interface.primary_interface);
    let Some(primary) = primaries.next() else {
        return true;
    };

    primary.attached_dpu_machine_id.is_some() && primaries.next().is_none()
}

/// Builds a desired boot target from a MAC address and optional Redfish interface ID.
/// Missing or blank interface IDs use the MAC address by itself.
fn boot_target_for_interface(
    mac_address: mac_address::MacAddress,
    interface_id: Option<String>,
) -> MachineBootInterfaceTarget {
    match interface_id
        .as_deref()
        .and_then(canonical_redfish_boot_interface_id)
    {
        Some(interface_id) => MachineBootInterfaceTarget::Pair(MachineBootInterface {
            mac_address,
            interface_id: interface_id.to_string(),
        }),
        None => MachineBootInterfaceTarget::MacOnly(mac_address),
    }
}

/// Builds the target for a scout candidate.
///
/// A matching MAC uses `MachineBootInterfaceTarget::MacOnly` so
/// `machine_desired_boot_interface::set` preserves any stored
/// `MachineBootInterfaceTarget::Pair`.
fn scout_boot_target_for_interface(
    interface: &MachineInterfaceSnapshot,
    desired_mac_address: Option<mac_address::MacAddress>,
) -> MachineBootInterfaceTarget {
    if desired_mac_address == Some(interface.mac_address) {
        MachineBootInterfaceTarget::MacOnly(interface.mac_address)
    } else {
        boot_target_for_interface(interface.mac_address, interface.boot_interface_id.clone())
    }
}

/// Updates the database primary and desired boot target atomically using Site Explorer's shared
/// lock order. Commits before returning; when `reconciliation_needed` is true, the caller invokes
/// `enqueue_boot_interface_reconciliation` after the commit.
pub(super) async fn update_primary_interface(
    api: &Api,
    host_machine_id: StableHostMachineId,
    selector: PrimaryInterfaceSelector,
    force_reconcile: bool,
) -> CarbideResult<PrimaryInterfaceUpdate> {
    // Take the Admin lock permit first so excess requests wait without using database connections.
    let _admin_admission = db::machine_interface::admin_lock_admission().await;
    let mut txn = api.txn_begin().await?;

    // Site Explorer takes these locks before it changes interface ownership.
    // Matching that order keeps an operator or scout write from deadlocking discovery.
    db::machine_interface::lock_all_admin_segments(&mut txn).await?;
    let interface_snapshots =
        db::machine_interface::find_by_machine_id_for_update(&mut txn, &host_machine_id).await?;
    // This locks the Machine row with `FOR UPDATE`; the snapshot below is read after that wait.
    db::machine_desired_boot_interface::lock(txn.as_pgconn(), &host_machine_id).await?;
    let machine = db::machine::find_one(&mut txn, &host_machine_id, MachineSearchConfig::default())
        .await?
        .ok_or_else(|| CarbideError::Internal {
            message: format!(
                "machine {host_machine_id} disappeared while its database record was locked"
            ),
        })?;

    let new_primary_interface_id = match selector {
        PrimaryInterfaceSelector::Interface(interface_id) => interface_id,
        PrimaryInterfaceSelector::Dpu(dpu_machine_id) => {
            if !interface_snapshots
                .iter()
                .any(|interface| interface.attached_dpu_machine_id.is_some())
            {
                return Err(CarbideError::FailedPrecondition(format!(
                    "host {host_machine_id} has no DPUs; set-primary-dpu does not apply to zero-DPU hosts"
                )));
            }

            interface_snapshots
                .iter()
                .find(|interface| {
                    interface.attached_dpu_machine_id.as_ref() == Some(&dpu_machine_id)
                })
                .map(|interface| interface.id)
                .ok_or_else(|| {
                    CarbideError::InvalidArgument(format!(
                        "DPU {dpu_machine_id} has no interface on host {host_machine_id}"
                    ))
                })?
        }
    };
    let new_primary_interface = interface_snapshots
        .iter()
        .find(|interface| interface.id == new_primary_interface_id)
        .ok_or_else(|| {
            CarbideError::InvalidArgument(format!(
                "interface {new_primary_interface_id} not found on host {host_machine_id}"
            ))
        })?;
    let boot_target = boot_target_for_interface(
        new_primary_interface.mac_address,
        new_primary_interface.boot_interface_id.clone(),
    );

    let selected_target_is_already_operator = machine
        .config
        .boot_interface_selection
        .is_some_and(|selection| selection.source == BootInterfaceSelectionSource::Operator)
        && machine
            .config
            .desired_boot_interface
            .as_ref()
            .is_some_and(|desired| desired.value == boot_target);
    if new_primary_interface.primary_interface
        && !force_reconcile
        && selected_target_is_already_operator
    {
        return Err(CarbideError::InvalidArgument(
            "requested interface is already the operator-selected primary interface".to_string(),
        ));
    }

    let host_has_dpu_backed_admin_interface = interface_snapshots.iter().any(|interface| {
        interface.attached_dpu_machine_id.is_some()
            && interface.network_segment_type == Some(NetworkSegmentType::Admin)
    });
    if host_has_dpu_backed_admin_interface
        && new_primary_interface.network_segment_type != Some(NetworkSegmentType::Admin)
    {
        return Err(CarbideError::InvalidArgument(format!(
            "interface {new_primary_interface_id} is not on the admin segment; a \
             DPU-managed host's primary interface must be an admin interface"
        )));
    }

    let instance =
        db::instance::find_live_by_machine_id_for_update(&mut txn, &host_machine_id).await?;
    let reconciliation_is_pending = machine.pending_boot_interface_config_version().is_some();
    let reconciliation_is_eligible =
        matches!(machine.current_state(), ManagedHostState::Ready) && instance.is_none();
    let primary_changed = !new_primary_interface.primary_interface;
    let desired_changed = apply_primary_interface_update(
        &mut txn,
        host_machine_id,
        &interface_snapshots,
        new_primary_interface,
        &boot_target,
        BootInterfaceSelectionSource::Operator,
        force_reconcile,
    )
    .await?;
    if primary_changed && let Some(instance) = &instance {
        db::instance::update_network_config(
            &mut txn,
            instance.id,
            instance.network_config_version,
            &instance.config.network,
            true,
        )
        .await?;
    }
    txn.commit().await?;

    Ok(PrimaryInterfaceUpdate {
        reconciliation_needed: reconciliation_is_eligible
            && (reconciliation_is_pending || desired_changed),
    })
}

/// Compares the PCI slots in a scout report with the stored boot interface.
///
/// When the report can be compared with the stored boot interface, NICo records the initial
/// comparison in a structured log and increments `carbide_scout_pci_evaluations_total`. Selections not
/// recorded as `RedfishChassisId` or `RedfishSerialNumber` remain unchanged. When reconciliation
/// is enabled and the machine may be changed automatically, the comparison is repeated under the
/// update locks before the shared primary interface path reconciles the stored selection.
pub(super) async fn update_primary_interface_from_scout(
    api: &Api,
    host_machine_id: StableHostMachineId,
    hardware_info: &HardwareInfo,
) -> CarbideResult<PrimaryInterfaceUpdate> {
    // First, load the machine and compare its stored boot interface with this scout report.
    let machine = db::machine::find_one(
        api.pg_pool(),
        &host_machine_id,
        MachineSearchConfig::default(),
    )
    .await?
    .ok_or_else(|| CarbideError::NotFoundError {
        kind: "Machine",
        id: host_machine_id.to_string(),
    })?;
    let Some(comparison) = scout_pci::compare(hardware_info, &machine) else {
        return Ok(PrimaryInterfaceUpdate {
            reconciliation_needed: false,
        });
    };
    let candidate_interface_id = comparison.candidate_interface_id();
    comparison.emit(&host_machine_id);

    // Then, stop before taking update locks unless this report can drive enabled reconciliation.
    let source_is_replaceable = machine
        .config
        .boot_interface_selection
        .is_some_and(|selection| scout_may_replace_source(selection.source));
    if !api.runtime_config.scout_boot_interface_correction_enabled
        || candidate_interface_id.is_none()
        || !source_is_replaceable
        || !scout_state_is_eligible(machine.current_state())
        || !scout_may_replace_current_primary(&machine.status.interfaces)
    {
        return Ok(PrimaryInterfaceUpdate {
            reconciliation_needed: false,
        });
    }

    // Now, take the shared update locks and repeat the comparison and safety checks against the
    // locked records before changing anything.
    let _admin_admission = db::machine_interface::admin_lock_admission().await;
    let mut txn = api.txn_begin().await?;
    db::machine_interface::lock_all_admin_segments(&mut txn).await?;
    let interface_snapshots =
        db::machine_interface::find_by_machine_id_for_update(&mut txn, &host_machine_id).await?;
    // This locks the Machine row with `FOR UPDATE`; the snapshot below is read after that wait.
    db::machine_desired_boot_interface::lock(txn.as_pgconn(), &host_machine_id).await?;
    let machine = db::machine::find_one(&mut txn, &host_machine_id, MachineSearchConfig::default())
        .await?
        .ok_or_else(|| CarbideError::Internal {
            message: format!(
                "machine {host_machine_id} disappeared while its database record was locked"
            ),
        })?;
    let Some((new_primary_interface, boot_target)) = select_primary_interface_from_scout(
        txn.as_pgconn(),
        host_machine_id,
        hardware_info,
        &machine,
        &interface_snapshots,
    )
    .await?
    else {
        txn.commit().await?;
        return Ok(PrimaryInterfaceUpdate {
            reconciliation_needed: false,
        });
    };
    let primary_changed = !new_primary_interface.primary_interface;
    // Changing only the primary flag still needs a desired generation so machine-controller runs.
    // Reuse any pending generation instead of creating another one.
    let force_reconcile =
        primary_changed && machine.pending_boot_interface_config_version().is_none();
    let desired_changed = apply_primary_interface_update(
        &mut txn,
        host_machine_id,
        &interface_snapshots,
        new_primary_interface,
        &boot_target,
        BootInterfaceSelectionSource::ScoutReportPci,
        force_reconcile,
    )
    .await?;
    txn.commit().await?;

    Ok(PrimaryInterfaceUpdate {
        reconciliation_needed: primary_changed || desired_changed,
    })
}

/// Rechecks a scout PCI selection against the locked machine and interface records.
///
/// Returns a selection only when scout is still allowed to replace the stored source and the
/// locked records permit an automatic update.
async fn select_primary_interface_from_scout<'a>(
    txn: &mut sqlx::PgConnection,
    host_machine_id: StableHostMachineId,
    hardware_info: &HardwareInfo,
    machine: &Machine<impl MachineIdSubtypeTrait>,
    interface_snapshots: &'a [MachineInterfaceSnapshot],
) -> CarbideResult<Option<(&'a MachineInterfaceSnapshot, MachineBootInterfaceTarget)>> {
    // First, confirm that scout can still replace the selection source now that the machine row is
    // locked.
    let source_is_replaceable = machine
        .config
        .boot_interface_selection
        .is_some_and(|selection| scout_may_replace_source(selection.source));
    if !source_is_replaceable {
        return Ok(None);
    }

    // Then, repeat the PCI comparison against the locked machine.
    let Some(comparison) = scout_pci::compare(hardware_info, machine) else {
        return Ok(None);
    };
    let candidate_interface_id = comparison.candidate_interface_id();

    // Now, make sure the locked machine state and primary records still allow an automatic update.
    if !scout_state_is_eligible(machine.current_state())
        || !scout_may_replace_current_primary(interface_snapshots)
    {
        return Ok(None);
    }

    let Some(interface_id) = candidate_interface_id else {
        return Ok(None);
    };

    let candidate = interface_snapshots
        .iter()
        .find(|interface| interface.id == interface_id)
        .ok_or_else(|| CarbideError::Internal {
            message: format!(
                "scout selected interface {interface_id} on host {host_machine_id}, but the locked interface does not exist"
            ),
        })?;
    // Finally, do not compete with a primary interface that Site Explorer has already planned. Its
    // writers take the same global Admin locks as this caller.
    let has_primary_prediction =
        db::predicted_machine_interface::find_by_machine_id(txn, &host_machine_id)
            .await?
            .iter()
            .any(|prediction| prediction.primary_interface);
    if has_primary_prediction {
        return Ok(None);
    }

    // Allocation also locks the Machine row, so it cannot commit concurrently. Read the Instance
    // ID without locking it to avoid reversing the lock order used by release.
    if db::instance::find_id_by_machine_id(txn, &host_machine_id)
        .await?
        .is_some()
    {
        return Ok(None);
    }

    let desired_mac_address = machine
        .config
        .desired_boot_interface
        .as_ref()
        .map(|desired| desired.value.mac_address());
    let boot_target = scout_boot_target_for_interface(candidate, desired_mac_address);
    Ok(Some((candidate, boot_target)))
}

/// Updates the primary flag and desired boot selection in one locked transaction.
async fn apply_primary_interface_update(
    txn: &mut db::Transaction<'_>,
    host_machine_id: StableHostMachineId,
    interface_snapshots: &[MachineInterfaceSnapshot],
    new_primary_interface: &MachineInterfaceSnapshot,
    boot_target: &MachineBootInterfaceTarget,
    source: BootInterfaceSelectionSource,
    force_reconcile: bool,
) -> CarbideResult<bool> {
    let current_primary_interface = interface_snapshots
        .iter()
        .find(|interface| interface.primary_interface);
    let current_primary_interface_id = current_primary_interface.map(|interface| interface.id);
    let current_primary_is_admin = current_primary_interface
        .is_some_and(|interface| interface.network_segment_type == Some(NetworkSegmentType::Admin));
    let new_primary_interface_id = new_primary_interface.id;
    let primary_is_unchanged = new_primary_interface.primary_interface;

    if !primary_is_unchanged {
        tracing::info!(
            machine_id = %host_machine_id,
            new_primary_interface_id = %new_primary_interface_id,
            previous_primary_interface_id = ?current_primary_interface_id,
            "Moving host primary interface",
        );

        // Preserve the active admin address before moving the primary flag. A host with no current
        // admin primary skips this pass; reconciliation assigns the address after the primary
        // flag changes.
        if current_primary_is_admin {
            db::machine_interface::reconcile_admin_addresses_for_host(txn, &host_machine_id)
                .await?;
        }

        if let Some(current_primary_interface_id) = current_primary_interface_id {
            db::machine_interface::set_primary_interface(&current_primary_interface_id, false, txn)
                .await?;
        }
        db::machine_interface::set_primary_interface(&new_primary_interface_id, true, txn).await?;
        db::machine_interface::reconcile_admin_addresses_for_host(txn, &host_machine_id).await?;

        let (network_config, network_config_version) =
            db::machine::get_network_config(txn.as_pgconn(), &host_machine_id)
                .await?
                .take();
        // The Machine row was locked before this version was read, so another transaction cannot
        // change the version before this update. The update must therefore match one row.
        if !db::machine::try_update_network_config(
            txn,
            &host_machine_id,
            network_config_version,
            &network_config,
        )
        .await?
        {
            return Err(CarbideError::Internal {
                message: format!(
                    "network configuration update for machine {host_machine_id} returned no row \
                     at version {network_config_version} while the machine record was locked"
                ),
            });
        }
    }

    let desired_update = if force_reconcile {
        db::machine_desired_boot_interface::force_set(txn, &host_machine_id, boot_target, source)
            .await?
    } else {
        db::machine_desired_boot_interface::set(txn, &host_machine_id, boot_target, source).await?
    };

    Ok(desired_update.desired_changed)
}

#[cfg(test)]
mod tests {
    use carbide_test_support::{Check, check_values, value_scenarios};
    use model::test_support::machine_snapshot::dpu_machine_id;

    use super::*;

    /// Only `RedfishChassisId` and `RedfishSerialNumber` may be replaced from scout reports.
    #[test]
    fn scout_replaces_only_chassis_or_serial_sources() {
        value_scenarios!(run = |source: BootInterfaceSelectionSource| {
            scout_may_replace_source(source)
        };
            "replaceable" {
                BootInterfaceSelectionSource::RedfishChassisId => true,
                BootInterfaceSelectionSource::RedfishSerialNumber => true,
            }
            "protected" {
                BootInterfaceSelectionSource::ExpectedMachine => false,
                BootInterfaceSelectionSource::Operator => false,
                BootInterfaceSelectionSource::RedfishUefiPci => false,
                BootInterfaceSelectionSource::ScoutReportPci => false,
                BootInterfaceSelectionSource::LegacyUnknown => false,
            }
        );
    }

    /// Automatic reconciliation is limited to Ready and discovered HostInit machines.
    #[test]
    fn scout_reconciles_only_eligible_machine_states() {
        value_scenarios!(run = |state: ManagedHostState| {
            scout_state_is_eligible(&state)
        };
            "eligible" {
                ManagedHostState::Ready => true,
                ManagedHostState::HostInit {
                    machine_state: MachineState::Discovered { skip_reboot_wait: false },
                } => true,
            }
            "ineligible" {
                ManagedHostState::HostInit { machine_state: MachineState::Init } => false,
                ManagedHostState::Created => false,
            }
        );
    }

    /// Conflicting primary rows and integrated NIC primaries remain unchanged.
    #[test]
    fn scout_leaves_conflicting_or_integrated_primaries_unchanged() {
        let primary = |attached_dpu_machine_id| {
            let mut interface =
                MachineInterfaceSnapshot::mock_with_mac("00:00:5e:00:53:02".parse().unwrap());
            interface.attached_dpu_machine_id = attached_dpu_machine_id;
            interface
        };
        value_scenarios!(run = |interfaces: Vec<MachineInterfaceSnapshot>| {
            scout_may_replace_current_primary(&interfaces)
        };
            "replaceable" {
                Vec::new() => true,
                vec![primary(Some(dpu_machine_id(0)))] => true,
            }
            "protected" {
                vec![primary(None)] => false,
                vec![primary(Some(dpu_machine_id(0))), primary(Some(dpu_machine_id(1)))] => false,
            }
        );
    }

    /// A matching scout comparison follows `desired_mac_address`, not `primary_interface`.
    #[test]
    fn scout_target_preserves_only_a_matching_stored_pair() {
        struct TargetCase {
            candidate_mac: mac_address::MacAddress,
            primary: bool,
            stored_mac: mac_address::MacAddress,
        }

        let desired_mac = "00:00:5e:00:53:02".parse().unwrap();
        let other_mac = "00:00:5e:00:53:03".parse().unwrap();
        let interface_id = "NIC.Slot.7-1-1";
        check_values(
            [
                Check {
                    scenario: "matching non-primary candidate",
                    input: TargetCase {
                        candidate_mac: desired_mac,
                        primary: false,
                        stored_mac: desired_mac,
                    },
                    expect: MachineBootInterfaceTarget::MacOnly(desired_mac),
                },
                Check {
                    scenario: "matching primary candidate",
                    input: TargetCase {
                        candidate_mac: desired_mac,
                        primary: true,
                        stored_mac: desired_mac,
                    },
                    expect: MachineBootInterfaceTarget::MacOnly(desired_mac),
                },
                Check {
                    scenario: "different primary candidate",
                    input: TargetCase {
                        candidate_mac: other_mac,
                        primary: true,
                        stored_mac: desired_mac,
                    },
                    expect: MachineBootInterfaceTarget::Pair(MachineBootInterface {
                        mac_address: other_mac,
                        interface_id: interface_id.to_string(),
                    }),
                },
            ],
            |case| {
                let mut interface = MachineInterfaceSnapshot::mock_with_mac(case.candidate_mac);
                interface.primary_interface = case.primary;
                interface.boot_interface_id = Some(interface_id.to_string());
                scout_boot_target_for_interface(&interface, Some(case.stored_mac))
            },
        );
    }

    /// Redfish interface IDs are normalized before the desired target is stored.
    #[test]
    fn boot_target_normalizes_interface_ids() {
        let (mac, id) = ("00:00:5e:00:53:02".parse().unwrap(), "NIC.Slot.7-1-1");
        let pair = |interface_id: &str| {
            MachineBootInterfaceTarget::Pair(MachineBootInterface {
                mac_address: mac,
                interface_id: interface_id.to_string(),
            })
        };
        let mac_only = || MachineBootInterfaceTarget::MacOnly(mac);
        value_scenarios!(run = |input: Option<&str>| {
            boot_target_for_interface(mac, input.map(str::to_string))
        };
            "complete" { Some(id) => pair(id), }
            "padded" { Some(" \tNIC.Slot.7-1-1\n ") => pair(id), }
            "blank" { Some("\t\n") => mac_only(), }
            "missing" { None => mac_only(), }
        );
    }
}

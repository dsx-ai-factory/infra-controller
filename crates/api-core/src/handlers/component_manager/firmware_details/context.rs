// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Loads stored firmware facts before any credential, manifest, or hardware read.

use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use carbide_firmware::FirmwareConfigSnapshot;
use carbide_uuid::machine::HostMachineId;
use carbide_uuid::power_shelf::PowerShelfId;
use carbide_uuid::rack::{RackId, RackProfileId};
use carbide_uuid::switch::SwitchId;
use db::DbSessionGuard;
use mac_address::MacAddress;
use model::firmware::HostFirmwareConfig;
use model::machine::machine_search_config::MachineSearchConfig;
use model::machine::{HostMachine, MachineInterfaceSnapshot};
use model::machine_interface::InterfaceType;
use model::power_shelf::PowerShelf;
use model::rack::Rack;
use model::rack_type::RackProfile;
use model::site_explorer::EndpointExplorationReport;
use model::switch::Switch;
use tokio::time::Instant;

use crate::CarbideError;
use crate::cfg::file::CarbideConfig;

#[cfg(test)]
mod tests;

/// Maximum input slots per preparation, counting repeated IDs as separate slots.
/// Admission is checked before catalog or database work.
pub(super) const MAX_TARGET_SLOTS: usize = 64;
// Includes acquisition, setup, all reads, and commit; a shorter caller deadline
// wins. DbSessionGuard also caps each statement at this duration while preserving
// any shorter operator timeout.
const SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(5);

/// `PreparationError` keeps expiry distinct without exposing stored payloads
/// through a public error or making callers parse an internal display string.
#[derive(Debug, thiserror::Error)]
pub(super) enum PreparationError {
    #[error("firmware details preparation deadline exceeded")]
    DeadlineExceeded,
    #[error(transparent)]
    Load(Box<CarbideError>),
}

impl From<CarbideError> for PreparationError {
    fn from(error: CarbideError) -> Self {
        Self::Load(Box::new(error))
    }
}

/// Borrows one homogeneous list of typed managed IDs from the caller. The loader
/// preserves its input slots while owning the stored facts returned for them.
#[derive(Clone, Copy)]
pub(super) enum FirmwareTargets<'a> {
    Machines(&'a [HostMachineId]),
    Switches(&'a [SwitchId]),
    Shelves(&'a [PowerShelfId]),
}

/// Missing members retain their IDs; repeated slots share immutable records.
pub(super) enum FirmwareReadEntry {
    Machine {
        machine_id: HostMachineId,
        machine: Option<Arc<HostMachine>>,
    },
    Switch {
        switch_id: SwitchId,
        switch: Option<Arc<Switch>>,
        endpoint: Option<Arc<db::switch::SwitchEndpointRow>>,
    },
    Shelf {
        power_shelf_id: PowerShelfId,
        shelf: Option<Arc<PowerShelf>>,
        endpoint: Option<Arc<db::power_shelf::PowerShelfEndpointRow>>,
    },
}

/// Owns stored identity, configuration and maintenance records after DB release.
/// Raw lifecycle/deletion flags remain available; this loader does not decide
/// public eligibility or combine rack and per-device requests.
#[derive(Default)]
pub(super) struct FirmwareReadContext {
    /// Original input slots, including missing IDs and shared repeated records.
    pub(super) entries: Vec<FirmwareReadEntry>,
    /// Found saved owners, including raw maintenance and deletion fields.
    pub(super) racks: HashMap<RackId, Rack>,
    /// Configured profiles referenced by found racks; unresolved keys stay absent.
    pub(super) profiles: HashMap<RackProfileId, RackProfile>,
    /// Some for nonempty host input, even if empty; None for other kinds or no input.
    pub(super) host_catalog: Option<FirmwareConfigSnapshot>,
    /// Reports keyed by selected stored BMC IP, for catalog identity selection.
    /// Their presence does not establish live inventory or refresh readiness.
    pub(super) host_reports: HashMap<IpAddr, EndpointExplorationReport>,
    /// Attached BMC MACs for found requested hosts, sorted by bytes and unique.
    /// A missing key can mean no BMCs; `entries` distinguishes that from no host.
    pub(super) bmc_aliases: HashMap<HostMachineId, Vec<MacAddress>>,
    /// Interfaces currently holding the selected management IPs in this snapshot.
    /// Reserved addresses without an interface are absent, not live endpoints.
    pub(super) endpoint_owners: HashMap<IpAddr, MachineInterfaceSnapshot>,
}

/// Prepares up to [`MAX_TARGET_SLOTS`] original slots, preserving order, repeats
/// and missing records. Empty input succeeds before deadline checks or any work;
/// an oversized nonempty input returns `InvalidArgument` before catalog/DB access.
///
/// Acquisition through commit shares the five-second DB budget, reduced by an
/// earlier caller deadline. `None` applies only that DB budget. Synchronous catalog
/// loading happens before acquisition and outside this budget; it cannot be
/// interrupted here. Caller expiry is checked before/after it and before return.
///
/// The session is released before catalog composition or profile resolution.
/// Catalog loading retains existing fallback behavior. Expiry is typed separately;
/// DB errors retain typed internal details that public callers must map and redact.
pub(super) async fn load(
    pool: &sqlx::PgPool,
    config: &CarbideConfig,
    targets: FirmwareTargets<'_>,
    caller_deadline: Option<Instant>,
) -> Result<FirmwareReadContext, PreparationError> {
    let slots = match targets {
        FirmwareTargets::Machines(ids) => ids.len(),
        FirmwareTargets::Switches(ids) => ids.len(),
        FirmwareTargets::Shelves(ids) => ids.len(),
    };
    if slots == 0 {
        return Ok(FirmwareReadContext::default());
    }
    if slots > MAX_TARGET_SLOTS {
        return Err(CarbideError::InvalidArgument(format!(
            "firmware details accepts at most {MAX_TARGET_SLOTS} input slots; received {slots}"
        ))
        .into());
    }
    check_deadline(caller_deadline)?;
    // Metadata loading can block on the filesystem. Finish it before taking a
    // pooled session; runtime overrides still belong to the database snapshot.
    let disk_catalog = matches!(targets, FirmwareTargets::Machines(_))
        .then(|| config.get_firmware_config().create_snapshot());
    check_deadline(caller_deadline)?;
    let budget_deadline = Instant::now() + SNAPSHOT_TIMEOUT;
    let deadline = caller_deadline.map_or(budget_deadline, |caller| caller.min(budget_deadline));
    let (mut context, overrides) = tokio::time::timeout_at(deadline, load_snapshot(pool, targets))
        .await
        .map_err(|_| PreparationError::DeadlineExceeded)??;
    check_deadline(Some(deadline))?;
    context.host_catalog = disk_catalog.map(|catalog| catalog.with_overrides(overrides));
    context.profiles = context
        .racks
        .values()
        .filter_map(|rack| rack.rack_profile_id.clone())
        .collect::<HashSet<_>>()
        .into_iter()
        .filter_map(|id| {
            config
                .rack_profiles
                .get(id.as_str())
                .map(|profile| (id, profile.clone()))
        })
        .collect();
    check_deadline(caller_deadline)?;
    Ok(context)
}

/// Rejects late completion even when Tokio polls a ready operation before its timer.
fn check_deadline(deadline: Option<Instant>) -> Result<(), PreparationError> {
    if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
        return Err(PreparationError::DeadlineExceeded);
    }
    Ok(())
}

/// Only database operations run while [`DbSessionGuard`] is live. No connection or
/// row borrow leaves the snapshot, including on timeout or caller cancellation.
async fn load_snapshot(
    pool: &sqlx::PgPool,
    targets: FirmwareTargets<'_>,
) -> Result<(FirmwareReadContext, Vec<HostFirmwareConfig>), CarbideError> {
    let mut connection = DbSessionGuard::acquire(pool).await?;
    let mut txn = connection
        .begin_read_only_snapshot(SNAPSHOT_TIMEOUT)
        .await?;
    let mut context = FirmwareReadContext::default();
    let mut rack_ids = HashSet::new();
    let mut overrides = Vec::new();
    match targets {
        FirmwareTargets::Machines(ids) => {
            let machines: HashMap<_, _> = db::machine::find(
                txn.as_pgconn(),
                db::ObjectFilter::List(ids),
                MachineSearchConfig::default(),
            )
            .await?
            .into_iter()
            .map(|machine| (machine.id, Arc::new(machine)))
            .collect();
            rack_ids.extend(
                machines
                    .values()
                    .filter_map(|machine| machine.rack_id.clone()),
            );
            let machine_ids = machines
                .keys()
                .map(|id| *id.as_machine_id())
                .collect::<Vec<_>>();
            let interfaces = db::machine_interface::find_by(
                txn.as_pgconn(),
                db::ObjectColumnFilter::List(db::machine_interface::MachineIdColumn, &machine_ids),
            )
            .await?;
            for interface in interfaces {
                if interface.interface_type != InterfaceType::Bmc {
                    continue;
                }
                if let Some(machine_id) = interface.machine_id {
                    // The query contains only host IDs already decoded by machine::find.
                    let machine_id =
                        HostMachineId::try_from(machine_id).map_err(db::DatabaseError::from)?;
                    context
                        .bmc_aliases
                        .entry(machine_id)
                        .or_default()
                        .push(interface.mac_address);
                }
            }
            for aliases in context.bmc_aliases.values_mut() {
                aliases.sort_unstable_by_key(|mac| mac.bytes());
                aliases.dedup();
            }
            let ips = machines
                .values()
                .filter_map(|machine| machine.status.bmc_info.ip)
                .collect::<HashSet<_>>()
                .into_iter()
                .collect();
            context.host_reports = db::explored_endpoints::find_by_ips(txn.as_pgconn(), ips)
                .await?
                .into_iter()
                .map(|endpoint| (endpoint.address, endpoint.report))
                .collect();
            context.entries = ids
                .iter()
                .map(|id| FirmwareReadEntry::Machine {
                    machine_id: *id,
                    machine: machines.get(id).cloned(),
                })
                .collect();
        }
        FirmwareTargets::Switches(ids) => {
            let switches: HashMap<_, _> = db::switch::find_by(
                txn.as_pgconn(),
                db::ObjectColumnFilter::List(db::switch::IdColumn, ids),
            )
            .await?
            .into_iter()
            .map(|switch| (switch.id, Arc::new(switch)))
            .collect();
            rack_ids.extend(
                switches
                    .values()
                    .filter_map(|switch| switch.rack_id.clone()),
            );
            let endpoints: HashMap<_, _> =
                db::switch::find_switch_endpoints_by_ids(txn.as_pgconn(), ids)
                    .await?
                    .into_iter()
                    .map(|row| (row.switch_id, Arc::new(row)))
                    .collect();
            context.entries = ids
                .iter()
                .map(|id| FirmwareReadEntry::Switch {
                    switch_id: *id,
                    switch: switches.get(id).cloned(),
                    endpoint: switches.get(id).and_then(|_| endpoints.get(id).cloned()),
                })
                .collect();
        }
        FirmwareTargets::Shelves(ids) => {
            let shelves: HashMap<_, _> = db::power_shelf::find_by(
                txn.as_pgconn(),
                db::ObjectColumnFilter::List(db::power_shelf::IdColumn, ids),
            )
            .await?
            .into_iter()
            .map(|shelf| (shelf.id, Arc::new(shelf)))
            .collect();
            rack_ids.extend(shelves.values().filter_map(|shelf| shelf.rack_id.clone()));
            let endpoints: HashMap<_, _> =
                db::power_shelf::find_power_shelf_endpoints_by_ids(txn.as_pgconn(), ids)
                    .await?
                    .into_iter()
                    .map(|row| (row.power_shelf_id, Arc::new(row)))
                    .collect();
            context.entries = ids
                .iter()
                .map(|id| FirmwareReadEntry::Shelf {
                    power_shelf_id: *id,
                    shelf: shelves.get(id).cloned(),
                    endpoint: shelves.get(id).and_then(|_| endpoints.get(id).cloned()),
                })
                .collect();
        }
    }
    let rack_ids = rack_ids.into_iter().collect::<Vec<_>>();
    context.racks = db::rack::find_by(
        txn.as_pgconn(),
        db::ObjectColumnFilter::List(db::rack::IdColumn, &rack_ids),
    )
    .await?
    .into_iter()
    .map(|rack| (rack.id.clone(), rack))
    .collect();
    if matches!(targets, FirmwareTargets::Machines(_)) {
        overrides = db::host_firmware_config::list_configs(txn.as_pgconn()).await?;
    }
    // Collect all roles against the same ownership snapshot as the selected
    // endpoints. In particular, a switch MAC lookup alone can find an interface
    // owned by another component. The read adapter validates these facts before credentials.
    let mut ips = HashSet::new();
    for entry in &context.entries {
        match entry {
            FirmwareReadEntry::Machine {
                machine: Some(machine),
                ..
            } => {
                ips.extend(machine.status.bmc_info.ip);
            }
            FirmwareReadEntry::Switch {
                endpoint: Some(endpoint),
                ..
            } => {
                ips.insert(endpoint.bmc_ip);
                ips.extend(endpoint.nvos_ip);
            }
            FirmwareReadEntry::Shelf {
                endpoint: Some(endpoint),
                ..
            } => {
                ips.insert(endpoint.pmc_ip);
            }
            _ => {}
        }
    }
    context.endpoint_owners =
        db::machine_interface::find_by_ips(txn.as_pgconn(), &ips.into_iter().collect::<Vec<_>>())
            .await?;
    txn.commit().await?;
    connection.return_to_pool();
    Ok((context, overrides))
}

/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use carbide_machine_controller::handler::is_bf4_dmi_product;
use carbide_uuid::machine::{DpuMachineId, HostMachineId, MachineId};
use model::machine::DpuMachine;
use model::machine::machine_search_config::MachineSearchConfig;
use model::machine::spx_multiplane::{MachineSpxMultiplane, MachineSpxMultiplaneObservation};
use rpc::forge;
use sqlx::PgConnection;
use tonic::{Request, Response, Status};

use crate::CarbideError;
use crate::api::{Api, log_request_data};

/// Initialize older hosts and reconcile expected declarations on the next config
/// poll. Unchanged declarations use an unlocked read. Initialization and syncing
/// re-read requests under the host lock so concurrent operator changes survive.
pub(super) async fn load_configuration(
    txn: &mut PgConnection,
    machine_id: HostMachineId,
) -> Result<Option<MachineSpxMultiplane>, CarbideError> {
    let machine = db::machine::find_one(&mut *txn, &machine_id, MachineSearchConfig::default())
        .await?
        .ok_or_else(|| CarbideError::NotFoundError {
            kind: "machine",
            id: machine_id.to_string(),
        })?;
    let Some(bmc_mac) = machine.status.bmc_info.mac else {
        tracing::info!(%machine_id, "spx-maintenance: [controller] SPX maintenance configuration unavailable: host has no BMC MAC");
        return Ok(None);
    };
    let Some(expected) = db::expected_machine::find_by_bmc_mac_address(&mut *txn, bmc_mac).await?
    else {
        tracing::info!(%machine_id, "spx-maintenance: [controller] SPX maintenance configuration unavailable: expected machine not found");
        return Ok(None);
    };
    let declared = MachineSpxMultiplane::from_expected_interfaces(&expected.data.interfaces)
        .map_err(CarbideError::InvalidArgument)?;
    let Some(declared) = declared else {
        tracing::info!(%machine_id, "spx-maintenance: [controller] SPX maintenance configuration unavailable: no declared CX9 logical interfaces");
        return Ok(None);
    };
    if machine
        .spx_multiplane
        .as_ref()
        .is_some_and(|configuration| {
            configuration
                .interfaces
                .keys()
                .eq(declared.interfaces.keys())
        })
    {
        return Ok(machine.spx_multiplane);
    }

    // A writer may have changed requests while this poll waited for the lock.
    // Merge declarations into the latest persisted configuration, never the earlier read.
    let previous = db::machine::lock_spx_multiplane(txn, &machine_id).await?;
    let mut configuration = previous.clone().unwrap_or_default();
    configuration.synchronize_declarations(declared);
    if previous.as_ref() != Some(&configuration) {
        tracing::info!(%machine_id, interface_count = configuration.interfaces.len(), initializing = previous.is_none(), "spx-maintenance: [controller] Staging SPX maintenance declarations");
        db::machine::update_spx_multiplane(txn, &machine_id, &configuration).await?;
    }
    Ok(Some(configuration))
}

fn host_id(id: Option<MachineId>) -> Result<HostMachineId, CarbideError> {
    id.ok_or(CarbideError::MissingArgument("machine_id"))?
        .try_into()
        .map_err(|_| CarbideError::InvalidArgument("machine_id must identify a host".to_string()))
}

/// A host's declared CX9 interfaces define Astra participation; only its BF4 DPUs
/// receive that configuration. Missing local logical names are agent errors, not exclusions.
pub(super) fn is_bf4_dpu(dpu: &DpuMachine) -> bool {
    dpu.status
        .hardware_info
        .as_ref()
        .and_then(|hardware| hardware.dmi_data.as_ref())
        .is_some_and(|dmi| is_bf4_dmi_product(&dmi.product_name))
}

async fn response(
    txn: &mut PgConnection,
    id: HostMachineId,
    configuration: MachineSpxMultiplane,
) -> Result<forge::MachineSpxMultiplane, CarbideError> {
    let snapshot = db::managed_host::load_snapshot(txn, &id, Default::default())
        .await?
        .ok_or_else(|| CarbideError::NotFoundError {
            kind: "machine",
            id: id.to_string(),
        })?;
    let mut dpu_statuses = snapshot
        .dpu_snapshots
        .iter()
        .filter(|dpu| is_bf4_dpu(dpu))
        .map(|dpu| forge::MachineSpxMultiplaneDpuStatus {
            dpu_machine_id: Some(dpu.id),
            interfaces: configuration
                .interfaces
                .keys()
                .map(|name| {
                    let observation = dpu
                        .status
                        .spx_multiplane_status
                        .as_ref()
                        .and_then(|status| status.interfaces.get(name));
                    forge::AstraMultiplaneInterfaceStatus {
                        logical_ifname: name.clone(),
                        maintenance_mode: observation.and_then(|o| o.maintenance_mode),
                        revision: observation
                            .map(|o| o.revision.to_string())
                            .unwrap_or_default(),
                        netdev: observation.and_then(|o| o.netdev.clone()),
                        error: observation.and_then(|o| o.error.clone()),
                        observed_at: observation.map(|o| o.observed_at.into()),
                    }
                })
                .collect(),
        })
        .collect::<Vec<_>>();
    dpu_statuses.sort_by_key(|status| status.dpu_machine_id);
    Ok(forge::MachineSpxMultiplane {
        machine_id: Some(id.into()),
        interfaces: configuration
            .interfaces
            .into_iter()
            .map(|(name, state)| {
                let (rail, plane) = MachineSpxMultiplane::interface_coordinates(&name)
                    .expect("configuration contains canonical declared logical interfaces");
                forge::MachineSpxMultiplaneInterface {
                    logical_ifname: name,
                    rail: rail.into(),
                    plane: plane.into(),
                    cfg_maintenance_mode: state.cfg_maintenance_mode,
                    reference: state.reference,
                    revision: state.revision.to_string(),
                }
            })
            .collect(),
        dpu_statuses,
    })
}

/// Store readback independently of attachment discovery. Ignore reports for an
/// older desired revision so a delayed status cannot acknowledge a newer request.
pub(super) async fn record_observations(
    txn: &mut PgConnection,
    id: HostMachineId,
    dpu_machine_id: DpuMachineId,
    observations: &[forge::AstraMultiplaneInterfaceStatus],
) -> Result<(), CarbideError> {
    if observations.is_empty() {
        return Ok(());
    }
    tracing::info!(machine_id = %id, dpu_machine_id = %dpu_machine_id, reported_interface_count = observations.len(), "spx-maintenance: [controller] Processing SPX maintenance observations");
    let Some(configuration) = db::machine::lock_spx_multiplane(txn, &id).await? else {
        tracing::info!(machine_id = %id, dpu_machine_id = %dpu_machine_id, "spx-maintenance: [controller] Ignoring SPX maintenance observations: no stored configuration");
        return Ok(());
    };
    let mut status = db::machine::lock_spx_multiplane_status(txn, &dpu_machine_id)
        .await?
        .unwrap_or_default();
    let previous_len = status.interfaces.len();
    status
        .interfaces
        .retain(|name, _| configuration.interfaces.contains_key(name));
    let mut changed = status.interfaces.len() != previous_len;
    let mut has_errors = false;
    let mut ignored_observations = Vec::new();
    for report in observations {
        let Some(state) = configuration.interfaces.get(&report.logical_ifname) else {
            ignored_observations.push((&report.logical_ifname, "interface is not declared"));
            continue;
        };
        if report.revision != state.revision.to_string() {
            ignored_observations.push((
                &report.logical_ifname,
                "revision does not match current request",
            ));
            continue;
        }
        let Some(observed_at) = report
            .observed_at
            .and_then(|t| chrono::DateTime::<chrono::Utc>::try_from(t).ok())
        else {
            ignored_observations.push((&report.logical_ifname, "timestamp is missing or invalid"));
            continue;
        };
        if status
            .interfaces
            .get(&report.logical_ifname)
            .is_some_and(|o| o.revision == state.revision && o.observed_at >= observed_at)
        {
            ignored_observations.push((&report.logical_ifname, "timestamp is not newer"));
            continue;
        }
        let error = report.error.clone().or_else(|| {
            (state
                .cfg_maintenance_mode
                .is_some_and(|desired| report.maintenance_mode != Some(desired)))
            .then(|| "Xplane readback does not match requested maintenance mode".to_string())
        });

        has_errors |= error.is_some();
        status.interfaces.insert(
            report.logical_ifname.clone(),
            MachineSpxMultiplaneObservation {
                maintenance_mode: report.maintenance_mode,
                revision: state.revision,
                netdev: report.netdev.clone(),
                error,
                observed_at,
            },
        );
        changed = true;
    }
    if changed {
        db::machine::update_spx_multiplane_status(txn, &dpu_machine_id, &status).await?;
    }
    tracing::info!(machine_id = %id, reported_interface_count = observations.len(), dpu_machine_id = %dpu_machine_id, updated = changed, interfaces = ?status.interfaces, ignored_observations = ?ignored_observations, message = if has_errors { "spx-maintenance: [controller] ERROR: Processed SPX maintenance observations with failures in transaction" } else { "spx-maintenance: [controller] Processed SPX maintenance observations in transaction" });
    Ok(())
}

fn require_configuration(
    configuration: Option<MachineSpxMultiplane>,
) -> Result<MachineSpxMultiplane, CarbideError> {
    configuration.ok_or_else(|| {
        CarbideError::FailedPrecondition(
            "host has no declared CX9 logical interfaces for SPX multiplane maintenance"
                .to_string(),
        )
    })
}

pub(crate) async fn get(
    api: &Api,
    request: Request<forge::MachineSpxMultiplaneRequest>,
) -> Result<Response<forge::MachineSpxMultiplane>, Status> {
    if !api.runtime_config.is_partial_multiplane_enabled() {
        return Err(Status::failed_precondition(
            "partial-multiplane maintenance is disabled for this site",
        ));
    }
    log_request_data(&request);
    let id = host_id(request.into_inner().machine_id)?;
    tracing::info!(machine_id = %id, "spx-maintenance: [controller] Reading SPX maintenance state");
    let mut txn = api.txn_begin().await?;
    let configuration = require_configuration(load_configuration(&mut txn, id).await?)?;
    let result = response(&mut txn, id, configuration).await?;
    txn.commit().await?;
    tracing::info!(machine_id = %id, interface_count = result.interfaces.len(), dpu_count = result.dpu_statuses.len(), "spx-maintenance: [controller] Returning SPX maintenance state");
    Ok(Response::new(result))
}

pub(crate) async fn update(
    api: &Api,
    request: Request<forge::MachineSpxMultiplaneMaintenanceUpdateRequest>,
) -> Result<Response<forge::MachineSpxMultiplane>, Status> {
    if !api.runtime_config.is_partial_multiplane_enabled() {
        return Err(Status::failed_precondition(
            "partial-multiplane maintenance is disabled for this site",
        ));
    }
    log_request_data(&request);
    let request = request.into_inner();
    let id = host_id(request.machine_id)?;
    tracing::info!(machine_id = %id, rail_selector = ?request.rail_selector, plane_selector = ?request.plane_selector, maintenance_mode = ?request.maintenance_mode, reference_supplied = request.reference.is_some(), "spx-maintenance: [controller] Received SPX maintenance update request");
    let planes: Vec<u32> = match request.plane_selector {
        Some(forge::machine_spx_multiplane_maintenance_update_request::PlaneSelector::Plane(
            plane,
        )) if plane <= 7 => vec![plane],
        Some(
            forge::machine_spx_multiplane_maintenance_update_request::PlaneSelector::AllPlanes(_),
        ) => (0..=7).collect(),
        Some(
            forge::machine_spx_multiplane_maintenance_update_request::PlaneSelector::PlaneRange(
                range,
            ),
        ) => expand_range(range, 7, "plane")?,
        Some(_) => {
            return Err(CarbideError::InvalidArgument(
                "plane must be from 0 through 7".to_string(),
            )
            .into());
        }
        None => return Err(CarbideError::MissingArgument("plane_selector").into()),
    };
    let maintenance_mode = request
        .maintenance_mode
        .ok_or(CarbideError::MissingArgument("maintenance_mode"))?;
    let rails: Vec<u32> = match request.rail_selector {
        Some(forge::machine_spx_multiplane_maintenance_update_request::RailSelector::Rail(
            rail,
        )) if rail <= 3 => vec![rail],
        Some(forge::machine_spx_multiplane_maintenance_update_request::RailSelector::AllRails(
            _,
        )) => (0..=3).collect(),
        Some(
            forge::machine_spx_multiplane_maintenance_update_request::RailSelector::RailRange(
                range,
            ),
        ) => expand_range(range, 3, "rail")?,
        Some(_) => {
            return Err(
                CarbideError::InvalidArgument("rail must be from 0 through 3".to_string()).into(),
            );
        }
        None => return Err(CarbideError::MissingArgument("rail_selector").into()),
    };
    let names: Vec<String> = rails
        .iter()
        .flat_map(|rail| {
            planes
                .iter()
                .map(move |plane| format!("eth_r{rail}_p{plane}"))
        })
        .collect();
    let mut txn = api.txn_begin().await?;
    // Updates must retain the host lock even when declarations already match.
    db::machine::lock_spx_multiplane(&mut txn, &id).await?;
    let mut configuration = require_configuration(load_configuration(&mut txn, id).await?)?;
    // Validate the full selection before mutating any entry. A missing rail
    // rejects the whole request, including initialization in this transaction.
    for name in &names {
        if !configuration.interfaces.contains_key(name) {
            tracing::info!(machine_id = %id, logical_ifname = %name, "spx-maintenance: [controller] ERROR: Rejecting SPX maintenance update: selected interface is not declared");
            return Err(CarbideError::InvalidArgument(format!(
                "logical interface {name} is not declared on the expected machine"
            ))
            .into());
        }
    }
    for name in names {
        let state = configuration
            .interfaces
            .get_mut(&name)
            .expect("selection was validated");
        let reference = match &request.reference {
            None => state.reference.clone(),
            Some(value) if value.is_empty() => None,
            Some(value) => Some(value.clone()),
        };
        if state.cfg_maintenance_mode != Some(maintenance_mode) || state.reference != reference {
            tracing::info!(machine_id = %id, logical_ifname = %name, previous_maintenance_mode = ?state.cfg_maintenance_mode, requested_maintenance_mode = maintenance_mode, previous_revision = %state.revision, reference_changed = state.reference != reference, "spx-maintenance: [controller] Updating SPX maintenance request in transaction");
            state.cfg_maintenance_mode = Some(maintenance_mode);
            state.reference = reference;
            state.revision = state.revision.increment();
            tracing::info!(machine_id = %id, logical_ifname = %name, maintenance_mode = ?state.cfg_maintenance_mode, revision = %state.revision, "spx-maintenance: [controller] SPX maintenance request staged");
        } else {
            tracing::info!(machine_id = %id, logical_ifname = %name, maintenance_mode = ?state.cfg_maintenance_mode, revision = %state.revision, "spx-maintenance: [controller] SPX maintenance request already matches; preserving revision");
        }
    }
    db::machine::update_spx_multiplane(&mut txn, &id, &configuration).await?;
    let result = response(&mut txn, id, configuration).await?;
    txn.commit().await?;
    tracing::info!(machine_id = %id, ?rails, ?planes, maintenance_mode, selected_interface_count = rails.len() * planes.len(), "spx-maintenance: [controller] Committed SPX maintenance update");
    Ok(Response::new(result))
}

fn expand_range(
    range: forge::SpxMultiplaneRange,
    maximum: u32,
    selector: &str,
) -> Result<Vec<u32>, CarbideError> {
    let (Some(start), Some(end)) = (range.start, range.end) else {
        return Err(CarbideError::InvalidArgument(format!(
            "{selector} range requires both start and end"
        )));
    };
    if start > end || end > maximum {
        return Err(CarbideError::InvalidArgument(format!(
            "{selector} range must satisfy 0 <= start <= end <= {maximum}"
        )));
    }
    Ok((start..=end).collect())
}

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;

    use super::*;

    #[test]
    fn maintenance_ranges_require_inclusive_ascending_bounded_endpoints() {
        value_scenarios!(
            run = |(start, end, maximum)| expand_range(
                forge::SpxMultiplaneRange { start, end }, maximum, "rail",
            ).ok();
            "both bounds are included" {
                (Some(0), Some(3), 3) => Some(vec![0, 1, 2, 3]),
            }
            "equal endpoints select one value" {
                (Some(7), Some(7), 7) => Some(vec![7]),
            }
            "descending range is rejected" {
                (Some(3), Some(1), 3) => None,
            }
            "rail bound is enforced" {
                (Some(0), Some(4), 3) => None,
            }
            "plane bound is enforced" {
                (Some(0), Some(8), 7) => None,
            }
            "start is required even when end is zero" {
                (None, Some(0), 3) => None,
            }
            "end is required even when start is zero" {
                (Some(0), None, 3) => None,
            }
        );
    }
}

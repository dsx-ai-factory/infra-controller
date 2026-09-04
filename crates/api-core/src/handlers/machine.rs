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

use std::collections::HashMap;
use std::time::Duration;

use ::rpc::errors::RpcDataConversionError;
use ::rpc::forge as rpc;
use ::rpc::model::machine::ManagedHostStateSnapshotRpc;
use carbide_redfish::libredfish::RedfishAuth;
use carbide_secrets::credentials::{BmcCredentialType, CredentialKey, Credentials};
use carbide_uuid::machine::{HostOrDpuId, MachineId};
use libredfish::SystemPowerControl;
use model::bmc_suppression::BmcSuppressionSubsystem;
use model::hardware_info::MachineNvLinkInfo;
use model::machine::machine_search_config::MachineSearchConfig;
use model::machine::{LoadSnapshotOptions, Machine, ManagedHostState, ManagedHostStateSnapshot};
use model::metadata::Metadata;
use model::network_segment::NetworkSegmentType;
use tonic::{Request, Response, Status};

use crate::CarbideError;
use crate::api::{Api, log_machine_id, log_request_data};
use crate::auth::AuthContext;
use crate::handlers::utils::convert_and_log_machine_id;

/// Resolve the host UEFI credential to authenticate a *clear* with while
/// force-deleting a machine, keyed by the password the device currently holds
/// (see `host_uefi_clear_credential_key`). Best effort: any failure returns
/// `None`, so the force-delete skips the clear rather than aborting.
///
/// Resolve the key while the txn is open, commit, then read the secret -- the
/// remote reader (Vault) request must not run while we hold the connection.
async fn resolve_host_uefi_clear_credentials(
    api: &Api,
    bmc_mac_address: mac_address::MacAddress,
) -> Option<Credentials> {
    let mut txn = api.txn_begin().await.ok()?;
    let key =
        crate::handlers::uefi::host_uefi_clear_credential_key(&mut txn, bmc_mac_address).await;
    if let Err(err) = txn.commit().await {
        tracing::warn!(
            %bmc_mac_address,
            error = %err,
            "Failed to commit while resolving host UEFI clear credentials; skipping clear"
        );
        return None;
    }
    let clear_key = key.ok()?;
    crate::handlers::uefi::read_uefi_credentials(
        api.bmc_credential_ops.credential_reader(),
        &clear_key,
    )
    .await
    .ok()
}

pub(crate) async fn find_machine_ids(
    api: &Api,
    request: Request<rpc::MachineSearchConfig>,
) -> Result<Response<::rpc::common::MachineIdList>, Status> {
    log_request_data(&request);

    let search_config = request.into_inner().try_into()?;

    let machine_ids =
        db::machine::find_machine_ids(&api.database_connection, search_config).await?;

    Ok(Response::new(::rpc::common::MachineIdList {
        machine_ids: machine_ids.into_iter().collect(),
    }))
}

pub(crate) async fn find_machine_ids_by_bmc_ips(
    api: &Api,
    request: Request<rpc::BmcIpList>,
) -> Result<Response<rpc::MachineIdBmcIpPairs>, Status> {
    log_request_data(&request);

    let pairs = db::machine_topology::find_machine_bmc_pairs(
        &api.database_connection,
        request.into_inner().bmc_ips,
    )
    .await?;
    let rpc_pairs = rpc::MachineIdBmcIpPairs {
        pairs: pairs
            .into_iter()
            .map(|(machine_id, bmc_ip)| rpc::MachineIdBmcIp {
                machine_id: Some(machine_id),
                bmc_ip,
            })
            .collect(),
    };

    Ok(Response::new(rpc_pairs))
}

pub(crate) async fn find_machines_by_ids(
    api: &Api,
    request: Request<::rpc::forge::MachinesByIdsRequest>,
) -> Result<Response<::rpc::MachineList>, Status> {
    log_request_data(&request);
    let request = request.into_inner();

    let mut txn = api.txn_begin().await?;

    let machine_ids = request.machine_ids;

    let max_find_by_ids = api.runtime_config.max_find_by_ids as usize;
    if machine_ids.len() > max_find_by_ids {
        return Err(CarbideError::InvalidArgument(format!(
            "no more than {max_find_by_ids} IDs can be accepted"
        ))
        .into());
    } else if machine_ids.is_empty() {
        return Err(
            CarbideError::InvalidArgument("at least one ID must be provided".to_string()).into(),
        );
    }

    let snapshots = db::managed_host::load_by_machine_ids(
        &mut txn,
        &machine_ids,
        LoadSnapshotOptions {
            include_history: request.include_history,
            include_instance_data: false,
            host_health_config: api.runtime_config.host_health,
        },
    )
    .await?;

    txn.commit().await?;

    let sla_config = model::machine::slas::MachineSlaConfig::new(
        api.runtime_config
            .machine_state_controller
            .failure_retry_time,
    );
    Ok(Response::new(snapshot_map_to_rpc_machines(
        snapshots,
        &sla_config,
    )))
}

pub(crate) async fn find_machine_state_histories(
    api: &Api,
    request: Request<rpc::MachineStateHistoriesRequest>,
) -> Result<Response<rpc::MachineStateHistories>, Status> {
    log_request_data(&request);
    let request = request.into_inner();

    let machine_ids = request.machine_ids;

    let max_find_by_ids = api.runtime_config.max_find_by_ids as usize;
    if machine_ids.len() > max_find_by_ids {
        return Err(CarbideError::InvalidArgument(format!(
            "no more than {max_find_by_ids} IDs can be accepted"
        ))
        .into());
    } else if machine_ids.is_empty() {
        return Err(
            CarbideError::InvalidArgument("at least one ID must be provided".to_string()).into(),
        );
    }

    let mut txn = api.txn_begin().await?;

    let results = db::state_history::find_by_object_ids(
        &mut txn,
        db::state_history::StateHistoryTableId::Machine,
        &machine_ids,
    )
    .await?;

    let mut response = rpc::MachineStateHistories::default();
    for (machine_id, records) in results {
        response.histories.insert(
            machine_id,
            ::rpc::forge::MachineStateHistoryRecords {
                records: records.into_iter().map(Into::into).collect(),
            },
        );
    }

    txn.commit().await?;

    Ok(Response::new(response))
}

pub(crate) async fn find_machine_health_histories(
    api: &Api,
    request: Request<rpc::MachineHealthHistoriesRequest>,
) -> Result<Response<rpc::HealthHistories>, Status> {
    log_request_data(&request);
    let request = request.into_inner();

    crate::handlers::health::find_health_histories(
        api,
        request.machine_ids,
        db::health_history::HealthHistoryTableId::Machine,
        request.start_time,
        request.end_time,
    )
    .await
}

pub(crate) async fn machine_set_auto_update(
    api: &Api,
    request: Request<rpc::MachineSetAutoUpdateRequest>,
) -> Result<Response<rpc::MachineSetAutoUpdateResponse>, Status> {
    log_request_data(&request);

    let request = request.into_inner();

    let mut txn = api.txn_begin().await?;

    let machine_id = convert_and_log_machine_id(request.machine_id.as_ref())?;
    let Some(_machine) =
        db::machine::find_one(&mut txn, &machine_id, MachineSearchConfig::default()).await?
    else {
        return Err(CarbideError::NotFoundError {
            kind: "machine",
            id: request.machine_id.unwrap_or_default().to_string(),
        }
        .into());
    };

    let state = match request.action() {
        rpc::machine_set_auto_update_request::SetAutoupdateAction::Enable => Some(true),
        rpc::machine_set_auto_update_request::SetAutoupdateAction::Disable => Some(false),
        rpc::machine_set_auto_update_request::SetAutoupdateAction::Clear => None,
    };
    db::machine::set_firmware_autoupdate(&mut txn, &machine_id, state).await?;

    txn.commit().await?;

    Ok(Response::new(rpc::MachineSetAutoUpdateResponse {}))
}

pub(crate) async fn update_machine_metadata(
    api: &Api,
    request: Request<rpc::MachineMetadataUpdateRequest>,
) -> std::result::Result<tonic::Response<()>, tonic::Status> {
    log_request_data(&request);
    let request = request.into_inner();
    let machine_id = convert_and_log_machine_id(request.machine_id.as_ref())?;

    // Prepare the metadata
    let metadata = match request.metadata {
        Some(m) => Metadata::try_from(m).map_err(CarbideError::from)?,
        _ => {
            return Err(
                CarbideError::from(RpcDataConversionError::MissingArgument("metadata")).into(),
            );
        }
    };
    metadata.validate(true).map_err(CarbideError::from)?;

    let (machine, mut txn) = api
        .load_machine(
            &machine_id,
            MachineSearchConfig {
                include_dpus: true,
                include_predicted_host: true,
                ..Default::default()
            },
        )
        .await?;

    let expected_version: config_version::ConfigVersion = match request.if_version_match {
        Some(version) => version.parse().map_err(CarbideError::from)?,
        None => machine.version,
    };

    db::machine::update_metadata(&mut txn, &machine_id, expected_version, metadata).await?;

    txn.commit().await?;

    Ok(tonic::Response::new(()))
}

/// Runs the DB cleanup transaction for a force-delete.
///
/// All operations follow the lock-ordering established for the rest of the
/// system so this transaction cannot deadlock with the site-explorer or
/// allocator under normal conditions. When it does lose a deadlock race, the
/// caller retries: every operation here is idempotent because a deadlock
/// causes Postgres to roll back this transaction, leaving the DB unchanged.
///
/// Response flags are accumulated into a local value and returned only after
/// commit. A Postgres rollback does not restore in-memory state, so keeping
/// the response local ensures the caller only ever sees flags that correspond
/// to the committed transaction.
///
/// BMC credential clearing is intentionally excluded: that is an out-of-band
/// operation the caller performs after the transaction commits.
async fn force_delete_cleanup_txn(
    api: &Api,
    host_machine: Option<&Machine>,
    dpu_machines: &[Machine],
    request: &rpc::AdminForceDeleteMachineRequest,
) -> Result<rpc::AdminForceDeleteMachineResponse, CarbideError> {
    let mut response = rpc::AdminForceDeleteMachineResponse::default();
    // Admission permit BEFORE the transaction: waiters on the admin-segment
    // advisory lock must queue in memory, not on open pool connections.
    let _admin_admission = db::machine_interface::admin_lock_admission().await;
    let mut txn = api.txn_begin().await?;

    // Advisory-lock the admin segments before any row locks so the deletion
    // follows the allocator lock order -- segment advisory lock first, then
    // machine interface/address rows (the convention
    // `reconcile_admin_addresses_for_host` documents). This serializes the
    // deletion against the allocator, reconcile, and discovery transactions
    // that hold those locks while they touch interface rows, so the two
    // sides can't hold segment locks and interface rows in opposite orders.
    // All admin segments, rather than a set computed from this machine's
    // interfaces: the snapshots predate the BMC work above, and they omit
    // BMC-typed interfaces that `force_cleanup` still row-locks.
    db::machine_interface::lock_all_admin_segments(&mut txn).await?;

    let machines = host_machine
        .iter()
        .copied()
        .chain(dpu_machines.iter())
        .collect::<Vec<_>>();
    let bmc_macs = machines
        .iter()
        .filter_map(|machine| machine.status.bmc_info.mac)
        .collect::<Vec<_>>();
    // Collect underlay MACs before interface rows are deleted so DHCP
    // suppression cleanup still sees them when `delete_bmc_suppressions` is set.
    let dhcp_suppression_macs = if request.delete_bmc_suppressions {
        let machine_ids = machines
            .iter()
            .map(|machine| machine.id)
            .collect::<Vec<_>>();
        let oob_macs = db::machine_interface::find_by_machine_ids(&mut txn, &machine_ids)
            .await?
            .into_values()
            .flatten()
            .filter(|interface| {
                interface.network_segment_type == Some(NetworkSegmentType::Underlay)
            })
            .map(|interface| interface.mac_address)
            .collect::<Vec<_>>();
        bmc_macs.iter().copied().chain(oob_macs).collect::<Vec<_>>()
    } else {
        Vec::new()
    };

    // Clean up the explored tables next, in site-explorer's write order
    // (`explored_managed_hosts`, then each machine topology and its
    // `explored_endpoints` row, then interface rows), so this delete and a
    // concurrent exploration pass can't hold the same tables in opposite
    // orders.
    if let Some(machine) = host_machine
        && let Some(addr) = machine.status.bmc_info.ip
    {
        // If this delete waited out a concurrent exploration rewrite, its
        // statement snapshot can miss the row that rewrite re-inserted; the
        // leftover clears on the next exploration pass, which rebuilds the
        // table from the (now deleted) explored endpoints.
        db::explored_managed_host::delete_by_host_bmc_addr(&mut txn, addr).await?;
    }

    let mut machines_by_bmc_ip = machines
        .iter()
        .copied()
        .filter_map(|machine| machine.status.bmc_info.ip.map(|address| (address, machine)))
        .collect::<Vec<_>>();
    // Any transaction touching multiple explored_endpoints needs to sort them the same way to avoid
    // deadlocks: sort by IP.
    machines_by_bmc_ip.sort_by_key(|(address, _)| *address);

    for (addr, machine) in machines_by_bmc_ip {
        tracing::info!(
            bmc_ip_address = %addr,
            machine_id = %machine.id,
            "Cleaning up explored endpoint",
        );

        // Site Explorer refreshes firmware in machine_topologies before it
        // updates this endpoint. Lock in the same order; force_cleanup later
        // deletes the already-locked topology row.
        db::machine_topology::lock_by_machine_id(&mut txn, &machine.id).await?;
        db::explored_endpoints::delete(&mut txn, addr).await?;
    }

    if let Some(machine) = host_machine {
        if request.delete_bmc_interfaces
            && let Some(bmc_ip) = machine.status.bmc_info.ip
        {
            response.host_bmc_interface_associated = true;
            if db::machine_interface::delete_by_ip(&mut txn, bmc_ip)
                .await?
                .is_some()
            {
                response.host_bmc_interface_deleted = true;
            }
        }
        db::machine::force_cleanup(&mut txn, &machine.id).await?;

        if request.delete_interfaces {
            for interface in &machine.status.interfaces {
                // The delete retains each row's boot interface pair in
                // `retained_boot_interfaces`, so a re-ingested machine
                // recovers its boot target before its first DHCP.
                db::machine_interface::delete(&interface.id, &mut txn).await?;
            }
            response.host_interfaces_deleted = true;
        }

        db::attestation::ek_cert_verification_status::delete_ca_verification_status_by_machine_id(
            &mut txn,
            &machine.id,
        )
        .await
        .inspect_err(|e| {
            tracing::error!(
                machine_id = %machine.id,
                error = %e,
                "Could not remove EK certificate status",
            );
        })?;
    }

    for dpu_machine in dpu_machines {
        // Free up all loopback IPs allocated for this DPU.
        db::vpc_dpu_loopback::delete_and_deallocate(
            &api.common_pools,
            &dpu_machine.id.try_into().map_err(|error| {
                CarbideError::internal(format!("invalid DPU machine ID: {error}"))
            })?,
            &mut txn,
            true,
        )
        .await?;

        if let Some(loopback_ip) = dpu_machine.network_config.loopback_ip {
            db::resource_pool::release(
                &api.common_pools.ethernet.pool_loopback_ip,
                &mut txn,
                loopback_ip,
            )
            .await?
        }

        // The machine snapshot predates `ForceDeletion`, so a concurrent
        // backfill can make its IPv6 field stale. The pool owner remains the
        // authoritative reservation source during deletion.
        if let Some(loopback_ip_v6) = db::resource_pool::find_owned_allocation(
            &api.common_pools.ethernet.pool_loopback_ip_v6,
            &mut txn,
            model::resource_pool::OwnerType::Machine,
            &dpu_machine.id.to_string(),
        )
        .await
        .map_err(CarbideError::from)?
        {
            db::resource_pool::release(
                &api.common_pools.ethernet.pool_loopback_ip_v6,
                &mut txn,
                loopback_ip_v6,
            )
            .await?
        }

        db::network_devices::dpu_to_network_device_map::delete(&mut txn, &dpu_machine.id).await?;

        if request.delete_bmc_interfaces
            && let Some(bmc_ip) = dpu_machine.status.bmc_info.ip
        {
            response.dpu_bmc_interface_associated = true;
            if db::machine_interface::delete_by_ip(&mut txn, bmc_ip)
                .await?
                .is_some()
            {
                response.dpu_bmc_interface_deleted = true;
            }
        }
        if let Some(asn) = dpu_machine.asn {
            db::resource_pool::release(&api.common_pools.ethernet.pool_fnn_asn, &mut txn, asn)
                .await?;
        }
        db::machine::force_cleanup(&mut txn, &dpu_machine.id).await?;

        if request.delete_interfaces {
            for interface in &dpu_machine.status.interfaces {
                db::machine_interface::delete(&interface.id, &mut txn).await?;
            }
            response.dpu_interfaces_deleted = true;
        }
    }

    // Optional permanent wipe: drop retained boot pairs written by interface
    // deletes above (and any leftover BMC MAC entries).
    if request.delete_retained_boot_interfaces {
        for machine in &machines {
            if let Some(bmc_mac) = machine.status.bmc_info.mac {
                db::retained_boot_interface::take_by_mac(&mut txn, bmc_mac, None).await?;
            }
            for interface in &machine.status.interfaces {
                db::retained_boot_interface::take_by_mac(&mut txn, interface.mac_address, None)
                    .await?;
            }
        }
    }

    if request.delete_bmc_suppressions {
        db::bmc_suppression::delete_many(
            &mut txn,
            &bmc_macs,
            BmcSuppressionSubsystem::SiteExplorer,
        )
        .await?;
        db::bmc_suppression::delete_many(
            &mut txn,
            &dhcp_suppression_macs,
            BmcSuppressionSubsystem::Dhcp,
        )
        .await?;
    }

    txn.commit().await?;

    Ok(response)
}

pub(crate) async fn admin_force_delete_machine(
    api: &Api,
    request: Request<rpc::AdminForceDeleteMachineRequest>,
) -> Result<Response<rpc::AdminForceDeleteMachineResponse>, Status> {
    log_request_data(&request);

    let (_metadata, extensions, request) = request.into_parts();
    let query = &request.host_query;

    let mut response = rpc::AdminForceDeleteMachineResponse {
        all_done: true,
        ..Default::default()
    };
    // This is the default
    // If we can't delete something in one go - we will reset it
    response.all_done = true;
    response.initial_lockdown_state = "".to_string();
    response.machine_unlocked = false;

    let mut txn = api.txn_begin().await?;

    let machine = match db::machine::find_by_query(&mut txn, query).await? {
        Some(machine) => machine,
        None => {
            // If the machine was already deleted, then there is nothing to do
            // and this is a success
            return Ok(Response::new(response));
        }
    };
    log_machine_id(&machine.id);

    let issued_by = extensions
        .get::<AuthContext>()
        .and_then(|ctx| ctx.get_external_user_name());

    let serial = machine
        .status
        .hardware_info
        .as_ref()
        .and_then(|hw| hw.dmi_data.as_ref())
        .map(|dmi| dmi.product_serial.as_str())
        .unwrap_or("unknown");

    tracing::info!(
        query = %query,
        machine_id = %machine.id,
        serial,
        issued_by = ?issued_by,
        "Admin force-delete machine request",
    );

    if machine.config.instance_type_id.is_some() && !request.allow_delete_with_instance_type {
        return Err(CarbideError::FailedPrecondition(format!(
            "association with instance type must be removed before deleting machine {}",
            machine.id
        ))
        .into());
    }

    // TODO: This should maybe just use the snapshot loading functionality that the
    // state controller will use - which already contains the combined state
    let mut host_machine;
    let dpu_machines;
    match machine.id.host_or_dpu_id() {
        HostOrDpuId::Dpu(dpu_machine_id) => {
            if let Some(host) =
                db::machine::find_host_by_dpu_machine_id(&mut txn, &dpu_machine_id).await?
            {
                tracing::info!(
                    host_machine_id = %host.id,
                    dpu_machine_id = %machine.id,
                    "Found host machine",
                );
                // Get all DPUs attached to this host, in case there are more than one.
                let host_machine_id = host.id.try_into().map_err(|error| {
                    CarbideError::internal(format!("invalid host machine ID: {error}"))
                })?;
                dpu_machines =
                    db::machine::find_dpus_by_host_machine_id(&mut txn, &host_machine_id).await?;
                host_machine = Some(host);
            } else {
                host_machine = None;
                dpu_machines = vec![machine];
            }
        }
        HostOrDpuId::Host(host_machine_id) => {
            dpu_machines =
                db::machine::find_dpus_by_host_machine_id(&mut txn, &host_machine_id).await?;
            tracing::info!(
                dpu_machine_ids = ?dpu_machines.iter().map(|m| &m.id).collect::<Vec<_>>(),
                "Found DPU machines",
            );
            host_machine = Some(machine);
        }
    }

    let instance_id = if let Some(host_machine) = &mut host_machine {
        let host_machine_id = host_machine.id;
        *host_machine = db::machine::find_one(
            &mut txn,
            &host_machine_id,
            MachineSearchConfig {
                for_update: true,
                ..MachineSearchConfig::default()
            },
        )
        .await?
        .ok_or_else(|| CarbideError::NotFoundError {
            kind: "Machine",
            id: host_machine_id.to_string(),
        })?;

        if host_machine.config.instance_type_id.is_some()
            && !request.allow_delete_with_instance_type
        {
            return Err(CarbideError::FailedPrecondition(format!(
                "association with instance type must be removed before deleting machine {}",
                host_machine.id
            ))
            .into());
        }

        let instance_id = db::instance::find_id_by_machine_id(&mut txn, &host_machine.id).await?;
        if instance_id.is_some() && !request.allow_delete_with_instance {
            return Err(CarbideError::FailedPrecondition(format!(
                "attached instance must be removed before deleting machine {}",
                host_machine.id
            ))
            .into());
        }
        instance_id
    } else {
        None
    };

    if let Some(host_machine) = &host_machine {
        response.managed_host_machine_id = host_machine.id.to_string();
        if let Some(iface) = host_machine.status.interfaces.first() {
            response.managed_host_machine_interface_id = iface.id.to_string();
        }
        if let Some(ip) = host_machine.status.bmc_info.ip.as_ref() {
            response.managed_host_bmc_ip = ip.to_string();
        }
    }
    if let Some(dpu_machine) = dpu_machines.first() {
        response.dpu_machine_ids = dpu_machines.iter().map(|m| m.id.to_string()).collect();
        // deprecated field:
        response.dpu_machine_id = dpu_machine.id.to_string();

        let dpu_interfaces = dpu_machines
            .iter()
            .flat_map(|m| m.status.interfaces.clone())
            .collect::<Vec<_>>();
        if let Some(iface) = dpu_interfaces.first() {
            response.dpu_machine_interface_ids =
                dpu_interfaces.iter().map(|i| i.id.to_string()).collect();
            // deprecated field:
            response.dpu_machine_interface_id = iface.id.to_string();
        }
        if let Some(ip) = dpu_machine.status.bmc_info.ip.as_ref() {
            response.dpu_bmc_ip = ip.to_string();
        }
    }

    if let Some(machine) = &host_machine
        && machine.config.dpf.used_for_ingestion
        && api.dpf_sdk.is_none()
        && !request.allow_delete_with_orphaned_dpf_crds
    {
        return Err(CarbideError::FailedPrecondition(format!(
            "failed force-delete host {}: DPF was used for ingestion \
                    but DPF is not configured. use \
                    --allow-delete-with-orphaned-dpf-crds to proceed, \
                    though this will require manual cleanup of DPF CRDs",
            machine.id
        ))
        .into());
    }

    // So far we only inspected state - now we start the deletion process
    // TODO: In the new model we might just need to move one Machine to this state
    if let Some(host_machine) = &host_machine {
        db::machine::advance(
            host_machine,
            &mut txn,
            &ManagedHostState::ForceDeletion,
            None,
        )
        .await?;
    }
    if let Some(instance_id) = &instance_id {
        response.instance_id = instance_id.to_string();
    }
    for dpu_machine in dpu_machines.iter() {
        db::machine::advance(
            dpu_machine,
            &mut txn,
            &ManagedHostState::ForceDeletion,
            None,
        )
        .await?;
    }

    if let Some(instance_id) = instance_id {
        // Record the current IB memberships after acquiring the Machine lock,
        // in the same transaction as ForceDeletion. UFM cleanup runs only
        // after this transaction commits.
        crate::handlers::instance::record_force_delete_retired_ib_memberships(
            &mut txn,
            instance_id,
        )
        .await?;
    }

    // Commit the transaction to make the the ForceDeletion state visible to other consumers, and to
    // avoid holding a long-running transaction while we issue redfish calls.
    txn.commit().await?;

    // Note: The following deletion steps are all ordered in an idempotent fashion
    if let Some(instance_id) = instance_id {
        crate::handlers::instance::force_delete_instance(instance_id, api, &mut response).await?;
    }

    if let Some(machine) = &host_machine {
        if let Some(ip) = machine.status.bmc_info.ip {
            if let Some(bmc_mac_address) = machine.status.bmc_info.mac {
                let ip_address = ip.to_string();
                tracing::info!(
                    bmc_ip_address = %ip,
                    machine_id = %machine.id,
                    "BMC IP and MAC address for machine was found. Trying to perform Bios unlock",
                );

                match api
                    .redfish_pool
                    .create_client(
                        &ip_address,
                        machine.status.bmc_info.port,
                        RedfishAuth::Key(CredentialKey::BmcCredentials {
                            credential_type: BmcCredentialType::BmcRoot { bmc_mac_address },
                        }),
                        None,
                    )
                    .await
                {
                    Ok(client) => {
                        let machine_id = machine.id;
                        let mut host_restart_needed = false;
                        match client.lockdown_status().await {
                            Ok(status) if status.is_fully_disabled() => {
                                tracing::info!(%machine_id, "Bios is not locked down");
                                response.initial_lockdown_state = status.to_string();
                                response.machine_unlocked = false;
                            }
                            Ok(status) => {
                                tracing::info!(%machine_id, ?status, "Unlocking BIOS");
                                if let Err(e) =
                                    client.lockdown(libredfish::EnabledDisabled::Disabled).await
                                {
                                    tracing::warn!(%machine_id, error = %e, "Failed to unlock");
                                    response.initial_lockdown_state = status.to_string();
                                    response.machine_unlocked = false;
                                } else {
                                    response.initial_lockdown_state = status.to_string();
                                    response.machine_unlocked = true;
                                }
                                // Dell, at least, needs a reboot after disabling lockdown.  Safest to just do this for everything.
                                host_restart_needed = true;
                            }
                            Err(e) => {
                                tracing::warn!(%machine_id, error = %e, "Failed to fetch lockdown status");
                                response.initial_lockdown_state = "".to_string();
                                response.machine_unlocked = false;
                            }
                        }

                        if machine.bios_password_set_time.is_some() {
                            // Resolve the credential the device currently carries to
                            // authenticate the clear (table-driven). Best effort: if it
                            // cannot be resolved, skip the clear rather than aborting the
                            // force-delete.
                            let clear_credentials =
                                resolve_host_uefi_clear_credentials(api, bmc_mac_address).await;
                            if let Some(clear_credentials) = clear_credentials {
                                let access = carbide_utils::redfish::BmcAccessInfo {
                                    host: ip_address.clone(),
                                    port: machine.status.bmc_info.port,
                                    mac_address: bmc_mac_address,
                                };
                                match api
                                    .bmc_credential_ops
                                    .clear_host_uefi_password(&access, clear_credentials)
                                    .await
                                {
                                    Ok(_) => {
                                        // The UEFI password was reset on the device, so the host no
                                        // longer carries the site-wide UEFI value: drop the host_uefi
                                        // convergence marker (keyed by the host BMC MAC, mirroring where
                                        // it is recorded when the password is set). Best-effort like the
                                        // clear itself -- the machine row is being deleted anyway, so a
                                        // surviving marker would be neutralized by the rotation engine's
                                        // live-device join regardless.
                                        if let Err(e) =
                                            forget_host_uefi_convergence(api, bmc_mac_address).await
                                        {
                                            tracing::warn!(%machine_id, error = %e, "Cleared host UEFI password but failed to delete its credential-rotation marker");
                                        }
                                    }
                                    Err(e) => {
                                        tracing::warn!(%machine_id, error = %e, "Failed to clear host UEFI password while force deleting machine");
                                    }
                                }
                            } else {
                                tracing::warn!(%machine_id, "Could not resolve host UEFI credentials to clear while force deleting machine; skipping clear");
                            }

                            // TODO (spyda): have libredfish return whether the client needs to reboot the host after clearing the host uefi password
                            if machine.bmc_vendor().is_lenovo() {
                                host_restart_needed = true;
                            }
                        }

                        if host_restart_needed
                            && let Err(e) = client.power(SystemPowerControl::ForceRestart).await
                        {
                            tracing::warn!(%machine_id, error = %e, "Failed to reboot host while force deleting machine");
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            machine_id = %machine.id,
                            error = %e,
                            "Failed to create Redfish client. Skipping bios unlock",
                        );
                    }
                }
            } else {
                tracing::warn!(
                    machine_id = %machine.id,
                    "Failed to unlock host because the BMC MAC address is missing",
                );
            }
        } else {
            tracing::warn!(
                machine_id = %machine.id,
                "Failed to unlock host because the BMC IP address is missing",
            );
        }

        if let Some(ref ops) = api.dpf_sdk
            && !dpu_machines.is_empty()
        {
            let host_dpf_id = machine
                .dpf_id()
                .ok_or_else(|| CarbideError::internal("BMC MAC not set for host".to_string()))?;
            let dpu_device_names: Vec<String> = dpu_machines
                .iter()
                .map(|d| {
                    d.dpf_id().ok_or_else(|| {
                        CarbideError::internal("BMC MAC not set for DPU".to_string())
                    })
                })
                .collect::<Result<_, _>>()?;
            ops.force_delete_host(&host_dpf_id, &dpu_device_names)
                .await
                .map_err(CarbideError::DpfError)?;
        }
    }

    // Retry the cleanup transaction on deadlock or serialization failure.
    // Both are transient: Postgres aborted our transaction, the conflicting
    // transaction has already committed, and a fresh attempt will succeed
    // once its locks are released. The backoff starts at 50 ms and doubles
    // each attempt (50, 100, 200, 400 ms) for up to five total attempts.
    const MAX_CLEANUP_ATTEMPTS: u32 = 5;
    for attempt in 0..MAX_CLEANUP_ATTEMPTS {
        match force_delete_cleanup_txn(api, host_machine.as_ref(), &dpu_machines, &request).await {
            Ok(cleanup) => {
                response.host_bmc_interface_associated = cleanup.host_bmc_interface_associated;
                response.host_bmc_interface_deleted = cleanup.host_bmc_interface_deleted;
                response.host_interfaces_deleted = cleanup.host_interfaces_deleted;
                response.dpu_bmc_interface_associated = cleanup.dpu_bmc_interface_associated;
                response.dpu_bmc_interface_deleted = cleanup.dpu_bmc_interface_deleted;
                response.dpu_interfaces_deleted = cleanup.dpu_interfaces_deleted;
                break;
            }
            Err(error)
                if error.is_retryable_transaction_error() && attempt + 1 < MAX_CLEANUP_ATTEMPTS =>
            {
                let delay = Duration::from_millis(50u64 << attempt);
                tracing::warn!(
                    attempt_number = attempt + 1,
                    retry_delay_milliseconds = delay.as_millis(),
                    pg_sqlstate = error.pg_sqlstate().as_deref().unwrap_or("unknown"),
                    error = %error,
                    "force-delete cleanup transaction conflict; retrying",
                );
                tokio::time::sleep(delay).await;
            }
            Err(error) => return Err(error.into()),
        }
    }

    // Do BMC operations outside a transaction to avoid long-running transactions
    if request.delete_bmc_credentials {
        if let Some(machine) = &host_machine {
            clear_bmc_credentials(api, machine).await?;
        }
        for dpu_machine in &dpu_machines {
            clear_bmc_credentials(api, dpu_machine).await?;
        }
    }

    Ok(Response::new(response))
}

/// Retrieves all DPU information including operational state.
pub(crate) async fn get_dpu_info_list(
    api: &Api,
    request: Request<rpc::GetDpuInfoListRequest>,
) -> Result<Response<rpc::GetDpuInfoListResponse>, Status> {
    log_request_data(&request);

    let mut txn = api.txn_begin().await?;

    let dpu_list = db::machine::find_dpu_infos(&mut txn).await?;

    txn.commit().await?;

    let response = rpc::GetDpuInfoListResponse {
        dpu_list: dpu_list.into_iter().map(rpc::DpuInfo::from).collect(),
    };
    Ok(Response::new(response))
}

fn snapshot_map_to_rpc_machines(
    snapshots: HashMap<MachineId, ManagedHostStateSnapshot>,
    sla_config: &model::machine::slas::MachineSlaConfig,
) -> rpc::MachineList {
    let mut result = rpc::MachineList {
        machines: Vec::with_capacity(snapshots.len()),
    };

    for (machine_id, snapshot) in snapshots.into_iter() {
        if let Some(rpc_machine) = snapshot.into_rpc_machine_state(
            match machine_id.machine_type().is_dpu() {
                true => Some(&machine_id),
                false => None,
            },
            sla_config,
        ) {
            result.machines.push(rpc_machine);
        }
        // A log message for the None case is already emitted inside
        // managed_host::load_by_machine_ids
    }

    result
}

async fn clear_bmc_credentials(api: &Api, machine: &Machine) -> Result<(), CarbideError> {
    if let Some(mac_address) = machine.status.bmc_info.mac {
        tracing::info!(
            bmc_mac_address = %mac_address,
            machine_id = %machine.id,
            "Cleaning up BMC credentials in vault",
        );
        crate::handlers::credential::delete_bmc_root_credentials_by_mac(api, mac_address).await?;
    }

    Ok(())
}

/// Deletes the `host_uefi` credential-rotation convergence marker for a host,
/// keyed by its BMC MAC. Called after force-delete resets the host UEFI password
/// on the device, where the host no longer carries the site-wide UEFI value.
async fn forget_host_uefi_convergence(
    api: &Api,
    bmc_mac_address: mac_address::MacAddress,
) -> Result<(), CarbideError> {
    let mut txn = api.txn_begin().await?;
    db::credential_rotation::delete_device_converged(
        &mut txn,
        bmc_mac_address,
        db::credential_rotation::CredentialRotationType::HostUefi,
    )
    .await?;
    txn.commit().await?;
    Ok(())
}

pub(crate) async fn get_machine_position_info(
    api: &Api,
    request: Request<rpc::MachinePositionQuery>,
) -> Result<Response<rpc::MachinePositionInfoList>, Status> {
    let request = request.into_inner();

    if request.machine_ids.is_empty() {
        return Err(CarbideError::InvalidArgument(
            "at least one machine ID must be specified".to_string(),
        )
        .into());
    }
    let mut txn = api.txn_begin().await?;

    // Translate the machine IDs to BMC IPs.
    // Note: Machines without linked BMC interfaces will be silently omitted from the result,
    // consistent with how find_machines_by_ids handles missing machines.
    let pairs =
        db::machine_topology::find_machine_bmc_pairs_by_machine_id(&mut txn, request.machine_ids)
            .await?;

    // Find the explored endpoints for those BMC IPs
    let explored_endpoints = db::explored_endpoints::find_by_ips(
        &mut txn,
        pairs
            .iter()
            .filter_map(|(machine_id, ip_opt)| match ip_opt {
                Some(ip_str) => ip_str.parse().ok().or_else(|| {
                    tracing::warn!(
                        bmc_ip_address = %ip_str,
                        machine_id = %machine_id,
                        "Failed to parse BMC IP",
                    );
                    None
                }),
                None => {
                    tracing::warn!(
                        machine_id = %machine_id,
                        "Machine has topology but no BMC IP configured",
                    );
                    None
                }
            })
            .collect(),
    )
    .await?;
    txn.commit().await?;

    // Redo the explored endpoints into a hashmap based on the IP address
    let as_hashmap = explored_endpoints
        .into_iter()
        .map(|x| (x.address.to_string(), x))
        .collect::<HashMap<String, model::site_explorer::ExploredEndpoint>>();

    // Build the response, looking up explored endpoints by BMC IP
    let ret = rpc::MachinePositionInfoList {
        machine_position_info: pairs
            .iter()
            .map(|(machine_id, ip_opt)| {
                let endpoint = ip_opt.as_ref().and_then(|ip| as_hashmap.get(ip));
                rpc::MachinePositionInfo {
                    machine_id: Some(*machine_id),
                    physical_slot_number: endpoint.and_then(|ep| ep.report.physical_slot_number),
                    compute_tray_index: endpoint.and_then(|ep| ep.report.compute_tray_index),
                    topology_id: endpoint.and_then(|ep| ep.report.topology_id),
                    revision_id: endpoint.and_then(|ep| ep.report.revision_id),
                    switch_id: endpoint.and_then(|ep| ep.report.switch_id),
                    power_shelf_id: endpoint.and_then(|ep| ep.report.power_shelf_id),
                }
            })
            .collect(),
    };

    Ok(Response::new(ret))
}

pub(crate) async fn update_machine_nv_link_info(
    api: &Api,
    request: Request<rpc::UpdateMachineNvLinkInfoRequest>,
) -> std::result::Result<tonic::Response<()>, tonic::Status> {
    log_request_data(&request);
    let request = request.into_inner();
    let machine_id = convert_and_log_machine_id(request.machine_id.as_ref())?;

    let nvlink_info = request.nvlink_info.ok_or_else(|| {
        CarbideError::from(RpcDataConversionError::MissingArgument("nvlink_info"))
    })?;

    let nvlink_info = MachineNvLinkInfo::try_from(nvlink_info).map_err(CarbideError::from)?;

    let mut txn = api.txn_begin().await?;

    db::machine::update_nvlink_info(&mut txn, &machine_id, nvlink_info).await?;

    txn.commit().await?;

    Ok(tonic::Response::new(()))
}

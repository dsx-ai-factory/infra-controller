// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use std::collections::BTreeMap;
use std::future::Future;
use std::time::Duration;

use carbide_instrument::red;
use librms::protos::rack_manager as rms;
use librms::{RackManagerError, RmsApi};
use mac_address::MacAddress;
use tokio::time::{Instant, timeout_at};

use super::{
    RMS_IDENTITY_LOOKUP_ERROR, RmsBackend, SwitchOrPowerShelfRole, build_compute_tray_node_info,
    build_power_shelf_node_info, build_switch_node_info, resolve_compute_tray_identities,
    resolve_power_shelf_identities, resolve_pre_ingestion_power_shelf_identities,
    resolve_switch_identities,
};
use crate::compute_tray_manager::ComputeTrayEndpoint;
use crate::error::ComponentManagerError;
use crate::firmware_inventory::{
    ComponentFirmwareInventoryResult, FirmwareComponent, FirmwareInventoryEntry, FirmwareReadError,
};
use crate::nv_switch_manager::SwitchEndpoint;
use crate::power_shelf_manager::PowerShelfEndpoint;

// Independent application limits bound one caller to four 64-node batches in
// a single rack. The 30-second budget includes preparation and sequential RPCs;
// these are work limits, not measured hardware latency or RMS server limits.
const MAX_ENDPOINTS: usize = 256;
const BATCH_SIZE: usize = 64;
const READ_TIMEOUT: Duration = Duration::from_secs(30);
// After snapshot setup, cap each statement at five seconds while preserving a
// shorter site limit. Acquisition, setup and commit also share READ_TIMEOUT.
const STATEMENT_TIMEOUT: Duration = Duration::from_secs(5);
// Retention limits bound component count and metadata independently of batch
// work. They apply after decoding and do not cap wire allocations.
const MAX_MEMBERS: usize = 256;
const MAX_FIELD_BYTES: usize = 1024;
const MAX_METADATA_BYTES: usize = 64 * 1024;

impl RmsBackend {
    /// Resolves ingested shelves first, then expected inventory for missing MACs.
    pub(super) async fn read_power_shelf_firmware(
        &self,
        endpoints: &[PowerShelfEndpoint],
    ) -> Result<Vec<ComponentFirmwareInventoryResult>, ComponentManagerError> {
        validate_endpoint_count(endpoints.len())?;
        let deadline = Instant::now() + READ_TIMEOUT;
        let macs: Vec<_> = endpoints.iter().map(|ep| ep.pmc_mac).collect();
        let prepare = async {
            // Retain the guard through setup and queries so cancellation releases
            // local pool capacity without waiting for SQLx cleanup.
            let mut session = db::DbSessionGuard::acquire(&self.db)
                .await
                .map_err(preparation_failed)?;
            let mut txn = session
                .begin_read_only_snapshot(STATEMENT_TIMEOUT)
                .await
                .map_err(preparation_failed)?;
            let mut identities = resolve_power_shelf_identities(&mut txn, &macs).await?;
            let missing: Vec<_> = macs
                .iter()
                .copied()
                .filter(|mac| !identities.contains_key(mac))
                .collect();
            identities
                .extend(resolve_pre_ingestion_power_shelf_identities(&mut txn, &missing).await?);
            let nodes = endpoints
                .iter()
                .map(|ep| {
                    self.resolve_switch_or_power_shelf_node(
                        &identities,
                        ep.pmc_mac,
                        SwitchOrPowerShelfRole::PowerShelf,
                    )
                    .map(|resolved| build_power_shelf_node_info(ep, &resolved))
                })
                .collect();
            txn.commit().await.map_err(preparation_failed)?;
            session.return_to_pool();
            Ok(nodes)
        };
        query(self.client.as_ref(), &macs, prepare, deadline).await
    }

    /// Keeps the BMC and NVUE endpoints together in each switch descriptor.
    pub(super) async fn read_switch_firmware(
        &self,
        endpoints: &[SwitchEndpoint],
    ) -> Result<Vec<ComponentFirmwareInventoryResult>, ComponentManagerError> {
        validate_endpoint_count(endpoints.len())?;
        let deadline = Instant::now() + READ_TIMEOUT;
        let macs: Vec<_> = endpoints.iter().map(|ep| ep.bmc_mac).collect();
        let prepare = async {
            // Retain the guard through setup and queries so cancellation releases
            // local pool capacity without waiting for SQLx cleanup.
            let mut session = db::DbSessionGuard::acquire(&self.db)
                .await
                .map_err(preparation_failed)?;
            let mut txn = session
                .begin_read_only_snapshot(STATEMENT_TIMEOUT)
                .await
                .map_err(preparation_failed)?;
            let identities = resolve_switch_identities(&mut txn, &macs).await?;
            // A caller-supplied hostname belongs to its endpoint slot. Query
            // only missing names so another slot cannot overwrite that value.
            let mut missing_names: Vec<_> = endpoints
                .iter()
                .filter(|ep| ep.nvos_host_name.as_ref().is_none_or(String::is_empty))
                .map(|ep| ep.nvos_mac)
                .collect();
            missing_names.sort_unstable();
            missing_names.dedup();
            let hostnames =
                db::machine_interface::find_hostnames_by_mac_addresses(&mut txn, &missing_names)
                    .await
                    .map_err(preparation_failed)?;
            let nodes = endpoints
                .iter()
                .map(|ep| {
                    self.resolve_switch_or_power_shelf_node(
                        &identities,
                        ep.bmc_mac,
                        SwitchOrPowerShelfRole::Switch,
                    )
                    .map(|resolved| {
                        let hostname = ep
                            .nvos_host_name
                            .as_ref()
                            .filter(|name| !name.is_empty())
                            .cloned()
                            .or_else(|| hostnames.get(&ep.nvos_mac).cloned());
                        build_switch_node_info(ep, &resolved, hostname)
                    })
                })
                .collect();
            txn.commit().await.map_err(preparation_failed)?;
            session.return_to_pool();
            Ok(nodes)
        };
        query(self.client.as_ref(), &macs, prepare, deadline).await
    }

    /// Checks current IP ownership before allowing expected-rack fallback.
    pub(super) async fn read_compute_firmware(
        &self,
        endpoints: &[ComputeTrayEndpoint],
    ) -> Result<Vec<ComponentFirmwareInventoryResult>, ComponentManagerError> {
        validate_endpoint_count(endpoints.len())?;
        let deadline = Instant::now() + READ_TIMEOUT;
        let macs: Vec<_> = endpoints.iter().map(|ep| ep.bmc_mac).collect();
        let prepare = async {
            // Retain the guard through setup and queries so cancellation releases
            // local pool capacity without waiting for SQLx cleanup.
            let mut session = db::DbSessionGuard::acquire(&self.db)
                .await
                .map_err(preparation_failed)?;
            let mut txn = session
                .begin_read_only_snapshot(STATEMENT_TIMEOUT)
                .await
                .map_err(preparation_failed)?;
            let ips: Vec<_> = endpoints.iter().map(|ep| ep.bmc_ip).collect();
            let ingested = resolve_compute_tray_identities(&mut txn, &ips).await?;
            let missing: Vec<_> = endpoints
                .iter()
                .filter(|ep| !ingested.rms_identities.contains_key(&ep.bmc_ip))
                .map(|ep| ep.bmc_mac)
                .collect();
            let expected =
                Self::resolve_pre_ingestion_compute_identities(&mut txn, &missing).await?;
            let nodes = endpoints
                .iter()
                .map(|ep| {
                    // Expected inventory cannot authorize credentials for an IP
                    // whose current owner has a different management MAC.
                    ingested.check_owner(ep)?;
                    let identity = ingested
                        .rms_identities
                        .get(&ep.bmc_ip)
                        .or_else(|| expected.get(&ep.bmc_mac))
                        .ok_or_else(|| RMS_IDENTITY_LOOKUP_ERROR.to_owned())?;
                    self.resolve_compute_node(identity).map(|resolved| {
                        build_compute_tray_node_info(ep, &resolved, identity.bmc_mac)
                    })
                })
                .collect();
            txn.commit().await.map_err(preparation_failed)?;
            session.return_to_pool();
            Ok(nodes)
        };
        query(self.client.as_ref(), &macs, prepare, deadline).await
    }
}

/// Rejects oversized input before endpoint vectors or preparation work exist.
fn validate_endpoint_count(count: usize) -> Result<(), ComponentManagerError> {
    if count > MAX_ENDPOINTS {
        return Err(ComponentManagerError::InvalidArgument(format!(
            "RMS firmware inventory accepts at most {MAX_ENDPOINTS} endpoints"
        )));
    }
    Ok(())
}

/// Keeps database setup and commit errors in the sanitized preparation path.
fn preparation_failed(_: db::DatabaseError) -> ComponentManagerError {
    ComponentManagerError::Internal("RMS firmware inventory database preparation failed".to_owned())
}

// All input slots share one descriptor and observation. Once a node has
// conflicting descriptors, later identical repeats cannot restore it.
struct NodeRead {
    node: rms::NodeInfo,
    slots: Vec<usize>,
    conflicting: bool,
}

/// Keeps one result per endpoint while sharing identical rack/node queries.
/// Preparation errors are deliberately sanitized at this observation boundary.
async fn query(
    client: &dyn RmsApi,
    macs: &[MacAddress],
    prepare: impl Future<Output = Result<Vec<Result<rms::NodeInfo, String>>, ComponentManagerError>>,
    deadline: Instant,
) -> Result<Vec<ComponentFirmwareInventoryResult>, ComponentManagerError> {
    validate_endpoint_count(macs.len())?;
    let mut results: Vec<_> = macs
        .iter()
        .map(|mac| ComponentFirmwareInventoryResult {
            mac_address: *mac,
            inventory: Err(FirmwareReadError::DeadlineExceeded),
        })
        .collect();
    if macs.is_empty() {
        return Ok(results);
    }
    let prepared = before_deadline(deadline, prepare).await;
    let nodes = match prepared {
        Ok(Ok(nodes)) if nodes.len() == macs.len() => nodes,
        Err(_) => {
            tracing::debug!(
                category = "preparation_deadline",
                component_count = macs.len(),
                "RMS firmware inventory preparation timed out"
            );
            return Ok(results);
        }
        failed => {
            match failed {
                Ok(Ok(nodes)) => tracing::debug!(
                    category = "preparation_length",
                    expected_count = macs.len(),
                    actual_count = nodes.len(),
                    "RMS firmware inventory preparation cardinality mismatch"
                ),
                _ => tracing::debug!(
                    category = "preparation_error",
                    component_count = macs.len(),
                    "RMS firmware inventory preparation failed"
                ),
            }
            for result in &mut results {
                result.inventory = Err(FirmwareReadError::ReadFailed);
            }
            return Ok(results);
        }
    };
    // Response groups omit rack IDs, so each RPC must contain one rack only.
    let mut racks: BTreeMap<String, BTreeMap<String, NodeRead>> = BTreeMap::new();
    for (slot, node) in nodes.into_iter().enumerate() {
        let Ok(node) = node else {
            tracing::debug!(category = "endpoint_preparation", management_mac_address = %macs[slot], "RMS firmware identity/profile preparation failed");
            results[slot].inventory = Err(FirmwareReadError::ReadFailed);
            continue;
        };
        match racks
            .entry(node.rack_id.clone())
            .or_default()
            .entry(node.node_id.clone())
        {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(NodeRead {
                    node,
                    slots: vec![slot],
                    conflicting: false,
                });
            }
            std::collections::btree_map::Entry::Occupied(mut entry) => {
                let read = entry.get_mut();
                // Once conflicting, a later repeat of the original descriptor
                // must not restore eligibility for this node.
                read.conflicting |= read.node != node;
                read.slots.push(slot);
            }
        }
    }
    for nodes in racks.values_mut() {
        nodes.retain(|_, read| {
            if read.conflicting {
                tracing::debug!(rack_id = %read.node.rack_id, node_id = %read.node.node_id,
                    "RMS firmware inventory request descriptors conflict");
                for slot in &read.slots {
                    results[*slot].inventory = Err(FirmwareReadError::ReadFailed);
                }
            }
            !read.conflicting
        });
    }
    for nodes in racks.values() {
        let nodes: Vec<_> = nodes.values().collect();
        for batch in nodes.chunks(BATCH_SIZE) {
            // An expired batch was never sent, so it must not produce an
            // outbound RED observation. Earlier completed slots remain intact.
            if Instant::now() >= deadline {
                tracing::debug!(
                    category = "unattempted_deadline",
                    node_count = batch.len(),
                    "RMS firmware inventory batch skipped after deadline"
                );
                return Ok(results);
            }
            let rack_id = &batch[0].node.rack_id;
            let request = rms::BatchGetFirmwareInventoryRequest {
                nodes: Some(rms::NodeSet {
                    nodes: batch.iter().map(|read| read.node.clone()).collect(),
                }),
            };
            let rpc = async {
                // Strip status/TLS text before RED sees it; RMS can echo
                // endpoint credentials or response bodies in those errors.
                client
                    .batch_get_firmware_inventory(request)
                    .await
                    .map_err(|error| match error {
                        RackManagerError::ApiInvocationError(status) => {
                            tracing::debug!(category = "rpc", grpc_status_code = ?status.code(), rack_id,
                                node_count = batch.len(), "RMS firmware inventory RPC failed");
                            if status.code() == tonic::Code::Unimplemented {
                                FirmwareReadError::Unsupported
                            } else {
                                FirmwareReadError::ReadFailed
                            }
                        }
                        RackManagerError::TlsError(_) => {
                            tracing::debug!(
                                category = "tls",
                                rack_id,
                                node_count = batch.len(),
                                "RMS firmware inventory TLS failed"
                            );
                            FirmwareReadError::ReadFailed
                        }
                    })
            };
            let bounded_attempt = async {
                let attempted = before_deadline(deadline, rpc).await;
                match attempted {
                    Ok(result) => result,
                    Err(error) => {
                        tracing::debug!(
                            category = "rpc_deadline",
                            rack_id,
                            node_count = batch.len(),
                            "RMS firmware inventory RPC timed out"
                        );
                        Err(error)
                    }
                }
            };
            let response =
                red::instrumented("rms", "batch_get_firmware_inventory", bounded_attempt).await;
            for read in batch {
                let inventory = match &response {
                    Ok(response) => inventory_for_node(response, &read.node.node_id),
                    Err(error) => Err(*error),
                };
                for slot in &read.slots {
                    results[*slot].inventory = inventory.clone();
                }
            }
        }
    }
    Ok(results)
}

/// Rejects evidence at or after the caller's deadline, including ready errors.
/// Tokio polls the future before its timeout, so both sides need a clock check.
async fn before_deadline<T>(
    deadline: Instant,
    future: impl Future<Output = T>,
) -> Result<T, FirmwareReadError> {
    if Instant::now() >= deadline {
        return Err(FirmwareReadError::DeadlineExceeded);
    }
    let result = timeout_at(deadline, future).await;
    if Instant::now() >= deadline {
        return Err(FirmwareReadError::DeadlineExceeded);
    }
    result.map_err(|_| FirmwareReadError::DeadlineExceeded)
}

/// Validates one node independently so malformed evidence cannot erase peers.
fn inventory_for_node(
    response: &rms::BatchGetFirmwareInventoryResponse,
    node_id: &str,
) -> Result<Vec<FirmwareInventoryEntry>, FirmwareReadError> {
    let reject = |reason| {
        tracing::debug!(
            category = "response",
            node_id,
            reason,
            "RMS firmware inventory evidence rejected"
        );
        FirmwareReadError::ReadFailed
    };
    let batch = response
        .response
        .as_ref()
        .ok_or_else(|| reject("missing_envelope"))?;
    let mut inventories = response.nodes.iter().filter(|node| node.node_id == node_id);
    let inventory = inventories.next();
    if inventories.next().is_some() {
        return Err(reject("duplicate_inventory"));
    }
    let mut operations = batch
        .node_results
        .iter()
        .filter(|node| node.node_id == node_id);
    let operation = operations.next();
    if operations.next().is_some() {
        return Err(reject("duplicate_operation"));
    }
    if let Some(operation) = operation
        && (operation.status != rms::ReturnCode::Success as i32
            || !operation.error_message.is_empty())
    {
        return Err(reject("failed_operation"));
    }
    // The schema defines `nodes` as successfully queried inventories. An
    // aggregate failure can describe another node and cannot erase this one.
    let inventory = inventory.ok_or_else(|| reject("missing_inventory"))?;
    if inventory.firmware_list.len() > MAX_MEMBERS {
        tracing::debug!(
            node_id,
            category = "response_limit",
            member_count = inventory.firmware_list.len(),
            reason = "member_count",
            "RMS firmware inventory limit exceeded"
        );
        return Err(FirmwareReadError::LimitExceeded);
    }
    let mut retained_bytes = 0;
    Ok(inventory
        .firmware_list
        .iter()
        .map(|member| {
            let fields = [&member.name, &member.version, &member.target];
            let bytes: usize = fields.iter().map(|field| field.len()).sum();
            let firmware = if fields.iter().any(|field| field.len() > MAX_FIELD_BYTES)
                || bytes > MAX_METADATA_BYTES - retained_bytes
            {
                tracing::debug!(
                    node_id,
                    category = "response_limit",
                    metadata_bytes = bytes,
                    reason = "member_metadata",
                    "RMS firmware inventory limit exceeded"
                );
                Err(FirmwareReadError::LimitExceeded)
            } else {
                retained_bytes += bytes;
                Ok(FirmwareComponent {
                    id: None,
                    resource_uri: None,
                    name: Some(member.name.clone()),
                    update_target: Some(member.target.clone()),
                    description: None,
                    version: Some(member.version.clone()),
                    release_date: None,
                })
            };
            FirmwareInventoryEntry {
                inventory_id: None,
                firmware,
            }
        })
        .collect())
}

#[cfg(test)]
mod tests;

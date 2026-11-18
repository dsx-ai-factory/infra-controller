// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Passive comparisons against caller-selected manifests, without creating RMS jobs.

use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::time::Duration;

use carbide_instrument::red;
use carbide_rack::firmware_update::{build_new_node_info, firmware_type_for_profile};
use carbide_rack::rms_node_type::{
    compute_node_identity_for_profile, firmware_object_component_filters_for_node_identities,
    power_shelf_node_identity_for_profile, switch_node_identity_for_profile,
};
use librms::RackManagerError;
use librms::protos::rack_manager as rms;
use tokio::time::{Instant, timeout_at};

use super::RmsRackFirmwareUpdateManager;
use crate::error::ComponentManagerError;
use crate::{
    DeviceFirmwareExpectations, FirmwareExpectation, FirmwareExpectationError,
    FirmwareExpectationStatus, RackFirmwareExpectationsRequest,
};

// Keep one comparison RPC active per call and cap the entire diagnostic read
// at 30 seconds, including SDK connection setup and retries. Batches never mix
// roles, so 256 slots can require six calls. QA must assess full-rack latency;
// this ceiling bounds caller work without promising every device completes.
const MAX_ENDPOINTS: usize = 256;
const BATCH_SIZE: usize = 64;
const READ_TIMEOUT: Duration = Duration::from_secs(30);
// Bound manifest copies independently of a deployment's transport limits.
const MAX_MANIFEST_BYTES: usize = 16 * 1024 * 1024;
const MAX_FILTERS: usize = 256;
const MAX_DESCRIPTOR_ATTRIBUTES: usize = 256;
const MAX_ROWS: usize = 256;
// Permit extra malformed evidence without rejecting a full 64-node reply for
// one duplicate. Beyond this cap, reject the batch before repeated node scans.
// This bounds post-decode work, not allocations made by the SDK decoder.
const MAX_RESPONSE_ENTRIES: usize = 256;
const MAX_FIELD_BYTES: usize = 1024;
const MAX_METADATA_BYTES: usize = 64 * 1024;

/// One distinct caller identity and every result slot that requested it.
struct NodeRead {
    node: Option<rms::NodeInfo>,
    role: usize,
    slots: Vec<usize>,
    conflicting: bool,
}

impl RmsRackFirmwareUpdateManager {
    /// Keeps descriptor preparation, role batches and projection within one deadline.
    pub(super) async fn read_firmware_expectations(
        &self,
        request: RackFirmwareExpectationsRequest<'_>,
    ) -> Result<Vec<DeviceFirmwareExpectations>, ComponentManagerError> {
        let deadline = Instant::now() + READ_TIMEOUT;
        validate_request(&request)?;
        let roles = [
            (
                "compute",
                request.machines,
                compute_node_identity_for_profile(request.profile),
            ),
            (
                "switch",
                request.switches,
                switch_node_identity_for_profile(request.profile),
            ),
            (
                "power_shelf",
                request.power_shelves,
                power_shelf_node_identity_for_profile(request.profile),
            ),
        ];
        let mut results = Vec::new();
        let mut nodes = BTreeMap::<&str, NodeRead>::new();
        // Resolve every descriptor before dispatch: a conflicting repeat in a
        // later batch must be found before sending credentials for that ID.
        for (role, (role_name, devices, identity)) in roles.iter().enumerate() {
            if !devices.is_empty() && identity.is_err() {
                tracing::debug!(
                    category = "profile",
                    role = *role_name,
                    "RMS firmware comparison profile identity unavailable"
                );
            }
            for device in *devices {
                let slot = results.len();
                results.push(DeviceFirmwareExpectations {
                    node_id: device.node_id.clone(),
                    expectations: Err(FirmwareExpectationError::DeadlineExceeded),
                });
                let node = identity
                    .as_ref()
                    .ok()
                    .map(|identity| build_new_node_info(request.rack_id, device, identity));
                match nodes.entry(&device.node_id) {
                    Entry::Vacant(entry) => {
                        entry.insert(NodeRead {
                            node,
                            role,
                            slots: vec![slot],
                            conflicting: false,
                        });
                    }
                    Entry::Occupied(mut entry) => {
                        let read = entry.get_mut();
                        read.conflicting |= read.role != role || read.node != node;
                        read.slots.push(slot);
                    }
                }
            }
        }
        for read in nodes
            .values()
            .filter(|read| read.conflicting || read.node.is_none())
        {
            if read.conflicting {
                tracing::debug!(
                    category = "descriptor_conflict",
                    "RMS firmware comparison request descriptors conflict"
                );
            }
            for slot in &read.slots {
                results[*slot].expectations = Err(FirmwareExpectationError::ReadFailed);
            }
        }

        // A role-specific descriptor/filter rejection must not prevent another
        // role from returning evidence. Each reply identifies nodes by ID only.
        for (role, (role_name, _, identity)) in roles.iter().enumerate() {
            let Ok(identity) = identity else { continue };
            let (component_filters, node_descriptor_component_filters) =
                firmware_object_component_filters_for_node_identities(
                    request.components,
                    [identity],
                );
            let reads: Vec<_> = nodes
                .values()
                .filter_map(|read| {
                    if read.role != role || read.conflicting {
                        return None;
                    }
                    read.node.as_ref().map(|node| (read, node))
                })
                .collect();
            for batch in reads.chunks(BATCH_SIZE) {
                // Skipped batches have no outbound observation. Completed
                // peers keep their evidence when a later batch times out.
                if Instant::now() >= deadline {
                    tracing::debug!(
                        category = "unattempted_deadline",
                        role = *role_name,
                        "RMS firmware comparison batch skipped after deadline"
                    );
                    return Ok(results);
                }
                let command = rms::CompareFirmwareObjectRequest {
                    config_json: request.config_json.to_owned(),
                    firmware_type: firmware_type_for_profile(request.profile).to_owned(),
                    nodes: Some(rms::NodeSet {
                        nodes: batch
                            .iter()
                            .map(|(_, node)| rms::NodeInfo::clone(node))
                            .collect(),
                    }),
                    component_filters: component_filters.clone(),
                    node_descriptor_component_filters: node_descriptor_component_filters.clone(),
                };
                if Instant::now() >= deadline {
                    return Ok(results);
                }
                let attempt = async {
                    // RED logs errors, so strip SDK diagnostic text before it
                    // can see an echoed credential, manifest or response body.
                    let rpc = async {
                        self.client
                            .compare_firmware_object(command)
                            .await
                            .map_err(|error| match error {
                                RackManagerError::ApiInvocationError(status) => {
                                    tracing::debug!(category = "rpc", grpc_status_code = ?status.code(),
                                        role = *role_name, node_count = batch.len(),
                                        "RMS firmware comparison RPC failed");
                                    if status.code() == tonic::Code::Unimplemented {
                                        FirmwareExpectationError::Unsupported
                                    } else {
                                        FirmwareExpectationError::ReadFailed
                                    }
                                }
                                RackManagerError::TlsError(_) => {
                                    tracing::debug!(category = "tls", role = *role_name,
                                        node_count = batch.len(), "RMS firmware comparison TLS failed");
                                    FirmwareExpectationError::ReadFailed
                                }
                            })
                    };
                    let response = timeout_at(deadline, rpc).await;
                    // Tokio polls a ready future before its timer. Evidence
                    // completed at the deadline is unavailable even if ready.
                    if Instant::now() >= deadline {
                        tracing::debug!(
                            category = "rpc_deadline",
                            role = *role_name,
                            "RMS firmware comparison RPC exceeded the shared deadline"
                        );
                        return Err(FirmwareExpectationError::DeadlineExceeded);
                    }
                    response.unwrap_or(Err(FirmwareExpectationError::DeadlineExceeded))
                };
                let response = red::instrumented("rms", "compare_firmware_object", attempt).await;
                for (read, node) in batch {
                    if Instant::now() >= deadline {
                        return Ok(results);
                    }
                    let expectations = match &response {
                        Ok(response) => expectations_for_node(response, &node.node_id),
                        Err(error) => Err(*error),
                    };
                    if Instant::now() >= deadline {
                        return Ok(results);
                    }
                    for slot in &read.slots {
                        results[*slot].expectations = expectations.clone();
                    }
                }
            }
        }
        Ok(results)
    }
}

/// Admits counts, text sizes and node IDs before descriptor or manifest copies.
fn validate_request(
    request: &RackFirmwareExpectationsRequest<'_>,
) -> Result<(), ComponentManagerError> {
    let invalid = |field| {
        ComponentManagerError::InvalidArgument(format!(
            "firmware comparison request {field} violates application limits"
        ))
    };
    let count = request
        .machines
        .len()
        .saturating_add(request.switches.len())
        .saturating_add(request.power_shelves.len());
    if count > MAX_ENDPOINTS {
        return Err(invalid("endpoint count"));
    }
    if request.config_json.len() > MAX_MANIFEST_BYTES {
        return Err(invalid("manifest size"));
    }
    if request.rack_id.as_str().len() > MAX_FIELD_BYTES {
        return Err(invalid("rack ID size"));
    }
    if request.components.len() > MAX_FILTERS {
        return Err(invalid("filter count"));
    }
    let mut filter_bytes = 0;
    for component in request.components {
        filter_bytes += component.len();
        if component.len() > MAX_FIELD_BYTES || filter_bytes > MAX_METADATA_BYTES {
            return Err(invalid("filter text size"));
        }
    }
    let profile = request.profile;
    let capabilities = &profile.rack_capabilities;
    // Size violations reject the whole input before preparation. Missing role
    // identity is different: it adds no work and only makes that role unavailable.
    let mut attribute_bytes = 0;
    for attributes in [
        &profile.attributes,
        &capabilities.compute.attributes,
        &capabilities.switch.attributes,
        &capabilities.power_shelf.attributes,
    ] {
        if attributes.len() > MAX_DESCRIPTOR_ATTRIBUTES {
            return Err(invalid("descriptor attribute count"));
        }
        for (key, value) in attributes {
            if key.len() > MAX_FIELD_BYTES || value.len() > MAX_FIELD_BYTES {
                return Err(invalid("descriptor attribute field size"));
            }
            attribute_bytes += key.len() + value.len();
            if attribute_bytes > MAX_METADATA_BYTES {
                return Err(invalid("descriptor attribute text size"));
            }
        }
    }
    for vendor in [
        &capabilities.compute.vendor,
        &capabilities.switch.vendor,
        &capabilities.power_shelf.vendor,
    ]
    .into_iter()
    .flatten()
    {
        if vendor.len() > MAX_FIELD_BYTES {
            return Err(invalid("vendor size"));
        }
    }
    if profile
        .product_family
        .as_ref()
        .is_some_and(|family| family.as_str().len() > MAX_FIELD_BYTES)
    {
        return Err(invalid("product family size"));
    }
    for device in request
        .machines
        .iter()
        .chain(request.switches)
        .chain(request.power_shelves)
    {
        let fields = [
            device.node_id.as_str(),
            device.mac.as_str(),
            device.bmc_ip.as_str(),
            device.bmc_username.as_str(),
            device.bmc_password.as_str(),
            device.os_mac.as_deref().unwrap_or_default(),
            device.os_ip.as_deref().unwrap_or_default(),
            device.os_username.as_deref().unwrap_or_default(),
            device.os_password.as_deref().unwrap_or_default(),
            device.os_hostname.as_deref().unwrap_or_default(),
        ];
        if device.node_id.is_empty() {
            return Err(invalid("empty node ID"));
        }
        if fields.iter().any(|field| field.len() > MAX_FIELD_BYTES) {
            return Err(invalid("endpoint field size"));
        }
    }
    Ok(())
}

/// Requires unique `nodes` evidence and successful `node_results` evidence.
/// An aggregate failure can describe another node and cannot erase this one.
fn expectations_for_node(
    response: &rms::CompareFirmwareObjectResponse,
    node_id: &str,
) -> Result<Vec<FirmwareExpectation>, FirmwareExpectationError> {
    use FirmwareExpectationError::{LimitExceeded, ReadFailed};

    let reject = |reason: &'static str| {
        tracing::debug!(
            category = "response",
            reason,
            "RMS firmware comparison evidence rejected"
        );
        ReadFailed
    };
    let limit = |reason: &'static str| {
        tracing::debug!(
            category = "response_limit",
            reason,
            "RMS firmware comparison limit exceeded"
        );
        LimitExceeded
    };
    let batch = response
        .response
        .as_ref()
        .ok_or_else(|| reject("missing_envelope"))?;
    if response.nodes.len() > MAX_RESPONSE_ENTRIES
        || batch.node_results.len() > MAX_RESPONSE_ENTRIES
    {
        return Err(limit("batch_records"));
    }
    let mut matches = response.nodes.iter().filter(|node| node.node_id == node_id);
    let node = matches.next().ok_or_else(|| reject("missing_comparison"))?;
    if matches.next().is_some() {
        return Err(reject("duplicate_comparison"));
    }
    let mut operations = batch
        .node_results
        .iter()
        .filter(|node| node.node_id == node_id);
    // The RMS comparison handler emits a result for each requested node. Unlike
    // inventory payloads alone, a comparison needs that explicit success evidence.
    let operation = operations
        .next()
        .ok_or_else(|| reject("missing_operation"))?;
    if operations.next().is_some() {
        return Err(reject("duplicate_operation"));
    }
    if operation.status != rms::ReturnCode::Success as i32 || !operation.error_message.is_empty() {
        return Err(reject("failed_operation"));
    }
    if node.components.len() > MAX_ROWS {
        return Err(limit("row_count"));
    }
    let mut retained_bytes = 0;
    let mut rows = Vec::with_capacity(node.components.len());
    for row in &node.components {
        // Only these protocol metadata fields enter the result. In particular,
        // `reason`, `installed_component` and `installed_version` can contain
        // diagnostics or grouped physical instances; they do not identify an
        // expected image.
        let fields = [
            &row.component,
            &row.proposed_version,
            &row.sku_id,
            &row.target,
        ];
        if fields.iter().any(|field| field.len() > MAX_FIELD_BYTES) {
            return Err(limit("field_bytes"));
        }
        retained_bytes += fields.iter().map(|field| field.len()).sum::<usize>();
        if retained_bytes > MAX_METADATA_BYTES {
            return Err(limit("metadata_bytes"));
        }
        let present = |field: &String| (!field.is_empty()).then(|| field.clone());
        rows.push(FirmwareExpectation {
            component: present(&row.component),
            proposed_version: present(&row.proposed_version),
            sku_id: present(&row.sku_id),
            reported_target: present(&row.target),
            status: match rms::FirmwareComparisonStatus::try_from(row.status) {
                Ok(rms::FirmwareComparisonStatus::Unspecified) => {
                    FirmwareExpectationStatus::Unspecified
                }
                Ok(rms::FirmwareComparisonStatus::Matched) => FirmwareExpectationStatus::Matched,
                Ok(rms::FirmwareComparisonStatus::Mismatched) => {
                    FirmwareExpectationStatus::Mismatched
                }
                Ok(rms::FirmwareComparisonStatus::Missing) => FirmwareExpectationStatus::Missing,
                Ok(rms::FirmwareComparisonStatus::Unverified) => {
                    FirmwareExpectationStatus::Unverified
                }
                Err(_) => FirmwareExpectationStatus::Unknown(row.status),
            },
        });
    }
    Ok(rows)
}

#[cfg(test)]
mod tests;

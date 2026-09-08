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
use std::net::IpAddr;
use std::str::FromStr;

use ::rpc::errors::RpcDataConversionError;
use ::rpc::forge::{self as rpc, AdminForceDeleteMachineResponse};
use ::rpc::model::RpcTryFrom;
use carbide_redfish::libredfish::RedfishAuth;
use carbide_secrets::credentials::{BmcCredentialType, CredentialKey};
use carbide_uuid::infiniband::IBPartitionId;
use carbide_uuid::instance::InstanceId;
use carbide_uuid::machine::MachineId;
use carbide_uuid::network::NetworkSegmentId;
use carbide_uuid::vpc::{VpcId, VpcPrefixId};
use db::{DatabaseError, ObjectColumnFilter, WithTransaction, network_security_group};
use futures_util::FutureExt;
use health_report::{
    HealthAlertClassification, HealthProbeAlert, HealthProbeId, HealthReport, HealthReportApplyMode,
};
use itertools::Itertools as _;
use model::ConfigValidationError;
use model::dpa_interface::DpaSearchConfig;
use model::instance::config::InstanceConfig;
use model::instance::config::extension_services::InstanceExtensionServicesConfig;
use model::instance::config::infiniband::InstanceInfinibandConfig;
use model::instance::config::network::InstanceNetworkConfig;
use model::instance::config::nvlink::InstanceNvLinkConfig;
use model::instance::config::spx::InstanceSpxConfig;
use model::instance::config::tenant_config::TenantConfig;
use model::instance::snapshot::InstanceSnapshot;
use model::machine::machine_search_config::MachineSearchConfig;
use model::machine::{
    HostHealthConfig, InstanceState, LoadSnapshotOptions, ManagedHostState,
    ManagedHostStateSnapshot,
};
use model::metadata::Metadata;
use model::network_segment::{NetworkSegmentSearchConfig, NetworkSegmentType};
use model::os::OperatingSystem;
use model::tenant::TenantOrganizationId;
use model::vpc::{FabricInterfaceType, VpcVirtualizationTypeCapabilities};
use serde_json::json;
use sqlx::PgConnection;
use tonic::{Request, Response, Status};
use tracing::Instrument;

use crate::api::{Api, log_machine_id, log_request_data, log_tenant_organization_id};
use crate::cfg::file::CarbideConfig;
use crate::ethernet_virtualization::validate_instance_interface_routing_profiles;
use crate::handlers::utils::convert_and_log_machine_id;
use crate::instance::{
    InstanceAllocationRequest, allocate_ib_port_guid, allocate_instance, allocate_network,
    allocate_spx_port_mac, ib_memberships_from_config, load_extension_services,
    load_ib_partition_pkeys, validate_ib_partition_ownership, validate_instance_extension_services,
    validate_instance_vfs_against_dpf_topology, validate_os_definition_usable,
    validate_spx_partition_ownership,
};
use crate::{CarbideError, CarbideResult};

/// Refuses `ReleaseInstance` when aggregate host health includes [`HealthAlertClassification::prevent_instance_deletion`].
/// Admin `force_delete_instance` is not subject to this check.
async fn ensure_instance_release_not_blocked_by_prevent_instance_deletion(
    txn: &mut db::Transaction<'_>,
    machine_id: &MachineId,
    host_health: HostHealthConfig,
) -> Result<(), CarbideError> {
    let Some(snapshot) = db::managed_host::load_snapshot(
        txn,
        machine_id,
        LoadSnapshotOptions::default().with_host_health(host_health),
    )
    .await?
    else {
        return Err(CarbideError::NotFoundError {
            kind: "machine",
            id: machine_id.to_string(),
        });
    };

    if snapshot
        .aggregate_health
        .has_classification(&HealthAlertClassification::prevent_instance_deletion())
    {
        return Err(ConfigValidationError::InstanceReleaseBlockedByPreventInstanceDeletion.into());
    }

    Ok(())
}

/// Represents the repair status label value set by RepairSystem
///
/// This enum is used to parse the `repair_status` label from machine metadata.
/// All string comparisons are case-insensitive to provide a more robust API.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RepairStatus {
    /// Repair was completed successfully
    Completed,
    /// Repair failed or encountered errors
    Failed,
    /// Repair is currently in progress
    InProgress,
    /// Any other status value not explicitly handled
    Unknown,
}

impl RepairStatus {
    /// Parse a repair status string with case-insensitive matching
    ///
    /// Supports multiple formats for flexibility:
    /// - `"completed"`, `"COMPLETED"`, etc.
    /// - `"failed"`, `"FAILED"`, etc.
    /// - `"inprogress"`, `"in_progress"`, `"in-progress"` (case-insensitive)
    ///
    /// Returns `RepairStatus::Unknown` for unrecognized values.
    fn parse(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            "inprogress" | "in_progress" | "in-progress" => Self::InProgress,
            _ => Self::Unknown,
        }
    }
}

impl FromStr for RepairStatus {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self::parse(s))
    }
}

pub(crate) async fn allocate(
    api: &Api,
    request: Request<rpc::InstanceAllocationRequest>,
) -> Result<Response<rpc::Instance>, Status> {
    log_request_data(&request);
    let mut request = request.into_inner();

    if request.needs_hydration_from_deprecated_fields() {
        api.with_txn(|txn| request.hydrate_from_deprecated_fields(txn).boxed())
            .await??;
    }

    let request = InstanceAllocationRequest::try_from(request)?;

    log_machine_id(&request.machine_id);
    log_tenant_organization_id(request.config.tenant.tenant_organization_id.as_str());

    // Row-locking on Machine records happens in allocate_instance
    let mh_snapshot = allocate_instance(api, request, api.runtime_config.host_health).await?;

    Ok(Response::new(snapshot_to_instance(mh_snapshot)?))
}

pub(crate) async fn batch_allocate(
    api: &Api,
    request: Request<rpc::BatchInstanceAllocationRequest>,
) -> Result<Response<rpc::BatchInstanceAllocationResponse>, Status> {
    log_request_data(&request);

    let mut batch_request = request.into_inner();

    if batch_request.instance_requests.is_empty() {
        return Err(CarbideError::InvalidArgument(
            "batch request must contain at least one instance".to_string(),
        )
        .into());
    }

    tracing::info!(
        instance_request_count = batch_request.instance_requests.len(),
        "Received batch instance allocation request"
    );

    if batch_request.needs_hydration_from_deprecated_fields() {
        api.with_txn(|txn| batch_request.hydrate_from_deprecated_fields(txn).boxed())
            .await??;
    }

    // Convert all requests
    let requests = batch_request
        .instance_requests
        .into_iter()
        .map(InstanceAllocationRequest::try_from)
        .collect::<Result<Vec<_>, _>>()
        .inspect_err(|e| {
            tracing::error!(error = ?e, "Failed to convert batch request");
        })?;

    // Log all machine IDs and tenant organization IDs
    for request in &requests {
        log_machine_id(&request.machine_id);
        log_tenant_organization_id(request.config.tenant.tenant_organization_id.as_str());
    }

    // Call batch allocation logic
    let snapshots =
        crate::instance::batch_allocate_instances(api, requests, api.runtime_config.host_health)
            .await
            .inspect_err(|e| {
                tracing::error!(error = %e, "Batch instance allocation failed");
            })?;

    // Convert all snapshots to Instance responses
    let instances = snapshots
        .into_iter()
        .map(snapshot_to_instance)
        .collect::<Result<Vec<_>, _>>()
        .inspect_err(|e| {
            tracing::error!(error = ?e, "Failed to convert snapshots to instances");
        })?;

    tracing::info!(
        instance_count = instances.len(),
        "Successfully allocated batch of instances"
    );

    Ok(Response::new(rpc::BatchInstanceAllocationResponse {
        instances,
    }))
}

pub(crate) async fn find_ids(
    api: &Api,
    request: Request<rpc::InstanceSearchFilter>,
) -> Result<Response<rpc::InstanceIdList>, Status> {
    log_request_data(&request);

    let filter: model::instance::InstanceSearchFilter = request.into_inner().into();

    let instance_ids = db::instance::find_ids(&api.database_connection, filter).await?;

    Ok(tonic::Response::new(rpc::InstanceIdList { instance_ids }))
}

pub(crate) async fn find_by_ids(
    api: &Api,
    request: Request<rpc::InstancesByIdsRequest>,
) -> Result<Response<rpc::InstanceList>, Status> {
    log_request_data(&request);

    let instance_ids = request.into_inner().instance_ids;

    let max_find_by_ids = api.runtime_config.max_find_by_ids as usize;
    if instance_ids.len() > max_find_by_ids {
        return Err(CarbideError::InvalidArgument(format!(
            "no more than {max_find_by_ids} IDs can be accepted"
        ))
        .into());
    } else if instance_ids.is_empty() {
        return Err(
            CarbideError::InvalidArgument("at least one ID must be provided".to_string()).into(),
        );
    }

    let mut txn = api.txn_begin().await?;

    let snapshots = db::managed_host::load_by_instance_ids(
        &mut txn,
        instance_ids.as_ref(),
        LoadSnapshotOptions::default().with_host_health(api.runtime_config.host_health),
    )
    .await?;
    let mut instances = Vec::with_capacity(snapshots.len());
    for snapshot in snapshots.into_iter() {
        instances.push(snapshot_to_instance(snapshot)?);
    }
    txn.rollback_or_log("read-only load of instances by id")
        .await;

    Ok(Response::new(rpc::InstanceList { instances }))
}

pub(crate) async fn find_by_machine_id(
    api: &Api,
    request: Request<MachineId>,
) -> Result<Response<rpc::InstanceList>, Status> {
    log_request_data(&request);

    let machine_id = convert_and_log_machine_id::<MachineId>(Some(&request.into_inner()))?;

    let mut txn = api.txn_begin().await?;

    let mh_snapshot = match db::managed_host::load_snapshot(
        &mut txn,
        &machine_id,
        LoadSnapshotOptions::default().with_host_health(api.runtime_config.host_health),
    )
    .await
    {
        Ok(Some(snapshot)) => snapshot,
        Ok(None) => return Ok(Response::new(rpc::InstanceList { instances: vec![] })),
        Err(e) => return Err(CarbideError::from(e).into()),
    };

    let maybe_instance =
        Option::<rpc::Instance>::rpc_try_from(mh_snapshot).map_err(CarbideError::from)?;

    let instances = if let Some(instance) = maybe_instance {
        vec![instance]
    } else {
        vec![]
    };

    let response = Response::new(rpc::InstanceList { instances });

    txn.commit().await?;

    Ok(response)
}

/// Creates a TenantReportedIssue health override template with issue details
fn create_tenant_reported_issue_override(
    issue: &rpc::Issue,
    tenant_organization_id: &str,
) -> HealthReport {
    HealthReport {
        source: "tenant-reported-issue".to_string(),
        observed_at: Some(chrono::Utc::now()),
        alerts: vec![HealthProbeAlert {
            id: HealthProbeId::from_str("TenantReportedIssue")
                .expect("TenantReportedIssue is a valid non-empty HealthProbeId"),
            target: Some("tenant-reported".to_string()),
            message: json!({
                "issue_category": format!("{:?}", rpc::IssueCategory::try_from(issue.category).unwrap_or(rpc::IssueCategory::Unspecified)),
                "summary": issue.summary,
                "details": issue.details,
                "tenant_organization_id": tenant_organization_id
            }).to_string(),
            tenant_message: Some(format!("TenantReportedIssue: {}", issue.summary)),
            classifications: vec![
                HealthAlertClassification::prevent_allocations(),
                HealthAlertClassification::suppress_external_alerting(),
            ],
            in_alert_since: None,
        }],
        ..Default::default()
    }
}

/// Creates a RequestRepair health override template
fn create_request_repair_override(issue: &rpc::Issue) -> HealthReport {
    HealthReport {
        source: health_report::REPAIR_REQUEST_MERGE_SOURCE.to_string(),
        observed_at: Some(chrono::Utc::now()),
        alerts: vec![HealthProbeAlert {
            id: HealthProbeId::from_str("RequestRepair")
                .expect("RequestRepair is a valid non-empty HealthProbeId"),
            target: Some("repair-requested".to_string()),
            message: json!({
                "issue_category": format!("{:?}", rpc::IssueCategory::try_from(issue.category).unwrap_or(rpc::IssueCategory::Unspecified)),
                "summary": issue.summary,
                "details": issue.details
            }).to_string(),
            tenant_message: Some(format!(
                "RepairSystem: Node ready for repair - {}",
                issue.summary
            )),
            classifications: vec![
                HealthAlertClassification::prevent_allocations(),
                HealthAlertClassification::suppress_external_alerting(),
            ],
            in_alert_since: None,
        }],
        ..Default::default()
    }
}

/// Helper function to apply health override with consistent error handling
async fn apply_health_override(
    txn: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    machine_id: &MachineId,
    override_report: &HealthReport,
    operation_desc: &str,
) -> Result<(), CarbideError> {
    db::machine::insert_health_report(
        txn,
        machine_id,
        HealthReportApplyMode::Merge,
        override_report,
        false,
    )
    .await
    .map_err(|e| {
        tracing::error!(
            machine_id = %machine_id,
            error = ?e,
            operation = %operation_desc,
            "Failed to apply health override"
        );
        CarbideError::from(e)
    })?;

    tracing::info!(
        machine_id = %machine_id,
        operation = %operation_desc,
        "Successfully applied health override"
    );
    Ok(())
}

/// Helper function to remove health override with consistent error handling
async fn remove_health_override(
    txn: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    machine_id: &MachineId,
    source: &str,
    operation_desc: &str,
) -> Result<(), CarbideError> {
    db::machine::remove_health_report(txn, machine_id, HealthReportApplyMode::Merge, source)
        .await
        .map_err(|e| {
            tracing::error!(
                machine_id = %machine_id,
                error = ?e,
                operation = %operation_desc,
                "Failed to remove health override"
            );
            CarbideError::from(e)
        })?;

    tracing::info!(
        machine_id = %machine_id,
        operation = %operation_desc,
        "Successfully removed health override"
    );
    Ok(())
}

/// Logs cloud-side delete attribution when present on the release request.
fn log_delete_attribution(delete_attribution: Option<&rpc::DeleteAttribution>) {
    let Some(attribution) = delete_attribution else {
        return;
    };
    let Some(initiated_by) = attribution.initiated_by.as_ref() else {
        return;
    };

    tracing::info!(
        organization = %initiated_by.org,
        organization_display_name = %initiated_by.org_display_name,
        user_id = %initiated_by.user_id,
        tenant_id = %initiated_by.tenant_id,
        "Instance delete attribution"
    );
}

/// Handles the Instance Release workflow when released from the Repair tenant.
///
/// This function implements the logic for when the RepairSystem releases an instance after
/// attempting repairs. It manages the transition from RequestRepair overrides to appropriate
/// final states based on repair outcomes.
///
/// ## Workflow Logic
///
/// ### No RequestRepair Override Present
/// - If issues are reported: Applies TenantReportedIssue (no auto-repair to prevent cycles)
/// - If no issues: No action taken (machine is healthy)
///
/// ### RequestRepair Override Present
/// - Checks machine metadata for `repair_status` label
/// - **Repair Completed**: Removes RequestRepair, handles any new issues without auto-repair
/// - **Repair Incomplete**: Remove RequestRepair override and replace with TenantReportedIssue
///   to transition back to manual intervention by the Forge team.
///
/// ## Infinite Cycle Prevention
/// The function specifically avoids triggering auto-repair (RequestRepair) for repair tenant
/// releases to prevent infinite loops where RepairSystem triggers itself repeatedly.
async fn handle_instance_release_from_repair_tenant(
    txn: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    machine_id: &MachineId,
    issue: Option<&rpc::Issue>,
    machine: &model::machine::Machine,
    tenant_organization_id: &str,
) -> Result<(), CarbideError> {
    let has_request_repair = machine
        .health_reports
        .merges
        .contains_key(health_report::REPAIR_REQUEST_MERGE_SOURCE);

    if !has_request_repair {
        // No existing RequestRepair override
        if let Some(issue) = issue {
            tracing::info!(
                machine_id = %machine_id,
                issue_category = ?issue.category,
                issue_summary = %issue.summary,
                "Repair tenant reports issues on machine without RequestRepair override"
            );

            let override_report =
                create_tenant_reported_issue_override(issue, tenant_organization_id);
            apply_health_override(
                txn,
                machine_id,
                &override_report,
                "TenantReportedIssue for repair tenant issues (no auto-repair to prevent cycles)",
            )
            .await?;
        } else {
            tracing::info!(
                machine_id = %machine_id,
                "Repair tenant release on machine without RequestRepair override and no issues - no action needed"
            );
        }
        return Ok(());
    }

    // Machine has RequestRepair override - check repair status
    let repair_status_str = machine.metadata.labels.get("repair_status");
    let repair_status = repair_status_str.map(|s| RepairStatus::parse(s.as_str()));

    tracing::info!(
        machine_id = %machine_id,
        repair_status = ?repair_status,
        repair_status_raw = ?repair_status_str,
        "Processing repair tenant release with repair status"
    );

    if repair_status == Some(RepairStatus::Completed) {
        // Repair completed successfully - Good to remove the RequestRepair override.
        remove_health_override(
            txn,
            machine_id,
            health_report::REPAIR_REQUEST_MERGE_SOURCE,
            "RequestRepair removed - repair completed successfully",
        )
        .await?;

        // Repair completed successfully - Apply TenantReportedIssue override if new
        // issues reported, else remove it to make machine available in ready pool.
        // Skipped applying RequestRepair override to avoid infinite loops.
        if let Some(issue) = issue {
            tracing::info!(
                machine_id = %machine_id,
                issue_category = ?issue.category,
                issue_summary = %issue.summary,
                "Repair tenant reports new issues after repair completion"
            );

            let override_report =
                create_tenant_reported_issue_override(issue, tenant_organization_id);
            apply_health_override(
                txn,
                machine_id,
                &override_report,
                "TenantReportedIssue updated for repair tenant (no auto-repair to prevent cycles)",
            )
            .await?;
        } else {
            // No new issues - Remove TenantReportedIssue override to make machine available in ready pool
            remove_health_override(
                txn,
                machine_id,
                "tenant-reported-issue",
                "TenantReportedIssue removed - repair completed successfully",
            )
            .await?;
        }
    } else {
        // Repair failed - Remove the existing RequestRepair override and set TenantReportedIssue
        // to send the machine for Forge team intervention, preventing auto-repair loops.
        remove_health_override(
            txn,
            machine_id,
            health_report::REPAIR_REQUEST_MERGE_SOURCE,
            "RequestRepair removed for incomplete repair",
        )
        .await?;

        // Determine which issue to use
        let issue_to_apply = if let Some(issue) = issue {
            tracing::info!(
                machine_id = %machine_id,
                issue_category = ?issue.category,
                issue_summary = %issue.summary,
                "Using tenant-provided issue details for incomplete repair"
            );
            issue.clone()
        } else {
            tracing::info!(
                machine_id = %machine_id,
                repair_status = ?repair_status,
                "Creating fallback issue for incomplete repair"
            );
            rpc::Issue {
                category: rpc::IssueCategory::Other as i32,
                summary: "RepairSystem processing incomplete".to_string(),
                details: format!(
                    "Machine released by repair tenant but repair status is: {}",
                    repair_status_str.map_or("Unknown", |s| s.as_str())
                ),
            }
        };

        let override_report =
            create_tenant_reported_issue_override(&issue_to_apply, tenant_organization_id);
        apply_health_override(
            txn,
            machine_id,
            &override_report,
            "TenantReportedIssue for incomplete repair (no auto-repair to prevent cycles)",
        )
        .await?;
    }

    Ok(())
}

/// Handles regular (non-repair) tenant workflow when releasing instances with reported issues.
///
/// This function implements the standard workflow for normal tenant instance releases that
/// include issue reports. It applies appropriate health overrides and conditionally triggers
/// auto-repair based on global configuration.
///
/// ## Workflow Steps
/// 1. **Always applies TenantReportedIssue**: Documents the reported issue in health overrides
/// 2. **Conditionally applies RequestRepair**: Only if auto-repair is enabled globally
///
/// ## Auto-Repair Logic
/// - **Enabled**: Applies both TenantReportedIssue and RequestRepair overrides
/// - **Disabled**: Applies only TenantReportedIssue override
///
/// The RequestRepair override signals the RepairSystem to attempt automated repairs
/// on the machine before it can be allocated to new instances.
async fn handle_instance_release_from_regular_tenant_and_report_issue(
    txn: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    machine_id: &MachineId,
    issue: &rpc::Issue,
    auto_repair_enabled: bool,
    tenant_organization_id: &str,
) -> Result<(), CarbideError> {
    tracing::info!(
        machine_id = %machine_id,
        issue_category = ?issue.category,
        issue_summary = %issue.summary,
        "Regular tenant reported issue during instance release"
    );

    // Apply TenantReportedIssue health override
    let tenant_override = create_tenant_reported_issue_override(issue, tenant_organization_id);
    apply_health_override(
        txn,
        machine_id,
        &tenant_override,
        "TenantReportedIssue for regular tenant",
    )
    .await?;

    // Apply RequestRepair if auto-repair is enabled
    if auto_repair_enabled {
        let repair_override = create_request_repair_override(issue);
        apply_health_override(
            txn,
            machine_id,
            &repair_override,
            "RequestRepair for regular tenant (auto-repair enabled)",
        )
        .await?;
    } else {
        tracing::info!(
            machine_id = %machine_id,
            "Auto-repair disabled - only applied TenantReportedIssue for regular tenant"
        );
    }

    Ok(())
}

/// Handles instance release requests with support for the Forge-RepairSystem integration.
///
/// This function processes instance deletion requests and applies appropriate health overrides
/// based on the requesting tenant type and reported issues. It supports two main workflows
/// (repair tenant and regular tenant) described below.
///
/// **PreventInstanceDeletion:** If aggregate host health includes a [`HealthAlertClassification::prevent_instance_deletion`]
/// alert, `ReleaseInstance` is rejected until the alert is cleared (only when the instance is not already
/// marked deleted). Admin machine force-delete does not use this check.
///
/// **Already deleted:** If the instance row is already marked deleted, this handler still runs repair-tenant
/// (and regular-tenant issue) health logic so the repair system can clear or update overrides, then returns
/// success without calling `mark_as_deleted` again.
///
/// **Membership serialization:** The release transaction locks and rereads the
/// authoritative `Instance`, then records its exact IB memberships in the same
/// transaction that marks the live `Instance` for deletion. An already-deleted
/// retry does not resolve or record memberships.
///
/// ## Repair Tenant Workflow
/// When `is_repair_tenant=true`, this indicates the RepairSystem is releasing an instance after
/// attempting repairs. The function:
/// - Checks for existing RequestRepair overrides on the machine
/// - Examines the repair status from machine metadata labels
/// - Removes RequestRepair overrides and applies TenantReportedIssue based on repair outcome
/// - Prevents infinite repair cycles by not triggering auto-repair for repair tenant releases
///
/// ## Regular Tenant Workflow
/// For normal tenant releases with reported issues:
/// - Always applies TenantReportedIssue health override to document the problem
/// - Conditionally applies RequestRepair override if auto-repair is enabled globally
/// - Respects the auto_machine_repair_plugin configuration setting
///
/// ## Health Override Sources
/// - `tenant-reported-issue`: Applied for all tenant-reported issues
/// - `repair-request`: Applied when auto-repair is enabled (regular tenants only)
pub(crate) async fn release(
    api: &Api,
    request: Request<rpc::InstanceReleaseRequest>,
) -> Result<Response<rpc::InstanceReleaseResult>, Status> {
    log_request_data(&request);
    let delete_instance = request.into_inner();
    release_one_instance(api, delete_instance).await?;
    Ok(Response::new(rpc::InstanceReleaseResult {}))
}

/// Releases multiple instances in one call. Each instance is released in its
/// own transaction via [`release_one_instance`] -- the exact same logic and
/// failure modes as calling `ReleaseInstance` once per instance -- so the only
/// thing this RPC saves versus a client-side loop is the client-server round
/// trips, not any change in per-instance behavior or transaction semantics.
///
/// Deliberately best-effort, not all-or-nothing: one instance failing (already
/// released, blocked by a health check, not found, etc.) does not roll back or
/// block the rest of the batch. A live 4,500-instance scale test found that
/// aborting an entire batch on the first per-instance failure stranded
/// thousands of instances with no way to resume (see admin-cli's
/// release_batch_with_retry, crates/admin-cli/src/instance/release/cmd.rs)
/// -- an all-or-nothing RPC-level transaction would reintroduce that same
/// failure mode one layer down, so each instance's outcome is independent and
/// reported individually instead.
pub(crate) async fn batch_release(
    api: &Api,
    request: Request<rpc::BatchInstanceReleaseRequest>,
) -> Result<Response<rpc::BatchInstanceReleaseResponse>, Status> {
    log_request_data(&request);
    let batch = request.into_inner();

    let mut results = Vec::with_capacity(batch.release_requests.len());

    for release_request in batch.release_requests {
        let Some(instance_id) = release_request.id else {
            // No id means there is nothing to attempt and nothing to key a
            // success on, but the caller still needs to see this entry
            // accounted for -- report it as a failed result with no id
            // rather than silently dropping it, so response counts always
            // reconcile against the request count.
            tracing::warn!("Batch release entry with no instance id reported as a failure");
            results.push(rpc::InstanceReleaseOutcome {
                id: None,
                status: rpc::InstanceReleaseStatusCode::InvalidArgument as i32,
                error: "release request is missing an instance id".to_string(),
            });
            continue;
        };
        // Per-instance span, not just a %instance_id log field: log_machine_id/
        // log_tenant_organization_id record onto Span::current(), and without a
        // dedicated span per iteration every call in this loop would record onto
        // the same batch-wide ReleaseInstances span -- last-write-wins, so the
        // completed span would only ever reflect the final instance's machine and
        // tenant, losing per-instance audit attribution for a destructive
        // fleet-scale operation. Field names must match what those helpers record.
        let instance_span = tracing::info_span!(
            "release_one_instance",
            instance_id = %instance_id,
            forge.machine_id = tracing::field::Empty,
            tenant.organization_id = tracing::field::Empty,
        );
        let outcome = match release_one_instance(api, release_request)
            .instrument(instance_span)
            .await
        {
            Ok(()) => rpc::InstanceReleaseOutcome {
                id: Some(instance_id),
                status: rpc::InstanceReleaseStatusCode::Success as i32,
                error: String::new(),
            },
            Err(status) => rpc::InstanceReleaseOutcome {
                id: Some(instance_id),
                status: instance_release_status_code_for(&status) as i32,
                error: status.message().to_string(),
            },
        };
        results.push(outcome);
    }

    Ok(Response::new(rpc::BatchInstanceReleaseResponse { results }))
}

/// Maps a per-instance release failure's gRPC status code onto the batch
/// response's stable [`rpc::InstanceReleaseStatusCode`], so callers can
/// distinguish retryable outcomes (e.g. `Unavailable`, `ResourceExhausted`)
/// from terminal ones (e.g. `NotFound`) without string-matching `error`.
fn instance_release_status_code_for(status: &Status) -> rpc::InstanceReleaseStatusCode {
    use rpc::InstanceReleaseStatusCode as Code;
    match status.code() {
        tonic::Code::InvalidArgument => Code::InvalidArgument,
        tonic::Code::NotFound => Code::NotFound,
        tonic::Code::FailedPrecondition => Code::FailedPrecondition,
        tonic::Code::ResourceExhausted => Code::ResourceExhausted,
        tonic::Code::PermissionDenied => Code::PermissionDenied,
        tonic::Code::Unavailable => Code::Unavailable,
        _ => Code::InternalError,
    }
}

/// Core single-instance release logic, shared by [`release`] and
/// [`batch_release`]. Owns its own transaction (begin through commit) so each
/// instance in a batch is fully independent -- a failure partway through one
/// instance's release never poisons or rolls back another instance's work.
async fn release_one_instance(
    api: &Api,
    delete_instance: rpc::InstanceReleaseRequest,
) -> Result<(), Status> {
    let instance_id = delete_instance
        .id
        .ok_or(RpcDataConversionError::MissingArgument("id"))?;

    let mut txn = api.txn_begin().await?;

    // Take the Instance lock before any optional Machine health update. IB
    // config changes use the same Instance-before-Machine order, and a
    // controller DELETE must acquire this Instance record before proceeding.
    let instance = db::instance::find_by_id_for_update(txn.as_mut(), instance_id)
        .await?
        .ok_or_else(|| CarbideError::NotFoundError {
            kind: "instance",
            id: instance_id.to_string(),
        })?;
    let machine_id = instance.machine_id;
    let instance_is_live = instance.deleted.is_none();
    let is_repair_tenant = delete_instance.is_repair_tenant == Some(true);

    log_machine_id(&machine_id);
    log_tenant_organization_id(instance.tenant_organization_id.as_str());
    log_delete_attribution(delete_instance.delete_attribution.as_ref());

    // Only enforce PreventInstanceDeletion for a real release. Repair tenant
    // follow-up calls after deletion may still adjust health below.
    if instance_is_live {
        ensure_instance_release_not_blocked_by_prevent_instance_deletion(
            &mut txn,
            &machine_id,
            api.runtime_config.host_health,
        )
        .await?;
    }

    // Instance Release called from the Repair tenant.
    if is_repair_tenant {
        tracing::info!(
            %instance_id,
            %machine_id,
            has_issues = delete_instance.issue.is_some(),
            "Instance release requested by repair tenant"
        );

        // Get machine details for repair tenant workflow
        let machine = db::machine::find_one(
            &mut txn,
            &machine_id,
            MachineSearchConfig {
                for_update: false,
                ..Default::default()
            },
        )
        .await?
        .ok_or_else(|| CarbideError::NotFoundError {
            kind: "machine",
            id: machine_id.to_string(),
        })?;

        // Handle repair tenant workflow
        handle_instance_release_from_repair_tenant(
            &mut txn,
            &machine_id,
            delete_instance.issue.as_ref(),
            &machine,
            instance.tenant_organization_id.as_str(),
        )
        .await
        .map_err(|e| CarbideError::Internal {
            message: e.to_string(),
        })?;
    } else if let Some(issue) = &delete_instance.issue {
        // Instance Release called from the regular tenant (not the Repair tenant) and has an issue to report.
        let auto_repair_enabled = api.runtime_config.auto_machine_repair_plugin.enabled;

        handle_instance_release_from_regular_tenant_and_report_issue(
            &mut txn,
            &machine_id,
            issue,
            auto_repair_enabled,
            instance.tenant_organization_id.as_str(),
        )
        .await
        .map_err(|e| CarbideError::Internal {
            message: e.to_string(),
        })?;
    }

    if !instance_is_live {
        tracing::info!(
            %instance_id,
            "Instance is already marked for deletion.",
        );
        txn.commit().await?;
        return Ok(());
    }

    let pkeys = load_ib_partition_pkeys(txn.as_mut(), &[&instance.infiniband_config]).await?;
    let memberships = ib_memberships_from_config(&instance.infiniband_config, &pkeys);
    for membership in memberships {
        db::retired_ib_membership::record(txn.as_mut(), &membership).await?;
    }
    db::instance::mark_as_deleted(instance_id, &mut txn).await?;

    txn.commit().await?;

    Ok(())
}

pub(crate) async fn update_phone_home_last_contact(
    api: &Api,
    request: Request<rpc::InstancePhoneHomeLastContactRequest>,
) -> Result<Response<rpc::InstancePhoneHomeLastContactResponse>, Status> {
    log_request_data(&request);

    // Extract the calling machine's ID from the SPIFFE certificate before consuming the request.
    // When bypass_rbac is enabled (integration tests), the cert check is skipped: test tooling
    // uses a generic TLS client cert that carries no SPIFFE identity.
    let caller_machine_id = if api.runtime_config.bypass_rbac {
        None
    } else {
        let id_str = request
            .extensions()
            .get::<crate::auth::AuthContext>()
            .and_then(|ctx| ctx.get_spiffe_machine_id())
            .ok_or_else(|| {
                CarbideError::ClientCertificateMissingInformation(
                    "phone-home request must include a valid machine SPIFFE certificate".into(),
                )
            })?;
        Some(MachineId::from_str(id_str).map_err(|_| {
            CarbideError::ClientCertificateMissingInformation(
                "machine ID in SPIFFE certificate is invalid".into(),
            )
        })?)
    };

    let request = request.into_inner();
    let instance_id = request
        .instance_id
        .ok_or(CarbideError::MissingArgument("id"))?;

    let mut txn = api.txn_begin().await?;

    let instance = db::instance::find_by_id(&mut txn, instance_id)
        .await?
        .ok_or_else(|| {
            // Return PermissionDenied (not NotFound) so callers cannot probe instance existence
            // by observing which error they receive.
            match &caller_machine_id {
                Some(mid) => CarbideError::PermissionDeniedError(format!(
                    "machine {mid} is not authorized to update phone-home for instance {instance_id}"
                )),
                None => CarbideError::NotFoundError {
                    kind: "instance",
                    id: instance_id.to_string(),
                },
            }
        })?;

    log_machine_id(&instance.machine_id);
    log_tenant_organization_id(instance.config.tenant.tenant_organization_id.as_str());

    // Verify the caller is authorized: either the instance's host machine itself, or a DPU
    // attached to that host. Phone-home calls originate from the DPU agent on the host.
    // Skipped when bypass_rbac is enabled (caller_machine_id is None).
    if let Some(ref caller_machine_id) = caller_machine_id {
        let caller_is_host = *caller_machine_id == instance.machine_id;
        let caller_is_attached_dpu = if caller_is_host {
            false
        } else {
            db::machine::find_host_by_dpu_machine_id(
                &mut txn,
                &carbide_uuid::machine::DpuMachineId::try_from(*caller_machine_id)
                    .map_err(|error| CarbideError::InvalidArgument(error.to_string()))?,
            )
            .await?
            .is_some_and(|host| host.id == instance.machine_id)
        };

        if !caller_is_host && !caller_is_attached_dpu {
            tracing::warn!(
                %caller_machine_id,
                instance_machine_id = %instance.machine_id,
                %instance_id,
                "phone-home denied: caller is not the instance host or an attached DPU",
            );
            return Err(CarbideError::PermissionDeniedError(format!(
                "machine {caller_machine_id} is not authorized to update phone-home for instance {instance_id}"
            ))
            .into());
        }
    }

    let res = db::instance::update_phone_home_last_contact(&mut txn, instance.id).await?;

    txn.commit().await?;

    Ok(Response::new(rpc::InstancePhoneHomeLastContactResponse {
        timestamp: Some(res.into()),
    }))
}

pub(crate) async fn invoke_power(
    api: &Api,
    request: Request<rpc::InstancePowerRequest>,
) -> Result<Response<rpc::InstancePowerResult>, Status> {
    log_request_data(&request);

    let mut txn = api.txn_begin().await?;

    let request = request.into_inner();
    let instance_id = request
        .instance_id
        .ok_or(CarbideError::MissingArgument("instance_id"))?;
    let snapshot = db::managed_host::load_by_instance_ids(
        &mut txn,
        &[instance_id],
        LoadSnapshotOptions::default().with_host_health(api.runtime_config.host_health),
    )
    .await?
    .pop()
    .ok_or(CarbideError::NotFoundError {
        kind: "instance",
        id: instance_id.to_string(),
    })?;
    let machine_id = snapshot.host_snapshot.id;
    log_machine_id(&machine_id);

    // Log tenant organization ID
    if let Some(ref instance) = snapshot.instance {
        log_tenant_organization_id(instance.config.tenant.tenant_organization_id.as_str());
    }

    let bmc_ip = snapshot
        .host_snapshot
        .status
        .bmc_info
        .ip
        .as_ref()
        .ok_or_else(|| CarbideError::NotFoundError {
            kind: "bmc_ip",
            id: machine_id.to_string(),
        })?;

    let run_provisioning_instructions_on_every_boot = snapshot
        .instance
        .as_ref()
        .map(|instance| {
            instance
                .config
                .os
                .run_provisioning_instructions_on_every_boot
        })
        .unwrap_or_default();

    // Determine if we should let the state machine handle the reboot.
    // This is true when:
    // 1. The machine is in Assigned/Ready state, AND
    // 2. The instance needs to PXE boot (either always-PXE or explicit request)
    //
    // In this case, we set the custom_pxe_reboot_requested flag and return early.
    // The Assigned/Ready handler will pick up the flag, verify boot order via
    // HostPlatformConfiguration, and initiate the reboot.
    //
    // Otherwise, we issue a synchronous Redfish reboot directly.
    // Determine how to handle the reboot based on the instance configuration and request.
    //
    // For custom PXE or always-PXE instances in Ready state, we use the state machine to
    // verify boot order before rebooting. For regular reboots, we clear the use_custom_pxe_on_boot
    // flag so the iPXE handler returns "exit" (boot from disk).
    let use_state_machine_for_reboot = matches!(
        snapshot.managed_state,
        ManagedHostState::Assigned {
            instance_state: InstanceState::Ready,
        }
    ) && (run_provisioning_instructions_on_every_boot
        || request.boot_with_custom_ipxe);

    if use_state_machine_for_reboot {
        db::instance::set_custom_pxe_reboot_requested(&machine_id, true, &mut txn).await?;
    }

    if request.boot_with_custom_ipxe
        && let Some(instance) = snapshot.instance.as_ref()
        && instance.config.os.phone_home_enabled
    {
        db::instance::clear_phone_home_last_contact(&mut txn, instance.id).await?;
    }

    // For non-always-PXE instances, set use_custom_pxe_on_boot based on the request.
    // This tells the iPXE handler whether to serve the custom script or return "exit".
    // If we are using the state machine for a custom pxe reboot, let the state machine set the use_custom_ipxe_on_next_boot flag.
    if !use_state_machine_for_reboot && !run_provisioning_instructions_on_every_boot {
        db::instance::use_custom_ipxe_on_next_boot(
            &machine_id,
            request.boot_with_custom_ipxe,
            &mut txn,
        )
        .await?;
    }

    // Check if reprovision is requested.
    // TODO: multidpu: Fix it for multiple dpus.
    let mut reprovision_handled = false;
    if request.apply_updates_on_reboot {
        for dpu_snapshot in &snapshot.dpu_snapshots {
            let Some(rr) = &dpu_snapshot.reprovision_requested else {
                continue;
            };

            if rr.started_at.is_some() {
                return Err(CarbideError::DpuReprovisioningInProgress(format!(
                    "can't reboot host: {machine_id}"
                ))
                .into());
            }

            reprovision_handled = true;

            // This will trigger DPU reprovisioning/update via state machine.
            db::machine::approve_dpu_reprovision_request(&dpu_snapshot.id, &mut txn)
                .await
                .map_err(|err| {
                    // print actual error for debugging, but don't leak internal info to user.
                    tracing::error!(
                        machine_id = %machine_id,
                        error = ?err,
                        "failed to approve DPU reprovision request",
                    );

                    // TODO: What does this error actually mean
                    CarbideError::internal(
                        "internal failure. try again after some time".to_string(),
                    )
                })?;
        }
        if snapshot.host_snapshot.host_reprovision_requested.is_some() {
            reprovision_handled = true;

            db::machine::approve_host_reprovision_request(&snapshot.host_snapshot.id, &mut txn)
                .await
                .map_err(|err| {
                    // print actual error for debugging, but don't leak internal info to user.
                    tracing::error!(
                        machine_id = %machine_id,
                        error = ?err,
                        "failed to approve host reprovision request",
                    );

                    CarbideError::internal(
                        "internal failure. try again after some time".to_string(),
                    )
                })?;
        }
    }

    txn.commit().await?;

    if reprovision_handled {
        // Host will reboot once DPU reprovisioning is successfully finished.
        return Ok(Response::new(rpc::InstancePowerResult {}));
    }

    // If using state machine for reboot, return early. The flag was set above and the
    // Assigned/Ready handler will pick it up, verify BIOS boot order, and initiate the reboot.
    if use_state_machine_for_reboot {
        tracing::info!(
            machine_id = %machine_id,
            run_provisioning_instructions_on_every_boot,
            boot_with_custom_ipxe = request.boot_with_custom_ipxe,
            "Delegating reboot to state machine for boot order verification"
        );
        return Ok(Response::new(rpc::InstancePowerResult {}));
    }

    let bmc_mac_address =
        snapshot
            .host_snapshot
            .status
            .bmc_info
            .mac
            .ok_or_else(|| CarbideError::NotFoundError {
                kind: "bmc_mac",
                id: machine_id.to_string(),
            })?;

    // TODO: The API call should maybe not directly trigger the reboot
    // but instead queue it for the state handler. That will avoid racing
    // with other internal reboot requests from the state handler.
    let bmc_ip = bmc_ip.to_string();
    let client = api
        .redfish_pool
        .create_client(
            &bmc_ip,
            snapshot.host_snapshot.status.bmc_info.port,
            RedfishAuth::Key(CredentialKey::BmcCredentials {
                credential_type: BmcCredentialType::BmcRoot { bmc_mac_address },
            }),
            None,
        )
        .await
        .map_err(|e| CarbideError::internal(e.to_string()))?;

    client
        .power(libredfish::SystemPowerControl::ForceRestart)
        .await
        .map_err(|e| CarbideError::internal(format!("failed redfish ForceRestart subtask: {e}")))?;

    Ok(Response::new(rpc::InstancePowerResult {}))
}

pub(crate) async fn update_operating_system(
    api: &Api,
    request: Request<rpc::InstanceOperatingSystemUpdateRequest>,
) -> Result<Response<rpc::Instance>, Status> {
    log_request_data(&request);

    let request = request.into_inner();
    let instance_id = request
        .instance_id
        .ok_or(CarbideError::MissingArgument("id"))?;

    let os: OperatingSystem = match request.os {
        None => return Err(CarbideError::MissingArgument("os").into()),
        Some(os) => os.try_into().map_err(CarbideError::from)?,
    };
    os.validate().map_err(CarbideError::from)?;

    let mut txn = api.txn_begin().await?;

    validate_os_definition_usable(&mut txn, &os).await?;

    let instance = db::instance::find_by_id(&mut txn, instance_id)
        .await?
        .ok_or(CarbideError::NotFoundError {
            kind: "instance",
            id: instance_id.to_string(),
        })?;

    log_machine_id(&instance.machine_id);
    log_tenant_organization_id(instance.config.tenant.tenant_organization_id.as_str());

    if instance.deleted.is_some() {
        return Err(CarbideError::InvalidArgument(
            "configuration for a terminating instance can not be changed".to_string(),
        )
        .into());
    }

    let expected_version = match request.if_version_match {
        Some(version) => version.parse().map_err(CarbideError::from)?,
        None => instance.config_version,
    };

    db::instance::update_os(&mut txn, instance.id, expected_version, os).await?;

    let mh_snapshot = db::managed_host::load_snapshot(
        &mut txn,
        &instance.machine_id,
        LoadSnapshotOptions::default().with_host_health(api.runtime_config.host_health),
    )
    .await?
    .ok_or(CarbideError::NotFoundError {
        kind: "instance",
        id: instance_id.to_string(),
    })?;
    let instance = snapshot_to_instance(mh_snapshot)?;

    txn.commit().await?;

    Ok(Response::new(instance))
}

pub(crate) async fn update_instance_config(
    api: &Api,
    request: tonic::Request<rpc::InstanceConfigUpdateRequest>,
) -> Result<tonic::Response<rpc::Instance>, Status> {
    log_request_data(&request);

    let request = request.into_inner();

    let instance_id = request
        .instance_id
        .ok_or(CarbideError::MissingArgument("id"))?;

    // RPC conversion intentionally ignores the deprecated boolean. Remember the exact legacy
    // wire form so the stored auto config can be restored after the instance lookup.
    #[allow(deprecated)]
    let uses_deprecated_auto_without_config = request
        .config
        .as_ref()
        .and_then(|config| config.network.as_ref())
        .is_some_and(|network| {
            network.auto && network.auto_config.is_none() && network.interfaces.is_empty()
        });

    let mut config: InstanceConfig = match request.config {
        None => return Err(CarbideError::MissingArgument("config").into()),
        Some(config) => config.try_into().map_err(CarbideError::from)?,
    };

    tracing::info!(
        spx_config = ?config.spxconfig,
        "Updating instance SPX config",
    );

    // Network validation is done only if network update is requested.
    config
        .validate(
            false,
            api.runtime_config
                .vmaas_config
                .as_ref()
                .map(|vc| vc.allow_instance_vf)
                .unwrap_or(true),
        )
        .map_err(CarbideError::from)?;

    // TODO: Should a missing metadata field
    // - be an error
    // - lead to writing empty metadata (same as initial instance creation will do)
    // - keep existing metadata
    let metadata: Metadata = match request.metadata {
        None => return Err(CarbideError::MissingArgument("metadata").into()),
        Some(metadata) => metadata.try_into().map_err(CarbideError::from)?,
    };
    metadata.validate(true).map_err(|e| {
        CarbideError::InvalidArgument(format!("instance metadata is not valid: {e}"))
    })?;
    let mut txn = api.txn_begin().await?;

    let (machine_id, initial_config_version) = {
        // Capture the Instance before any IB lock wait. If another request
        // updates it while this request waits, this version remains the
        // implicit optimistic token rather than silently rebasing.
        let request_start_instance = db::instance::find_by_id(&mut txn, instance_id)
            .await?
            .ok_or(CarbideError::NotFoundError {
                kind: "instance",
                id: instance_id.to_string(),
            })?;
        (
            request_start_instance.machine_id,
            request_start_instance.config_version,
        )
    };

    let mut mh_snapshot = db::managed_host::load_snapshot(
        &mut txn,
        &machine_id,
        LoadSnapshotOptions::default().with_host_health(api.runtime_config.host_health),
    )
    .await?
    .ok_or(CarbideError::NotFoundError {
        kind: "machine",
        id: machine_id.to_string(),
    })?;
    // We assign `initial_instance` from this first snapshot as the baseline for
    // request validation and resource updates. An IB change later locks the
    // Instance and Machine, reloads the snapshot, and uses the refreshed
    // `instance` below.
    let initial_instance = mh_snapshot
        .instance
        .as_ref()
        .filter(|instance| instance.id == instance_id)
        .ok_or(CarbideError::NotFoundError {
            kind: "instance",
            id: instance_id.to_string(),
        })?;

    log_machine_id(&initial_instance.machine_id);
    log_tenant_organization_id(
        initial_instance
            .config
            .tenant
            .tenant_organization_id
            .as_str(),
    );

    if initial_instance.deleted.is_some() {
        return Err(CarbideError::InvalidArgument(
            "configuration for a terminating instance can not be changed".to_string(),
        )
        .into());
    }

    let ib_config_update_requested = initial_instance
        .config
        .infiniband
        .is_ib_config_update_requested(&config.infiniband);

    // power_profile was added to the complete config update request after the
    // API was deployed. Preserve the stored value when older clients omit it;
    // an explicit empty string is the wire representation for clearing it.
    config.power_profile = match config.power_profile.take() {
        Some(profile) if profile.is_empty() => None,
        Some(profile) => Some(profile),
        None => initial_instance.config.power_profile.clone(),
    };

    if uses_deprecated_auto_without_config {
        let Some(auto_config) = initial_instance.config.network.auto_config else {
            return Err(CarbideError::InvalidArgument(
                "cannot enable automatic networking on an existing instance through deprecated `InstanceNetworkConfig.auto`"
                    .to_string(),
            )
            .into());
        };
        config.network.auto_config = Some(auto_config);
    }

    // Check whether the update is allowed
    initial_instance
        .config
        .verify_update_allowed_to(&config)
        .map_err(CarbideError::from)?;

    validate_os_definition_usable(&mut txn, &config.os).await?;

    let expected_version = match request.if_version_match {
        Some(version) => version.parse().map_err(CarbideError::from)?,
        None => initial_config_version,
    };

    // If an NSG is applied, we need to do a little more validation.
    if let InstanceConfig {
        network_security_group_id: Some(ref nsg_id),
        tenant:
            TenantConfig {
                tenant_organization_id: ref tenant_org,
                ..
            },
        ..
    } = config
    {
        // Query to check the validity of the NSG ID but to also grab
        // a row-level lock on it if it exists.
        if network_security_group::find_by_ids(
            &mut txn,
            std::slice::from_ref(nsg_id),
            Some(tenant_org),
            true,
        )
        .await?
        .pop()
        .is_none()
        {
            return Err(CarbideError::FailedPrecondition(format!(
                "NetworkSecurityGroup `{}` does not exist or is not owned by tenant `{}`",
                nsg_id,
                tenant_org.clone(),
            ))
            .into());
        }
    }

    update_instance_extension_services_config(
        &mh_snapshot,
        initial_instance,
        &config.extension_services,
        &mut txn,
    )
    .await?;

    update_instance_network_config(
        &api.runtime_config,
        initial_instance,
        &mut config.network,
        &mh_snapshot,
        &mut txn,
    )
    .await?;

    // Complete config requests acquire shared resource locks and perform any
    // network change above. An IB change then locks and rereads the Instance
    // (or confirms the lock taken by that network change), followed by the
    // Machine lock used by the IB monitor and force-delete. Reread after both
    // locks so no IB or later specialized config mutation uses stale state.
    if ib_config_update_requested {
        let locked_instance = db::instance::find_by_id_for_update(txn.as_mut(), instance_id)
            .await?
            .ok_or(CarbideError::NotFoundError {
                kind: "instance",
                id: instance_id.to_string(),
            })?;

        if locked_instance.deleted.is_some() {
            return Err(CarbideError::InvalidArgument(
                "configuration for a terminating instance can not be changed".to_string(),
            )
            .into());
        }

        let machine_id = locked_instance.machine_id;
        if db::machine::find_one(
            &mut txn,
            &machine_id,
            MachineSearchConfig {
                for_update: true,
                ..Default::default()
            },
        )
        .await?
        .is_none()
        {
            return Err(CarbideError::NotFoundError {
                kind: "machine",
                id: machine_id.to_string(),
            }
            .into());
        }

        mh_snapshot = db::managed_host::load_snapshot(
            &mut txn,
            &machine_id,
            LoadSnapshotOptions::default().with_host_health(api.runtime_config.host_health),
        )
        .await?
        .ok_or(CarbideError::NotFoundError {
            kind: "machine",
            id: machine_id.to_string(),
        })?;
    }

    // Use the Instance from the latest managed-host snapshot for the remaining
    // updates. For an IB change, this is the authoritative reread after locking
    // both the Instance and Machine records.
    let instance = mh_snapshot
        .instance
        .as_ref()
        .filter(|instance| instance.id == instance_id)
        .ok_or(CarbideError::NotFoundError {
            kind: "instance",
            id: instance_id.to_string(),
        })?;

    // Checks if the instance IB configuration was updated
    // If yes - assign devices (GUIDs) to the new configuration, update
    // the database and increment the IB version number
    update_instance_infiniband_config(&mh_snapshot, instance, &mut config.infiniband, &mut txn)
        .await?;

    tracing::debug!(
        instance_id = %instance.id,
        nvlink = ?config.nvlink,
        "Updating instance NVLink configuration",
    );
    update_instance_nvlink_config(&mh_snapshot, instance, &config.nvlink, &mut txn).await?;

    tracing::debug!(
        instance_id = %instance.id,
        spx_config = ?config.spxconfig,
        "Updating instance SPX configuration",
    );
    update_instance_spx_config(&mh_snapshot, instance, &mut config.spxconfig, &mut txn).await?;

    db::instance::update_config(&mut txn, instance.id, expected_version, config, metadata).await?;

    let mh_snapshot = db::managed_host::load_snapshot(
        &mut txn,
        &instance.machine_id,
        LoadSnapshotOptions::default().with_host_health(api.runtime_config.host_health),
    )
    .await?
    .ok_or(CarbideError::NotFoundError {
        kind: "instance",
        id: instance_id.to_string(),
    })?;
    let instance = snapshot_to_instance(mh_snapshot)?;

    txn.commit().await?;

    Ok(Response::new(instance))
}

/// This function checks if network config update is requested and update db to initiate the
/// process.
///
/// If it is requested, validate if update is allowed or not. If update is allowed, copy existing
/// resources to avoid re-allocation, allocate resources for new interfaces and update the db to
/// indicate the state machine to start updating network on DPUs. This function also increments
/// network_config_version.
async fn update_instance_network_config(
    runtime_config: &CarbideConfig,
    instance: &InstanceSnapshot,
    network: &mut InstanceNetworkConfig,
    mh_snapshot: &ManagedHostStateSnapshot,
    txn: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> Result<(), CarbideError> {
    if instance.update_network_config_request.is_some() {
        return Err(ConfigValidationError::InstanceNetworkConfigUpdateAlreadyInProgress.into());
    }

    // Auto-ness can't change for an existing instance. If a tenant has created
    // an instance with auto, it must remain auto until it is released. Maybe
    // eventually this can change, but for now this is what we support.
    if network.auto_config != instance.config.network.auto_config {
        return Err(CarbideError::InvalidArgument(format!(
            "cannot change `InstanceNetworkConfig.auto_config` on an existing instance (was {:?}, update requested {:?})",
            instance.config.network.auto_config, network.auto_config,
        )));
    }

    // For auto instances, simply re-resolve from the machine's current
    // HostInband segments before any diff check. Same machine state == no-op
    // via the diff check below. Operator-added or removed HostInband segments
    // since allocation == reflected in the update.
    if let Some(requested_auto_config) = network.auto_config {
        if let Some(current_auto_config) = instance.config.network.auto_config
            && current_auto_config.vpc_id != requested_auto_config.vpc_id
        {
            return Err(CarbideError::InvalidArgument(format!(
                "cannot change `InstanceNetworkConfig.vpc_id` on an existing auto-networked instance (was {}, update requested {})",
                current_auto_config.vpc_id, requested_auto_config.vpc_id
            )));
        }

        // Just to make sure, an auto instance should still be on a zero-DPU
        // host. If this machine suddenly has DPUs, we should yell, at least
        // for now. I guess there's a world where we might have a primary NIC
        // that is a dumb NIC (or DPU in NIC mode), and also happens to have
        // a secondary DPU into some other leg of the network, but we can
        // think about that later; that would mean we'd support a mix of
        // auto AND non-auto interfaces.
        if mh_snapshot.has_managed_dpus() {
            return Err(CarbideError::InvalidArgument(format!(
                "instance was allocated with `auto: true` but host {} is no longer zero-DPU; cannot update via the auto path",
                instance.machine_id,
            )));
        }

        let inband_segment_ids =
            db::instance_network_config::batch_get_inband_segments_by_machine_ids(
                txn.as_mut(),
                &[instance.machine_id],
            )
            .await?
            .get(&instance.machine_id)
            .cloned()
            .unwrap_or_default();
        validate_auto_inband_segment_vpc_bindings(
            txn,
            mh_snapshot,
            &inband_segment_ids,
            requested_auto_config.vpc_id,
        )
        .await?;
        *network = db::instance_network_config::add_inband_interfaces_to_config(
            network.clone(),
            &inband_segment_ids,
        )?;
    }

    if !instance
        .config
        .network
        .is_network_config_update_requested(network)
    {
        return Ok(());
    }

    if !matches!(
        mh_snapshot.host_snapshot.current_state(),
        ManagedHostState::Assigned {
            instance_state: InstanceState::Ready,
        }
    ) {
        return Err(ConfigValidationError::InvalidState.into());
    }

    if instance.deleted.is_some() {
        return Err(ConfigValidationError::InstanceDeletionIsRequested.into());
    }

    // Preserve caller intent long enough to enforce prefix family and VPC allocation policy.
    // Resource reuse below deliberately restores stored requested addresses for matching explicit
    // prefixes, which must not erase a newly requested address before validation.
    validate_primary_address_update_intent_before_resource_reuse(
        network,
        &instance.config.tenant.tenant_organization_id,
        txn,
    )
    .await?;

    // This is the use case of adding/removing new VF.
    // Copy the resources if same interface and network are mentioned.
    network.copy_existing_resources(&instance.config.network);

    // Resolve resources associated with network prefixes before validating the generated
    // segment IDs.
    allocate_network(network, &instance.config.tenant.tenant_organization_id, txn).await?;
    network
        .validate(
            runtime_config
                .vmaas_config
                .as_ref()
                .map(|config| config.allow_instance_vf)
                .unwrap_or(true),
        )
        .map_err(CarbideError::from)?;
    validate_instance_vfs_against_dpf_topology(network, runtime_config)?;
    validate_instance_interface_routing_profiles(txn, network, runtime_config.fnn.as_ref()).await?;

    // Allocate IPs and add them to the network config
    let updated_network_config = db::instance_network_config::with_allocated_ips(
        network.clone(),
        txn,
        instance.id,
        &mh_snapshot.host_snapshot,
    )
    .await?;

    // Update network config in db.
    db::instance::trigger_update_network_config_request(
        &instance.id,
        &instance.config.network,
        &updated_network_config,
        txn,
    )
    .await?;

    Ok(())
}

/// Validates explicit primary update intent before stored resources can replace it.
///
/// Resource reuse intentionally restores the stored primary requested address before normal
/// allocation validation. Validate that caller intent here so it cannot be erased. An IPv6
/// sidecar participates in resource identity, so a changed sidecar reaches the canonical
/// allocation validator without this earlier check.
async fn validate_primary_address_update_intent_before_resource_reuse(
    network: &InstanceNetworkConfig,
    tenant_organization_id: &TenantOrganizationId,
    txn: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> Result<(), CarbideError> {
    let requested_prefixes: Vec<(VpcPrefixId, IpAddr)> = network
        .interfaces
        .iter()
        .filter_map(|interface| {
            interface.requested_ip_addr.and_then(|address| {
                match interface.network_details.as_ref() {
                    Some(model::instance::config::network::NetworkDetails::VpcPrefixId(
                        vpc_prefix_id,
                    )) => Some((*vpc_prefix_id, address)),
                    _ => None,
                }
            })
        })
        .collect();

    if requested_prefixes.is_empty() {
        return Ok(());
    }

    // Load the same persisted prefix and VPC policy used by allocation. Missing or deleted
    // selections remain the allocation path's responsibility so it retains its canonical errors.
    let mut vpc_prefix_ids = requested_prefixes
        .iter()
        .map(|(vpc_prefix_id, _)| *vpc_prefix_id)
        .collect_vec();
    vpc_prefix_ids.sort_unstable();
    vpc_prefix_ids.dedup();
    let prefixes = db::vpc_prefix::get_for_allocation_by_ids(txn.as_mut(), &vpc_prefix_ids)
        .await?
        .into_iter()
        .map(|prefix| (prefix.id, prefix))
        .collect::<HashMap<_, _>>();

    let mut vpc_ids = prefixes
        .values()
        .filter(|prefix| !prefix.is_marked_as_deleted())
        .map(|prefix| prefix.vpc_id)
        .collect_vec();
    vpc_ids.sort_unstable();
    vpc_ids.dedup();
    let vpcs = if vpc_ids.is_empty() {
        HashMap::new()
    } else {
        db::vpc::find_by(
            txn.as_mut(),
            ObjectColumnFilter::List(db::vpc::IdColumn, &vpc_ids),
        )
        .await?
        .into_iter()
        .map(|vpc| (vpc.id, vpc))
        .collect::<HashMap<_, _>>()
    };

    for (vpc_prefix_id, requested_ip_addr) in requested_prefixes {
        let Some(prefix) = prefixes
            .get(&vpc_prefix_id)
            .filter(|prefix| !prefix.is_marked_as_deleted())
        else {
            continue;
        };
        let Some(vpc) = vpcs.get(&prefix.vpc_id) else {
            continue;
        };
        if vpc.config.tenant_organization_id != tenant_organization_id.to_string() {
            return Err(CarbideError::FailedPrecondition(format!(
                "VPC prefix `{vpc_prefix_id}` belongs to VPC `{}`, which is not owned by tenant `{tenant_organization_id}`",
                vpc.id,
            )));
        }
        if prefix.config.prefix.is_ipv6() != requested_ip_addr.is_ipv6() {
            return Err(CarbideError::InvalidArgument(format!(
                "requested IP address `{requested_ip_addr}` does not match VPC prefix `{vpc_prefix_id}`",
            )));
        }
        if prefix.config.prefix.is_ipv6() && vpc.config.slaac_enabled {
            return Err(CarbideError::InvalidArgument(format!(
                "requested IPv6 address `{requested_ip_addr}` is invalid because VPC `{}` has SLAAC enabled",
                vpc.id,
            )));
        }
    }

    Ok(())
}

async fn validate_auto_inband_segment_vpc_bindings(
    txn: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    mh_snapshot: &ManagedHostStateSnapshot,
    inband_segment_ids: &[NetworkSegmentId],
    requested_vpc_id: VpcId,
) -> Result<(), CarbideError> {
    for segment_id in inband_segment_ids {
        let Some(vpc) = db::vpc::find_by_segment(txn.as_mut(), *segment_id).await? else {
            continue;
        };

        if vpc.id != requested_vpc_id {
            return Err(CarbideError::FailedPrecondition(format!(
                "zero-DPU host {} has HostInband segment {} bound to VPC {}, but auto networking requested VPC {}; shared flat segments must be left unbound",
                mh_snapshot.host_snapshot.id, segment_id, vpc.id, requested_vpc_id,
            )));
        }

        let vpc_iface = vpc
            .config
            .network_virtualization_type
            .fabric_interface_type();
        if vpc_iface != FabricInterfaceType::Nic {
            return Err(CarbideError::FailedPrecondition(format!(
                "zero-DPU host {} has HostInband segment {} bound to VPC {} ({}); zero-DPU hosts can only allocate into VPCs whose fabric_interface_type is `nic` (got `{vpc_iface}`)",
                mh_snapshot.host_snapshot.id,
                segment_id,
                vpc.id,
                vpc.config.network_virtualization_type,
            )));
        }
    }

    Ok(())
}

async fn update_instance_infiniband_config(
    mh_snapshot: &ManagedHostStateSnapshot,
    instance: &InstanceSnapshot,
    ib_config: &mut InstanceInfinibandConfig,
    txn: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> Result<(), CarbideError> {
    if !instance
        .config
        .infiniband
        .is_ib_config_update_requested(ib_config)
    {
        return Ok(());
    }

    if !matches!(
        mh_snapshot.managed_state,
        ManagedHostState::Assigned {
            instance_state: InstanceState::Ready,
        }
    ) {
        return Err(ConfigValidationError::InvalidState.into());
    }

    validate_ib_partition_ownership(
        txn,
        &instance.config.tenant.tenant_organization_id,
        ib_config,
    )
    .await?;

    // Allocate GUID for infiniband interfaces/ports.
    let ib_config_with_ports = allocate_ib_port_guid(ib_config, &mh_snapshot.host_snapshot)?;

    *ib_config = ib_config_with_ports;

    let pkeys =
        load_ib_partition_pkeys(txn.as_mut(), &[&instance.config.infiniband, &*ib_config]).await?;
    let current_memberships = ib_memberships_from_config(&instance.config.infiniband, &pkeys);
    let requested_memberships = ib_memberships_from_config(ib_config, &pkeys);

    // Persist the GUID for Infiniband configuration.
    // We need to increment the version number.
    db::instance::update_ib_config(
        txn,
        instance.id,
        instance.ib_config_version,
        ib_config,
        true,
    )
    .await?;

    // The `Instance` and `Machine` records remain locked through this
    // transaction. Apply every resolvable side of the transition in exact tuple
    // order, so even inconsistent duplicate assignments cannot invert locks in
    // the retirement table. An incomplete interface cannot identify a tuple to
    // record or remove.
    for membership in current_memberships.symmetric_difference(&requested_memberships) {
        if requested_memberships.contains(membership) {
            db::retired_ib_membership::remove_for_reuse(txn.as_mut(), membership).await?;
        } else {
            db::retired_ib_membership::record(txn.as_mut(), membership).await?;
        }
    }

    Ok(())
}

/// Applies a requested extension-service attachment change.
///
/// `extension_services` is the caller-visible view: the services the instance
/// should be running. It is merged with the durable config, which additionally
/// carries attachments that are still terminating, and the merged result is
/// what gets validated and persisted.
async fn update_instance_extension_services_config(
    mh_snapshot: &ManagedHostStateSnapshot,
    instance: &InstanceSnapshot,
    extension_services: &InstanceExtensionServicesConfig,
    txn: &mut db::Transaction<'_>,
) -> Result<(), CarbideError> {
    let current = &instance.config.extension_services;

    if !current.is_extension_services_config_update_requested(extension_services) {
        return Ok(());
    }

    if !matches!(
        mh_snapshot.managed_state,
        ManagedHostState::Assigned {
            instance_state: InstanceState::Ready,
        }
    ) {
        return Err(ConfigValidationError::InvalidState.into());
    }

    if instance.deleted.is_some() {
        return Err(ConfigValidationError::InstanceDeletionIsRequested.into());
    }

    // A service being detached remains durably represented with `removed:
    // true`, so the merged config references every service the instance is
    // attached to before and after this update.
    let new_extension_services_config =
        current.calculate_new_extension_services_config(extension_services);
    let service_ids = new_extension_services_config
        .service_configs
        .iter()
        .map(|service| service.service_id)
        .unique()
        .collect_vec();

    // Resolve the services while holding the service and version row locks.
    let (services, versions) = load_extension_services(txn, &service_ids).await?;
    let existing_active_service_ids = current
        .active_services()
        .into_iter()
        .map(|service| service.service_id)
        .collect();

    validate_instance_extension_services(
        mh_snapshot.host_snapshot.id,
        mh_snapshot.host_snapshot.config.dpf.used_for_ingestion,
        &new_extension_services_config,
        &services,
        &versions,
        &existing_active_service_ids,
    )?;

    db::instance::update_extension_services_config(
        txn,
        instance.id,
        instance.extension_services_config_version,
        &new_extension_services_config,
        true,
    )
    .await?;

    Ok(())
}

/// Extracts the RPC representation of Instances from a ManagedHost snapshot
///
/// This method expects that the snapshot must contain an instance definition.
/// If this is not required, then `Option::<rpc::Instance>::try_from(mh_snapshot)`
/// can be utilized.
fn snapshot_to_instance(
    mh_snapshot: ManagedHostStateSnapshot,
) -> Result<rpc::Instance, CarbideError> {
    let machine_id = mh_snapshot.host_snapshot.id;
    Option::<rpc::Instance>::rpc_try_from(mh_snapshot)
        .map_err(CarbideError::from)?
        .ok_or_else(|| {
            CarbideError::internal(format!(
                "instance on machine {machine_id} can be converted from snapshot"
            ))
        })
}

/// Records every exact IB membership available from the current `Instance`
/// before force-delete removes its last live database owner.
///
/// The caller holds the owning `Machine` lock. This lookup deliberately does
/// not lock the `Instance`, because IB config updates acquire those records in
/// Instance-then-Machine order.
pub(super) async fn record_force_delete_retired_ib_memberships(
    txn: &mut PgConnection,
    instance_id: InstanceId,
) -> CarbideResult<()> {
    let instance = db::instance::find_by_id(&mut *txn, instance_id)
        .await?
        .ok_or_else(|| {
            CarbideError::internal(format!("could not find an instance for {instance_id}"))
        })?;
    let pkeys = load_ib_partition_pkeys(txn, &[&instance.config.infiniband]).await?;
    for membership in ib_memberships_from_config(&instance.config.infiniband, &pkeys) {
        db::retired_ib_membership::record(txn, &membership).await?;
    }

    Ok(())
}

pub(super) async fn force_delete_instance(
    instance_id: InstanceId,
    api: &Api,
    response: &mut AdminForceDeleteMachineResponse,
) -> CarbideResult<()> {
    // The caller has already committed the Machine ForceDeletion state. Lock
    // and reread the Instance in a separate transaction so this path never
    // holds Machine and Instance locks together; IB updates lock the Instance
    // before the Machine. Once the deletion marker commits, the captured
    // snapshot is the last configuration a tenant update can commit before
    // external cleanup.
    let mut txn = api.txn_begin().await?;
    let Some(locked_instance) =
        db::instance::find_by_id_for_update(txn.as_mut(), instance_id).await?
    else {
        txn.commit().await?;
        return Ok(());
    };

    if locked_instance.deleted.is_none() {
        db::instance::mark_as_deleted(instance_id, txn.as_mut()).await?;
    }
    let instance = db::instance::find_by_id(&mut txn, instance_id)
        .await?
        .ok_or_else(|| {
            CarbideError::internal(format!("could not find an instance for {instance_id}"))
        })?;
    txn.commit().await?;

    response.ufm_unregistrations += unbind_all_instance_ib_ports(api, &instance).await?;

    // Delete the instance and allocated address
    // TODO: This might need some changes with the new state machine
    let mut txn = api.txn_begin().await?;
    db::instance::delete(instance_id, &mut txn).await?;

    let mut network_segment_ids_with_vpc = vec![];
    if let Some(update_network_req) = &instance.update_network_config_request {
        network_segment_ids_with_vpc = update_network_req
            .new_config
            .interfaces
            .iter()
            .filter_map(|x| x.generated_network_segment_id())
            .collect_vec();
        network_segment_ids_with_vpc.extend(
            update_network_req
                .old_config
                .interfaces
                .iter()
                .filter_map(|x| x.generated_network_segment_id()),
        );
    }

    network_segment_ids_with_vpc.extend(
        instance
            .config
            .network
            .interfaces
            .iter()
            .filter_map(|x| x.generated_network_segment_id()),
    );

    let network_segments_set: std::collections::HashSet<::carbide_uuid::network::NetworkSegmentId> =
        network_segment_ids_with_vpc.drain(..).collect();
    network_segment_ids_with_vpc.extend(network_segments_set);

    // Mark all network ready for delete which were created for vpc_prefixes.
    if !network_segment_ids_with_vpc.is_empty() {
        db::network_segment::mark_as_deleted_no_validation(&mut txn, &network_segment_ids_with_vpc)
            .await?;
    }

    let snapshot = db::managed_host::load_snapshot(
        &mut txn,
        &instance.machine_id,
        LoadSnapshotOptions::default(),
    )
    .await?
    .ok_or(CarbideError::NotFoundError {
        kind: "machine",
        id: instance.machine_id.to_string(),
    })?;

    carbide_machine_controller::handler::release_vpc_dpu_loopback(
        &snapshot,
        Some(api.common_pools.as_ref()),
        &mut txn,
    )
    .await
    .map_err(|e| CarbideError::internal(e.to_string()))?;

    txn.commit().await?;

    Ok(())
}

async fn update_instance_nvlink_config(
    mh_snapshot: &ManagedHostStateSnapshot,
    instance: &InstanceSnapshot,
    nvlcfg: &InstanceNvLinkConfig,
    txn: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> Result<(), CarbideError> {
    if !instance
        .config
        .nvlink
        .is_nvlink_config_update_requested(nvlcfg)
    {
        return Ok(());
    }

    if !matches!(
        mh_snapshot.managed_state,
        ManagedHostState::Assigned {
            instance_state: InstanceState::Ready,
        }
    ) {
        return Err(ConfigValidationError::InvalidState.into());
    }

    if instance.deleted.is_some() {
        return Err(ConfigValidationError::InstanceDeletionIsRequested.into());
    }

    // Update config in db.
    db::instance::update_nvlink_config(
        txn,
        instance.id,
        instance.nvlink_config_version,
        nvlcfg,
        true,
    )
    .await?;

    Ok(())
}

async fn update_instance_spx_config(
    mh_snapshot: &ManagedHostStateSnapshot,
    instance: &InstanceSnapshot,
    spxcfg: &mut InstanceSpxConfig,
    txn: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> Result<(), CarbideError> {
    let mid = instance.machine_id;

    let mut mh_snapshot = mh_snapshot.clone();

    if !instance
        .config
        .spxconfig
        .is_spx_config_update_requested(spxcfg)
    {
        tracing::info!("SPX update_instance_spx_config is_spx_config_update_requested is false");
        return Ok(());
    }

    if !matches!(
        mh_snapshot.managed_state,
        ManagedHostState::Assigned {
            instance_state: InstanceState::Ready,
        }
    ) {
        tracing::info!("SPX update_instance_spx_config not Assigned");
        return Err(ConfigValidationError::InvalidState.into());
    }

    if instance.deleted.is_some() {
        tracing::info!("SPX update_instance_spx_config instance deleted");
        return Err(ConfigValidationError::InstanceDeletionIsRequested.into());
    }

    let dpa_search_config = DpaSearchConfig {
        only_svpc: false,
        only_astra: false,
    };
    let host_machine_id = carbide_uuid::machine::HostMachineId::try_from(mid)
        .map_err(|error| CarbideError::internal(error.to_string()))?;
    let dpa_interfaces =
        db::dpa_interface::find_by_machine_id(txn.as_mut(), host_machine_id, dpa_search_config)
            .await?;

    mh_snapshot.dpa_interface_snapshots = dpa_interfaces;

    validate_spx_partition_ownership(txn, &instance.config.tenant.tenant_organization_id, spxcfg)
        .await?;

    let spx_config_with_ports = allocate_spx_port_mac(spxcfg, &mh_snapshot)?;
    *spxcfg = spx_config_with_ports;

    // Update config in db.
    db::instance::update_spx_config(txn, instance.id, instance.spx_config_version, spxcfg, true)
        .await?;

    tracing::info!("SPX update_instance_spx_config updating config in db done");

    Ok(())
}

async fn unbind_all_instance_ib_ports(
    api: &Api,
    instance: &InstanceSnapshot,
) -> Result<u32, CarbideError> {
    // Create an index of PartitionId -> GUID from the instance's ib_interfaces
    let ib_config_map: HashMap<IBPartitionId, Vec<String>> = instance
        .config
        .infiniband
        .ib_interfaces
        .iter()
        .filter_map(|ib| {
            ib.guid
                .as_ref()
                .map(|guid| (ib.ib_partition_id, guid.to_string()))
        })
        .into_group_map();
    // No need to proceed if there's nothing to do
    if ib_config_map.is_empty() {
        return Ok(0);
    }

    // Get the infiniband info from UFM, being careful not to call UFM while holding an open db
    // transaction.
    // First, look up the pkey in the database for each partition ID, in one transaction
    let pkeys_with_guids_to_unbind: Vec<(u16, Vec<String>)> = api
        .with_txn(|txn| {
            async move {
                let mut result = Vec::with_capacity(ib_config_map.len());
                for (ib_partition_id, guids) in ib_config_map.into_iter() {
                    if let Some(pkey) =
                        db::ib_partition::find_pkey_by_partition_id(txn, ib_partition_id).await?
                    {
                        result.push((pkey, guids))
                    }
                }
                Ok::<_, DatabaseError>(result)
            }
            .boxed()
        })
        .await??;

    // Then, tell UFM to unbind the port for each pkey we found
    let ib_fabric = api
        .ib_fabric_manager
        .new_client(model::ib::DEFAULT_IB_FABRIC_NAME)
        .await?;

    let mut ufm_unregistrations = 0;
    for (pkey, guids) in pkeys_with_guids_to_unbind {
        ib_fabric.unbind_ib_ports(pkey, guids).await?;
        ufm_unregistrations += 1;
        //TODO: release VF GUID resource when VF supported.
    }
    Ok(ufm_unregistrations)
}

#[async_trait::async_trait]
trait HydrateFromDeprecatedFields {
    fn needs_hydration_from_deprecated_fields(&self) -> bool;

    async fn hydrate_from_deprecated_fields(&mut self, txn: &mut PgConnection)
    -> CarbideResult<()>;
}

#[async_trait::async_trait]
impl HydrateFromDeprecatedFields for rpc::InstanceAllocationRequest {
    fn needs_hydration_from_deprecated_fields(&self) -> bool {
        let Some(netconf) = self.config.as_ref().and_then(|c| c.network.as_ref()) else {
            // Conversion to model type will fail, return Ok
            return false;
        };

        #[allow(deprecated)]
        let auto = netconf.auto;

        // Only hydrate if the VPC ID isn't set
        auto && netconf
            .auto_config
            .as_ref()
            .is_none_or(|c| c.vpc_id.is_none())
    }

    async fn hydrate_from_deprecated_fields(
        &mut self,
        txn: &mut PgConnection,
    ) -> CarbideResult<()> {
        let Some(netconf) = self.config.as_mut().and_then(|c| c.network.as_mut()) else {
            // Conversion to model type will fail, return Ok
            return Ok(());
        };

        let Some(machine_id) = self.machine_id else {
            // Conversion to model type will fail, return Ok
            return Ok(());
        };

        tracing::info!(
            %machine_id,
            "Deprecated instance allocation request with auto=true with no VPC ID. Assigning VPC from host network segment, if it is set."
        );

        let ns_id = match db::network_segment::find_ids_by_machine_id(
            txn,
            &machine_id,
            Some(NetworkSegmentType::HostInband),
        )
        .await?
        .as_slice()
        {
            &[ns_id] => ns_id,
            [] => {
                tracing::warn!(
                    "Assigned machine is not in a HostInband network, cannot auto-assign VPC ID"
                );
                return Ok(());
            }
            _ => {
                tracing::warn!(
                    "Assigned machine is in multiple a HostInband networks, cannot auto-assign VPC ID"
                );
                return Ok(());
            }
        };

        let maybe_vpc_id = match db::network_segment::find_by(
            txn,
            ObjectColumnFilter::One(db::network_segment::IdColumn, &ns_id),
            NetworkSegmentSearchConfig::default(),
        )
        .await?
        .as_slice()
        {
            [ns] => ns.config.vpc_id,
            _ => None,
        };

        let Some(vpc_id) = maybe_vpc_id else {
            tracing::warn!(network_segment_id = %ns_id, "Network segment does not have a VPC ID, cannot auto-assign VPC ID");
            return Ok(());
        };

        tracing::info!(%vpc_id, network_segment_id = %ns_id, "Assigning vpc_id from HostInband network segment");
        netconf.auto_config = Some(rpc::InstanceNetworkAutoConfig {
            vpc_id: Some(vpc_id),
        });

        Ok(())
    }
}

#[async_trait::async_trait]
impl HydrateFromDeprecatedFields for rpc::BatchInstanceAllocationRequest {
    fn needs_hydration_from_deprecated_fields(&self) -> bool {
        self.instance_requests
            .iter()
            .any(|req| req.needs_hydration_from_deprecated_fields())
    }

    async fn hydrate_from_deprecated_fields(
        &mut self,
        txn: &mut PgConnection,
    ) -> CarbideResult<()> {
        for req in self.instance_requests.iter_mut() {
            req.hydrate_from_deprecated_fields(txn).await?;
        }
        Ok(())
    }
}

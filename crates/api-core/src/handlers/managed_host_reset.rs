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
use ::rpc::forge as rpc;
use carbide_uuid::machine::MachineId;
use itertools::Itertools;
use model::machine::{LoadSnapshotOptions, ManagedHostState, ManagedHostStateSnapshot};
use model::machine_update_module::HOST_UPDATE_HEALTH_PROBE_ID;
use tonic::{Request, Response, Status};

use crate::CarbideError;
use crate::api::{Api, log_request_data};
use crate::handlers::utils::convert_and_log_machine_id;

/// Preconditions for requesting a reset; `Clear` only withdraws one and skips them.
fn validate_managed_host_reset_request(
    snapshot: &ManagedHostStateSnapshot,
    machine_id: &MachineId,
    allow_reset_with_instance: bool,
) -> Result<(), CarbideError> {
    // The state controller stops managing a force-deleting machine, so a reset would never run.
    if matches!(snapshot.managed_state, ManagedHostState::ForceDeletion) {
        return Err(CarbideError::FailedPrecondition(format!(
            "cannot reset host {machine_id}: machine is being force-deleted"
        )));
    }

    // Re-ingestion re-registers the host's DPF CRs, which only exist for DPF-ingested hosts.
    if !snapshot.host_snapshot.config.dpf.used_for_ingestion {
        return Err(CarbideError::FailedPrecondition(format!(
            "cannot reset host {machine_id}: machine was not ingested via DPF"
        )));
    }

    // The snapshot's instance join is unfiltered, so a terminated instance is still present.
    let has_live_instance = snapshot
        .instance
        .as_ref()
        .is_some_and(|instance| instance.deleted.is_none());
    if has_live_instance && !allow_reset_with_instance {
        return Err(CarbideError::FailedPrecondition(format!(
            "host {machine_id} has a live instance; resetting destroys it and its data. \
             pass --allow-reset-with-instance to proceed"
        )));
    }

    let update_alert = snapshot
        .aggregate_health
        .alerts
        .iter()
        .find(|alert| alert.id == *HOST_UPDATE_HEALTH_PROBE_ID);
    if !update_alert.is_some_and(|alert| {
        alert
            .classifications
            .contains(&health_report::HealthAlertClassification::prevent_allocations())
    }) {
        return Err(CarbideError::InvalidArgument(format!(
            "machine {machine_id} must have a 'HostUpdateInProgress' health alert with the \
             'PreventAllocations' classification before resetting. set this precondition \
             with: `machine health-report add --template host-update <id>`, or pass \
             --update-message to `managed-host reset set`",
        )));
    }

    Ok(())
}

pub(crate) async fn trigger_managed_host_reset(
    api: &Api,
    request: Request<rpc::ManagedHostResetRequest>,
) -> Result<Response<()>, Status> {
    use ::rpc::forge::managed_host_reset_request::Mode;

    log_request_data(&request);
    let req = request.into_inner();
    let machine_id: MachineId = convert_and_log_machine_id(req.machine_id.as_ref())?;

    // A reset tears down every attached DPU, so it is only expressible against the host.
    if !machine_id.machine_type().is_host() {
        return Err(CarbideError::InvalidArgument(format!(
            "{machine_id} is not a host machine; reset targets the host, not an individual DPU"
        ))
        .into());
    }

    let mut txn = api.txn_begin().await?;

    let snapshot = db::managed_host::load_snapshot(
        &mut txn,
        &machine_id,
        LoadSnapshotOptions {
            include_history: false,
            // The live-instance precondition reads the instance row.
            include_instance_data: true,
            host_health_config: api.runtime_config.host_health,
        },
    )
    .await?
    .ok_or(CarbideError::NotFoundError {
        kind: "machine",
        id: machine_id.to_string(),
    })?;

    match req.mode() {
        Mode::Set => {
            validate_managed_host_reset_request(
                &snapshot,
                &machine_id,
                req.allow_reset_with_instance,
            )?;

            // Re-requesting is allowed and restarts from scratch; the DB call clears `started_at`.
            if let Some(previous) = &snapshot.host_snapshot.reset_requested {
                tracing::warn!(
                    %machine_id,
                    previous_initiator = %previous.initiator,
                    previous_requested_at = %previous.requested_at,
                    previous_started = previous.started_at.is_some(),
                    "Reset re-requested for a host that already had one; restarting it",
                );
            }

            db::machine::trigger_managed_host_reset_request(
                &mut txn,
                req.initiator().as_str_name(),
                &machine_id,
            )
            .await?;
        }
        Mode::Clear => {
            // Once teardown has begun there is nothing to withdraw to.
            if snapshot
                .host_snapshot
                .reset_requested
                .as_ref()
                .is_some_and(|request| request.started_at.is_some())
            {
                return Err(CarbideError::FailedPrecondition(format!(
                    "reset for host {machine_id} has already started and cannot be cleared"
                ))
                .into());
            }

            db::machine::clear_managed_host_reset_request(&mut txn, &machine_id, true).await?;
        }
        // An omitted mode decodes here; reject rather than pick an action for it.
        Mode::Unspecified => {
            return Err(
                CarbideError::InvalidArgument("mode must be set or clear".to_string()).into(),
            );
        }
    }

    txn.commit().await?;

    Ok(Response::new(()))
}

pub(crate) async fn list_managed_hosts_waiting_for_reset(
    api: &Api,
    request: Request<rpc::ManagedHostResetListRequest>,
) -> Result<Response<rpc::ManagedHostResetListResponse>, Status> {
    log_request_data(&request);

    let hosts = db::machine::list_machines_requested_for_reset(&api.database_connection)
        .await?
        .into_iter()
        .map(
            |x| rpc::managed_host_reset_list_response::ManagedHostResetListItem {
                id: Some(*x.id.as_machine_id()),
                state: x.current_state().to_string(),
                requested_at: x.reset_requested.as_ref().map(|a| a.requested_at.into()),
                initiator: x
                    .reset_requested
                    .as_ref()
                    .map(|a| a.initiator.clone())
                    .unwrap_or_default(),
                started_at: x
                    .reset_requested
                    .as_ref()
                    .and_then(|a| a.started_at.map(Into::into)),
            },
        )
        .collect_vec();

    Ok(Response::new(rpc::ManagedHostResetListResponse { hosts }))
}

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
use carbide_uuid::machine::{HostMachineId, MachineId};
use itertools::Itertools;
use model::machine::{
    HostReprovisionState, InstanceState, LoadSnapshotOptions, ManagedHostState, ScoutUpgradeResult,
};
use tonic::{Request, Response, Status};

use crate::CarbideError;
use crate::api::{Api, log_request_data, truncate};
use crate::handlers::utils::{StateHandlerWakeupFailed, WakeupTrigger, convert_and_log_machine_id};

pub(crate) async fn reset_host_reprovisioning(
    api: &Api,
    request: Request<MachineId>,
) -> Result<Response<()>, Status> {
    log_request_data(&request);
    let machine_id = convert_and_log_machine_id(Some(&request.into_inner()))?;

    let mut txn = api.txn_begin().await?;

    db::host_machine_update::reset_host_reprovisioning_request(&mut txn, &machine_id, false)
        .await?;
    txn.commit().await?;

    Ok(Response::new(()))
}

pub(crate) async fn trigger_host_reprovisioning(
    api: &Api,
    request: Request<rpc::HostReprovisioningRequest>,
) -> Result<Response<()>, Status> {
    use ::rpc::forge::host_reprovisioning_request::Mode;

    log_request_data(&request);
    let req = request.into_inner();
    let machine_id = convert_and_log_machine_id::<MachineId>(req.machine_id.as_ref())?;

    let mut txn = api.txn_begin().await?;

    let snapshot =
        db::managed_host::load_snapshot(&mut txn, &machine_id, LoadSnapshotOptions::default())
            .await?
            .ok_or(CarbideError::NotFoundError {
                kind: "machine",
                id: machine_id.to_string(),
            })?;

    if let Some(request) = snapshot.host_snapshot.reprovision_requested
        && request.started_at.is_some()
    {
        return Err(CarbideError::internal("reprovisioning is already started".to_string()).into());
    }

    let started_initiator = match req.mode() {
        Mode::Set => {
            let initiator = req.initiator().as_str_name();
            db::host_machine_update::trigger_host_reprovisioning_request(
                &mut txn,
                initiator,
                &machine_id,
            )
            .await?;
            Some(initiator)
        }
        Mode::Clear => {
            db::host_machine_update::clear_host_reprovisioning_request(&mut txn, &machine_id)
                .await?;
            None
        }
    };

    txn.commit().await?;

    // Manual initiations pair with the same completion emit the update
    // manager's automatic path gets, keeping the started-to-completed gap
    // truthful for every initiator. Counted only after the commit: a
    // rolled-back trigger never started anything.
    if let Some(initiator) = started_initiator {
        carbide_instrument::emit(
            crate::machine_update_manager::metrics::FirmwareUpdateProgress {
                target: crate::machine_update_manager::metrics::FirmwareUpdateTarget::Host,
                phase: crate::machine_update_manager::metrics::FirmwareUpdatePhase::Started,
                machine_id,
                detail: initiator.to_string(),
            },
        );
    }

    Ok(Response::new(()))
}

pub(crate) async fn list_hosts_waiting_for_reprovisioning(
    api: &Api,
    request: Request<rpc::HostReprovisioningListRequest>,
) -> Result<Response<rpc::HostReprovisioningListResponse>, Status> {
    log_request_data(&request);

    let hosts =
        db::machine::list_machines_requested_for_host_reprovisioning(&api.database_connection)
            .await?
            .into_iter()
            .map(
                |x| rpc::host_reprovisioning_list_response::HostReprovisioningListItem {
                    id: Some(x.id),
                    state: x.current_state().to_string(),
                    requested_at: x
                        .reprovision_requested
                        .as_ref()
                        .map(|a| a.requested_at.into()),
                    initiator: x
                        .reprovision_requested
                        .as_ref()
                        .map(|a| a.initiator.clone())
                        .unwrap_or_default(),
                    initiated_at: x
                        .reprovision_requested
                        .as_ref()
                        .map(|a| a.started_at.map(|x| x.into()))
                        .unwrap_or_default(),
                    user_approval_received: x
                        .reprovision_requested
                        .as_ref()
                        .map(|x| x.user_approval_received)
                        .unwrap_or_default(),
                },
            )
            .collect_vec();

    Ok(Response::new(rpc::HostReprovisioningListResponse { hosts }))
}

pub(crate) async fn mark_manual_firmware_upgrade_complete(
    api: &Api,
    request: Request<MachineId>,
) -> Result<Response<()>, Status> {
    log_request_data(&request);
    let machine_id = convert_and_log_machine_id(Some(&request.into_inner()))?;

    let mut txn = api.txn_begin().await?;

    db::host_machine_update::set_manual_firmware_upgrade_completed(&mut txn, &machine_id).await?;

    txn.commit().await?;

    Ok(Response::new(()))
}

pub(crate) async fn report_scout_firmware_upgrade_status(
    api: &Api,
    request: Request<rpc::ScoutFirmwareUpgradeStatusRequest>,
) -> Result<Response<()>, Status> {
    log_request_data(&request);

    let req = request.into_inner();
    let machine_id = convert_and_log_machine_id::<HostMachineId>(req.machine_id.as_ref())?;

    let (machine, mut txn) = api.load_machine(&machine_id, Default::default()).await?;

    enum HostReprovisionContext {
        Ready { retry_count: u32 },
        Assigned,
    }

    let (reprovision_state, context) = match machine.current_state().clone() {
        ManagedHostState::HostReprovision {
            reprovision_state,
            retry_count,
        } => (
            reprovision_state,
            HostReprovisionContext::Ready { retry_count },
        ),
        ManagedHostState::Assigned {
            instance_state: InstanceState::HostReprovision { reprovision_state },
        } => (reprovision_state, HostReprovisionContext::Assigned),
        _ => {
            return Err(CarbideError::FailedPrecondition(format!(
                "machine {machine_id} is not in WaitingForScoutUpgrade state"
            ))
            .into());
        }
    };

    let HostReprovisionState::WaitingForScoutUpgrade {
        upgrade_task_id,
        firmware_type,
        final_version,
        power_drains_needed,
        started_at,
        deadline,
        task_json,
        ..
    } = reprovision_state
    else {
        return Err(CarbideError::FailedPrecondition(format!(
            "machine {machine_id} is not in WaitingForScoutUpgrade state"
        ))
        .into());
    };

    if req.upgrade_task_id != upgrade_task_id {
        tracing::warn!(
            %machine_id,
            expected_upgrade_task_id = %upgrade_task_id,
            reported_upgrade_task_id = %req.upgrade_task_id,
            "Rejecting stale scout firmware upgrade status report",
        );
        return Err(CarbideError::FailedPrecondition(format!(
            "scout firmware upgrade status task ID mismatch for machine {machine_id}"
        ))
        .into());
    }

    // Cap stdout/stderr/error so the JSON state row stays small; full output
    // is available in the scout logs if an operator needs to dig deeper.
    const MAX_STORED_OUTPUT_SIZE: usize = 1500;

    let reprovision_state = HostReprovisionState::WaitingForScoutUpgrade {
        upgrade_task_id,
        firmware_type,
        final_version,
        power_drains_needed,
        started_at,
        deadline,
        task_json,
        result: Some(ScoutUpgradeResult {
            success: req.success,
            exit_code: req.exit_code,
            stdout: truncate(req.stdout, MAX_STORED_OUTPUT_SIZE),
            stderr: truncate(req.stderr, MAX_STORED_OUTPUT_SIZE),
            error: truncate(req.error, MAX_STORED_OUTPUT_SIZE),
        }),
    };
    let new_state = match context {
        HostReprovisionContext::Ready { retry_count } => ManagedHostState::HostReprovision {
            reprovision_state,
            retry_count,
        },
        HostReprovisionContext::Assigned => ManagedHostState::Assigned {
            instance_state: InstanceState::HostReprovision { reprovision_state },
        },
    };

    db::machine::advance(&machine, &mut txn, &new_state, None).await?;

    txn.commit().await?;

    if let Err(err) = api
        .machine_state_handler_enqueuer
        .enqueue_object(&machine_id)
        .await
    {
        carbide_instrument::emit(StateHandlerWakeupFailed {
            trigger: WakeupTrigger::ScoutFirmwareUpgradeStatus,
            machine_id: machine_id.into(),
            err: err.to_string(),
        });
    }

    Ok(Response::new(()))
}

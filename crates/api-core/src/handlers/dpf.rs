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
use carbide_dpf::dpu_node_cr_name;
use carbide_uuid::machine::HostMachineId;
use db::ObjectFilter;
use db::machine::find_one;
use db::managed_host::load_snapshot;
use model::machine::LoadSnapshotOptions;
use model::machine::machine_search_config::MachineSearchConfig;
use tonic::{Request, Response, Status};

use crate::CarbideError;
use crate::api::{Api, log_request_data};
use crate::handlers::utils::{MachinePreparationDeadline, convert_and_log_machine_id};

async fn lock_host_for_dpf_update(
    txn: &mut sqlx::PgConnection,
    machine_id: &HostMachineId,
) -> Result<(), CarbideError> {
    if !db::machine::lock_by_id_for_update_if_present(txn, &(*machine_id).into()).await? {
        return Err(CarbideError::NotFoundError {
            kind: "snapshot",
            id: machine_id.to_string(),
        });
    }
    Ok(())
}

pub(crate) async fn modify_dpf_state(
    api: &Api,
    request: Request<rpc::ModifyDpfStateRequest>,
) -> Result<Response<()>, Status> {
    let deadline = MachinePreparationDeadline::from_request(&request);
    log_request_data(&request);
    let request = request.get_ref();
    let machine_id: HostMachineId = convert_and_log_machine_id(request.machine_id.as_ref())?;

    deadline
        .run(|| async {
            loop {
                let mut txn = api.txn_begin().await?;
                lock_host_for_dpf_update(txn.as_mut(), &machine_id).await?;
                match db::machine::try_lock_host_interfaces(txn.as_mut(), &machine_id).await? {
                    db::machine::HostInterfaceAdmission::Acquired => {}
                    db::machine::HostInterfaceAdmission::Contended(interface_id) => {
                        txn.rollback().await?;
                        db::machine::wait_for_managed_host_transition_lock(
                            api.pg_pool(),
                            &db::machine::ManagedHostTransitionLock::HostInterface(interface_id),
                        )
                        .await?;
                        continue;
                    }
                    db::machine::HostInterfaceAdmission::Changed => {
                        txn.rollback().await?;
                        continue;
                    }
                }
                let machine_snapshot =
                    load_snapshot(&mut txn, &machine_id, LoadSnapshotOptions::default())
                        .await?
                        .ok_or_else(|| CarbideError::NotFoundError {
                            kind: "snapshot",
                            id: machine_id.to_string(),
                        })?;

                if !request.dpf_enabled
                    && machine_snapshot.host_snapshot.config.dpf.used_for_ingestion
                {
                    return Err(CarbideError::FailedPrecondition(format!(
                        "cannot disable DPF for host {}: machine was ingested via DPF",
                        machine_id
                    ))
                    .into());
                }

                let mut dpu_ids = machine_snapshot
                    .dpu_snapshots
                    .iter()
                    .map(|dpu| dpu.id)
                    .collect::<Vec<_>>();
                dpu_ids.sort_unstable();
                dpu_ids.dedup();
                // Reports and queued controller writes can own a DPU before
                // needing the host or its interfaces.
                let mut contended = None;
                for dpu_id in &dpu_ids {
                    if !db::machine::try_lock_by_id_for_no_key_update(
                        txn.as_mut(),
                        &(*dpu_id).into(),
                    )
                    .await?
                    {
                        contended = Some(*dpu_id);
                        break;
                    }
                }
                if let Some(dpu_id) = contended {
                    txn.rollback().await?;
                    db::machine::wait_for_machine_nonkey_update(api.pg_pool(), &dpu_id.into())
                        .await?;
                    continue;
                }
                db::machine::modify_dpf_state(&mut txn, &machine_id, request.dpf_enabled).await?;
                for dpu_id in dpu_ids {
                    db::machine::modify_dpf_state(&mut txn, &dpu_id, request.dpf_enabled).await?;
                }
                txn.commit().await?;
                return Ok(Response::new(()));
            }
        })
        .await
}

// Since this function sends only a bool with ids, we might not need pagination for this.
pub(crate) async fn get_dpf_state(
    api: &Api,
    request: Request<rpc::GetDpfStateRequest>,
) -> Result<Response<rpc::DpfStateResponse>, Status> {
    log_request_data(&request);
    let request = request.get_ref();

    for machine_id in &request.machine_ids {
        if machine_id.machine_type().is_dpu() {
            return Err(
                CarbideError::InvalidArgument("only host id is expected!!".to_string()).into(),
            );
        }
    }

    let mut txn = api.txn_begin().await?;
    let filter = if request.machine_ids.is_empty() {
        ObjectFilter::All
    } else {
        ObjectFilter::List(&request.machine_ids)
    };

    let dpf_states = db::machine::find(&mut txn, filter, MachineSearchConfig::default()).await?;
    txn.commit().await?;

    Ok(Response::new(rpc::DpfStateResponse {
        dpf_states: dpf_states
            .into_iter()
            .map(|machine| machine.into())
            .collect(),
    }))
}

pub(crate) async fn get_dpf_host_snapshot(
    api: &Api,
    request: Request<rpc::GetDpfHostSnapshotRequest>,
) -> Result<Response<rpc::DpfHostSnapshotResponse>, Status> {
    log_request_data(&request);
    let request = request.get_ref();
    let machine_id: HostMachineId = convert_and_log_machine_id(request.host_machine_id.as_ref())?;

    let Some(ops) = api.dpf_sdk.as_ref() else {
        return Err(CarbideError::InvalidArgument(
            "DPF is not enabled on this carbide instance".to_string(),
        )
        .into());
    };

    let mut txn = api.txn_begin().await?;
    let machine = find_one(&mut txn, &machine_id, MachineSearchConfig::default())
        .await?
        .ok_or_else(|| CarbideError::NotFoundError {
            kind: "machine",
            id: machine_id.to_string(),
        })?;
    txn.commit().await?;

    let host_dpf_id = machine.dpf_id().ok_or_else(|| {
        CarbideError::InvalidArgument(format!(
            "host {machine_id} has no BMC MAC; cannot derive DPF node name"
        ))
    })?;
    let node_name = dpu_node_cr_name(&host_dpf_id);

    let snapshot = ops
        .snapshot_host(&node_name)
        .await
        .map_err(CarbideError::DpfError)?;

    let json_payload = serde_json::to_string_pretty(&snapshot).map_err(|e| {
        CarbideError::internal(format!("failed to serialize DPF host snapshot: {e}"))
    })?;

    Ok(Response::new(rpc::DpfHostSnapshotResponse { json_payload }))
}

pub(crate) async fn get_dpf_service_versions(
    api: &Api,
    request: Request<rpc::GetDpfServiceVersionsRequest>,
) -> Result<Response<rpc::DpfServiceVersionsResponse>, Status> {
    log_request_data(&request);

    let cfg = &api.runtime_config.dpf.services;
    let configured = [
        &cfg.dts,
        &cfg.doca_hbn,
        &cfg.dpu_agent,
        &cfg.dhcp_server,
        &cfg.fmds,
        &cfg.otel,
    ];

    let live = if let Some(ops) = api.dpf_sdk.as_ref() {
        ops.list_service_template_versions()
            .await
            .map_err(CarbideError::DpfError)?
    } else {
        Vec::new()
    };

    let services = configured
        .iter()
        .map(|svc_cfg| {
            let matched = live
                .iter()
                .find(|t| t.deployment_service_name == svc_cfg.name);
            rpc::DpfServiceVersion {
                service: svc_cfg.name.clone(),
                config_helm_version: svc_cfg.helm_version.clone(),
                config_docker_image_tag: svc_cfg.docker_image_tag.clone(),
                live_helm_version: matched.map(|t| t.helm_version.clone()).unwrap_or_default(),
                live_docker_image_tag: matched
                    .map(|t| t.docker_image_tag.clone())
                    .unwrap_or_default(),
            }
        })
        .collect();

    Ok(Response::new(rpc::DpfServiceVersionsResponse { services }))
}

#[cfg(test)]
mod tests {
    use carbide_uuid::machine::{HostMachineId, MachineId, MachineIdSource, MachineType};

    use super::lock_host_for_dpf_update;
    use crate::CarbideError;

    #[crate::sqlx_test]
    async fn dpf_host_admission_rejects_missing_host(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let host_id = HostMachineId::try_from(MachineId::new(
            MachineIdSource::ProductBoardChassisSerial,
            [53; 32],
            MachineType::Host,
        ))?;
        let mut attempt = pool.begin().await?;
        let rejected = lock_host_for_dpf_update(attempt.as_mut(), &host_id).await;
        attempt.rollback().await?;
        assert!(matches!(
            rejected,
            Err(CarbideError::NotFoundError { kind: "snapshot", id }) if id == host_id.to_string()
        ));
        Ok(())
    }
}

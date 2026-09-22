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
use crate::handlers::utils::convert_and_log_machine_id;

pub(crate) async fn modify_dpf_state(
    api: &Api,
    request: Request<rpc::ModifyDpfStateRequest>,
) -> Result<Response<()>, Status> {
    log_request_data(&request);
    let request = request.get_ref();
    let machine_id: HostMachineId = convert_and_log_machine_id(request.machine_id.as_ref())?;

    let mut txn = api.txn_begin().await?;
    let machine_snapshot = load_snapshot(&mut txn, &machine_id, LoadSnapshotOptions::default())
        .await?
        .ok_or_else(|| CarbideError::NotFoundError {
            kind: "snapshot",
            id: machine_id.to_string(),
        })?;

    if !request.dpf_enabled && machine_snapshot.host_snapshot.config.dpf.used_for_ingestion {
        return Err(CarbideError::FailedPrecondition(format!(
            "cannot disable DPF for host {}: machine was ingested via DPF",
            machine_id
        ))
        .into());
    }

    db::machine::modify_dpf_state(&mut txn, &machine_id, request.dpf_enabled).await?;

    // Keep DPUs also in sync.
    for dpu in machine_snapshot.dpu_snapshots {
        db::machine::modify_dpf_state(&mut txn, &dpu.id, request.dpf_enabled).await?;
    }
    txn.commit().await?;

    Ok(Response::new(()))
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

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
use carbide_dpf::sdk::deployment_cr_suffix;
use carbide_dpf::types::{DpuDeploymentType, ServiceTemplateVersion};
use carbide_uuid::machine::HostMachineId;
use db::ObjectFilter;
use db::machine::find_one;
use db::managed_host::load_snapshot;
use model::machine::LoadSnapshotOptions;
use model::machine::machine_search_config::MachineSearchConfig;
use tonic::{Request, Response, Status};

use crate::CarbideError;
use crate::api::{Api, log_request_data};
use crate::cfg::file::DpfConfig;
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

    let live = if let Some(ops) = api.dpf_sdk.as_ref() {
        ops.list_service_template_versions()
            .await
            .map_err(CarbideError::DpfError)?
    } else {
        Vec::new()
    };

    Ok(Response::new(rpc::DpfServiceVersionsResponse {
        services: service_versions(&api.runtime_config.dpf, &live),
    }))
}

fn service_versions(
    config: &DpfConfig,
    live: &[ServiceTemplateVersion],
) -> Vec<rpc::DpfServiceVersion> {
    let bf3_gb200 = config.deployments.bf3.bf3_gb200();
    let mut services = Vec::new();
    for (deployment_type, deployment) in [
        (DpuDeploymentType::Bf3, Some(&config.deployments.bf3)),
        (DpuDeploymentType::Bf3Gb200, Some(&bf3_gb200)),
        (
            DpuDeploymentType::Bf4Generic,
            config.deployments.bf4_generic.as_ref(),
        ),
        (
            DpuDeploymentType::Bf4Astra,
            config.deployments.bf4_astra.as_ref(),
        ),
    ] {
        let Some(deployment) = deployment else {
            continue;
        };
        let resolved = config.resolved_services_for(deployment, deployment_type);
        let suffix = deployment_cr_suffix(deployment_type);
        for svc_cfg in [
            resolved.base.dts,
            resolved.base.doca_hbn,
            resolved.base.dpu_agent,
            resolved.base.dhcp_server,
            resolved.base.fmds,
            resolved.base.otel,
        ] {
            let cr_name = if suffix.is_empty() {
                svc_cfg.name.clone()
            } else {
                format!("{}-{suffix}", svc_cfg.name)
            };
            let matched = live.iter().find(|t| t.cr_name == cr_name);
            services.push(rpc::DpfServiceVersion {
                service: cr_name,
                config_helm_version: svc_cfg.helm_version,
                config_docker_image_tag: svc_cfg.docker_image_tag,
                live_helm_version: matched.map(|t| t.helm_version.clone()).unwrap_or_default(),
                live_docker_image_tag: matched
                    .map(|t| t.docker_image_tag.clone())
                    .unwrap_or_default(),
            });
        }
    }
    services
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cfg::file::{DpfDeploymentConfig, DpfDeploymentsConfig, DpfMandatoryServicesConfig};

    #[test]
    fn dpf_dts_helm_version_reporting_uses_deployment_templates() {
        let mut astra_services = DpfMandatoryServicesConfig::default();
        astra_services.dts.helm_version = "astra-version".to_string();
        let config = DpfConfig {
            deployments: DpfDeploymentsConfig {
                bf4_generic: Some(DpfDeploymentConfig::default()),
                bf4_astra: Some(DpfDeploymentConfig {
                    services: Some(Box::new(astra_services)),
                    ..DpfDeploymentConfig::default()
                }),
                ..DpfDeploymentsConfig::default()
            },
            ..DpfConfig::default()
        };
        // All templates share the service name. Listing order must not determine the match.
        let live = [
            ("dts-obsolete", "obsolete-version"),
            ("dts-bf4generic", "1.25.43"),
            ("dts-bf4astra", "astra-version"),
            ("dts", "1.27.8"),
        ]
        .into_iter()
        .map(|(cr_name, helm_version)| ServiceTemplateVersion {
            cr_name: cr_name.to_string(),
            deployment_service_name: "dts".to_string(),
            helm_repo_url: String::new(),
            helm_chart: None,
            helm_version: helm_version.to_string(),
            docker_image_tag: String::new(),
        })
        .collect::<Vec<_>>();

        let versions = service_versions(&config, &live);
        let dts_versions = versions
            .iter()
            .filter(|service| service.service.starts_with("dts"))
            .map(|service| {
                (
                    service.service.as_str(),
                    service.config_helm_version.as_str(),
                    service.live_helm_version.as_str(),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            dts_versions,
            vec![
                ("dts", "1.27.8", "1.27.8"),
                ("dts-bf3gb200", "1.27.8", ""),
                ("dts-bf4generic", "1.25.43", "1.25.43"),
                ("dts-bf4astra", "astra-version", "astra-version"),
            ]
        );
    }
}

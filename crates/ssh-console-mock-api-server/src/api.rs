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

use carbide_version::v;
use tonic::{Request, Response, Status};
use uuid::Uuid;

use crate::MockApiServer;
use crate::generated::forge::forge_server::Forge;
use crate::generated::forge::{
    BmcMetaDataGetResponse, BuildInfo, InstanceList, InstancesByIdsRequest,
    ValidateTenantPublicKeyRequest, ValidateTenantPublicKeyResponse, VersionRequest,
};
use crate::generated::{common, forge};

#[tonic::async_trait]
impl Forge for MockApiServer {
    async fn version(
        &self,
        _request: Request<VersionRequest>,
    ) -> Result<Response<BuildInfo>, Status> {
        Ok(Response::new(BuildInfo {
            build_version: v!(build_version).to_string(),
            build_date: v!(build_date).to_string(),
            git_sha: v!(git_sha).to_string(),
            rust_version: v!(rust_version).to_string(),
            build_user: v!(build_user).to_string(),
            build_hostname: v!(build_hostname).to_string(),
            capabilities: vec![],
            runtime_config: None,
        }))
    }

    async fn validate_tenant_public_key(
        &self,
        request: Request<ValidateTenantPublicKeyRequest>,
    ) -> Result<Response<ValidateTenantPublicKeyResponse>, Status> {
        let request = request.into_inner();
        let Ok(instance_id) = request.instance_id.parse::<Uuid>() else {
            return Err(Status::invalid_argument("invalid instance ID"));
        };

        let Some(mock_host) = self
            .mock_hosts
            .iter()
            .find(|host| host.instance_id == instance_id)
        else {
            return Err(Status::not_found(format!(
                "no instance found with ID {instance_id}"
            )));
        };

        let pub_key_split = mock_host
            .tenant_public_key
            .split_ascii_whitespace()
            .collect::<Vec<_>>();
        let pub_key_base64 = if pub_key_split.len() == 1 {
            pub_key_split[0]
        } else {
            pub_key_split[1]
        };

        if pub_key_base64 == request.tenant_public_key {
            Ok(Response::new(ValidateTenantPublicKeyResponse {}))
        } else {
            Err(Status::internal("public key does not match"))
        }
    }

    async fn find_instances_by_ids(
        &self,
        request: Request<InstancesByIdsRequest>,
    ) -> Result<Response<InstanceList>, Status> {
        let request = request.into_inner();
        let mock_instances = request
            .instance_ids
            .iter()
            .filter_map(|instance_id| {
                self.mock_hosts
                    .iter()
                    .find(|h| {
                        h.instance_id.to_string().to_lowercase() == instance_id.value.to_lowercase()
                    })
                    .and_then(|h| {
                        carbide_uuid::machine::StableHostMachineId::try_from(h.machine_id)
                            .ok()
                            .map(|machine_id| (instance_id, machine_id))
                    })
            })
            .collect::<Vec<_>>();

        let instances = mock_instances
            .into_iter()
            .map(|(instance_id, machine_id)| forge::Instance {
                id: Some(instance_id.clone()),
                machine_id: Some(machine_id),
                ..Default::default()
            })
            .collect::<Vec<_>>();

        Ok(Response::new(forge::InstanceList { instances }))
    }

    async fn get_bmc_meta_data(
        &self,
        request: tonic::Request<forge::BmcMetaDataGetRequest>,
    ) -> std::result::Result<tonic::Response<forge::BmcMetaDataGetResponse>, tonic::Status> {
        let request = request.into_inner();
        let Some(machine_id) = request.machine_id else {
            return Err(Status::invalid_argument("missing machine ID"));
        };

        let Some(mock_host) = self
            .mock_hosts
            .iter()
            .find(|mock_host| mock_host.machine_id == machine_id)
        else {
            return Err(Status::not_found("no machine with that ID"));
        };

        Ok(Response::new(BmcMetaDataGetResponse {
            ip: mock_host.bmc_ip.to_string(),
            user: mock_host.bmc_user.clone(),
            password: mock_host.bmc_password.clone(),
            serial_console_ssh_port: mock_host.serial_console_ssh_port.map(Into::into),
            ipmi_port: mock_host.ipmi_port.map(Into::into),
            vendor: Some(mock_host.sys_vendor.to_string()),
            ..Default::default()
        }))
    }

    async fn find_machine_ids(
        &self,
        _request: Request<forge::MachineSearchConfig>,
    ) -> Result<Response<common::MachineIdList>, Status> {
        Ok(Response::new(common::MachineIdList {
            machine_ids: self
                .mock_hosts
                .iter()
                .map(|mock_host| mock_host.machine_id)
                .collect(),
        }))
    }
}

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
use carbide_uuid::vpc::VpcId;
use rpc::forge_server::Forge;

use super::TestEnv;
use crate::tests::common::api_fixtures::instance::default_tenant_config;
use crate::tests::common::rpc_builder::VpcCreationRequest;

pub(in crate::tests) async fn create_vpc(
    env: &TestEnv,
    name: String,
    tenant_org_id: Option<String>,
    vpc_metadata: Option<rpc::Metadata>,
) -> (VpcId, rpc::Vpc) {
    let tenant_config = default_tenant_config();

    let vpc_id = VpcId::new();
    let request =
        VpcCreationRequest::builder(tenant_org_id.unwrap_or(tenant_config.tenant_organization_id))
            .id(vpc_id)
            .metadata(rpc::Metadata {
                name,
                description: vpc_metadata
                    .as_ref()
                    .map_or("".to_string(), |s| s.description.clone()),
                labels: vpc_metadata
                    .as_ref()
                    .map_or(Vec::new(), |s| s.labels.clone()),
            })
            .tonic_request();

    let response = env.api.create_vpc(request).await;
    let vpc = response.unwrap().into_inner();

    (vpc_id, vpc)
}

/// Creates a Flat VPC for the given (or default) tenant.
pub(in crate::tests) async fn create_flat_vpc(
    env: &TestEnv,
    name: String,
    tenant_org_id: Option<String>,
) -> (VpcId, rpc::Vpc) {
    let tenant_config = default_tenant_config();
    let vpc_id = VpcId::new();
    let request =
        VpcCreationRequest::builder(tenant_org_id.unwrap_or(tenant_config.tenant_organization_id))
            .id(vpc_id)
            .network_virtualization_type(rpc::VpcVirtualizationType::Flat as i32)
            .metadata(rpc::Metadata {
                name,
                ..Default::default()
            })
            .tonic_request();

    let vpc = env
        .api
        .create_vpc(request)
        .await
        .expect("create_flat_vpc should succeed")
        .into_inner();
    (vpc_id, vpc)
}

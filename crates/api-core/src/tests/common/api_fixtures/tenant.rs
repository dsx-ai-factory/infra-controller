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
use rpc::forge_server::Forge;

use super::TestEnv;

/// Creates a tenant using the test site's default tenant routing behavior.
pub(in crate::tests) async fn create_fixture_tenant(
    env: &TestEnv,
    organization_id: impl Into<String>,
) -> Result<rpc::Tenant, tonic::Status> {
    let organization_id = organization_id.into();

    // Let the API apply the runtime default routing-profile behavior.
    let response = env
        .api
        .create_tenant(tonic::Request::new(rpc::CreateTenantRequest {
            organization_id: organization_id.clone(),
            routing_profile_type: None,
            metadata: Some(rpc::Metadata {
                name: organization_id,
                ..Default::default()
            }),
        }))
        .await?;

    Ok(response
        .into_inner()
        .tenant
        .expect("created tenant response must include tenant"))
}

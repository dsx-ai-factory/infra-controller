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

use carbide_uuid::infiniband::IBPartitionId;
use tonic::Request;

use super::TestEnv;
use crate::api::rpc::forge_server::Forge;
use crate::api::rpc::{IbPartitionConfig, IbPartitionCreationRequest};

pub(in crate::tests) const DEFAULT_TENANT: &str = "Tenant1";

pub(in crate::tests) async fn create_ib_partition(
    env: &TestEnv,
    name: String,
    tenant: String,
) -> (IBPartitionId, rpc::IbPartition) {
    let ib_partition = env
        .api
        .create_ib_partition(Request::new(IbPartitionCreationRequest {
            id: None,
            config: Some(IbPartitionConfig {
                name: name.clone(),
                tenant_organization_id: tenant,
                pkey: None,
            }),
            metadata: Some(rpc::Metadata {
                name,
                labels: Default::default(),
                description: "".to_string(),
            }),
        }))
        .await
        .unwrap()
        .into_inner();

    let ib_partition_id = ib_partition.id.expect("Missing ib partition ID");

    env.run_ib_partition_controller_iteration().await;

    let ib_partition = env
        .api
        .find_ib_partitions_by_ids(Request::new(rpc::forge::IbPartitionsByIdsRequest {
            ib_partition_ids: vec![ib_partition_id],
            include_history: false,
        }))
        .await
        .unwrap()
        .into_inner()
        .ib_partitions
        .remove(0);

    // check the IB partition status to make sure it is ready.
    let status = ib_partition.status.clone().unwrap();
    assert_eq!(status.state, rpc::TenantState::Ready as i32);

    (ib_partition_id, ib_partition)
}

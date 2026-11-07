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

use ::rpc::forge::{self as rpc};
use carbide_uuid::nvlink::NvLinkLogicalPartitionId;
use tonic::Request;

use super::TestEnv;
use crate::api::rpc::forge_server::Forge;
use crate::api::rpc::{NvLinkLogicalPartitionConfig, NvLinkLogicalPartitionCreationRequest};

pub(in crate::tests) struct NvlLogicalPartitionFixture {
    pub(in crate::tests) id: NvLinkLogicalPartitionId,
    pub(in crate::tests) logical_partition: rpc::NvLinkLogicalPartition,
}

pub(in crate::tests) async fn create_nvl_logical_partition(
    env: &TestEnv,
    name: String,
) -> NvlLogicalPartitionFixture {
    let partition = env
        .api
        .create_nv_link_logical_partition(Request::new(NvLinkLogicalPartitionCreationRequest {
            id: None,
            config: Some(NvLinkLogicalPartitionConfig {
                metadata: Some(rpc::Metadata {
                    name,
                    ..Default::default()
                }),
                tenant_organization_id: "example".to_string(),
            }),
        }))
        .await
        .unwrap()
        .into_inner();

    let partition_id = partition.id.expect("Missing nvlink logical partition ID");

    let logical_partition = env
        .api
        .find_nv_link_logical_partitions_by_ids(Request::new(
            rpc::NvLinkLogicalPartitionsByIdsRequest {
                partition_ids: vec![partition_id],
                include_history: false,
            },
        ))
        .await
        .unwrap()
        .into_inner()
        .partitions
        .remove(0);

    NvlLogicalPartitionFixture {
        id: partition_id,
        logical_partition,
    }
}

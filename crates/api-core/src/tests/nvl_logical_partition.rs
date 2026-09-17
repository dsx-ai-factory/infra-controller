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

//use crate::tests::common;
//use crate::tests::common::api_fixtures::TestEnvOverrides;
use ::rpc::forge as rpc;
use config_version::ConfigVersion;
use rpc::TenantState;
use rpc::forge_server::Forge;

use crate::tests::common::api_fixtures::create_test_env;
use crate::tests::common::api_fixtures::nvl_logical_partition::{
    NvlLogicalPartitionFixture, create_nvl_logical_partition,
};

#[crate::sqlx_test]

async fn test_find_nvl_logical_partition_ids(pool: sqlx::PgPool) {
    let env = create_test_env(pool.clone()).await;

    for i in 0..6 {
        let NvlLogicalPartitionFixture {
            id: _id,
            logical_partition: _partition,
        } = create_nvl_logical_partition(&env, format!("partition_{i}")).await;
    }

    // test getting all ids
    let request_all = tonic::Request::new(rpc::NvLinkLogicalPartitionSearchFilter { name: None });

    let ids_all = env
        .api
        .find_nv_link_logical_partition_ids(request_all)
        .await
        .map(|response| response.into_inner())
        .unwrap();
    assert_eq!(ids_all.partition_ids.len(), 6);

    // test getting ids based on name
    let request_name = tonic::Request::new(rpc::NvLinkLogicalPartitionSearchFilter {
        name: Some("partition_5".to_string()),
    });

    let ids_name = env
        .api
        .find_nv_link_logical_partition_ids(request_name)
        .await
        .map(|response| response.into_inner())
        .unwrap();
    assert_eq!(ids_name.partition_ids.len(), 1);
}

#[crate::sqlx_test]
async fn test_find_nvl_logical_partitions_by_ids(pool: sqlx::PgPool) {
    let env = create_test_env(pool.clone()).await;

    let mut partition3 = rpc::NvLinkLogicalPartition::default();
    for i in 0..6 {
        let NvlLogicalPartitionFixture {
            id: _id,
            logical_partition: partition,
        } = create_nvl_logical_partition(&env, format!("partition_{i}")).await;
        if i == 3 {
            partition3 = partition;
        }
    }

    let request_ids = tonic::Request::new(rpc::NvLinkLogicalPartitionSearchFilter {
        name: Some("partition_3".to_string()),
    });

    let ids_list = env
        .api
        .find_nv_link_logical_partition_ids(request_ids)
        .await
        .map(|response| response.into_inner())
        .unwrap();
    assert_eq!(ids_list.partition_ids.len(), 1);

    let request_partitions = tonic::Request::new(rpc::NvLinkLogicalPartitionsByIdsRequest {
        partition_ids: ids_list.partition_ids,
        include_history: false,
    });

    let partition_list = env
        .api
        .find_nv_link_logical_partitions_by_ids(request_partitions)
        .await
        .map(|response| response.into_inner())
        .unwrap();
    assert_eq!(partition_list.partitions.len(), 1);

    let clone3 = partition_list.partitions[0].clone();
    assert_eq!(partition3.id, clone3.id);
    assert_eq!(
        partition3.config.unwrap().metadata.unwrap().name,
        clone3.config.unwrap().metadata.unwrap().name
    );
    let status = clone3.status.unwrap();
    assert_eq!(
        TenantState::try_from(status.state).unwrap(),
        TenantState::Ready
    );
}

#[crate::sqlx_test]
async fn test_delete_nvl_logical_partitions_by_ids(pool: sqlx::PgPool) {
    let env = create_test_env(pool.clone()).await;

    let NvlLogicalPartitionFixture {
        id,
        logical_partition: partition,
    } = create_nvl_logical_partition(&env, "partition3".to_string()).await;

    let request_ids = tonic::Request::new(rpc::NvLinkLogicalPartitionSearchFilter {
        // name: Some("partition3".to_string()),
        name: None,
    });

    let ids_list = env
        .api
        .find_nv_link_logical_partition_ids(request_ids)
        .await
        .map(|response| response.into_inner())
        .unwrap();
    assert_eq!(ids_list.partition_ids.len(), 1);

    env.api
        .delete_nv_link_logical_partition(tonic::Request::new(
            rpc::NvLinkLogicalPartitionDeletionRequest { id: Some(id) },
        ))
        .await
        .expect("expect deletion to succeed");

    let request_partitions = tonic::Request::new(rpc::NvLinkLogicalPartitionsByIdsRequest {
        partition_ids: ids_list.partition_ids,
        include_history: false,
    });

    let partition_list = env
        .api
        .find_nv_link_logical_partitions_by_ids(request_partitions)
        .await
        .map(|response| response.into_inner())
        .unwrap();
    assert_eq!(partition_list.partitions.len(), 1);

    let clone3 = partition_list.partitions[0].clone();
    assert_eq!(id, clone3.id.unwrap());
    assert_eq!(
        partition.config.unwrap().metadata.unwrap().name,
        clone3.config.unwrap().metadata.unwrap().name
    );
    let status = clone3.status.unwrap();
    assert_eq!(
        TenantState::try_from(status.state).unwrap(),
        TenantState::Terminating
    );
}

#[crate::sqlx_test]
async fn test_update_nvl_logical_partition(pool: sqlx::PgPool) {
    let env = create_test_env(pool.clone()).await;

    let NvlLogicalPartitionFixture {
        id,
        logical_partition: _partition,
    } = create_nvl_logical_partition(&env, "partition3".to_string()).await;

    let request_ids = tonic::Request::new(rpc::NvLinkLogicalPartitionSearchFilter { name: None });

    let ids_list = env
        .api
        .find_nv_link_logical_partition_ids(request_ids)
        .await
        .map(|response| response.into_inner())
        .unwrap();
    assert_eq!(ids_list.partition_ids.len(), 1);

    let request_partitions = tonic::Request::new(rpc::NvLinkLogicalPartitionsByIdsRequest {
        partition_ids: ids_list.partition_ids,
        include_history: false,
    });
    let partition_list = env
        .api
        .find_nv_link_logical_partitions_by_ids(request_partitions)
        .await
        .map(|response| response.into_inner())
        .unwrap();
    assert_eq!(partition_list.partitions.len(), 1);

    let partition = partition_list.partitions[0].clone();
    let old_version: ConfigVersion = partition.config_version.parse().unwrap();

    let config = rpc::NvLinkLogicalPartitionConfig {
        metadata: Some(rpc::Metadata {
            name: "new_partition3".to_string(),
            ..partition.config.clone().unwrap().metadata.unwrap()
        }),
        ..partition.config.unwrap()
    };
    env.api
        .update_nv_link_logical_partition(tonic::Request::new(
            rpc::NvLinkLogicalPartitionUpdateRequest {
                id: Some(id),
                config: Some(config),
                if_version_match: None,
            },
        ))
        .await
        .expect("expect update to succeed");

    let request_ids = tonic::Request::new(rpc::NvLinkLogicalPartitionSearchFilter { name: None });
    let ids_list = env
        .api
        .find_nv_link_logical_partition_ids(request_ids)
        .await
        .map(|response| response.into_inner())
        .unwrap();
    assert_eq!(ids_list.partition_ids.len(), 1);

    let request_partitions = tonic::Request::new(rpc::NvLinkLogicalPartitionsByIdsRequest {
        partition_ids: ids_list.partition_ids,
        include_history: false,
    });

    let partition_list = env
        .api
        .find_nv_link_logical_partitions_by_ids(request_partitions)
        .await
        .map(|response| response.into_inner())
        .unwrap();
    assert_eq!(partition_list.partitions.len(), 1);

    let clone3 = partition_list.partitions[0].clone();
    assert_eq!(id, clone3.id.unwrap());
    let new_version: ConfigVersion = clone3.config_version.parse().unwrap();
    assert_eq!(new_version.version_nr(), old_version.version_nr() + 1);
    assert_eq!(
        "new_partition3".to_string(),
        clone3.config.unwrap().metadata.unwrap().name
    );
}

#[crate::sqlx_test]
async fn test_update_nvl_logical_partition_rejects_reused_version(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let NvlLogicalPartitionFixture {
        id,
        logical_partition: partition,
    } = create_nvl_logical_partition(&env, "partition".to_string()).await;

    let mut config = partition.config.unwrap();
    config.metadata.as_mut().unwrap().name = "accepted".to_string();
    let mut request = rpc::NvLinkLogicalPartitionUpdateRequest {
        id: Some(id),
        config: Some(config.clone()),
        if_version_match: Some(partition.config_version.clone()),
    };
    env.api
        .update_nv_link_logical_partition(tonic::Request::new(request.clone()))
        .await
        .expect("the current version must allow the metadata update");

    request
        .config
        .as_mut()
        .unwrap()
        .metadata
        .as_mut()
        .unwrap()
        .name = "rejected".to_string();
    let error = env
        .api
        .update_nv_link_logical_partition(tonic::Request::new(request))
        .await
        .expect_err("the previous version must not allow another metadata update");
    assert_eq!(error.code(), tonic::Code::FailedPrecondition);
    assert_eq!(
        error.message(),
        crate::CarbideError::ConcurrentModificationError(
            "LogicalPartition",
            partition.config_version,
        )
        .to_string()
    );

    let persisted = env
        .api
        .find_nv_link_logical_partitions_by_ids(tonic::Request::new(
            rpc::NvLinkLogicalPartitionsByIdsRequest {
                partition_ids: vec![id],
                include_history: false,
            },
        ))
        .await
        .unwrap()
        .into_inner()
        .partitions
        .remove(0);
    assert_eq!(persisted.config, Some(config));
}

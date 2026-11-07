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
use carbide_ib_fabric::config::IBFabricConfig;
use rpc::forge_server::Forge;

use crate::tests::common;
use crate::tests::common::api_fixtures::ib_partition::create_ib_partition;
use crate::tests::common::api_fixtures::{TestEnvOverrides, create_test_env};

#[crate::sqlx_test]
async fn test_find_ib_partition_ids(pool: sqlx::PgPool) {
    let env = create_test_env(pool.clone()).await;

    for i in 0..6 {
        let mut tenant_org_id = "tenant_org_1";
        if i % 2 != 0 {
            tenant_org_id = "tenant_org_2";
        }
        let (_id, _partition) =
            create_ib_partition(&env, format!("partition_{i}"), tenant_org_id.to_string()).await;
    }

    // test getting all ids
    let request_all = tonic::Request::new(rpc::IbPartitionSearchFilter {
        name: None,
        tenant_org_id: None,
    });

    let ids_all = env
        .api
        .find_ib_partition_ids(request_all)
        .await
        .map(|response| response.into_inner())
        .unwrap();
    assert_eq!(ids_all.ib_partition_ids.len(), 6);

    // test getting ids based on name
    let request_name = tonic::Request::new(rpc::IbPartitionSearchFilter {
        name: Some("partition_5".to_string()),
        tenant_org_id: None,
    });

    let ids_name = env
        .api
        .find_ib_partition_ids(request_name)
        .await
        .map(|response| response.into_inner())
        .unwrap();
    assert_eq!(ids_name.ib_partition_ids.len(), 1);

    // test search by tenant_org_id
    let request_tenant = tonic::Request::new(rpc::IbPartitionSearchFilter {
        name: None,
        tenant_org_id: Some("tenant_org_2".to_string()),
    });

    let ids_tenant = env
        .api
        .find_ib_partition_ids(request_tenant)
        .await
        .map(|response| response.into_inner())
        .unwrap();
    assert_eq!(ids_tenant.ib_partition_ids.len(), 3);

    // test search by tenant_org_id and name
    let request_tenant_name = tonic::Request::new(rpc::IbPartitionSearchFilter {
        name: Some("partition_4".to_string()),
        tenant_org_id: Some("tenant_org_1".to_string()),
    });

    let ids_tenant_name = env
        .api
        .find_ib_partition_ids(request_tenant_name)
        .await
        .map(|response| response.into_inner())
        .unwrap();
    assert_eq!(ids_tenant_name.ib_partition_ids.len(), 1);
}

#[crate::sqlx_test]
async fn test_find_ib_partitions_by_ids(pool: sqlx::PgPool) {
    let mut config = common::api_fixtures::get_config();
    config.ib_config = Some(IBFabricConfig {
        enabled: true,
        ..Default::default()
    });

    let env = common::api_fixtures::create_test_env_with_overrides(
        pool,
        TestEnvOverrides::with_config(config),
    )
    .await;

    let mut partition3 = rpc::IbPartition::default();
    for i in 0..6 {
        let mut tenant_org_id = "tenant_org_1";
        if i % 2 != 0 {
            tenant_org_id = "tenant_org_2";
        }
        let (_id, partition) =
            create_ib_partition(&env, format!("partition_{i}"), tenant_org_id.to_string()).await;
        if i == 3 {
            partition3 = partition;
        }
    }

    let request_ids = tonic::Request::new(rpc::IbPartitionSearchFilter {
        name: Some("partition_3".to_string()),
        tenant_org_id: None,
    });

    let ids_list = env
        .api
        .find_ib_partition_ids(request_ids)
        .await
        .map(|response| response.into_inner())
        .unwrap();
    assert_eq!(ids_list.ib_partition_ids.len(), 1);

    let request_partitions = tonic::Request::new(rpc::IbPartitionsByIdsRequest {
        ib_partition_ids: ids_list.ib_partition_ids,
        include_history: false,
    });

    let partition_list = env
        .api
        .find_ib_partitions_by_ids(request_partitions)
        .await
        .map(|response| response.into_inner())
        .unwrap();
    assert_eq!(partition_list.ib_partitions.len(), 1);

    let partition3config = partition3.config.unwrap();
    let part3_list = partition_list.ib_partitions[0].clone();
    let clone3config = part3_list.config.unwrap();
    assert_eq!(
        partition3.metadata.unwrap().name,
        part3_list.metadata.unwrap().name
    );
    assert_eq!(
        partition3config.tenant_organization_id,
        clone3config.tenant_organization_id
    );
}

// The empty-list and over-max guards for `find_ib_partitions_by_ids` are shared
// API-layer code, proven once across representative RPCs in
// `tests::find_by_ids_guards`. The shared test exercises this RPC under the
// default env: the guard fires on the ID-list length before any IB-config check.

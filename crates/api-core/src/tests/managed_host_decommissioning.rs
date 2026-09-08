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

use rpc::forge::DecommissionManagedHostRequest;
use rpc::forge::forge_server::Forge;
use tonic::{Code, Request};

use crate::tests::common::api_fixtures::{create_managed_host, create_test_env};

#[crate::sqlx_test]
async fn decommission_requires_redfish_bfb_install_support(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let managed_host = create_managed_host(&env).await;
    let dpu_id = managed_host.dpu().id;

    sqlx::query(
        "UPDATE machine_topologies
         SET topology = jsonb_set(
             topology,
             '{bmc_info,firmware_version}',
             to_jsonb('BF-24.04-1'::text)
         )
         WHERE machine_id = $1",
    )
    .bind(dpu_id)
    .execute(&env.pool)
    .await
    .unwrap();

    let error = env
        .api
        .decommission_managed_host(Request::new(DecommissionManagedHostRequest {
            machine_id: Some(managed_host.id.into()),
        }))
        .await
        .unwrap_err();
    assert_eq!(error.code(), Code::FailedPrecondition);
    assert!(error.message().contains(&dpu_id.to_string()));
    assert!(error.message().contains("BF-24.04-1"));
    assert!(
        error
            .message()
            .contains("dpus do not support bfb installation")
    );

    let mut txn = env.pool.begin().await.unwrap();
    let host = managed_host.host().db_machine(&mut txn).await;
    assert!(!host.decommission_requested);
    txn.commit().await.unwrap();

    sqlx::query(
        "UPDATE machine_topologies
         SET topology = jsonb_set(
             topology,
             '{bmc_info,firmware_version}',
             to_jsonb('BF-24.10-0'::text)
         )
         WHERE machine_id = $1",
    )
    .bind(dpu_id)
    .execute(&env.pool)
    .await
    .unwrap();

    env.api
        .decommission_managed_host(Request::new(DecommissionManagedHostRequest {
            machine_id: Some(managed_host.id.into()),
        }))
        .await
        .unwrap();

    let mut txn = env.pool.begin().await.unwrap();
    let host = managed_host.host().db_machine(&mut txn).await;
    assert!(host.decommission_requested);
    txn.commit().await.unwrap();
}

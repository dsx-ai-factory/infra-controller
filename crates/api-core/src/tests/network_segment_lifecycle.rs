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

use std::time::Duration;

use carbide_uuid::network::NetworkSegmentId;
use common::api_fixtures::network_segment::create_network_segment;
use common::api_fixtures::{TestEnvOverrides, create_test_env, create_test_env_with_overrides};
use common::network_segment::{
    create_network_segment_with_api, get_segment_state, tenant_state_from_segment, text_history,
};
use rpc::forge::forge_server::Forge;
use tonic::Request;

use crate::tests::common;

async fn test_network_segment_lifecycle_impl(
    pool: sqlx::PgPool,
    use_subdomain: bool,
    use_vpc: bool,
    seg_type: i32,
    num_reserved: i32,
    test_num_free_ips: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env_with_overrides(pool, TestEnvOverrides::no_network_segments()).await;

    let segment =
        create_network_segment_with_api(&env, use_subdomain, use_vpc, None, seg_type, num_reserved)
            .await;
    assert!(segment.created.is_some());
    assert!(segment.deleted.is_none());
    assert_eq!(
        tenant_state_from_segment(&segment),
        rpc::forge::TenantState::Provisioning
    );
    assert_eq!(
        segment
            .config
            .as_ref()
            .map(|c| c.segment_type)
            .unwrap_or_default(),
        seg_type
    );
    let segment_id: NetworkSegmentId = segment.id.unwrap();
    let _: uuid::Uuid = segment
        .config
        .as_ref()
        .unwrap()
        .prefixes
        .first()
        .unwrap()
        .id
        .unwrap()
        .into();

    assert_eq!(
        get_segment_state(&env.api, segment_id).await,
        rpc::forge::TenantState::Provisioning
    );

    env.run_network_segment_controller_iteration().await;
    env.run_network_segment_controller_iteration().await;

    assert_eq!(
        get_segment_state(&env.api, segment_id).await,
        rpc::forge::TenantState::Ready
    );

    if test_num_free_ips {
        let segments = env
            .api
            .find_network_segments_by_ids(Request::new(rpc::forge::NetworkSegmentsByIdsRequest {
                network_segments_ids: vec![segment_id],
                include_history: false,
                include_num_free_ips: true,
            }))
            .await
            .unwrap()
            .into_inner()
            .network_segments;

        assert_eq!(segments.len(), 1);
        let prefixes = segments[0]
            .config
            .as_ref()
            .map(|c| c.prefixes.as_slice())
            .unwrap_or(&[]);
        assert_eq!(prefixes.len(), 1);
        let expected_count = 255 - u64::try_from(num_reserved).unwrap();
        assert_eq!(
            prefixes[0].free_ip_count,
            u32::try_from(expected_count).unwrap()
        );
        assert_eq!(prefixes[0].free_ip_count_v2, Some(expected_count));
        assert!(!prefixes[0].free_ip_count_saturated);
    }

    env.api
        .delete_network_segment(Request::new(rpc::forge::NetworkSegmentDeletionRequest {
            id: segment.id,
        }))
        .await
        .expect("expect deletion to succeed");

    // After the API request, the segment should show up as deleting
    assert_eq!(
        get_segment_state(&env.api, segment_id).await,
        rpc::forge::TenantState::Terminating
    );

    // Calling the API again in this state should be a noop
    env.api
        .delete_network_segment(Request::new(rpc::forge::NetworkSegmentDeletionRequest {
            id: segment.id,
        }))
        .await
        .expect("expect deletion to succeed");

    // Make the controller aware about termination too
    env.run_network_segment_controller_iteration().await;

    // Wait for the drain period
    tokio::time::sleep(Duration::from_secs(1)).await;

    // delete the segment
    env.run_network_segment_controller_iteration().await;
    env.run_network_segment_controller_iteration().await;

    let segments = env
        .api
        .find_network_segments_by_ids(Request::new(rpc::forge::NetworkSegmentsByIdsRequest {
            network_segments_ids: vec![segment_id],
            include_num_free_ips: false,
            include_history: false,
        }))
        .await
        .unwrap()
        .into_inner()
        .network_segments;
    assert!(segments.is_empty(), "Found network segments {segments:?}");

    // After the segment is fully gone, deleting it again should return NotFound
    // Calling the API again in this state should be a noop
    let err = env
        .api
        .delete_network_segment(Request::new(rpc::forge::NetworkSegmentDeletionRequest {
            id: segment.id,
        }))
        .await
        .expect_err("expect deletion to fail");
    assert_eq!(err.code(), tonic::Code::NotFound);
    assert_eq!(
        err.message(),
        format!("network segment not found: {}", segment.id.unwrap())
    );

    let mut txn = env.pool.begin().await.unwrap();
    let expected_history = ["provisioning", "ready", "drainallocatedips", "dbdelete"];
    let history = text_history(&mut txn, segment_id).await;
    for (i, state) in history.iter().enumerate() {
        assert!(state.contains(expected_history[i]));
    }
    txn.commit().await.unwrap();

    Ok(())
}

#[crate::sqlx_test]
async fn test_network_segment_lifecycle(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    test_network_segment_lifecycle_impl(
        pool,
        false,
        false,
        rpc::forge::NetworkSegmentType::Admin as i32,
        1,
        false,
    )
    .await
}

#[crate::sqlx_test]
async fn test_network_segment_lifecycle_with_vpc(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    test_network_segment_lifecycle_impl(
        pool,
        false,
        true,
        rpc::forge::NetworkSegmentType::Admin as i32,
        1,
        false,
    )
    .await
}

#[crate::sqlx_test]
async fn test_network_segment_lifecycle_with_domain(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    test_network_segment_lifecycle_impl(
        pool,
        true,
        false,
        rpc::forge::NetworkSegmentType::Admin as i32,
        1,
        false,
    )
    .await
}

#[crate::sqlx_test]
async fn test_network_segment_lifecycle_with_vpc_and_domain(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    test_network_segment_lifecycle_impl(
        pool,
        true,
        true,
        rpc::forge::NetworkSegmentType::Admin as i32,
        1,
        false,
    )
    .await
}

#[crate::sqlx_test]
async fn test_admin_network_exists(pool: sqlx::PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let mut txn = env.pool.begin().await?;

    let segments = db::network_segment::admin(&mut txn).await?;

    assert_eq!(segments[0].id, env.admin_segment());

    Ok(())
}

#[crate::sqlx_test]
async fn test_multiple_admin_network_segments_exist(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let first_admin_segment = env.admin_segment();

    // Create a second admin segment through the same API path operators use.
    let second_admin_segment = create_network_segment(
        &env.api,
        "ADMIN_2",
        "192.0.12.0/24",
        "192.0.12.1",
        rpc::forge::NetworkSegmentType::Admin,
        None,
        true,
    )
    .await;

    // Verify both admin segments can be found through the RPC find-by-ids path.
    let segments = env
        .api
        .find_network_segments_by_ids(Request::new(rpc::forge::NetworkSegmentsByIdsRequest {
            network_segments_ids: vec![first_admin_segment, second_admin_segment],
            include_history: false,
            include_num_free_ips: false,
        }))
        .await?
        .into_inner()
        .network_segments;

    let segment_ids = segments
        .iter()
        .map(|segment| segment.id.unwrap())
        .collect::<Vec<_>>();
    assert_eq!(segment_ids.len(), 2);
    assert!(segment_ids.contains(&first_admin_segment));
    assert!(segment_ids.contains(&second_admin_segment));

    // Verify the DB admin lookup returns both admin segments as candidates.
    let mut txn = env.pool.begin().await?;
    let admin_segments = db::network_segment::admin(&mut txn).await?;
    let admin_segment_ids = admin_segments
        .iter()
        .map(|segment| segment.id)
        .collect::<Vec<_>>();
    assert_eq!(admin_segment_ids.len(), 2);
    assert!(admin_segment_ids.contains(&first_admin_segment));
    assert!(admin_segment_ids.contains(&second_admin_segment));

    Ok(())
}

#[crate::sqlx_test]
async fn test_network_segment_admin_free_ips(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    test_network_segment_lifecycle_impl(
        pool,
        false,
        true,
        rpc::forge::NetworkSegmentType::Admin as i32,
        2,
        true,
    )
    .await
}

#[crate::sqlx_test]
async fn test_network_segment_tenant_free_ips(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    test_network_segment_lifecycle_impl(
        pool,
        false,
        true,
        rpc::forge::NetworkSegmentType::Tenant as i32,
        10,
        true,
    )
    .await
}

#[crate::sqlx_test]
async fn test_network_segment_underlay_free_ips(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    test_network_segment_lifecycle_impl(
        pool,
        false,
        true,
        rpc::forge::NetworkSegmentType::Underlay as i32,
        6,
        true,
    )
    .await
}

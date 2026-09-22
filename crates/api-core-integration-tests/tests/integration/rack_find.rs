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

use carbide_test_harness::prelude::*;
use carbide_uuid::rack::{RackId, RackProfileId};
use rpc::forge::{AdminForceDeleteRackRequest, DeleteRackRequest};
use tonic::Code;

#[sqlx_test]
async fn test_find_rack_by_id(pool: PgPool) {
    let env = TestHarness::builder(pool).build().await;
    let TestRack { id: rack_id1 } = env.create_rack(RackProfileId::new("rack")).await;
    let TestRack { id: rack_id2 } = env.create_rack(RackProfileId::new("rack")).await;

    // Check the returned list of rack ids is what we expect.
    let rack_ids: Vec<RackId> = env
        .api()
        .find_rack_ids(tonic::Request::new(rpc::forge::RackSearchFilter::default()))
        .await
        .unwrap()
        .into_inner()
        .rack_ids;
    assert_eq!(rack_ids.len(), 2);
    assert!(rack_ids.contains(&rack_id1));
    assert!(rack_ids.contains(&rack_id2));

    // Find the first Rack by its id; check core fields.
    let racks: Vec<rpc::forge::Rack> = env
        .api()
        .find_racks_by_ids(tonic::Request::new(rpc::forge::RacksByIdsRequest {
            rack_ids: vec![rack_id1.clone()],
        }))
        .await
        .unwrap()
        .into_inner()
        .racks;
    assert_eq!(racks.len(), 1);
    assert_eq!(racks[0].id, Some(rack_id1));
    assert_eq!(racks[0].rack_state, "Created");
    assert_eq!(
        racks[0]
            .status
            .as_ref()
            .unwrap()
            .lifecycle
            .as_ref()
            .unwrap()
            .state,
        r#"{"state":"created"}"#
    );
    assert!(racks[0].updated.is_some());
    assert!(racks[0].created.is_some());
    assert!(racks[0].deleted.is_none());
    assert!(!racks[0].version.is_empty());

    // Find the second Rack by its id; check core fields.
    let racks: Vec<rpc::forge::Rack> = env
        .api()
        .find_racks_by_ids(tonic::Request::new(rpc::forge::RacksByIdsRequest {
            rack_ids: vec![rack_id2.clone()],
        }))
        .await
        .unwrap()
        .into_inner()
        .racks;
    assert_eq!(racks.len(), 1);
    assert_eq!(racks[0].id, Some(rack_id2));
    assert_eq!(racks[0].rack_state, "Created");
    assert_eq!(
        racks[0]
            .status
            .as_ref()
            .unwrap()
            .lifecycle
            .as_ref()
            .unwrap()
            .state,
        r#"{"state":"created"}"#
    );
    assert!(racks[0].updated.is_some());
    assert!(racks[0].created.is_some());
    assert!(racks[0].deleted.is_none());
    assert!(!racks[0].version.is_empty());
}

#[sqlx_test]
async fn test_force_delete_rack_success(pool: PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let env = TestHarness::builder(pool.clone()).build().await;
    let TestRack { id: rack_id } = env.create_rack(RackProfileId::new("rack")).await;

    let mut txn = pool.begin().await?;
    db::state_history::persist(
        &mut txn,
        db::state_history::StateHistoryTableId::Rack,
        &rack_id,
        &"retained-before-force-delete",
        config_version::ConfigVersion::initial(),
    )
    .await?;
    txn.commit().await?;

    let response = env
        .api()
        .admin_force_delete_rack(tonic::Request::new(AdminForceDeleteRackRequest {
            rack_id: Some(rack_id.clone()),
        }))
        .await?
        .into_inner();

    assert_eq!(response.rack_id, rack_id.to_string());

    let racks = env
        .api()
        .find_racks_by_ids(tonic::Request::new(rpc::forge::RacksByIdsRequest {
            rack_ids: vec![rack_id.clone()],
        }))
        .await?
        .into_inner()
        .racks;

    assert!(racks.is_empty(), "Rack should be hard-deleted");

    let mut conn = pool.acquire().await?;
    let history = db::state_history::for_object(
        &mut conn,
        db::state_history::StateHistoryTableId::Rack,
        &rack_id,
    )
    .await?;
    assert!(
        history
            .iter()
            .any(|record| record.state == r#""retained-before-force-delete""#),
        "Rack state history should be retained",
    );

    Ok(())
}

#[sqlx_test]
async fn test_force_delete_rack_not_found(pool: PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let env = TestHarness::builder(pool).build().await;

    let non_existent_id: RackId = "MissingRack".parse().unwrap();
    let result = env
        .api()
        .admin_force_delete_rack(tonic::Request::new(AdminForceDeleteRackRequest {
            rack_id: Some(non_existent_id),
        }))
        .await;

    assert!(result.is_err());
    assert_eq!(result.unwrap_err().code(), Code::NotFound);

    Ok(())
}

#[sqlx_test]
async fn test_force_delete_rack_already_soft_deleted(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = TestHarness::builder(pool).build().await;
    let TestRack { id: rack_id } = env.create_rack(RackProfileId::new("rack")).await;

    env.api()
        .delete_rack(tonic::Request::new(DeleteRackRequest {
            id: rack_id.to_string(),
        }))
        .await?;

    let response = env
        .api()
        .admin_force_delete_rack(tonic::Request::new(AdminForceDeleteRackRequest {
            rack_id: Some(rack_id.clone()),
        }))
        .await?
        .into_inner();

    assert_eq!(response.rack_id, rack_id.to_string());

    let racks = env
        .api()
        .find_racks_by_ids(tonic::Request::new(rpc::forge::RacksByIdsRequest {
            rack_ids: vec![rack_id],
        }))
        .await?
        .into_inner()
        .racks;

    assert!(racks.is_empty(), "Rack should be hard-deleted");

    Ok(())
}

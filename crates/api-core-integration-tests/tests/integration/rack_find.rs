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

use carbide_secrets::credentials::{CredentialKey, Credentials};
use carbide_test_harness::prelude::*;
use carbide_uuid::rack::{RackId, RackProfileId};
use rpc::forge::{AdminForceDeleteRackRequest, DeleteRackRequest, DeletedFilter};
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
async fn test_find_rack_ids_filters_deleted_racks(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = TestHarness::builder(pool).build().await;
    let TestRack { id: rack_id } = env.create_rack(RackProfileId::new("rack")).await;

    env.api()
        .delete_rack(tonic::Request::new(DeleteRackRequest {
            id: rack_id.to_string(),
        }))
        .await?;

    let active_ids = env
        .api()
        .find_rack_ids(tonic::Request::new(rpc::forge::RackSearchFilter::default()))
        .await?
        .into_inner()
        .rack_ids;
    assert!(!active_ids.contains(&rack_id));

    let deleted_ids = env
        .api()
        .find_rack_ids(tonic::Request::new(rpc::forge::RackSearchFilter {
            deleted: DeletedFilter::Only as i32,
            ..Default::default()
        }))
        .await?
        .into_inner()
        .rack_ids;
    assert_eq!(deleted_ids, vec![rack_id]);

    Ok(())
}

#[sqlx_test]
async fn test_force_delete_rack_with_undecodable_health_reports_retains_history(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = TestHarness::builder(pool.clone()).build().await;
    let TestRack { id: rack_id } = env.create_rack(RackProfileId::new("rack")).await;
    let credential_manager = env.api().credential_manager();
    let token_key = CredentialKey::RackMaintenanceAccessToken {
        rack_id: rack_id.clone(),
    };
    let unrelated_key = CredentialKey::RackMaintenanceAccessToken {
        rack_id: RackId::new("unrelated-rack"),
    };
    let unrelated_token = Credentials::new("access_token", "unrelated-token");
    credential_manager
        .set_credentials(
            &token_key,
            &Credentials::new("access_token", "target-token"),
        )
        .await
        .expect("seed target maintenance token");
    credential_manager
        .set_credentials(&unrelated_key, &unrelated_token)
        .await
        .expect("seed unrelated maintenance token");

    let mut txn = pool.begin().await?;
    db::state_history::persist(
        &mut txn,
        db::state_history::StateHistoryTableId::Rack,
        &rack_id,
        &"retained-before-force-delete",
        config_version::ConfigVersion::initial(),
    )
    .await?;

    // Health reports are unused by force-delete, even when a normal `Rack` read
    // cannot decode them.
    let updated = sqlx::query("UPDATE racks SET health_reports = '{}'::jsonb WHERE id = $1")
        .bind(&rack_id)
        .execute(txn.as_mut())
        .await?;
    assert_eq!(updated.rows_affected(), 1);
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
    assert_eq!(
        credential_manager
            .get_credentials(&token_key)
            .await
            .expect("read target maintenance token"),
        None,
    );
    assert_eq!(
        credential_manager
            .get_credentials(&unrelated_key)
            .await
            .expect("read unrelated maintenance token"),
        Some(unrelated_token),
    );

    Ok(())
}

#[sqlx_test]
async fn test_force_delete_rack_not_found(pool: PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let env = TestHarness::builder(pool).build().await;

    let non_existent_id: RackId = "MissingRack".parse().unwrap();
    let credential_manager = env.api().credential_manager();
    let token_key = CredentialKey::RackMaintenanceAccessToken {
        rack_id: non_existent_id.clone(),
    };
    let token = Credentials::new("access_token", "missing-rack-token");
    credential_manager
        .set_credentials(&token_key, &token)
        .await
        .expect("seed missing rack's maintenance token");
    let result = env
        .api()
        .admin_force_delete_rack(tonic::Request::new(AdminForceDeleteRackRequest {
            rack_id: Some(non_existent_id),
        }))
        .await;

    assert!(result.is_err());
    assert_eq!(result.unwrap_err().code(), Code::NotFound);
    assert_eq!(
        credential_manager
            .get_credentials(&token_key)
            .await
            .expect("read missing rack's maintenance token"),
        Some(token),
    );

    Ok(())
}

#[sqlx_test]
async fn test_force_delete_rack_commit_failure_preserves_rack_and_token(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = TestHarness::builder(pool.clone()).build().await;
    let TestRack { id: rack_id } = env.create_rack(RackProfileId::new("rack")).await;
    let credential_manager = env.api().credential_manager();
    let token_key = CredentialKey::RackMaintenanceAccessToken {
        rack_id: rack_id.clone(),
    };
    let token = Credentials::new("access_token", "retained-on-commit-failure");
    credential_manager
        .set_credentials(&token_key, &token)
        .await
        .expect("seed maintenance token before commit failure");

    // The deferred foreign key lets DELETE succeed but rejects COMMIT.
    // It exists only in this test's isolated database.
    sqlx::raw_sql(
        "CREATE TABLE rack_delete_commit_guard (
             rack_id varchar(64) NOT NULL,
             CONSTRAINT test_rack_delete_commit_failure
                 FOREIGN KEY (rack_id) REFERENCES racks (id)
                 DEFERRABLE INITIALLY DEFERRED
         );",
    )
    .execute(&pool)
    .await?;
    sqlx::query("INSERT INTO rack_delete_commit_guard (rack_id) VALUES ($1)")
        .bind(&rack_id)
        .execute(&pool)
        .await?;

    let error = env
        .api()
        .admin_force_delete_rack(tonic::Request::new(AdminForceDeleteRackRequest {
            rack_id: Some(rack_id.clone()),
        }))
        .await
        .expect_err("deferred rack reference must reject commit");
    assert_eq!(error.code(), Code::Internal);
    assert_eq!(error.message(), "internal database error");
    let source = std::error::Error::source(&error)
        .expect("database status source")
        .source()
        .expect("original database error")
        .downcast_ref::<db::DatabaseError>()
        .expect("typed database error");
    assert!(
        source.violates_constraint("test_rack_delete_commit_failure"),
        "{source}"
    );
    assert!(
        source.to_string().contains("query=transaction commit."),
        "{source}"
    );

    let racks = env
        .api()
        .find_racks_by_ids(tonic::Request::new(rpc::forge::RacksByIdsRequest {
            rack_ids: vec![rack_id.clone()],
        }))
        .await?
        .into_inner()
        .racks;
    assert_eq!(racks.len(), 1);
    assert_eq!(racks[0].id, Some(rack_id));
    assert!(racks[0].deleted.is_none());
    assert_eq!(
        credential_manager
            .get_credentials(&token_key)
            .await
            .expect("read maintenance token after commit failure"),
        Some(token),
    );

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

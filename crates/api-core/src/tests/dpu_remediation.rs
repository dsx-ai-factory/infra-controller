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
use model::dpu_remediation::{
    ApproveRemediation, EnableRemediation, NewRemediation, RemediationApplicationStatus, Reviewer,
    RevokeRemediation,
};
use rpc::forge::CreateRemediationRequest;
use rpc::model::RpcTryFrom;

use crate::tests::common::api_fixtures::{create_managed_host_multi_dpu, create_test_env};
use crate::tests::common::postgres::wait_for_blocked_query;

#[test]
fn test_try_from_rpc() -> Result<(), Box<dyn std::error::Error>> {
    let bad_request_negative_retries = CreateRemediationRequest {
        retries: -1,
        ..Default::default()
    };
    let good_author = "good_author_string".to_string();

    assert!(
        NewRemediation::rpc_try_from((bad_request_negative_retries, good_author.clone())).is_err()
    );

    let mut bad_request_too_long_script = CreateRemediationRequest::default();
    let too_long_string = "x".repeat(2 << (13 + 1));
    bad_request_too_long_script.script = too_long_string;

    assert!(
        NewRemediation::rpc_try_from((bad_request_too_long_script, good_author.clone())).is_err()
    );

    let mut bad_request_no_script = CreateRemediationRequest::default();
    let empty_string = String::new();
    bad_request_no_script.script = empty_string;

    assert!(NewRemediation::rpc_try_from((bad_request_no_script, good_author.clone())).is_err());

    let good_request = CreateRemediationRequest {
        script: "echo 'hello world'".to_string(),
        ..Default::default()
    };

    assert!(NewRemediation::rpc_try_from((good_request, good_author)).is_ok());

    Ok(())
}
#[crate::sqlx_test(fixtures("create_dpu_remediation"))]
async fn test_dpu_remediations(pool: sqlx::PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let test_managed_host = create_managed_host_multi_dpu(&env, 2).await;
    let mut txn = env.pool.begin().await?;
    let mut all_dpus = test_managed_host.dpu_db_machines(&mut txn).await;
    let test_dpu_1 = all_dpus.pop().expect("missing first dpu?");
    let test_dpu_2 = all_dpus.pop().expect("missing second dpu?");

    let mut txn = env
        .pool
        .begin()
        .await
        .expect("unable to create transaction on database pool");

    let ids = db::dpu_remediation::find_remediation_ids(&mut txn).await?;
    let remediations = db::dpu_remediation::find_remediations_by_ids(&mut txn, &ids).await?;

    assert_eq!(ids.len(), remediations.len());

    let (enabled_remediations, disabled_remediations): (Vec<_>, Vec<_>) = remediations
        .into_iter()
        .partition(|remediation| remediation.enabled);

    assert_eq!(enabled_remediations.len(), 2);

    // first, we test that we can apply the same remediation to two different machines concurrently.
    let next_remediation_to_apply_1 =
        db::dpu_remediation::find_next_remediation_for_machine(&mut txn, test_dpu_1.id)
            .await?
            .expect("no remediation to apply?");
    let next_remediation_to_apply_2 =
        db::dpu_remediation::find_next_remediation_for_machine(&mut txn, test_dpu_2.id)
            .await?
            .expect("no remediation to apply?");

    db::dpu_remediation::remediation_applied(
        &mut txn,
        test_dpu_1.id,
        next_remediation_to_apply_1.id,
        RemediationApplicationStatus {
            succeeded: true,
            metadata: None,
        },
    )
    .await?;

    db::dpu_remediation::remediation_applied(
        &mut txn,
        test_dpu_2.id,
        next_remediation_to_apply_2.id,
        RemediationApplicationStatus {
            succeeded: true,
            metadata: None,
        },
    )
    .await?;

    // now we validate they're applied and successful
    let applied_remediations_1 =
        db::dpu_remediation::find_remediations_by_remediation_id_and_machine(
            &mut txn,
            next_remediation_to_apply_1.id,
            &test_dpu_1.id,
        )
        .await?;
    let applied_remediation_1 = applied_remediations_1
        .first()
        .expect("no applied remediation?");
    assert!(applied_remediation_1.succeeded);

    let applied_remediations_2 =
        db::dpu_remediation::find_remediations_by_remediation_id_and_machine(
            &mut txn,
            next_remediation_to_apply_2.id,
            &test_dpu_2.id,
        )
        .await?;
    let applied_remediation_2 = applied_remediations_2
        .first()
        .expect("no applied remediation?");
    assert!(applied_remediation_2.succeeded);

    // now we test the retry logic
    // first, apply the same remediation 3 times and fail it, and validate that the 4th attempt does not give a remediation back to the DPU.
    let next_remediation_to_apply_1 =
        db::dpu_remediation::find_next_remediation_for_machine(&mut txn, test_dpu_1.id)
            .await?
            .expect("no remediation to apply?");
    let same_id_1 = next_remediation_to_apply_1.id;
    db::dpu_remediation::remediation_applied(
        &mut txn,
        test_dpu_1.id,
        next_remediation_to_apply_1.id,
        RemediationApplicationStatus {
            succeeded: false,
            metadata: None,
        },
    )
    .await?;
    let next_remediation_to_apply_1 =
        db::dpu_remediation::find_next_remediation_for_machine(&mut txn, test_dpu_1.id)
            .await?
            .expect("no remediation to apply?");
    // it should give back the same id
    assert_eq!(same_id_1, next_remediation_to_apply_1.id.clone());

    db::dpu_remediation::remediation_applied(
        &mut txn,
        test_dpu_1.id,
        next_remediation_to_apply_1.id,
        RemediationApplicationStatus {
            succeeded: false,
            metadata: None,
        },
    )
    .await?;
    let next_remediation_to_apply_1 =
        db::dpu_remediation::find_next_remediation_for_machine(&mut txn, test_dpu_1.id)
            .await?
            .expect("no remediation to apply?");
    // it should give back the same id
    assert_eq!(same_id_1, next_remediation_to_apply_1.id.clone());

    db::dpu_remediation::remediation_applied(
        &mut txn,
        test_dpu_1.id,
        next_remediation_to_apply_1.id,
        RemediationApplicationStatus {
            succeeded: false,
            metadata: None,
        },
    )
    .await?;
    let next_remediation_to_apply_1 =
        db::dpu_remediation::find_next_remediation_for_machine(&mut txn, test_dpu_1.id).await?;
    assert!(next_remediation_to_apply_1.is_none());

    // finally, validate we can fail once, and then pass once, and then not get the remediation back after it succeeds
    let next_remediation_to_apply_2 =
        db::dpu_remediation::find_next_remediation_for_machine(&mut txn, test_dpu_2.id)
            .await?
            .expect("no remediation to apply?");
    let same_id_2 = next_remediation_to_apply_2.id;

    db::dpu_remediation::remediation_applied(
        &mut txn,
        test_dpu_2.id,
        next_remediation_to_apply_2.id,
        RemediationApplicationStatus {
            succeeded: false,
            metadata: None,
        },
    )
    .await?;

    let next_remediation_to_apply_2 =
        db::dpu_remediation::find_next_remediation_for_machine(&mut txn, test_dpu_2.id)
            .await?
            .expect("no remediation to apply?");
    // it should give back the same id
    assert_eq!(same_id_2, next_remediation_to_apply_2.id.clone());
    db::dpu_remediation::remediation_applied(
        &mut txn,
        test_dpu_2.id,
        next_remediation_to_apply_2.id,
        RemediationApplicationStatus {
            succeeded: true,
            metadata: None,
        },
    )
    .await?;

    let next_remediation_to_apply_2 =
        db::dpu_remediation::find_next_remediation_for_machine(&mut txn, test_dpu_2.id).await?;
    assert!(next_remediation_to_apply_2.is_none());

    // next up, test the enabling logic.  we should _not_ be able to enable a remediation that isn't reviewed
    let (reviewed_remediations, unreviewed_remediations): (Vec<_>, Vec<_>) = disabled_remediations
        .into_iter()
        .partition(|remediation| remediation.reviewer.is_some());

    let unreviewed_remediation_id = unreviewed_remediations.first().unwrap().id;
    let result = db::dpu_remediation::persist_enable_remediation(
        EnableRemediation {
            id: unreviewed_remediation_id,
        },
        &mut txn,
    )
    .await;
    assert!(result.is_err());

    // and we _should_ be able to enable one that has been approved
    let reviewed_remediation_id = reviewed_remediations.first().unwrap().id;
    db::dpu_remediation::persist_enable_remediation(
        EnableRemediation {
            id: reviewed_remediation_id,
        },
        &mut txn,
    )
    .await?;

    // once it has been enabled, we should be able to successfully FindNext it
    let next_remediation_to_apply_1 =
        db::dpu_remediation::find_next_remediation_for_machine(&mut txn, test_dpu_1.id)
            .await?
            .expect("no remediation to apply?");
    db::dpu_remediation::remediation_applied(
        &mut txn,
        test_dpu_1.id,
        next_remediation_to_apply_1.id,
        RemediationApplicationStatus {
            succeeded: true,
            metadata: None,
        },
    )
    .await?;
    // and it should be successfully applied
    let applied_remediations_1 =
        db::dpu_remediation::find_remediations_by_remediation_id_and_machine(
            &mut txn,
            next_remediation_to_apply_1.id,
            &test_dpu_1.id,
        )
        .await?;
    let applied_remediation_1 = applied_remediations_1
        .first()
        .expect("no applied remediation?");
    assert!(applied_remediation_1.succeeded);

    // test: we should _not_ be able to review unreviewed remediations using the same reviewer as the author of the remediation
    let unreviewed_remediation_id = unreviewed_remediations.first().unwrap().id;
    let unreviewed_remediation_author = unreviewed_remediations.first().unwrap().author.clone();
    let should_fail = db::dpu_remediation::persist_approve_remediation(
        ApproveRemediation {
            id: unreviewed_remediation_id,
            reviewer: Reviewer::from(unreviewed_remediation_author.to_string()),
        },
        &mut txn,
    )
    .await;
    assert!(should_fail.is_err());

    // and lastly, we should be able to review a remediation, and then enable it
    let remediation_id_to_test = unreviewed_remediation_id;
    db::dpu_remediation::persist_approve_remediation(
        ApproveRemediation {
            id: remediation_id_to_test,
            reviewer: Reviewer::from("totally_a_real_reviewer".to_string()),
        },
        &mut txn,
    )
    .await?;
    db::dpu_remediation::persist_enable_remediation(
        EnableRemediation {
            id: remediation_id_to_test,
        },
        &mut txn,
    )
    .await?;

    Ok(())
}

#[crate::sqlx_test]
async fn test_enable_rechecks_approval_after_concurrent_revoke(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut setup = pool.begin().await?;
    let remediation = db::dpu_remediation::persist_remediation(
        NewRemediation {
            script: "echo remediation".to_string(),
            metadata: None,
            retries: 0,
            author: "author".to_string().into(),
        },
        &mut setup,
    )
    .await?;
    db::dpu_remediation::persist_approve_remediation(
        ApproveRemediation {
            id: remediation.id,
            reviewer: "reviewer".to_string().into(),
        },
        &mut setup,
    )
    .await?;
    db::dpu_remediation::persist_enable_remediation(
        EnableRemediation { id: remediation.id },
        &mut setup,
    )
    .await?;
    setup.commit().await?;

    let mut revoke_txn = pool.begin().await?;
    let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(revoke_txn.as_mut())
        .await?;
    db::dpu_remediation::persist_revoke_remediation(
        RevokeRemediation { id: remediation.id },
        &mut revoke_txn,
    )
    .await?;

    let mut enable_txn = pool.begin().await?;
    let enable = db::dpu_remediation::persist_enable_remediation(
        EnableRemediation { id: remediation.id },
        &mut enable_txn,
    );
    let release_revoke = async {
        wait_for_blocked_query(&pool, blocker_pid, "dpu_remediations").await;
        revoke_txn.commit().await
    };
    let (result, released) = tokio::join!(enable, release_revoke);
    released?;
    // Commit successes just like the API, so an incorrect enablement remains
    // visible when we reload the row below.
    if result.is_ok() {
        enable_txn.commit().await?;
    } else {
        enable_txn.rollback().await?;
    }

    let mut read = pool.begin().await?;
    let persisted = db::dpu_remediation::find_remediations_by_ids(&mut read, &[remediation.id])
        .await?
        .pop()
        .expect("revoked remediation should remain stored");
    assert!(persisted.reviewer.is_none());
    assert!(!persisted.enabled);
    assert!(
        matches!(
            &result,
            Err(db::DatabaseError::InvalidArgument(message))
                if message.contains("has not been approved")
        ),
        "{result:?}"
    );
    read.rollback().await?;

    Ok(())
}

#[crate::sqlx_test]
async fn test_competing_approval_preserves_first_reviewer(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut setup = pool.begin().await?;
    let remediation = db::dpu_remediation::persist_remediation(
        NewRemediation {
            script: "echo remediation".to_string(),
            metadata: None,
            retries: 0,
            author: "author".to_string().into(),
        },
        &mut setup,
    )
    .await?;
    setup.commit().await?;

    let mut first_approval_txn = pool.begin().await?;
    let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(first_approval_txn.as_mut())
        .await?;
    db::dpu_remediation::persist_approve_remediation(
        ApproveRemediation {
            id: remediation.id,
            reviewer: "first-reviewer".to_string().into(),
        },
        &mut first_approval_txn,
    )
    .await?;

    let mut second_approval_txn = pool.begin().await?;
    let second_approval = db::dpu_remediation::persist_approve_remediation(
        ApproveRemediation {
            id: remediation.id,
            reviewer: "second-reviewer".to_string().into(),
        },
        &mut second_approval_txn,
    );
    let release_first_approval = async {
        wait_for_blocked_query(&pool, blocker_pid, "dpu_remediations").await;
        first_approval_txn.commit().await
    };
    let (result, released) = tokio::join!(second_approval, release_first_approval);
    released?;
    // Commit successes just like the API, so an overwritten reviewer remains
    // visible when we reload the row below.
    if result.is_ok() {
        second_approval_txn.commit().await?;
    } else {
        second_approval_txn.rollback().await?;
    }

    let mut read = pool.begin().await?;
    let persisted = db::dpu_remediation::find_remediations_by_ids(&mut read, &[remediation.id])
        .await?
        .pop()
        .expect("approved remediation should remain stored");
    assert_eq!(
        persisted.reviewer.as_ref().map(ToString::to_string),
        Some("first-reviewer".to_string())
    );
    assert!(
        matches!(
            &result,
            Err(db::DatabaseError::InvalidArgument(message))
                if message.contains("is already set to 'first-reviewer'")
        ),
        "{result:?}"
    );
    read.rollback().await?;

    Ok(())
}

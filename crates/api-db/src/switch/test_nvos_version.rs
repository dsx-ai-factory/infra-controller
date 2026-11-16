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

use model::switch::SwitchConfig;

use super::*;
use crate::test_support::postgres::wait_for_blocked_query;

async fn create_ready_switch(txn: &mut PgConnection) -> DatabaseResult<Switch> {
    let switch = create(
        txn,
        &NewSwitch {
            id: SwitchId::from(uuid::Uuid::new_v4()),
            config: SwitchConfig {
                name: "nvos-version".to_string(),
                enable_nmxc: false,
                fabric_manager_config: None,
            },
            bmc_mac_address: None,
            metadata: None,
            rack_id: None,
            slot_number: None,
            tray_index: None,
        },
    )
    .await?;
    assert_eq!(
        try_update_controller_state(
            txn,
            switch.id,
            switch.controller_state.version,
            switch.controller_state.version.increment(),
            &SwitchControllerState::Ready,
        )
        .await?,
        ConditionalWrite::Applied(())
    );
    Ok(find_by_id(txn, &switch.id).await?.expect("created switch"))
}

#[crate::sqlx_test]
async fn nvos_version_refresh_preserves_a_concurrent_request(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut setup = pool.begin().await?;
    let switch = create_ready_switch(&mut setup).await?;

    // NULL storage accepts the first request, then records the initial observation.
    request_nvos_version_refresh(&mut setup, switch.id).await?;
    let initial_request = find_by_id(&mut setup, &switch.id).await?.unwrap();
    assert_eq!(initial_request.nvos_version.version, None);
    assert!(initial_request.nvos_version.refresh_nvos_version);
    assert_eq!(
        initial_request.version.version_nr(),
        switch.version.version_nr() + 1
    );
    assert_eq!(
        try_complete_nvos_version_refresh(
            &mut setup,
            switch.id,
            initial_request.version,
            "nvos-old",
        )
        .await?,
        ConditionalWrite::Applied(())
    );
    request_nvos_version_refresh(&mut setup, switch.id).await?;
    let captured = find_by_id(&mut setup, &switch.id).await?.unwrap();
    assert_eq!(captured.nvos_version.version.as_deref(), Some("nvos-old"));
    assert!(captured.nvos_version.refresh_nvos_version);
    setup.commit().await?;

    // A repeated request must advance the token even while the flag is already true.
    let mut replacement = pool.begin().await?;
    request_nvos_version_refresh(&mut replacement, switch.id).await?;
    let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *replacement)
        .await?;
    let complete_earlier_observation = async {
        let mut txn = pool.begin().await?;
        let result = try_complete_nvos_version_refresh(
            &mut txn,
            switch.id,
            captured.version,
            "nvos-obsolete-observation",
        )
        .await?;
        txn.commit().await?;
        Ok::<_, Box<dyn std::error::Error>>(result)
    };
    let release_replacement = async {
        wait_for_blocked_query(&pool, blocker_pid, "UPDATE switches SET nvos_version").await;
        replacement.commit().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    };
    let (result, ()) = tokio::try_join!(complete_earlier_observation, release_replacement)?;
    assert_eq!(
        result,
        ConditionalWrite::NotApplied(NvosVersionRefreshNotCurrent)
    );

    let mut txn = pool.begin().await?;
    let pending = find_by_id(&mut txn, &switch.id).await?.unwrap();
    assert_eq!(pending.nvos_version, captured.nvos_version);
    assert_eq!(
        pending.version.version_nr(),
        captured.version.version_nr() + 1
    );
    assert_eq!(
        try_complete_nvos_version_refresh(&mut txn, switch.id, pending.version, "nvos-new").await?,
        ConditionalWrite::Applied(())
    );
    let completed = find_by_id(&mut txn, &switch.id).await?.unwrap();
    assert_eq!(
        completed.nvos_version,
        NvosVersion {
            version: Some("nvos-new".to_string()),
            refresh_nvos_version: false,
        }
    );
    assert_eq!(
        completed.version.version_nr(),
        pending.version.version_nr() + 1
    );
    txn.commit().await?;
    Ok(())
}

#[crate::sqlx_test]
async fn nvos_version_refresh_completion_rejects_ineligible_switches(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let cases = [
        (
            "no pending request",
            false,
            SwitchControllerState::Ready,
            false,
            true,
        ),
        (
            "not Ready",
            true,
            SwitchControllerState::Created,
            false,
            true,
        ),
        ("deleted", true, SwitchControllerState::Ready, true, true),
        ("missing", true, SwitchControllerState::Ready, false, false),
    ];
    for (scenario, pending, state, deleted, exists) in cases {
        let mut txn = pool.begin().await?;
        let switch = create_ready_switch(&mut txn).await?;
        let before = NvosVersion {
            version: Some("nvos-old".to_string()),
            refresh_nvos_version: pending,
        };
        sqlx::query(
            "UPDATE switches SET nvos_version = $1, controller_state = $2,
             deleted = $3 WHERE id = $4",
        )
        .bind(sqlx::types::Json(&before))
        .bind(sqlx::types::Json(&state))
        .bind(deleted.then(Utc::now))
        .bind(switch.id)
        .execute(&mut *txn)
        .await?;
        if !exists {
            sqlx::query("DELETE FROM switches WHERE id = $1")
                .bind(switch.id)
                .execute(&mut *txn)
                .await?;
        }

        assert_eq!(
            try_complete_nvos_version_refresh(&mut txn, switch.id, switch.version, "nvos-new")
                .await?,
            ConditionalWrite::NotApplied(NvosVersionRefreshNotCurrent),
            "{scenario}"
        );
        let stored = find_by_id(&mut txn, &switch.id).await?;
        if exists {
            let stored = stored.unwrap();
            assert_eq!(stored.nvos_version, before, "{scenario}");
            assert_eq!(stored.version, switch.version, "{scenario}");
        } else {
            assert!(stored.is_none(), "{scenario}");
        }
        txn.rollback().await?;
    }
    Ok(())
}

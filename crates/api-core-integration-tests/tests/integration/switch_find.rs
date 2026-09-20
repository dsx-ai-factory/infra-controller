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
use carbide_uuid::switch::SwitchId;

async fn create_discovered_switch(
    env: &TestHarness,
    seed: u8,
    name: &str,
) -> Result<SwitchId, Box<dyn std::error::Error>> {
    let mut txn = env.db_txn().await;
    let switch = db::test_support::switch::create_seeded_discovered(&mut txn, seed, name).await?;
    txn.commit().await?;
    Ok(switch.id)
}

#[sqlx_test]
async fn test_find_switch_ids_and_by_ids(pool: PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let env = TestHarness::builder(pool).build().await;
    let TestSwitch { id: switch_id1 } = env.create_switch(1, 1).await;
    let TestSwitch { id: switch_id2 } = env.create_switch(2, 1).await;

    let switch_ids = env
        .api()
        .find_switch_ids(tonic::Request::new(rpc::forge::SwitchSearchFilter {
            ..Default::default()
        }))
        .await?
        .into_inner()
        .ids;
    assert!(switch_ids.contains(&switch_id1));
    assert!(switch_ids.contains(&switch_id2));

    let switches = env
        .api()
        .find_switches_by_ids(tonic::Request::new(rpc::forge::SwitchesByIdsRequest {
            switch_ids: vec![switch_id1],
        }))
        .await?
        .into_inner()
        .switches;
    assert_eq!(switches.len(), 1);
    assert_eq!(switches[0].id, Some(switch_id1));

    let switches = env
        .api()
        .find_switches_by_ids(tonic::Request::new(rpc::forge::SwitchesByIdsRequest {
            switch_ids: vec![switch_id1, switch_id2],
        }))
        .await?
        .into_inner()
        .switches;
    assert_eq!(switches.len(), 2);

    Ok(())
}

// The empty-list and over-max guards for `find_switches_by_ids` are shared
// API-layer code, proven once across representative RPCs in
// `tests::find_by_ids_guards`.

#[sqlx_test]
async fn test_find_switch_ids_excludes_deleted(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = TestHarness::builder(pool).build().await;
    let TestSwitch { id: switch_id1 } = env.create_switch(1, 1).await;
    let TestSwitch { id: switch_id2 } = env.create_switch(2, 1).await;

    env.api()
        .delete_switch(tonic::Request::new(rpc::forge::SwitchDeletionRequest {
            id: Some(switch_id2),
        }))
        .await?;

    let switch_ids = env
        .api()
        .find_switch_ids(tonic::Request::new(rpc::forge::SwitchSearchFilter {
            ..Default::default()
        }))
        .await?
        .into_inner()
        .ids;
    assert!(switch_ids.contains(&switch_id1));
    assert!(!switch_ids.contains(&switch_id2));

    Ok(())
}

#[sqlx_test]
async fn test_find_switch_ids_deleted_only(pool: PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let env = TestHarness::builder(pool).build().await;
    let TestSwitch { id: switch_id1 } = env.create_switch(1, 1).await;
    let TestSwitch { id: switch_id2 } = env.create_switch(2, 1).await;

    env.api()
        .delete_switch(tonic::Request::new(rpc::forge::SwitchDeletionRequest {
            id: Some(switch_id2),
        }))
        .await?;

    let switch_ids = env
        .api()
        .find_switch_ids(tonic::Request::new(rpc::forge::SwitchSearchFilter {
            deleted: 1,
            ..Default::default()
        }))
        .await?
        .into_inner()
        .ids;
    assert!(!switch_ids.contains(&switch_id1));
    assert!(switch_ids.contains(&switch_id2));

    let switch_ids = env
        .api()
        .find_switch_ids(tonic::Request::new(rpc::forge::SwitchSearchFilter {
            deleted: 2,
            ..Default::default()
        }))
        .await?
        .into_inner()
        .ids;
    assert!(switch_ids.contains(&switch_id1));
    assert!(switch_ids.contains(&switch_id2));

    Ok(())
}

#[sqlx_test]
async fn test_find_switch_ids_by_controller_state(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = TestHarness::builder(pool).build().await;
    let TestSwitch { id: switch_id } = env.create_switch(1, 1).await;

    let switch_ids = env
        .api()
        .find_switch_ids(tonic::Request::new(rpc::forge::SwitchSearchFilter {
            controller_state: Some("created".to_string()),
            ..Default::default()
        }))
        .await?
        .into_inner()
        .ids;
    assert!(switch_ids.contains(&switch_id));

    let switch_ids = env
        .api()
        .find_switch_ids(tonic::Request::new(rpc::forge::SwitchSearchFilter {
            controller_state: Some("ready".to_string()),
            ..Default::default()
        }))
        .await?
        .into_inner()
        .ids;
    assert!(!switch_ids.contains(&switch_id));

    Ok(())
}

#[sqlx_test]
async fn test_find_switches_by_ids_response_fields(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = TestHarness::builder(pool).build().await;
    let switch_id = create_discovered_switch(&env, 1, "Switch1").await?;

    let switches = env
        .api()
        .find_switches_by_ids(tonic::Request::new(rpc::forge::SwitchesByIdsRequest {
            switch_ids: vec![switch_id],
        }))
        .await?
        .into_inner()
        .switches;
    assert_eq!(switches.len(), 1);

    let switch = &switches[0];

    // controller_state should be populated both on the top-level and in status
    assert!(!switch.controller_state.is_empty());
    let status = switch.status.as_ref().expect("status should be present");
    assert_eq!(
        status.controller_state.as_deref(),
        Some(switch.controller_state.as_str()),
    );

    // state_version should be populated
    assert!(!switch.state_version.is_empty());

    // bmc_info should be populated from the seeded machine_interface discovery data
    assert!(
        switch.bmc_info.is_some(),
        "bmc_info should be present when discovery data exists"
    );

    Ok(())
}

#[sqlx_test]
async fn test_find_switches_by_ids_includes_resolved_nvos_info(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = TestHarness::builder(pool).build().await;
    let switch_id = create_discovered_switch(&env, 1, "Switch1").await?;

    let mut txn = env.db_txn().await;
    let mut rows = db::switch::find_switch_endpoints_by_ids(txn.as_mut(), &[switch_id]).await?;
    let expected = rows.pop().expect("switch endpoint row");
    let host_mac = expected.nvos_mac.expect("nvos mac");
    let host_ip = expected.nvos_ip.expect("nvos ip");
    txn.rollback().await?;

    let response = env
        .api()
        .find_switches_by_ids(tonic::Request::new(rpc::forge::SwitchesByIdsRequest {
            switch_ids: vec![switch_id],
        }))
        .await?
        .into_inner();

    assert_eq!(response.switches.len(), 1);
    let switch = &response.switches[0];
    assert_eq!(switch.id, Some(switch_id));
    assert_eq!(
        switch.bmc_info.as_ref().and_then(|info| info.mac.clone()),
        Some(expected.bmc_mac.to_string())
    );
    assert_eq!(
        switch.bmc_info.as_ref().and_then(|info| info.ip.clone()),
        Some(expected.bmc_ip.to_string())
    );

    let nvos_info = switch.nvos_info.as_ref().expect("nvos info");
    let _: &rpc::forge::SwitchNvosInfo = nvos_info;
    assert_eq!(nvos_info.mac, Some(host_mac.to_string()));
    assert_eq!(nvos_info.ip, Some(host_ip.to_string()));
    assert!(nvos_info.port.is_none());

    Ok(())
}

#[sqlx_test]
async fn test_find_switches_includes_resolved_nvos_info(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = TestHarness::builder(pool).build().await;
    let switch_id = create_discovered_switch(&env, 1, "Switch1").await?;

    let mut txn = env.db_txn().await;
    let mut rows = db::switch::find_switch_endpoints_by_ids(txn.as_mut(), &[switch_id]).await?;
    let expected = rows.pop().expect("switch endpoint row");
    let host_mac = expected.nvos_mac.expect("nvos mac");
    let host_ip = expected.nvos_ip.expect("nvos ip");
    txn.rollback().await?;

    let response = env
        .api()
        .find_switches(tonic::Request::new(rpc::forge::SwitchQuery {
            name: None,
            switch_id: Some(switch_id),
        }))
        .await?
        .into_inner();

    assert_eq!(response.switches.len(), 1);
    let nvos_info = response.switches[0].nvos_info.as_ref().expect("nvos info");
    let _: &rpc::forge::SwitchNvosInfo = nvos_info;
    assert_eq!(nvos_info.mac, Some(host_mac.to_string()));
    assert_eq!(nvos_info.ip, Some(host_ip.to_string()));
    assert!(nvos_info.port.is_none());

    Ok(())
}

#[sqlx_test]
async fn test_find_switches_by_ids_returns_no_nvos_info_when_unresolved(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = TestHarness::builder(pool).build().await;
    let switch_id = create_discovered_switch(&env, 1, "Switch1").await?;

    let mut txn = env.db_txn().await;
    let rows = db::switch::find_switch_endpoints_by_ids(txn.as_mut(), &[switch_id]).await?;
    let bmc_mac = rows.first().expect("switch endpoint row").bmc_mac;
    db::expected_switch::update_nvos_mac_addresses(&mut txn, bmc_mac, &[]).await?;
    txn.commit().await?;

    let response = env
        .api()
        .find_switches_by_ids(tonic::Request::new(rpc::forge::SwitchesByIdsRequest {
            switch_ids: vec![switch_id],
        }))
        .await?
        .into_inner();

    assert_eq!(response.switches.len(), 1);
    assert!(response.switches[0].nvos_info.is_none());

    Ok(())
}

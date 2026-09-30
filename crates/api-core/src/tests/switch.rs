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
use carbide_uuid::switch::SwitchId;
use db::switch as db_switch;
use model::switch::{
    NewSwitch, SwitchConfig, SwitchControllerState, SwitchSearchFilter, SwitchStatus,
};
use rpc::forge::forge_server::Forge;
use rpc::forge::{AdminForceDeleteSwitchRequest, SwitchDeletionRequest, SwitchQuery};
use tonic::Code;

use crate::tests::common::api_fixtures::create_test_env;
use crate::tests::common::api_fixtures::site_explorer::new_switch;

#[crate::sqlx_test]
async fn test_find_switch_by_id(pool: sqlx::PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let switch_id = new_switch(&env, Some("Switch1".to_string()), None).await?;

    // Now find the switch by ID
    let find_request = SwitchQuery {
        name: None,
        switch_id: Some(switch_id),
    };

    let find_response = env
        .api
        .find_switches(tonic::Request::new(find_request))
        .await?;

    let switch_list = find_response.into_inner();
    assert_eq!(switch_list.switches.len(), 1);

    let found_switch = &switch_list.switches[0];
    assert_eq!(
        found_switch.id.as_ref().unwrap().to_string(),
        switch_id.to_string()
    );
    assert_eq!(found_switch.config.as_ref().unwrap().name, "Switch1");

    Ok(())
}

#[crate::sqlx_test]
async fn test_find_switch_not_found(pool: sqlx::PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;

    let non_existent_id = SwitchId::from(uuid::Uuid::new_v4());
    let find_request = SwitchQuery {
        name: None,
        switch_id: Some(non_existent_id),
    };

    let find_response = env
        .api
        .find_switches(tonic::Request::new(find_request))
        .await?;

    let switch_list = find_response.into_inner();
    assert_eq!(switch_list.switches.len(), 0);

    Ok(())
}

#[crate::sqlx_test]
async fn test_find_switch_all(pool: sqlx::PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;

    // Create multiple switches
    let configs = vec![("Switch1"), ("Switch2"), ("Switch3")];

    for name in configs {
        let _ = new_switch(
            &env,
            Some(name.to_string()),
            Some("Data Center".to_string()),
        )
        .await?;
    }

    // Find all switches
    let find_request = SwitchQuery {
        name: None,
        switch_id: None,
    };

    let find_response = env
        .api
        .find_switches(tonic::Request::new(find_request))
        .await?;

    let switch_list = find_response.into_inner();
    assert_eq!(switch_list.switches.len(), 3);

    // Verify all switches are present
    let names: Vec<String> = switch_list
        .switches
        .iter()
        .map(|s| s.config.as_ref().unwrap().name.clone())
        .collect();

    assert!(names.contains(&"Switch1".to_string()));
    assert!(names.contains(&"Switch2".to_string()));
    assert!(names.contains(&"Switch3".to_string()));

    Ok(())
}

#[crate::sqlx_test]
async fn test_delete_switch_success(pool: sqlx::PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;

    // First create a switch
    let switch_id = new_switch(
        &env,
        Some("Switch1".to_string()),
        Some("Rack 3".to_string()),
    )
    .await?;

    // Now delete the switch
    let delete_request = SwitchDeletionRequest {
        id: Some(switch_id),
    };

    let _delete_response = env
        .api
        .delete_switch(tonic::Request::new(delete_request))
        .await?;

    // Verify deletion was successful
    // The deletion result is empty, so we just check it doesn't error

    // Verify the switch is no longer findable
    let find_request = SwitchQuery {
        name: None,
        switch_id: Some(switch_id),
    };

    let find_result = env
        .api
        .find_switches(tonic::Request::new(find_request))
        .await;
    assert!(find_result.is_ok());
    let switch_list = find_result.unwrap().into_inner();

    let switch = &switch_list.switches[0];
    assert!(
        switch.deleted.is_some(),
        "Switch should have a deleted timestamp"
    );

    Ok(())
}

#[crate::sqlx_test]
async fn test_delete_switch_not_found(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;

    let non_existent_id = SwitchId::from(uuid::Uuid::new_v4());
    let delete_request = SwitchDeletionRequest {
        id: Some(non_existent_id),
    };

    let result = env
        .api
        .delete_switch(tonic::Request::new(delete_request))
        .await;

    assert!(result.is_err());
    assert_eq!(result.unwrap_err().code(), Code::NotFound);

    Ok(())
}

#[crate::sqlx_test]
async fn test_switch_database_operations(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = pool.begin().await?;

    // Test NewSwitch creation
    let config = SwitchConfig {
        name: "Switch1".to_string(),
        enable_nmxc: false,
        fabric_manager_config: None,
    };

    let switch_id = SwitchId::from(uuid::Uuid::new_v4());
    let new_switch = NewSwitch {
        id: switch_id,
        config: config.clone(),
        bmc_mac_address: None,
        metadata: None,
        rack_id: None,
        slot_number: Some(5),
        tray_index: Some(3),
    };

    let created_switch = db_switch::create(&mut txn, &new_switch).await?;

    assert_eq!(created_switch.id, switch_id);
    assert_eq!(created_switch.config.name, "Switch1");
    assert_eq!(created_switch.slot_number, Some(5));
    assert_eq!(created_switch.tray_index, Some(3));

    // Test finding the switch
    let found_switches = db_switch::find_by(
        &mut txn,
        db::ObjectColumnFilter::One(db_switch::IdColumn, &switch_id),
    )
    .await?;

    assert_eq!(found_switches.len(), 1);
    let mut found_switch = found_switches[0].clone();
    assert_eq!(found_switch.id, switch_id);
    assert_eq!(found_switch.config.name, "Switch1");

    // Test marking as deleted
    let deleted_switch = db_switch::mark_as_deleted(&mut found_switch, &mut txn).await?;
    assert!(deleted_switch.deleted.is_some());
    assert!(deleted_switch.is_marked_as_deleted());

    txn.rollback().await?;

    Ok(())
}

#[crate::sqlx_test]
async fn test_switch_status_update(pool: sqlx::PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = pool.begin().await?;

    // Create a switch
    let config = SwitchConfig {
        name: "Switch1".to_string(),
        enable_nmxc: false,
        fabric_manager_config: None,
    };

    let switch_id = SwitchId::from(uuid::Uuid::new_v4());
    let new_switch = NewSwitch {
        id: switch_id,
        config: config.clone(),
        bmc_mac_address: None,
        metadata: None,
        rack_id: None,
        slot_number: Some(1),
        tray_index: Some(0),
    };

    let mut switch = db_switch::create(&mut txn, &new_switch).await?;

    // Update the switch with status
    let status = SwitchStatus {
        switch_name: "Switch1".to_string(),
        power_state: "on".to_string(),
        health_status: "ok".to_string(),
    };

    switch.status = Some(status.clone());
    let updated_switch = db_switch::update(&switch, &mut txn).await?;

    assert!(updated_switch.status.is_some());
    let updated_status = updated_switch.status.as_ref().unwrap();
    assert_eq!(updated_status.switch_name, "Switch1");
    assert_eq!(updated_status.power_state, "on");
    assert_eq!(updated_status.health_status, "ok");

    txn.rollback().await?;

    Ok(())
}

#[crate::sqlx_test]
async fn test_switch_controller_state_transitions(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = pool.begin().await?;

    // Create a switch
    let config = SwitchConfig {
        name: "Switch1".to_string(),
        enable_nmxc: false,
        fabric_manager_config: None,
    };

    let switch_id = SwitchId::from(uuid::Uuid::new_v4());
    let new_switch = NewSwitch {
        id: switch_id,
        config: config.clone(),
        bmc_mac_address: None,
        metadata: None,
        rack_id: None,
        slot_number: Some(2),
        tray_index: Some(1),
    };

    let switch = db_switch::create(&mut txn, &new_switch).await?;

    // Test controller state transitions
    let initial_state = &switch.controller_state.value;
    assert!(matches!(initial_state, SwitchControllerState::Created));

    // Test updating controller state
    let new_state = SwitchControllerState::Ready;
    let current_version = switch.controller_state.version;

    let next_version = current_version.increment();
    let updated = db_switch::try_update_controller_state(
        &mut txn,
        switch_id,
        current_version,
        next_version,
        &new_state,
    )
    .await?;
    assert_eq!(
        updated,
        db::ConditionalWrite::Applied(()),
        "update with correct version should succeed"
    );

    // Verify the state was updated
    let updated_switches = db_switch::find_by(
        &mut txn,
        db::ObjectColumnFilter::One(db_switch::IdColumn, &switch_id),
    )
    .await?;

    assert_eq!(updated_switches.len(), 1);
    let updated_switch = &updated_switches[0];
    assert!(matches!(
        updated_switch.controller_state.value,
        SwitchControllerState::Ready
    ));

    // Version should have been incremented
    assert_eq!(
        updated_switch.controller_state.version.version_nr(),
        current_version.version_nr() + 1,
        "version should be incremented after update"
    );

    // Trying to update with the old version should fail (optimistic lock)
    let stale_update = db_switch::try_update_controller_state(
        &mut txn,
        switch_id,
        current_version,
        current_version.increment(),
        &SwitchControllerState::Created,
    )
    .await?;
    assert_eq!(
        stale_update,
        db::ConditionalWrite::NotApplied(db::ControllerStateNotCurrent),
        "update with stale version should be rejected"
    );

    // Updating with the new version should succeed
    let new_version = updated_switch.controller_state.version;
    let updated_again = db_switch::try_update_controller_state(
        &mut txn,
        switch_id,
        new_version,
        new_version.increment(),
        &SwitchControllerState::Created,
    )
    .await?;
    assert_eq!(
        updated_again,
        db::ConditionalWrite::Applied(()),
        "update with current version should succeed"
    );

    txn.rollback().await?;

    Ok(())
}

#[crate::sqlx_test]
async fn test_switch_conversion_roundtrip(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = pool.begin().await?;

    // Create a switch with status
    let config = SwitchConfig {
        name: "Switch1".to_string(),
        enable_nmxc: false,
        fabric_manager_config: None,
    };

    let switch_id = SwitchId::from(uuid::Uuid::new_v4());
    let new_switch = NewSwitch {
        id: switch_id,
        config: config.clone(),
        bmc_mac_address: None,
        metadata: None,
        rack_id: None,
        slot_number: Some(3),
        tray_index: Some(2),
    };

    let mut switch = db_switch::create(&mut txn, &new_switch).await?;

    // Add status
    let status = SwitchStatus {
        switch_name: "Switch1".to_string(),
        power_state: "on".to_string(),
        health_status: "ok".to_string(),
    };

    switch.status = Some(status);
    db_switch::update(&switch, &mut txn).await?;

    // Test conversion to RPC format
    let rpc_switch = rpc::forge::Switch::try_from(switch.clone())?;

    assert_eq!(rpc_switch.id.unwrap().to_string(), switch_id.to_string());
    assert_eq!(rpc_switch.config.as_ref().unwrap().name, "Switch1");

    // Verify status conversion
    let rpc_status = rpc_switch.status.unwrap();
    assert_eq!(rpc_status.switch_name, Some("Switch1".to_string()));
    assert_eq!(rpc_status.power_state, Some("on".to_string()));
    assert_eq!(rpc_status.health_status, Some("ok".to_string()));

    txn.rollback().await?;

    Ok(())
}

#[crate::sqlx_test]
async fn test_switch_find_all(pool: sqlx::PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = pool.begin().await?;

    // Create multiple switches
    let configs = vec![("Switch1"), ("Switch2"), ("Switch3")];

    let mut created_ids = Vec::new();

    for name in configs {
        let config = SwitchConfig {
            name: name.to_string(),
            enable_nmxc: false,
            fabric_manager_config: None,
        };

        let switch_id = SwitchId::from(uuid::Uuid::new_v4());
        let new_switch = NewSwitch {
            id: switch_id,
            config: config.clone(),
            bmc_mac_address: None,
            metadata: None,
            rack_id: None,
            slot_number: Some(0),
            tray_index: Some(0),
        };

        let switch = db_switch::create(&mut txn, &new_switch).await?;
        created_ids.push(switch.id);
    }

    // Test listing all switch IDs
    let listed_ids = db_switch::find_ids(txn.as_mut(), SwitchSearchFilter::default()).await?;

    // Verify all created IDs are in the list
    for created_id in &created_ids {
        assert!(listed_ids.contains(created_id));
    }

    // Verify the list contains at least our created IDs
    assert!(listed_ids.len() >= created_ids.len());

    txn.rollback().await?;

    Ok(())
}

#[crate::sqlx_test]
async fn test_switch_controller_state_outcome(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = pool.begin().await?;

    // Create a switch
    let config = SwitchConfig {
        name: "Switch1".to_string(),
        enable_nmxc: false,
        fabric_manager_config: None,
    };

    let switch_id = SwitchId::from(uuid::Uuid::new_v4());
    let new_switch = NewSwitch {
        id: switch_id,
        config: config.clone(),
        bmc_mac_address: None,
        metadata: None,
        rack_id: None,
        slot_number: Some(4),
        tray_index: Some(1),
    };

    let _switch = db_switch::create(&mut txn, &new_switch).await?;

    // Test updating controller state outcome
    let outcome =
        model::controller_outcome::PersistentStateHandlerOutcome::Transition { source_ref: None };

    db_switch::update_controller_state_outcome(&mut txn, switch_id, outcome).await?;

    // Verify the outcome was updated
    let updated_switches = db_switch::find_by(
        &mut txn,
        db::ObjectColumnFilter::One(db_switch::IdColumn, &switch_id),
    )
    .await?;

    assert_eq!(updated_switches.len(), 1);
    let updated_switch = &updated_switches[0];
    assert!(updated_switch.controller_state_outcome.is_some());

    let updated_outcome = updated_switch.controller_state_outcome.as_ref().unwrap();
    assert!(matches!(
        updated_outcome,
        model::controller_outcome::PersistentStateHandlerOutcome::Transition { .. }
    ));

    txn.rollback().await?;

    Ok(())
}

#[crate::sqlx_test]
async fn test_new_switch_fixture(pool: sqlx::PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;

    // Test creating a switch with default values
    let switch_id = new_switch(&env, None, None).await?;

    // Verify the switch was created
    assert!(!switch_id.to_string().is_empty());

    // Test creating a switch with custom values
    let custom_switch_id = new_switch(
        &env,
        Some("Switch2".to_string()),
        Some("Custom Location".to_string()),
    )
    .await?;

    // Verify the custom switch was created
    assert!(!custom_switch_id.to_string().is_empty());
    assert_ne!(switch_id, custom_switch_id);

    Ok(())
}

#[crate::sqlx_test]
async fn test_find_switch_bmc_info_no_matching_data(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;

    // Create a switch without the corresponding expected switch data
    let switch_id = model::switch::switch_id::from_hardware_info(
        "NO-BMC-SERIAL",
        "NVIDIA",
        "Switch",
        carbide_uuid::switch::SwitchIdSource::ProductBoardChassisSerial,
        carbide_uuid::switch::SwitchType::NvLink,
    )
    .unwrap();

    let new_switch_record = NewSwitch {
        id: switch_id,
        config: SwitchConfig {
            name: "SwitchNoBmc".to_string(),
            enable_nmxc: false,
            fabric_manager_config: None,
        },
        bmc_mac_address: None,
        metadata: None,
        rack_id: None,
        slot_number: Some(0),
        tray_index: Some(0),
    };

    let mut txn = env.pool.begin().await.unwrap();
    db_switch::create(&mut txn, &new_switch_record).await?;
    txn.commit().await.unwrap();

    let find_request = SwitchQuery {
        name: None,
        switch_id: Some(switch_id),
    };

    let find_response = env
        .api
        .find_switches(tonic::Request::new(find_request))
        .await?;

    let switch_list = find_response.into_inner();
    assert_eq!(switch_list.switches.len(), 1);

    let found_switch = &switch_list.switches[0];
    assert!(
        found_switch.bmc_info.is_none(),
        "bmc_info should be None when no expected switch data exists"
    );

    Ok(())
}

#[crate::sqlx_test]
async fn test_force_delete_switch_success(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;

    let switch_id = new_switch(&env, None, None).await?;

    let mut txn = env.pool.begin().await?;
    db::state_history::persist(
        &mut txn,
        db::state_history::StateHistoryTableId::Switch,
        &switch_id,
        &"retained-before-force-delete",
        config_version::ConfigVersion::initial(),
    )
    .await?;
    txn.commit().await?;

    // Force delete without deleting interfaces.
    let response = env
        .api
        .admin_force_delete_switch(tonic::Request::new(AdminForceDeleteSwitchRequest {
            switch_id: Some(switch_id),
            delete_interfaces: false,
            delete_bmc_suppressions: false,
        }))
        .await?
        .into_inner();

    assert_eq!(response.switch_id, switch_id.to_string());
    assert_eq!(response.interfaces_deleted, 0);

    // Verify the switch is completely gone (not just soft-deleted).
    let find_result = env
        .api
        .find_switches(tonic::Request::new(SwitchQuery {
            name: None,
            switch_id: Some(switch_id),
        }))
        .await?
        .into_inner();

    assert!(
        find_result.switches.is_empty(),
        "Switch should be hard-deleted"
    );

    let mut conn = env.pool.acquire().await?;
    let history = db::state_history::for_object(
        &mut conn,
        db::state_history::StateHistoryTableId::Switch,
        &switch_id,
    )
    .await?;
    assert!(
        history
            .iter()
            .any(|record| record.state == r#""retained-before-force-delete""#),
        "Switch state history should be retained",
    );

    Ok(())
}

#[crate::sqlx_test]
async fn test_force_delete_switch_not_found(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;

    let non_existent_id = SwitchId::from(uuid::Uuid::new_v4());
    let result = env
        .api
        .admin_force_delete_switch(tonic::Request::new(AdminForceDeleteSwitchRequest {
            switch_id: Some(non_existent_id),
            delete_interfaces: false,
            delete_bmc_suppressions: false,
        }))
        .await;

    assert!(result.is_err());
    assert_eq!(result.unwrap_err().code(), Code::NotFound);

    Ok(())
}

#[crate::sqlx_test]
async fn test_force_delete_switch_already_soft_deleted(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;

    let switch_id = new_switch(&env, None, None).await?;

    // Soft-delete the switch first.
    env.api
        .delete_switch(tonic::Request::new(SwitchDeletionRequest {
            id: Some(switch_id),
        }))
        .await?;

    // Force-delete should still work on a soft-deleted switch.
    let response = env
        .api
        .admin_force_delete_switch(tonic::Request::new(AdminForceDeleteSwitchRequest {
            switch_id: Some(switch_id),
            delete_interfaces: false,
            delete_bmc_suppressions: false,
        }))
        .await?
        .into_inner();

    assert_eq!(response.switch_id, switch_id.to_string());

    // Verify completely gone.
    let find_result = env
        .api
        .find_switches(tonic::Request::new(SwitchQuery {
            name: None,
            switch_id: Some(switch_id),
        }))
        .await?
        .into_inner();

    assert!(
        find_result.switches.is_empty(),
        "Switch should be hard-deleted after force delete"
    );

    Ok(())
}

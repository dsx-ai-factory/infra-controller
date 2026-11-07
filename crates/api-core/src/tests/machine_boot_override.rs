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

use common::api_fixtures::create_test_env;
use common::api_fixtures::dpu::dpu_discover_dhcp;
use rpc::protos::forge::forge_server::Forge;

use crate::DatabaseError;
use crate::test_support::mac_address_pool::DPU_OOB_MAC_ADDRESS_POOL;
use crate::tests::common;

#[crate::sqlx_test]
async fn only_one_custom_pxe_per_interface(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let new_interface_id =
        dpu_discover_dhcp(&env, &DPU_OOB_MAC_ADDRESS_POOL.allocate().to_string()).await;

    let mut txn = env.pool.begin().await?;

    let expected_pxe = Some("custom_pxe_string".to_string());
    let expected_user_data = Some("custom_user_data_string".to_string());

    db::machine_boot_override::create(
        &mut txn,
        new_interface_id,
        expected_pxe.clone(),
        expected_user_data.clone(),
    )
    .await?
    .expect("Could not create custom pxe");

    let machine_boot_override =
        db::machine_boot_override::find_optional(txn.as_mut(), new_interface_id)
            .await
            .expect("Could not load custom boot")
            .unwrap();

    txn.commit().await.unwrap();

    assert_eq!(machine_boot_override.custom_pxe, expected_pxe);
    assert_eq!(machine_boot_override.custom_user_data, expected_user_data);

    let mut txn = env.pool.begin().await?;

    let output = db::machine_boot_override::create(
        &mut txn,
        new_interface_id,
        Some("custom_pxe_string".to_string()),
        None,
    )
    .await;

    txn.commit().await.unwrap();

    assert!(matches!(output, Err(DatabaseError::Sqlx(_))));
    Ok(())
}

#[crate::sqlx_test]
async fn confirm_null_fields(pool: sqlx::PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let new_interface_id =
        dpu_discover_dhcp(&env, &DPU_OOB_MAC_ADDRESS_POOL.allocate().to_string()).await;

    let mut txn = env.pool.begin().await?;

    db::machine_boot_override::create(&mut txn, new_interface_id, None, None)
        .await?
        .expect("Could not create custom pxe");

    // ensure these stay Nones as we have code that will react to them not being None
    let machine_boot_override =
        db::machine_boot_override::find_optional(txn.as_mut(), new_interface_id)
            .await
            .expect("Could not load custom boot")
            .unwrap();

    txn.commit().await.unwrap();

    assert!(machine_boot_override.custom_pxe.is_none());
    assert!(machine_boot_override.custom_user_data.is_none());
    Ok(())
}

#[crate::sqlx_test]
async fn api_get(pool: sqlx::PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let expected_pxe = Some("custom pxe".to_owned());
    let expected_user_data = Some("custom user data".to_owned());

    let env = create_test_env(pool).await;
    let new_interface_id =
        dpu_discover_dhcp(&env, &DPU_OOB_MAC_ADDRESS_POOL.allocate().to_string()).await;

    let mut txn = env.pool.begin().await?;

    db::machine_boot_override::create(
        &mut txn,
        new_interface_id,
        expected_pxe.clone(),
        expected_user_data.clone(),
    )
    .await?
    .expect("Could not create custom pxe");

    txn.commit().await.unwrap();

    let req = tonic::Request::new(new_interface_id);
    let machine_boot_override = env
        .api
        .get_machine_boot_override(req)
        .await
        .expect("Failed to get overrides via API")
        .into_inner();

    println!(
        "mbo: {}",
        serde_json::to_string_pretty(&machine_boot_override)
            .expect("failed to serialize machine_boot_override")
    );

    assert_eq!(machine_boot_override.custom_pxe, expected_pxe);
    assert_eq!(machine_boot_override.custom_user_data, expected_user_data);
    Ok(())
}

#[crate::sqlx_test]
async fn api_set(pool: sqlx::PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let expected_pxe = Some("custom pxe".to_owned());
    let expected_user_data = Some("custom user data".to_owned());

    let env = create_test_env(pool).await;
    let machine_interface_id =
        dpu_discover_dhcp(&env, &DPU_OOB_MAC_ADDRESS_POOL.allocate().to_string()).await;

    let req = tonic::Request::new(rpc::forge::MachineBootOverride {
        machine_interface_id: Some(machine_interface_id),
        custom_pxe: expected_pxe.clone(),
        custom_user_data: expected_user_data.clone(),
    });

    env.api
        .set_machine_boot_override(req)
        .await
        .expect("Failed to set overrides via API")
        .into_inner();

    let mut txn = env.pool.begin().await?;

    let machine_boot_override =
        db::machine_boot_override::find_optional(txn.as_mut(), machine_interface_id)
            .await
            .expect("Could not load custom boot")
            .unwrap();

    println!("{machine_boot_override:?}");
    assert_eq!(machine_boot_override.custom_pxe, expected_pxe);
    assert_eq!(machine_boot_override.custom_user_data, expected_user_data);
    Ok(())
}

#[crate::sqlx_test]
async fn api_clear(pool: sqlx::PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let expected_pxe = Some("custom pxe".to_owned());
    let expected_user_data = Some("custom user data".to_owned());

    let env = create_test_env(pool).await;
    let new_interface_id =
        dpu_discover_dhcp(&env, &DPU_OOB_MAC_ADDRESS_POOL.allocate().to_string()).await;

    let mut txn = env.pool.begin().await?;

    db::machine_boot_override::create(
        &mut txn,
        new_interface_id,
        expected_pxe.clone(),
        expected_user_data.clone(),
    )
    .await?
    .expect("Could not create custom pxe");

    txn.commit().await.unwrap();

    let req = tonic::Request::new(new_interface_id);
    env.api
        .clear_machine_boot_override(req)
        .await
        .expect("Failed to clear overrides via API");

    let mut txn = env.pool.begin().await?;

    // ensure these stay Nones as we have code that will react to them not being None
    let machine_boot_override =
        db::machine_boot_override::find_optional(txn.as_mut(), new_interface_id)
            .await
            .expect("Could not load custom boot");

    assert!(machine_boot_override.is_none());
    Ok(())
}

#[crate::sqlx_test]
async fn api_update(pool: sqlx::PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let expected_pxe = Some("custom pxe".to_owned());
    let expected_user_data = Some("custom user data".to_owned());

    let env = create_test_env(pool).await;
    let machine_interface_id =
        dpu_discover_dhcp(&env, &DPU_OOB_MAC_ADDRESS_POOL.allocate().to_string()).await;

    let req = tonic::Request::new(rpc::forge::MachineBootOverride {
        machine_interface_id: Some(machine_interface_id),
        custom_pxe: expected_pxe.clone(),
        custom_user_data: None,
    });

    env.api
        .set_machine_boot_override(req)
        .await
        .expect("Failed to set overrides via API")
        .into_inner();

    let req = tonic::Request::new(rpc::forge::MachineBootOverride {
        machine_interface_id: Some(machine_interface_id),
        custom_pxe: None,
        custom_user_data: expected_user_data.clone(),
    });

    env.api
        .set_machine_boot_override(req)
        .await
        .expect("Failed to set overrides via API")
        .into_inner();

    let mut txn = env.pool.begin().await?;

    let machine_boot_override =
        db::machine_boot_override::find_optional(txn.as_mut(), machine_interface_id)
            .await
            .expect("Could not load custom boot")
            .unwrap();

    assert_eq!(machine_boot_override.custom_pxe, expected_pxe);
    assert_eq!(machine_boot_override.custom_user_data, expected_user_data);
    Ok(())
}

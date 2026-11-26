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

use std::net::IpAddr;

use carbide_test_harness::prelude::*;
use carbide_uuid::machine::MachineInterfaceId;
use carbide_uuid::network::NetworkSegmentId;
use carbide_uuid::power_shelf::PowerShelfId;
use model::allocation_type::AllocationType;
use rpc::forge::{AdminForceDeletePowerShelfRequest, PowerShelfDeletionRequest, PowerShelfQuery};
use tonic::Code;

use crate::power_shelf::create_custom_power_shelf;

#[sqlx_test]
async fn test_delete_power_shelf_success(pool: PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let env = TestHarness::builder(pool).build().await;
    let power_shelf_id =
        create_custom_power_shelf(&env, "Delete Test Power Shelf", Some(5000), Some(240)).await?;

    env.api()
        .delete_power_shelf(tonic::Request::new(PowerShelfDeletionRequest {
            id: Some(power_shelf_id),
        }))
        .await?;

    // Verify deletion was successful
    // The deletion result is empty, so we just check it doesn't error

    // Soft-deleted power shelves are still returned by this endpoint, with
    // `deleted` populated.
    let find_result = env
        .api()
        .find_power_shelves(tonic::Request::new(PowerShelfQuery {
            name: None,
            power_shelf_id: Some(power_shelf_id),
        }))
        .await;
    assert!(find_result.is_ok());
    let power_shelf_list = find_result.unwrap().into_inner();

    assert_eq!(
        power_shelf_list.power_shelves.len(),
        1,
        "expected exactly one power shelf after delete"
    );
    let power_shelf = &power_shelf_list.power_shelves[0];
    assert!(
        power_shelf.deleted.is_some(),
        "Power shelf should have a deleted timestamp"
    );

    Ok(())
}

#[sqlx_test]
async fn test_delete_power_shelf_not_found(pool: PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let env = TestHarness::builder(pool).build().await;

    let non_existent_id = PowerShelfId::from(uuid::Uuid::new_v4());
    let result = env
        .api()
        .delete_power_shelf(tonic::Request::new(PowerShelfDeletionRequest {
            id: Some(non_existent_id),
        }))
        .await;

    assert!(result.is_err());
    assert_eq!(result.unwrap_err().code(), Code::NotFound);

    Ok(())
}

#[sqlx_test]
async fn test_force_delete_power_shelf_success(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = TestHarness::builder(pool.clone()).build().await;
    let power_shelf_id =
        create_custom_power_shelf(&env, "ForceDelete Power Shelf", Some(5000), Some(240)).await?;

    // Power shelf state history is retained even when the shelf row itself is
    // hard-deleted.
    let mut txn = env.db_txn().await;
    db::state_history::persist(
        &mut txn,
        db::state_history::StateHistoryTableId::PowerShelf,
        &power_shelf_id,
        &"retained-before-force-delete",
        config_version::ConfigVersion::initial(),
    )
    .await?;
    txn.commit().await?;

    // Force delete without deleting interfaces.
    let response = env
        .api()
        .admin_force_delete_power_shelf(tonic::Request::new(AdminForceDeletePowerShelfRequest {
            power_shelf_id: Some(power_shelf_id),
            delete_interfaces: false,
            delete_bmc_suppressions: false,
        }))
        .await?
        .into_inner();

    assert_eq!(response.power_shelf_id, power_shelf_id.to_string());
    assert_eq!(response.interfaces_deleted, 0);

    // Verify the power shelf is completely gone (not just soft-deleted).
    let find_result = env
        .api()
        .find_power_shelves(tonic::Request::new(PowerShelfQuery {
            name: None,
            power_shelf_id: Some(power_shelf_id),
        }))
        .await?
        .into_inner();

    assert!(
        find_result.power_shelves.is_empty(),
        "Power shelf should be hard-deleted"
    );

    let history = db::state_history::for_object(
        &pool,
        db::state_history::StateHistoryTableId::PowerShelf,
        &power_shelf_id,
    )
    .await?;
    assert!(
        history
            .iter()
            .any(|record| record.state == r#""retained-before-force-delete""#),
        "Power shelf state history should be retained",
    );

    Ok(())
}

#[sqlx_test]
async fn test_force_delete_power_shelf_clears_explored_endpoints(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = TestHarness::builder(pool.clone()).build().await;
    let unrelated_address: IpAddr = "192.0.2.100".parse()?;
    let mut txn = env.db_txn().await;
    let segment_id: NetworkSegmentId = sqlx::query_scalar(
        "INSERT INTO network_segments (name, version, network_segment_type)
         VALUES ('power-shelf-force-delete', 'V1-T0', 'underlay') RETURNING id",
    )
    .fetch_one(txn.as_mut())
    .await?;
    db::explored_endpoints::insert(unrelated_address, &Default::default(), false, &mut txn).await?;
    txn.commit().await?;

    for (delete_interfaces, mac, ipv4_address, ipv6_address) in [
        (false, "02:00:00:00:0a:01", "192.0.2.10", "2001:db8::10"),
        (true, "02:00:00:00:0a:02", "192.0.2.11", "2001:db8::11"),
    ] {
        let power_shelf_id =
            create_custom_power_shelf(&env, "Endpoint cleanup", None, None).await?;
        let mut txn = env.db_txn().await;
        let interface_id: MachineInterfaceId = sqlx::query_scalar(
            "INSERT INTO machine_interfaces
                 (power_shelf_id, association_type, segment_id, mac_address,
                  primary_interface, hostname, interface_type)
             VALUES ($1, 'PowerShelf', $2, $3::macaddr, false, 'pmc', 'Bmc')
             RETURNING id",
        )
        .bind(power_shelf_id)
        .bind(segment_id)
        .bind(mac)
        .fetch_one(txn.as_mut())
        .await?;
        let addresses: [IpAddr; 2] = [ipv4_address.parse()?, ipv6_address.parse()?];
        for address in addresses {
            db::machine_interface_address::insert(
                txn.as_mut(),
                interface_id,
                address,
                AllocationType::Dhcp,
            )
            .await?;
            db::explored_endpoints::insert(address, &Default::default(), false, &mut txn).await?;
        }
        txn.commit().await?;

        let response = env
            .api()
            .admin_force_delete_power_shelf(tonic::Request::new(
                AdminForceDeletePowerShelfRequest {
                    power_shelf_id: Some(power_shelf_id),
                    delete_interfaces,
                    delete_bmc_suppressions: false,
                },
            ))
            .await?
            .into_inner();

        assert_eq!(response.interfaces_deleted, u32::from(delete_interfaces));
        assert!(
            db::power_shelf::find_by_id(&pool, &power_shelf_id)
                .await?
                .is_none()
        );
        assert!(
            db::explored_endpoints::find_by_ips(&pool, addresses.to_vec())
                .await?
                .is_empty(),
            "all PMC aliases must be removed with delete_interfaces={delete_interfaces}"
        );
        assert_eq!(
            db::machine_interface::find_by_ip(&pool, addresses[0])
                .await?
                .is_none(),
            delete_interfaces,
        );
        assert_eq!(
            db::explored_endpoints::find_by_ips(&pool, vec![unrelated_address])
                .await?
                .len(),
            1,
        );
    }

    Ok(())
}

#[sqlx_test]
async fn test_force_delete_power_shelf_not_found(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = TestHarness::builder(pool).build().await;

    let non_existent_id = PowerShelfId::from(uuid::Uuid::new_v4());
    let result = env
        .api()
        .admin_force_delete_power_shelf(tonic::Request::new(AdminForceDeletePowerShelfRequest {
            power_shelf_id: Some(non_existent_id),
            delete_interfaces: false,
            delete_bmc_suppressions: false,
        }))
        .await;

    assert!(result.is_err());
    assert_eq!(result.unwrap_err().code(), Code::NotFound);

    Ok(())
}

#[sqlx_test]
async fn test_force_delete_power_shelf_already_soft_deleted(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = TestHarness::builder(pool).build().await;
    let power_shelf_id =
        create_custom_power_shelf(&env, "SoftDeleted Power Shelf", Some(3000), Some(120)).await?;

    env.api()
        .delete_power_shelf(tonic::Request::new(PowerShelfDeletionRequest {
            id: Some(power_shelf_id),
        }))
        .await?;

    let response = env
        .api()
        .admin_force_delete_power_shelf(tonic::Request::new(AdminForceDeletePowerShelfRequest {
            power_shelf_id: Some(power_shelf_id),
            delete_interfaces: false,
            delete_bmc_suppressions: false,
        }))
        .await?
        .into_inner();

    assert_eq!(response.power_shelf_id, power_shelf_id.to_string());

    let find_result = env
        .api()
        .find_power_shelves(tonic::Request::new(PowerShelfQuery {
            name: None,
            power_shelf_id: Some(power_shelf_id),
        }))
        .await?
        .into_inner();

    assert!(
        find_result.power_shelves.is_empty(),
        "Power shelf should be hard-deleted after force delete"
    );

    Ok(())
}

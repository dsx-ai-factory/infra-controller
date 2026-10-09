/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use model::expected_machine::{
    BmcIpAllocationType, ExpectedInterface, ExpectedInterfaceIpAllocation, ExpectedMachineData,
    HostDpuPolicy, HostLifecycleProfile,
};
use model::metadata::Metadata;
use sqlx::Connection;

use super::*;

#[crate::sqlx_test]
async fn expected_machine_queries_survive_added_columns(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut api_connection = pool.acquire().await?;
    exercise_expected_machine_queries(&mut api_connection).await?;
    assert!(api_connection.cached_statements_size() > 0);

    // Keep the API's prepared statements while another connection applies DDL.
    let mut migration = pool.begin().await?;
    sqlx::raw_sql(
        "SET LOCAL lock_timeout = '5s';
         ALTER TABLE expected_machines ADD COLUMN test_added_column text;",
    )
    .execute(&mut *migration)
    .await?;
    migration.commit().await?;

    exercise_expected_machine_queries(&mut api_connection).await?;
    Ok(())
}

async fn exercise_expected_machine_queries(
    connection: &mut PgConnection,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = connection.begin().await?;
    let id = Uuid::new_v4();
    let rack_id = RackId::new("projection-rack");
    let interface_mac = "02:00:00:00:01:02".parse()?;
    let expected = ExpectedMachine {
        id: Some(id),
        bmc_mac_address: "02:00:00:00:01:01".parse()?,
        data: ExpectedMachineData {
            bmc_username: "test-user".to_string(),
            bmc_password: "test-password".to_string(),
            serial_number: "projection-machine".to_string(),
            fallback_dpu_serial_numbers: vec!["projection-dpu".to_string()],
            sku_id: Some("projection-sku".to_string()),
            metadata: Metadata {
                name: "expected machine".to_string(),
                description: "populated projection fixture".to_string(),
                labels: HashMap::from([("location".to_string(), "rack-1".to_string())]),
            },
            interfaces: vec![ExpectedInterface {
                mac_address: interface_mac,
                ip_allocation: Some(ExpectedInterfaceIpAllocation::Fixed),
                fixed_ip: Some("192.0.2.11".parse()?),
                fixed_mask: Some("255.255.255.0".to_string()),
                fixed_gateway: Some("192.0.2.1".parse()?),
                primary: Some(true),
                ..Default::default()
            }],
            rack_id: Some(rack_id.clone()),
            default_pause_ingestion_and_poweron: Some(true),
            dpf_enabled: Some(false),
            bmc_ip_address: Some("192.0.2.10".parse()?),
            bmc_retain_credentials: Some(true),
            dpu_policy: HostDpuPolicy::Nic,
            bmc_ip_allocation: BmcIpAllocationType::Fixed,
            host_lifecycle_profile: HostLifecycleProfile {
                disable_lockdown: Some(true),
            },
        },
    };
    assert_machine(&create(&mut txn, expected.clone()).await?, &expected);

    for found in [
        find_by_bmc_mac_address(&mut *txn, expected.bmc_mac_address).await?,
        find_by_id(&mut *txn, id).await?,
        find_by_interface_mac_address(&mut txn, interface_mac).await?,
        find_for_update(
            &mut txn,
            &ExpectedMachineRequest {
                id: Some(id),
                bmc_mac_address: None,
            },
        )
        .await?,
        find_for_update(
            &mut txn,
            &ExpectedMachineRequest {
                id: None,
                bmc_mac_address: Some(expected.bmc_mac_address),
            },
        )
        .await?,
    ] {
        assert_machine(&found.expect("the expected machine exists"), &expected);
    }

    for found in [
        find_all(&mut *txn).await?,
        find_all_for_replace(&mut txn).await?,
        find_all_by_rack_id(&mut txn, &rack_id).await?,
    ] {
        assert_eq!(found.len(), 1);
        assert_machine(&found[0], &expected);
    }
    let found = find_many_by_bmc_mac_address(&mut txn, &[expected.bmc_mac_address]).await?;
    assert_eq!(found.len(), 1);
    assert_machine(&found[&expected.bmc_mac_address], &expected);

    // Roll back the fixture and locks, but retain the cached statements.
    txn.rollback().await?;
    Ok(())
}

fn assert_machine(actual: &ExpectedMachine, expected: &ExpectedMachine) {
    assert_eq!(actual.id, expected.id);
    assert_eq!(actual.bmc_mac_address, expected.bmc_mac_address);
    let actual = &actual.data;
    let expected = &expected.data;
    assert_eq!(actual.bmc_username, expected.bmc_username);
    assert_eq!(actual.bmc_password, expected.bmc_password);
    assert_eq!(actual.serial_number, expected.serial_number);
    assert_eq!(
        actual.fallback_dpu_serial_numbers,
        expected.fallback_dpu_serial_numbers
    );
    assert_eq!(actual.metadata, expected.metadata);
    assert_eq!(actual.sku_id, expected.sku_id);
    assert_eq!(actual.interfaces, expected.interfaces);
    assert_eq!(actual.rack_id, expected.rack_id);
    assert_eq!(
        actual.default_pause_ingestion_and_poweron,
        expected.default_pause_ingestion_and_poweron
    );
    assert_eq!(actual.dpf_enabled, expected.dpf_enabled);
    assert_eq!(actual.bmc_ip_address, expected.bmc_ip_address);
    assert_eq!(
        actual.bmc_retain_credentials,
        expected.bmc_retain_credentials
    );
    assert_eq!(actual.dpu_policy, expected.dpu_policy);
    assert_eq!(actual.bmc_ip_allocation, expected.bmc_ip_allocation);
    assert_eq!(
        actual.host_lifecycle_profile,
        expected.host_lifecycle_profile
    );
}

// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use model::expected_machine::BmcIpAllocationType;
use sqlx::PgPool;

#[crate::sqlx_test]
async fn existing_bmc_policy_can_opt_in_to_preservation(pool: PgPool) -> Result<(), sqlx::Error> {
    super::migrate_before(&pool, 20261120192545).await;
    sqlx::query(
        "INSERT INTO expected_machines
             (id, serial_number, bmc_mac_address, bmc_username, bmc_password, bmc_ip_allocation)
         VALUES (gen_random_uuid(), 'host-bmc', '02:00:00:00:00:01', 'admin', 'pw', 'retained')",
    )
    .execute(&pool)
    .await?;
    sqlx::raw_sql(include_str!(
        "../../migrations/20261120192545_preserved_bmc_ip_allocation.sql"
    ))
    .execute(&pool)
    .await?;

    // The upgrade leaves the old policy intact until a writer selects Preserved.
    let stored: BmcIpAllocationType = sqlx::query_scalar(
        "UPDATE expected_machines SET bmc_ip_allocation = $1
         WHERE serial_number = 'host-bmc' AND bmc_ip_allocation = 'retained'
         RETURNING bmc_ip_allocation",
    )
    .bind(BmcIpAllocationType::Preserved)
    .fetch_one(&pool)
    .await?;
    assert_eq!(stored, BmcIpAllocationType::Preserved);
    Ok(())
}

// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use sqlx::PgPool;

use super::migrate_before;

const VERSION: i64 = 20261108004831;
const MIGRATION: &str =
    include_str!("../../migrations/20261108004831_machine_validation_lenovo_7dhecto1ww.sql");
const SKU: &str = "7dhecto1ww";
const TEST_IDS: [&str; 9] = [
    "forge_DcgmFullLong",
    "forge_DcgmFullShort",
    "forge_MqStresserLong",
    "forge_MqStresserShort",
    "forge_CPUTestLong",
    "forge_CPUTestShort",
    "forge_MemoryTestLong",
    "forge_MemoryTestShort",
    "forge_NvmeCheck",
];

async fn supported_platforms(pool: &PgPool) -> Vec<(String, String, Vec<String>)> {
    sqlx::query_as(
        "SELECT test_id, version, supported_platforms FROM machine_validation_tests
         ORDER BY test_id, version",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

#[crate::sqlx_test]
async fn migration_adds_the_sku_to_each_listed_test_only(pool: PgPool) {
    migrate_before(&pool, VERSION).await;
    let before = supported_platforms(&pool).await;
    for test_id in TEST_IDS {
        assert!(
            before.iter().any(|(id, _, _)| id == test_id),
            "{test_id} is not in the predecessor schema"
        );
    }

    sqlx::raw_sql(MIGRATION).execute(&pool).await.unwrap();

    let after = supported_platforms(&pool).await;
    assert_eq!(after.len(), before.len());
    for ((test_id, version, old), (_, _, new)) in before.iter().zip(&after) {
        let mut expected = old.clone();
        if TEST_IDS.contains(&test_id.as_str()) {
            assert!(!old.iter().any(|platform| platform == SKU));
            expected.push(SKU.to_string());
        }
        assert_eq!(new, &expected, "{test_id} {version}");
    }
}

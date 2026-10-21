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

use std::num::TryFromIntError;

use model::sku::Sku;
use sqlx::PgPool;

use crate::DatabaseError;

#[crate::sqlx_test]
async fn sku_reads_check_schema_version_without_rejecting_historical_versions(pool: PgPool) {
    struct TestCase {
        id: &'static str,
        stored_version: i32,
        check: fn(Result<Vec<Sku>, DatabaseError>),
    }

    let cases = [
        TestCase {
            id: "historical-version",
            stored_version: 0,
            check: |result| {
                let skus = result.expect("read historical SKU version");
                assert_eq!(skus.len(), 1);
                assert_eq!(skus[0].schema_version, 0);
            },
        },
        TestCase {
            id: "negative-version",
            stored_version: -1,
            check: |result| {
                let error = result.expect_err("negative SKU version must fail decoding");
                let DatabaseError::Sqlx(annotated) = &error else {
                    panic!("expected Sqlx, got {error:?}");
                };
                let sqlx::Error::ColumnDecode { index, source } = &annotated.source else {
                    panic!("expected ColumnDecode, got {error:?}");
                };
                assert_eq!(index, "schema_version");
                assert!(source.is::<TryFromIntError>(), "{source:?}");
            },
        },
    ];

    let mut txn = pool.begin().await.expect("begin SKU decode test");
    // `create` accepts only the current SKU version. Insert directly to check
    // historical zero and a negative value that PostgreSQL's `INTEGER` permits.
    for case in cases {
        sqlx::query(
            "INSERT INTO machine_skus (id, description, components, schema_version)
             VALUES ($1, '', $2, $3)",
        )
        .bind(case.id)
        .bind(serde_json::json!({
            "chassis": {"vendor": "", "model": "", "architecture": ""},
            "cpus": [],
            "gpus": [],
            "memory": [],
            "infiniband_devices": []
        }))
        .bind(case.stored_version)
        .execute(&mut *txn)
        .await
        .expect("store SKU schema version");

        let result = super::find(&mut txn, &[case.id]).await;
        (case.check)(result);
    }
}

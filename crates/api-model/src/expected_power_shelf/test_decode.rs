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

use sqlx::FromRow;

use super::ExpectedPowerShelf;

#[crate::sqlx_test]
async fn expected_power_shelf_requires_a_decodable_rack_column(pool: sqlx::PgPool) {
    struct Case {
        scenario: &'static str,
        rack_projection: &'static str,
        check: fn(sqlx::Error),
    }

    // `RackId` accepts stored text, so these cases check missing or wrongly
    // typed query columns rather than invalid rack names.
    let cases = [
        Case {
            scenario: "missing rack column",
            rack_projection: "NULL::text AS another_column",
            check: |error| {
                assert!(
                    matches!(&error, sqlx::Error::ColumnNotFound(column) if column == "rack_id"),
                    "{error:?}"
                );
            },
        },
        Case {
            scenario: "wrong rack column type",
            rack_projection: "1::integer AS rack_id",
            check: |error| {
                assert!(
                    matches!(&error, sqlx::Error::ColumnDecode { index, .. } if index == "\"rack_id\""),
                    "{error:?}"
                );
            },
        },
    ];
    for case in cases {
        let query = format!(
            "SELECT NULL::uuid AS expected_power_shelf_id,
                '02:00:00:00:03:01'::macaddr AS bmc_mac_address,
                'admin'::text AS bmc_username, 'test-password'::text AS bmc_password,
                'projection-shelf'::text AS serial_number, NULL::inet AS bmc_ip_address,
                ''::text AS metadata_name, ''::text AS metadata_description,
                '{{}}'::jsonb AS metadata_labels, false AS bmc_retain_credentials, {}",
            case.rack_projection
        );
        let row = sqlx::query(sqlx::AssertSqlSafe(query))
            .fetch_one(&pool)
            .await
            .expect("fetch projected power shelf");
        let error = ExpectedPowerShelf::from_row(&row)
            .map(|_| ())
            .expect_err(case.scenario);
        (case.check)(error);
    }
}

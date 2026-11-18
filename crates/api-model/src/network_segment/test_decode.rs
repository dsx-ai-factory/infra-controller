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

use carbide_uuid::network::NetworkSegmentId;
use sqlx::postgres::PgRow;
use sqlx::{Column, FromRow, Row};

use super::NetworkSegment;

#[crate::sqlx_test]
async fn network_segment_rejects_invalid_column_projections(pool: sqlx::PgPool) {
    struct Case {
        scenario: &'static str,
        projection: &'static str,
        check: fn(&PgRow, sqlx::Error),
    }

    // `vlan_id` is SMALLINT and `vni_id` is INTEGER in PostgreSQL, so their
    // failure cases exercise missing or wrongly typed query projections.
    let cases = [
        Case {
            scenario: "missing VLAN column",
            projection: "NULL::integer AS vni_id",
            check: |_, error| {
                assert!(
                    matches!(&error, sqlx::Error::ColumnNotFound(column) if column == "vlan_id"),
                    "{error:?}"
                );
            },
        },
        Case {
            scenario: "wrong VLAN column type",
            projection: "'invalid'::text AS vlan_id, NULL::integer AS vni_id",
            check: |_, error| {
                assert!(
                    matches!(&error, sqlx::Error::ColumnDecode { index, .. } if index == "\"vlan_id\""),
                    "{error:?}"
                );
            },
        },
        Case {
            scenario: "missing VNI column",
            projection: "NULL::smallint AS vlan_id",
            check: |_, error| {
                assert!(
                    matches!(&error, sqlx::Error::ColumnNotFound(column) if column == "vni_id"),
                    "{error:?}"
                );
            },
        },
        Case {
            scenario: "wrong VNI column type",
            projection: "NULL::smallint AS vlan_id, 'invalid'::text AS vni_id",
            check: |_, error| {
                assert!(
                    matches!(&error, sqlx::Error::ColumnDecode { index, .. } if index == "\"vni_id\""),
                    "{error:?}"
                );
            },
        },
        Case {
            scenario: "present malformed history",
            projection: "NULL::smallint AS vlan_id, NULL::integer AS vni_id, '{}'::jsonb AS history",
            check: |row, error| {
                let sqlx::Error::ColumnDecode { index, source } = error else {
                    panic!("expected ColumnDecode, got {error:?}");
                };
                assert_eq!(index, row.column("history").ordinal().to_string());
                assert!(
                    source
                        .downcast_ref::<serde_json::Error>()
                        .is_some_and(serde_json::Error::is_data),
                    "{source:?}"
                );
            },
        },
    ];
    for case in cases {
        let query = format!(
            r#"SELECT $1::uuid AS id, 'V1-T0'::text AS version,
                'projection-segment'::text AS name, NULL::uuid AS subdomain_id,
                1500::integer AS mtu, 'admin'::network_segment_type_t AS network_segment_type,
                'dynamic'::text AS allocation_strategy, false AS infer_slaac_eui64_addresses,
                NULL::uuid AS vpc_id, '{{"state":"ready"}}'::jsonb AS controller_state,
                'V1-T0'::text AS controller_state_version, NULL::jsonb AS controller_state_outcome,
                NULL::boolean AS can_stretch, '[]'::jsonb AS prefixes,
                now() AS created, now() AS updated, NULL::timestamptz AS deleted, {}"#,
            case.projection
        );
        let row = sqlx::query(sqlx::AssertSqlSafe(query))
            .bind(NetworkSegmentId::new())
            .fetch_one(&pool)
            .await
            .expect("fetch projected network segment");
        let error = NetworkSegment::from_row(&row).expect_err(case.scenario);
        (case.check)(&row, error);
    }
}

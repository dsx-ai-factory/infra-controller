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

use carbide_uuid::infiniband::IBPartitionId;
use sqlx::FromRow;
use sqlx::error::BoxDynError;

use super::{IBPartition, InvalidPartitionKeyError};
use crate::errors::ModelError;

#[crate::sqlx_test]
async fn ib_partition_rejects_invalid_stored_integers(pool: sqlx::PgPool) {
    struct StoredConfig {
        pkey: i32,
        mtu: i32,
        rate_limit: i32,
        service_level: i32,
    }
    struct Case {
        scenario: &'static str,
        config: StoredConfig,
        column: &'static str,
        check_source: fn(&BoxDynError) -> bool,
    }

    // Change one stored integer at a time. Both out-of-range PKeys would
    // become the valid key 42 if narrowed with `as u16` before validation.
    let valid = StoredConfig {
        pkey: 42,
        mtu: 4,
        rate_limit: 200,
        service_level: 0,
    };
    let cases = [
        Case {
            scenario: "PKey exceeds u16",
            config: StoredConfig {
                pkey: 65578,
                ..valid
            },
            column: "pkey",
            check_source: |source| source.is::<TryFromIntError>(),
        },
        Case {
            scenario: "negative PKey",
            config: StoredConfig {
                pkey: -65494,
                ..valid
            },
            column: "pkey",
            check_source: |source| source.is::<TryFromIntError>(),
        },
        Case {
            scenario: "PKey exceeds 0x7fff",
            config: StoredConfig {
                pkey: 0x8000,
                ..valid
            },
            column: "pkey",
            check_source: |source| source.is::<InvalidPartitionKeyError>(),
        },
        Case {
            scenario: "invalid MTU",
            config: StoredConfig { mtu: 3, ..valid },
            column: "mtu",
            check_source: |source| source.is::<ModelError>(),
        },
        Case {
            scenario: "invalid rate limit",
            config: StoredConfig {
                rate_limit: 7,
                ..valid
            },
            column: "rate_limit",
            check_source: |source| source.is::<ModelError>(),
        },
        Case {
            scenario: "invalid service level",
            config: StoredConfig {
                service_level: 16,
                ..valid
            },
            column: "service_level",
            check_source: |source| source.is::<ModelError>(),
        },
    ];

    for case in cases {
        let row = sqlx::query(
            r#"SELECT $1::uuid AS id, 'V1-T0'::text AS config_version,
                'decode-partition'::text AS name, '{}'::jsonb AS labels,
                ''::text AS description, 'decode-tenant'::text AS organization_id,
                '{"state":"provisioning"}'::jsonb AS controller_state,
                'V1-T0'::text AS controller_state_version,
                NULL::jsonb AS controller_state_outcome, NULL::jsonb AS status,
                NULL::timestamptz AS deleted, $2::integer AS pkey,
                $3::integer AS mtu, $4::integer AS rate_limit,
                $5::integer AS service_level"#,
        )
        .bind(IBPartitionId::new())
        .bind(case.config.pkey)
        .bind(case.config.mtu)
        .bind(case.config.rate_limit)
        .bind(case.config.service_level)
        .fetch_one(&pool)
        .await
        .expect("fetch projected IB partition");

        let error = IBPartition::from_row(&row).expect_err(case.scenario);
        let sqlx::Error::ColumnDecode { index, source } = error else {
            panic!("{}: expected ColumnDecode, got {error:?}", case.scenario);
        };
        assert_eq!(index, case.column, "{}", case.scenario);
        assert!(
            (case.check_source)(&source),
            "{}: {source:?}",
            case.scenario
        );
    }
}

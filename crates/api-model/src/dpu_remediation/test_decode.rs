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

use carbide_uuid::machine::{MachineId, MachineIdSource, MachineType};
use sqlx::PgPool;

use super::*;

#[crate::sqlx_test]
async fn remediation_requires_optional_columns_in_query_results(pool: PgPool) {
    let optional_columns = [
        ("metadata_labels", "'{}'::jsonb"),
        ("metadata_name", "NULL::text"),
        ("metadata_description", "NULL::text"),
        ("script_reviewed_by", "NULL::text"),
    ];

    for (omitted_column, _) in optional_columns {
        let selected_columns = optional_columns
            .iter()
            .filter(|(column, _)| *column != omitted_column)
            .map(|(column, value)| format!("{value} AS {column}"))
            .collect::<Vec<_>>()
            .join(", ");
        let query = format!(
            "SELECT
                $1::uuid AS id,
                'echo repair-interface'::text AS script,
                0::integer AS retries,
                false AS enabled,
                'script-author'::text AS script_author,
                now() AS creation_time,
                {selected_columns}"
        );
        let row = sqlx::query(sqlx::AssertSqlSafe(query))
            .bind(RemediationId::new())
            .fetch_one(&pool)
            .await
            .expect("fetch remediation projection");
        let error = Remediation::from_row(&row)
            .expect_err("an omitted column must not become an absent value");
        assert!(
            matches!(&error, sqlx::Error::ColumnNotFound(column) if column == omitted_column),
            "{omitted_column}: {error:?}"
        );
    }
}

#[crate::sqlx_test]
async fn applied_remediation_requires_status_in_query_results(pool: PgPool) {
    let machine_id = MachineId::new(
        MachineIdSource::ProductBoardChassisSerial,
        [0x74; 32],
        MachineType::Dpu,
    );
    let row = sqlx::query(
        "SELECT
            $1::uuid AS id,
            $2::text AS dpu_machine_id,
            1::integer AS attempt,
            false AS succeeded,
            now() AS applied_time",
    )
    .bind(RemediationId::new())
    .bind(machine_id)
    .fetch_one(&pool)
    .await
    .expect("fetch applied remediation projection without status");
    let error = AppliedRemediation::from_row(&row)
        .expect_err("omitted status must not become an empty map");
    assert!(
        matches!(&error, sqlx::Error::ColumnNotFound(column) if column == "status"),
        "{error:?}"
    );
}

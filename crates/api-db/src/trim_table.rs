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

use sqlx::PgConnection;

use crate::DatabaseError;

pub async fn trim_table(
    txn: &mut PgConnection,
    target: model::trim_table::TrimTableTarget,
    keep_entries: u32,
) -> Result<i32, DatabaseError> {
    // choose a target and call an appropriate stored procedure/function
    match target {
        model::trim_table::TrimTableTarget::MeasuredBoot => {
            let query = "SELECT * FROM measured_boot_reports_keep_limit($1)";

            let val: (i32,) = sqlx::query_as(query)
                .bind(keep_entries as i32)
                .fetch_one(txn)
                .await
                .map_err(|e| DatabaseError::new(query, e))?;
            Ok(val.0)
        }
    }
}

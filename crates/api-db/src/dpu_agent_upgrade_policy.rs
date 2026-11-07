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
use model::machine::upgrade_policy::AgentUpgradePolicy;
use sqlx::{PgConnection, Row};

use crate::DatabaseError;

pub async fn get(txn: &mut PgConnection) -> Result<Option<AgentUpgradePolicy>, DatabaseError> {
    let query = "SELECT policy FROM dpu_agent_upgrade_policy ORDER BY created DESC LIMIT 1";
    let Some(row) = sqlx::query(query)
        .fetch_optional(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?
    else {
        return Ok(None);
    };
    let str_policy: &str = row
        .try_get("policy")
        .map_err(|e| DatabaseError::query(query, e))?;
    Ok(Some(str_policy.into()))
}

pub async fn set(txn: &mut PgConnection, policy: AgentUpgradePolicy) -> Result<(), DatabaseError> {
    let query = "INSERT INTO dpu_agent_upgrade_policy VALUES ($1)";
    sqlx::query(query)
        .bind(policy.to_string())
        .execute(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    Ok(())
}

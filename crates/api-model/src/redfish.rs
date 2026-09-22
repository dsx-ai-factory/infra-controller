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
use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgRow;
use sqlx::types::Json;
use sqlx::{FromRow, Row};

pub struct ActionRequest {
    pub request_id: i64,
    pub requester: String,
    pub approvers: Vec<String>,
    pub approver_dates: Vec<DateTime<Utc>>,
    pub machine_ips: Vec<String>,
    pub board_serials: Vec<String>,
    pub target: String,
    pub action: String,
    pub parameters: String,
    pub applied_at: Option<DateTime<Utc>>,
    pub applier: Option<String>,
    pub results: Vec<Option<BMCResponse>>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct BMCResponse {
    pub headers: HashMap<String, String>,
    pub status: String,
    pub body: String,
    pub completed_at: DateTime<Utc>,
}

impl<'r> FromRow<'r, PgRow> for ActionRequest {
    fn from_row(row: &'r PgRow) -> Result<Self, sqlx::Error> {
        let request_id = row.try_get("request_id")?;
        let requester = row.try_get("requester")?;
        let approvers: Vec<_> = row.try_get("approvers")?;
        let approver_dates: Vec<_> = row.try_get("approver_dates")?;
        let machine_ips: Vec<String> = row.try_get("machine_ips")?;
        let board_serials: Vec<String> = row.try_get("board_serials")?;
        let target = row.try_get("target")?;
        let action = row.try_get("action")?;
        let parameters = row.try_get("parameters")?;
        let applied_at = row.try_get("applied_at")?;
        let applier = row.try_get("applier")?;
        let results: Option<Vec<Option<Json<BMCResponse>>>> = row.try_get("results")?;
        Ok(Self {
            request_id,
            requester,
            approvers,
            approver_dates,
            machine_ips,
            board_serials,
            target,
            action,
            parameters,
            applied_at,
            applier,
            results: results
                .unwrap_or_default()
                .into_iter()
                .map(|option| option.map(|json| json.0))
                .collect(),
        })
    }
}

#[derive(Clone, Copy, Debug)]
pub struct RedfishActionId {
    pub request_id: i64,
}

impl From<i64> for RedfishActionId {
    fn from(request_id: i64) -> Self {
        RedfishActionId { request_id }
    }
}

#[derive(Clone, Debug, Default)]
pub struct RedfishListActionsFilter {
    pub machine_ip: Option<String>,
}

#[derive(Clone, Debug)]
pub struct RedfishCreateAction {
    pub target: String,
    pub action: String,
    pub parameters: String,
}

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

use chrono::{DateTime, Utc};
use config_version::ConfigVersion;
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgRow;
use sqlx::{FromRow, Row};

/// History of states for a single object.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateHistoryRecord {
    /// The state that was entered
    pub state: String,
    // The version number associated with the state change
    pub state_version: ConfigVersion,
    /// The time when the state was observed.
    ///
    /// Older history payloads did not carry a separate timestamp, so RPC
    /// conversions fall back to the version timestamp when this field is absent.
    #[serde(default, alias = "timestamp")]
    pub time: Option<DateTime<Utc>>,
}

impl<'r> FromRow<'r, PgRow> for StateHistoryRecord {
    fn from_row(row: &'r PgRow) -> Result<Self, sqlx::Error> {
        let time = match row.try_get("timestamp") {
            Ok(time) => Some(time),
            Err(sqlx::Error::ColumnNotFound(_)) => match row.try_get("time") {
                Ok(time) => Some(time),
                Err(sqlx::Error::ColumnNotFound(_)) => None,
                Err(err) => return Err(err),
            },
            Err(err) => return Err(err),
        };

        Ok(StateHistoryRecord {
            state: row.try_get("state")?,
            state_version: row.try_get("state_version")?,
            time,
        })
    }
}

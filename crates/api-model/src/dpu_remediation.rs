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
use std::fmt::{Display, Formatter};

use carbide_uuid::dpu_remediations::RemediationId;
use carbide_uuid::machine::DpuMachineId;
use chrono::{DateTime, Utc};
use sqlx::postgres::PgRow;
use sqlx::{FromRow, Row};

use crate::metadata::Metadata;

pub struct RemediationApplicationStatus {
    pub succeeded: bool,
    pub metadata: Option<Metadata>,
}

pub struct NewRemediation {
    pub script: String,
    pub metadata: Option<Metadata>,
    pub retries: i32,
    pub author: Author,
}

#[derive(Clone, Debug)]
pub struct Author {
    name: String,
}

impl From<String> for Author {
    fn from(value: String) -> Self {
        Self { name: value }
    }
}

impl Display for Author {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.name)
    }
}

#[derive(Clone, Debug)]
pub struct Reviewer {
    name: String,
}

impl From<String> for Reviewer {
    fn from(value: String) -> Self {
        Self { name: value }
    }
}

impl Display for Reviewer {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.name)
    }
}

#[derive(Debug, Clone)]
pub struct Remediation {
    pub id: RemediationId,
    pub script: String,
    pub metadata: Option<Metadata>,
    pub reviewer: Option<Reviewer>,
    pub author: Author,
    pub retries: i32,
    pub enabled: bool,
    pub creation_time: DateTime<Utc>,
}

impl<'r> FromRow<'r, PgRow> for Remediation {
    fn from_row(row: &'r PgRow) -> Result<Self, sqlx::Error> {
        let metadata_labels: Option<sqlx::types::Json<HashMap<String, String>>> =
            row.try_get("metadata_labels").ok();
        let metadata_name: Option<String> = row.try_get("metadata_name").ok();
        let metadata_description: Option<String> = row.try_get("metadata_description").ok();

        let metadata = if metadata_name
            .as_ref()
            .map(|x| !x.trim().is_empty())
            .unwrap_or(false)
            || metadata_description
                .as_ref()
                .map(|x| !x.trim().is_empty())
                .unwrap_or(false)
            || metadata_labels
                .as_ref()
                .map(|x| !x.is_empty())
                .unwrap_or(false)
        {
            Some(Metadata {
                name: metadata_name.unwrap_or_default(),
                description: metadata_description.unwrap_or_default(),
                labels: metadata_labels.map(|x| x.0).unwrap_or_default(),
            })
        } else {
            None
        };

        let reviewer: Option<String> = row.try_get("script_reviewed_by").ok();
        let author: String = row.try_get("script_author")?;

        Ok(Self {
            id: row.try_get("id")?,
            script: row.try_get("script")?,
            retries: row.try_get("retries")?,
            enabled: row.try_get("enabled")?,
            reviewer: reviewer.map(Reviewer::from),
            author: Author::from(author),
            creation_time: row.try_get("creation_time")?,
            metadata,
        })
    }
}

pub struct NewAppliedRemediation {
    pub id: RemediationId,
    pub dpu_machine_id: String,
    pub attempt: i32,
    pub succeeded: bool,
    pub status: HashMap<String, String>,
}

#[derive(Clone, Debug)]
pub struct AppliedRemediation {
    pub id: RemediationId,
    pub dpu_machine_id: DpuMachineId,
    pub attempt: i32,
    pub succeeded: bool,
    pub status: HashMap<String, String>,
    pub applied_time: DateTime<Utc>,
}

impl<'r> FromRow<'r, PgRow> for AppliedRemediation {
    fn from_row(row: &'r PgRow) -> Result<Self, sqlx::Error> {
        let status: Option<sqlx::types::Json<HashMap<String, String>>> = row.try_get("status").ok();
        let status = status.map(|x| x.0).unwrap_or_default();

        Ok(Self {
            id: row.try_get("id")?,
            dpu_machine_id: row.try_get("dpu_machine_id")?,
            attempt: row.try_get("attempt")?,
            succeeded: row.try_get("succeeded")?,
            applied_time: row.try_get("applied_time")?,
            status,
        })
    }
}

#[derive(Debug, Clone)]
pub struct ApproveRemediation {
    pub id: RemediationId,
    pub reviewer: Reviewer,
}

#[derive(Debug, Clone)]
pub struct RevokeRemediation {
    pub id: RemediationId,
}

#[derive(Debug, Clone)]
pub struct EnableRemediation {
    pub id: RemediationId,
}

#[derive(Debug, Clone)]
pub struct DisableRemediation {
    pub id: RemediationId,
}

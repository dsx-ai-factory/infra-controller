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

use carbide_uuid::nvlink::NvLinkLogicalPartitionId;
use chrono::{DateTime, Utc};
use config_version::ConfigVersion;
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgRow;
use sqlx::{FromRow, Row};

use crate::metadata::Metadata;
use crate::tenant::TenantOrganizationId;

#[derive(Clone, Debug, Default)]
pub struct NvLinkLogicalPartitionSearchFilter {
    pub name: Option<String>,
}

#[derive(Debug, Clone)]
pub struct NewLogicalPartition {
    pub id: NvLinkLogicalPartitionId,
    pub config: LogicalPartitionConfig,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct LogicalPartitionConfig {
    pub metadata: Metadata,
    pub tenant_organization_id: TenantOrganizationId,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct LogicalPartitionName(String);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum LogicalPartitionState {
    Provisioning,
    Ready,
    Updating,
    Error,
    Deleting,
}

#[derive(Debug, Clone)]
pub struct LogicalPartition {
    pub id: NvLinkLogicalPartitionId,

    pub name: String,
    pub description: String,
    pub tenant_organization_id: TenantOrganizationId,

    pub config_version: ConfigVersion,

    pub partition_state: LogicalPartitionState,

    pub created: DateTime<Utc>,
    pub updated: DateTime<Utc>,
    pub deleted: Option<DateTime<Utc>>,
}

/// Returns whether a logical partition was deleted by user
pub fn is_marked_as_deleted(partition: &LogicalPartition) -> bool {
    partition.deleted.is_some()
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LogicalPartitionSnapshotPgJson {
    pub id: NvLinkLogicalPartitionId,
    pub name: String,
    pub description: String,
    pub tenant_organization_id: TenantOrganizationId,
    pub config_version: ConfigVersion,
    pub partition_state: LogicalPartitionState,
    pub created: DateTime<Utc>,
    pub updated: DateTime<Utc>,
    pub deleted: Option<DateTime<Utc>>,
}

impl TryFrom<LogicalPartitionSnapshotPgJson> for LogicalPartition {
    type Error = sqlx::Error;
    fn try_from(value: LogicalPartitionSnapshotPgJson) -> sqlx::Result<Self> {
        Ok(Self {
            id: value.id,
            name: value.name,
            description: value.description,
            tenant_organization_id: value.tenant_organization_id,
            config_version: value.config_version,
            partition_state: value.partition_state,
            created: value.created,
            updated: value.updated,
            deleted: value.deleted,
        })
    }
}

impl<'r> FromRow<'r, PgRow> for LogicalPartitionSnapshotPgJson {
    fn from_row(row: &'r PgRow) -> Result<Self, sqlx::Error> {
        // Json<T> deserializes the row bytes straight into the snapshot
        // struct, skipping the intermediate serde_json::Value DOM.
        let json: sqlx::types::Json<LogicalPartitionSnapshotPgJson> = row.try_get(0)?;
        Ok(json.0)
    }
}

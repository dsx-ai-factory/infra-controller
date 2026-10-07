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

use carbide_uuid::spx::SpxPartitionId;
use chrono::{DateTime, Utc};
use config_version::ConfigVersion;
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgRow;
use sqlx::{FromRow, Row};

use crate::tenant::TenantOrganizationId;

#[derive(Clone, Debug, Default)]
pub struct SpxPartitionSearchFilter {
    pub name: Option<String>,
    pub tenant_org_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct NewSpxPartition {
    pub id: SpxPartitionId,
    pub name: String,
    pub description: String,
    pub tenant_organization_id: String,
    pub vni: Option<i32>,
}

#[derive(Debug, Clone)]
pub struct SpxPartition {
    pub id: SpxPartitionId,
    pub name: String,
    pub description: String,
    pub tenant_organization_id: TenantOrganizationId,
    pub config_version: ConfigVersion,
    pub vni: Option<i32>,
    pub created: DateTime<Utc>,
    pub updated: DateTime<Utc>,
    pub deleted: Option<DateTime<Utc>>,
}

/// Returns whether the SPX partition has been soft-deleted
pub fn is_marked_as_deleted(partition: &SpxPartition) -> bool {
    partition.deleted.is_some()
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SpxPartitionSnapshotPgJson {
    pub id: SpxPartitionId,
    pub name: String,
    pub description: String,
    pub tenant_organization_id: String,
    pub config_version: ConfigVersion,
    pub vni: Option<i32>,
    pub created: DateTime<Utc>,
    pub updated: DateTime<Utc>,
    pub deleted: Option<DateTime<Utc>>,
}

impl TryFrom<SpxPartitionSnapshotPgJson> for SpxPartition {
    type Error = sqlx::Error;
    fn try_from(value: SpxPartitionSnapshotPgJson) -> sqlx::Result<Self> {
        let tenant_organization_id =
            TenantOrganizationId::try_from(value.tenant_organization_id.clone())
                .map_err(|e| sqlx::Error::Decode(Box::new(e)))?;

        Ok(Self {
            id: value.id,
            name: value.name,
            description: value.description,
            tenant_organization_id,
            config_version: value.config_version,
            vni: value.vni,
            created: value.created,
            updated: value.updated,
            deleted: value.deleted,
        })
    }
}

impl<'r> FromRow<'r, PgRow> for SpxPartitionSnapshotPgJson {
    fn from_row(row: &'r PgRow) -> Result<Self, sqlx::Error> {
        // Json<T> deserializes the row bytes straight into the snapshot
        // struct, skipping the intermediate serde_json::Value DOM.
        let json: sqlx::types::Json<SpxPartitionSnapshotPgJson> = row.try_get(0)?;
        Ok(json.0)
    }
}

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

use carbide_uuid::compute_allocation::ComputeAllocationId;
use carbide_uuid::instance_type::InstanceTypeId;
use chrono::prelude::*;
use config_version::ConfigVersion;
use sqlx::Row;
use sqlx::postgres::PgRow;

use super::tenant::TenantOrganizationId;
use crate::metadata::Metadata;

pub const MAX_COMPUTE_ALLOCATION_SIZE: u32 = 100000;

/* ********************************** */
/*          ComputeAllocation         */
/* ********************************** */

/// ComputeAllocation represents an amount of compute
/// resources that should be made available to a tenant.
#[derive(Clone, Debug, PartialEq)]
pub struct ComputeAllocation {
    pub id: ComputeAllocationId,
    pub version: ConfigVersion,
    pub tenant_organization_id: TenantOrganizationId,
    pub instance_type_id: InstanceTypeId,
    pub count: u32,
    pub created: DateTime<Utc>,
    pub deleted: Option<DateTime<Utc>>,
    pub created_by: Option<String>,
    pub updated_by: Option<String>,
    pub metadata: Metadata,
}

impl<'r> sqlx::FromRow<'r, PgRow> for ComputeAllocation {
    fn from_row(row: &'r PgRow) -> Result<Self, sqlx::Error> {
        let labels: sqlx::types::Json<HashMap<String, String>> = row.try_get("labels")?;

        let metadata = Metadata {
            name: row.try_get("name")?,
            description: row.try_get("description")?,
            labels: labels.0,
        };

        let count: i32 = row.try_get("count")?;

        let tenant_organization_id: String = row.try_get("tenant_organization_id")?;

        Ok(ComputeAllocation {
            id: row.try_get("id")?,
            version: row.try_get("version")?,
            tenant_organization_id: tenant_organization_id
                .parse::<TenantOrganizationId>()
                .map_err(|e| sqlx::Error::Decode(Box::new(e)))?,
            instance_type_id: row.try_get("instance_type_id")?,
            created_by: row.try_get("created_by")?,
            updated_by: row.try_get("updated_by")?,
            created: row.try_get("created")?,
            deleted: row.try_get("deleted")?,
            metadata,
            count: count
                .try_into()
                .map_err(|e| sqlx::Error::Decode(Box::new(e)))?,
        })
    }
}

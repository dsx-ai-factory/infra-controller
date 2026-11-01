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

use std::fmt;
use std::fmt::Debug;
use std::str::FromStr;

use carbide_uuid::{DbColumns, DbTable};
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgRow;
use sqlx::{Error, Row};
use uuid::Uuid;

use crate::errors::ModelError;
use crate::tenant::TenantOrganizationId;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OsImageAttributes {
    pub id: Uuid,
    pub source_url: String,
    pub digest: String,
    pub tenant_organization_id: TenantOrganizationId,
    pub create_volume: bool,
    pub name: Option<String>,
    pub description: Option<String>,
    pub auth_type: Option<String>,
    pub auth_token: Option<String>,
    pub rootfs_id: Option<String>,
    pub rootfs_label: Option<String>,
    pub boot_disk: Option<String>,
    pub capacity: Option<u64>,
    pub bootfs_id: Option<String>,
    pub efifs_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, sqlx::Type)]
#[sqlx(rename_all = "lowercase")]
#[sqlx(type_name = "os_image_status")]
/// Note: "Ready" is the only actually-used variant as of today. Other statuses are meant for when
/// carbide manages storage volumes, which is not the case today.
pub enum OsImageStatus {
    Uninitialized = 0, // initial state when db entry created
    InProgress,        // golden volume creation in progress if applicable
    Failed,            // golden volume creation error
    Ready,             // ready for use during allocate instance calls
    Disabled,          // disabled or deprecated, no new instance allocations can use it
}

impl fmt::Display for OsImageStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let string = match self {
            OsImageStatus::Uninitialized => "uninitialized",
            OsImageStatus::InProgress => "inprogress",
            OsImageStatus::Failed => "failed",
            OsImageStatus::Ready => "ready",
            OsImageStatus::Disabled => "disabled",
        };
        write!(f, "{string}")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OsImage {
    pub attributes: OsImageAttributes,
    pub status: OsImageStatus,
    pub status_message: Option<String>,
    pub created_at: Option<String>,
    pub modified_at: Option<String>,
}

impl FromStr for OsImageStatus {
    type Err = ModelError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "uninitialized" => Ok(OsImageStatus::Uninitialized),
            "inprogress" => Ok(OsImageStatus::InProgress),
            "failed" => Ok(OsImageStatus::Failed),
            "ready" => Ok(OsImageStatus::Ready),
            "disabled" => Ok(OsImageStatus::Disabled),
            "" => Ok(OsImageStatus::Uninitialized),
            _ => Err(ModelError::InvalidArgument(format!(
                "Invalid OsImageStatus: {s}"
            ))),
        }
    }
}

// `attributes` groups several table columns; `create_volume` is not stored.
impl DbTable for OsImage {
    fn db_table_name() -> &'static str {
        "os_images"
    }

    fn db_table_columns() -> DbColumns {
        DbColumns::new(&[
            "id",
            "name",
            "description",
            "source_url",
            "digest",
            "organization_id",
            "auth_type",
            "auth_token",
            "rootfs_id",
            "rootfs_label",
            "boot_disk",
            "bootfs_id",
            "efifs_id",
            "capacity",
            "status",
            "status_message",
            "created_at",
            "modified_at",
        ])
    }

    fn db_aliased_table() -> &'static str {
        "os_images AS l"
    }

    fn db_aliased_columns() -> DbColumns {
        DbColumns::new(&[
            "l.id",
            "l.name",
            "l.description",
            "l.source_url",
            "l.digest",
            "l.organization_id",
            "l.auth_type",
            "l.auth_token",
            "l.rootfs_id",
            "l.rootfs_label",
            "l.boot_disk",
            "l.bootfs_id",
            "l.efifs_id",
            "l.capacity",
            "l.status",
            "l.status_message",
            "l.created_at",
            "l.modified_at",
        ])
    }
}

impl<'r> sqlx::FromRow<'r, PgRow> for OsImage {
    fn from_row(row: &'r PgRow) -> Result<Self, Error> {
        let tenant_organization_id: String = row.try_get("organization_id")?;
        let cap: i64 = row.try_get("capacity")?;
        let capacity = if cap == 0 { None } else { Some(cap as u64) };
        Ok(OsImage {
            attributes: OsImageAttributes {
                id: row.try_get("id")?,
                source_url: row.try_get("source_url")?,
                digest: row.try_get("digest")?,
                tenant_organization_id: TenantOrganizationId::from_str(&tenant_organization_id)
                    .map_err(|e| sqlx::Error::Protocol(e.to_string()))?,
                create_volume: false,
                name: row.try_get("name")?,
                description: row.try_get("description")?,
                auth_type: row.try_get("auth_type")?,
                auth_token: row.try_get("auth_token")?,
                rootfs_id: row.try_get("rootfs_id")?,
                rootfs_label: row.try_get("rootfs_label")?,
                boot_disk: row.try_get("boot_disk")?,
                bootfs_id: row.try_get("bootfs_id")?,
                efifs_id: row.try_get("efifs_id")?,
                capacity,
            },
            status: row.try_get("status")?,
            status_message: row.try_get("status_message")?,
            created_at: row.try_get("created_at")?,
            modified_at: row.try_get("modified_at")?,
        })
    }
}

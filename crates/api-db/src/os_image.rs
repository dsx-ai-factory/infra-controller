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
use model::storage::{OsImage, OsImageAttributes, OsImageStatus};
use model::tenant::TenantOrganizationId;
use sqlx::PgConnection;
use uuid::Uuid;

use crate::DatabaseError;

pub async fn list(
    txn: &mut PgConnection,
    tenant_organization_id: Option<TenantOrganizationId>,
) -> Result<Vec<OsImage>, DatabaseError> {
    if let Some(tenant_organization_id) = tenant_organization_id {
        let query = "SELECT * from os_images l WHERE l.organization_id=$1";
        sqlx::query_as(query)
            .bind(tenant_organization_id.to_string())
            .fetch_all(txn)
            .await
            .map_err(|e| DatabaseError::new("os_images All", e))
    } else {
        let query = "SELECT * from os_images l";
        sqlx::query_as(query)
            .fetch_all(txn)
            .await
            .map_err(|e| DatabaseError::new("os_images All", e))
    }
}

pub async fn get(txn: &mut PgConnection, os_image_id: Uuid) -> Result<OsImage, DatabaseError> {
    let query = "SELECT * from os_images l WHERE l.id = $1";
    sqlx::query_as(query)
        .bind(os_image_id)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::new("os_images All", e))
}

pub async fn create(
    txn: &mut PgConnection,
    attrs: &OsImageAttributes,
) -> Result<OsImage, DatabaseError> {
    let timestamp: DateTime<Utc> = Utc::now();
    let os_image = OsImage {
        attributes: attrs.clone(),
        status: OsImageStatus::Ready,
        status_message: None,
        created_at: Some(timestamp.to_string()),
        modified_at: None,
    };

    persist(os_image, txn, false).await
}

pub async fn delete(value: &OsImage, txn: &mut PgConnection) -> Result<(), DatabaseError> {
    let query = "DELETE FROM os_images WHERE id = $1";
    sqlx::query(query)
        .bind(value.attributes.id)
        .execute(txn)
        .await
        .map(|_| ())
        .map_err(|e| DatabaseError::query(query, e))
}

pub async fn update(
    value: &OsImage,
    txn: &mut PgConnection,
    new_attrs: OsImageAttributes,
) -> Result<OsImage, DatabaseError> {
    let timestamp: DateTime<Utc> = Utc::now();
    let os_image = OsImage {
        attributes: new_attrs,
        status: value.status.clone(),
        status_message: value.status_message.clone(),
        created_at: value.created_at.clone(),
        modified_at: Some(timestamp.to_string()),
    };
    persist(os_image, txn, true).await
}

async fn persist(
    value: OsImage,
    txn: &mut PgConnection,
    update: bool,
) -> Result<OsImage, DatabaseError> {
    let os_image = if update {
        let query = "UPDATE os_images SET name = $1, description = $2, auth_type = $3, auth_token = $4, rootfs_id = $5, rootfs_label = $6, boot_disk = $7, bootfs_id = $8, efifs_id = $9, modified_at = $10, status = $11, status_message = $12 WHERE id = $13 RETURNING *";
        sqlx::query_as(query)
            .bind(&value.attributes.name)
            .bind(&value.attributes.description)
            .bind(&value.attributes.auth_type)
            .bind(&value.attributes.auth_token)
            .bind(&value.attributes.rootfs_id)
            .bind(&value.attributes.rootfs_label)
            .bind(&value.attributes.boot_disk)
            .bind(&value.attributes.bootfs_id)
            .bind(&value.attributes.efifs_id)
            .bind(&value.modified_at)
            .bind(value.status.clone())
            .bind(&value.status_message)
            .bind(value.attributes.id)
            .fetch_one(txn)
            .await
            .map_err(|e| DatabaseError::query(query, e))?
    } else {
        let capacity = match value.attributes.capacity {
            Some(x) => x as i64,
            None => 0,
        };
        let query = "INSERT INTO os_images(id, name, description, source_url, digest, organization_id, auth_type, auth_token, rootfs_id, rootfs_label, boot_disk, bootfs_id, efifs_id, capacity, status, status_message, created_at, modified_at) VALUES($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18) RETURNING *";
        sqlx::query_as(query)
            .bind(value.attributes.id)
            .bind(&value.attributes.name)
            .bind(&value.attributes.description)
            .bind(&value.attributes.source_url)
            .bind(&value.attributes.digest)
            .bind(value.attributes.tenant_organization_id.to_string())
            .bind(&value.attributes.auth_type)
            .bind(&value.attributes.auth_token)
            .bind(&value.attributes.rootfs_id)
            .bind(&value.attributes.rootfs_label)
            .bind(&value.attributes.boot_disk)
            .bind(&value.attributes.bootfs_id)
            .bind(&value.attributes.efifs_id)
            .bind(capacity)
            .bind(value.status.clone())
            .bind(&value.status_message)
            .bind(&value.created_at)
            .bind(&value.modified_at)
            .fetch_one(txn)
            .await
            .map_err(|e| DatabaseError::query(query, e))?
    };
    Ok(os_image)
}

#[cfg(test)]
mod tests {
    use sqlx::PgPool;
    use uuid::Uuid;

    const EXPAND_BOOT_DISK_MIGRATION: &str =
        include_str!("../migrations/20260828202227_expand_os_image_boot_disk.sql");

    #[crate::sqlx_test]
    async fn boot_disk_migration_preserves_existing_value_and_accepts_long_path(pool: PgPool) {
        sqlx::query("ALTER TABLE os_images ALTER COLUMN boot_disk TYPE varchar(64)")
            .execute(&pool)
            .await
            .unwrap();

        let id = Uuid::new_v4();
        let original_boot_disk = "/dev/disk/by-id/nvme-short";
        sqlx::query(
            "INSERT INTO os_images
                 (id, source_url, digest, organization_id, boot_disk)
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(id)
        .bind("https://example.invalid/image")
        .bind("sha256:test")
        .bind("test-organization")
        .bind(original_boot_disk)
        .execute(&pool)
        .await
        .unwrap();

        sqlx::raw_sql(EXPAND_BOOT_DISK_MIGRATION)
            .execute(&pool)
            .await
            .unwrap();

        let stored_boot_disk: String =
            sqlx::query_scalar("SELECT boot_disk FROM os_images WHERE id = $1")
                .bind(id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(stored_boot_disk, original_boot_disk);

        let long_boot_disk =
            "/dev/disk/by-id/nvme-Dell_DC_NVMe_CD7_U.2_960GB_Z3W0A01DTXBH-extra-long";
        assert!(long_boot_disk.len() > 64);
        sqlx::query("UPDATE os_images SET boot_disk = $1 WHERE id = $2")
            .bind(long_boot_disk)
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();

        let stored_boot_disk: String =
            sqlx::query_scalar("SELECT boot_disk FROM os_images WHERE id = $1")
                .bind(id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(stored_boot_disk, long_boot_disk);
    }
}

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

use model::storage::{OsImage, OsImageAttributes, OsImageStatus};
use model::tenant::TenantOrganizationId;
use uuid::Uuid;

use crate as rpc;
use crate::errors::RpcDataConversionError;

impl TryFrom<OsImageAttributes> for rpc::forge::OsImageAttributes {
    type Error = RpcDataConversionError;
    fn try_from(image_attrs: OsImageAttributes) -> Result<Self, Self::Error> {
        let id = rpc::Uuid::from(image_attrs.id);
        Ok(Self {
            id: Some(id),
            source_url: image_attrs.source_url,
            digest: image_attrs.digest,
            tenant_organization_id: image_attrs.tenant_organization_id.to_string(),
            create_volume: image_attrs.create_volume,
            name: image_attrs.name,
            description: image_attrs.description,
            auth_type: image_attrs.auth_type,
            auth_token: image_attrs.auth_token,
            rootfs_id: image_attrs.rootfs_id,
            rootfs_label: image_attrs.rootfs_label,
            boot_disk: image_attrs.boot_disk,
            capacity: image_attrs.capacity,
            bootfs_id: image_attrs.bootfs_id,
            efifs_id: image_attrs.efifs_id,
        })
    }
}

impl TryFrom<rpc::forge::OsImageAttributes> for OsImageAttributes {
    type Error = RpcDataConversionError;
    fn try_from(image_attrs: rpc::forge::OsImageAttributes) -> Result<Self, Self::Error> {
        if image_attrs.id.is_none() {
            return Err(RpcDataConversionError::MissingArgument("image id"));
        }
        let id = Uuid::try_from(image_attrs.id.clone().unwrap()).map_err(|_e| {
            RpcDataConversionError::InvalidUuid("os image id", image_attrs.id.unwrap().to_string())
        })?;
        Ok(Self {
            id,
            source_url: image_attrs.source_url,
            digest: image_attrs.digest,
            tenant_organization_id: TenantOrganizationId::try_from(
                image_attrs.tenant_organization_id,
            )
            .map_err(|e| {
                RpcDataConversionError::InvalidValue(
                    "tenant_organization_id".to_string(),
                    e.to_string(),
                )
            })?,
            create_volume: image_attrs.create_volume,
            name: image_attrs.name,
            description: image_attrs.description,
            auth_type: image_attrs.auth_type,
            auth_token: image_attrs.auth_token,
            rootfs_id: image_attrs.rootfs_id,
            rootfs_label: image_attrs.rootfs_label,
            boot_disk: image_attrs.boot_disk,
            capacity: image_attrs.capacity,
            bootfs_id: image_attrs.bootfs_id,
            efifs_id: image_attrs.efifs_id,
        })
    }
}

impl TryFrom<OsImageStatus> for rpc::forge::OsImageStatus {
    type Error = RpcDataConversionError;
    fn try_from(value: OsImageStatus) -> Result<Self, Self::Error> {
        match value {
            OsImageStatus::Uninitialized => Ok(rpc::forge::OsImageStatus::ImageUninitialized),
            OsImageStatus::InProgress => Ok(rpc::forge::OsImageStatus::ImageInProgress),
            OsImageStatus::Failed => Ok(rpc::forge::OsImageStatus::ImageFailed),
            OsImageStatus::Ready => Ok(rpc::forge::OsImageStatus::ImageReady),
            OsImageStatus::Disabled => Ok(rpc::forge::OsImageStatus::ImageDisabled),
        }
    }
}

impl TryFrom<OsImage> for rpc::forge::OsImage {
    type Error = RpcDataConversionError;
    fn try_from(image: OsImage) -> Result<Self, Self::Error> {
        Ok(Self {
            attributes: Some(rpc::forge::OsImageAttributes::try_from(image.attributes)?),
            status: rpc::forge::OsImageStatus::try_from(image.status)? as i32,
            status_message: image.status_message,
            created_at: image.created_at,
            modified_at: image.modified_at,
        })
    }
}

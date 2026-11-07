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
use model::storage::{OsImageAttributes, OsImageStatus};
use model::tenant::TenantOrganizationId;
use tonic::{Request, Response, Status};
use uuid::Uuid;

use crate::CarbideError;
use crate::api::Api;

// these functions are the grpc api handlers called from api.rs
// todo: maybe move these to api/src/handlers directory

pub(crate) async fn create_os_image(
    api: &Api,
    request: Request<crate::api::rpc::OsImageAttributes>,
) -> Result<Response<crate::api::rpc::OsImage>, Status> {
    let mut txn = api.txn_begin().await?;
    let attrs: OsImageAttributes = OsImageAttributes::try_from(request.into_inner())
        .map_err(|e| CarbideError::InvalidArgument(e.to_string()))?;
    if attrs.source_url.is_empty() || attrs.digest.is_empty() {
        return Err(
            CarbideError::InvalidArgument("os_image url or digest is empty".to_string()).into(),
        );
    }
    let image =
        db::os_image::create(&mut txn, &attrs)
            .await
            .map_err(|e| CarbideError::Internal {
                message: e.to_string(),
            })?;
    txn.commit().await.map_err(|e| CarbideError::Internal {
        message: e.to_string(),
    })?;

    let resp: crate::api::rpc::OsImage =
        rpc::forge::OsImage::try_from(image).map_err(|e| CarbideError::Internal {
            message: e.to_string(),
        })?;
    Ok(Response::new(resp))
}

pub(crate) async fn list_os_image(
    api: &Api,
    request: Request<crate::api::rpc::ListOsImageRequest>,
) -> Result<Response<crate::api::rpc::ListOsImageResponse>, Status> {
    let mut txn = api.txn_begin().await?;
    let tenant: Option<TenantOrganizationId> = match request.into_inner().tenant_organization_id {
        Some(x) => Some(
            TenantOrganizationId::try_from(x)
                .map_err(|e| CarbideError::InvalidArgument(e.to_string()))?,
        ),
        None => None,
    };
    let os_images =
        db::os_image::list(&mut txn, tenant)
            .await
            .map_err(|e| CarbideError::Internal {
                message: e.to_string(),
            })?;
    txn.commit().await.map_err(|e| CarbideError::Internal {
        message: e.to_string(),
    })?;

    let mut images: Vec<crate::api::rpc::OsImage> = Vec::new();
    for os_image in os_images.iter() {
        let image = rpc::forge::OsImage::try_from(os_image.clone()).map_err(|e| {
            CarbideError::Internal {
                message: e.to_string(),
            }
        })?;
        images.push(image);
    }
    let resp = crate::api::rpc::ListOsImageResponse { images };
    Ok(Response::new(resp))
}

pub(crate) async fn get_os_image(
    api: &Api,
    request: Request<rpc::Uuid>,
) -> Result<Response<crate::api::rpc::OsImage>, Status> {
    let mut txn = api.txn_begin().await?;
    let image_id: Uuid = Uuid::try_from(request.into_inner())
        .map_err(|e| CarbideError::InvalidArgument(e.to_string()))?;
    let image =
        db::os_image::get(&mut txn, image_id)
            .await
            .map_err(|e| CarbideError::Internal {
                message: e.to_string(),
            })?;
    txn.commit().await.map_err(|e| CarbideError::Internal {
        message: e.to_string(),
    })?;

    let resp: crate::api::rpc::OsImage =
        rpc::forge::OsImage::try_from(image).map_err(|e| CarbideError::Internal {
            message: e.to_string(),
        })?;
    Ok(Response::new(resp))
}

pub(crate) async fn delete_os_image(
    api: &Api,
    request: Request<crate::api::rpc::DeleteOsImageRequest>,
) -> Result<Response<crate::api::rpc::DeleteOsImageResponse>, Status> {
    let mut txn = api.txn_begin().await?;
    let req = request.into_inner();
    if req.id.is_none() {
        return Err(CarbideError::InvalidArgument("os image id missing".to_string()).into());
    }
    let image_id: Uuid = Uuid::try_from(req.id.unwrap())
        .map_err(|e| CarbideError::InvalidArgument(e.to_string()))?;
    let tenant: TenantOrganizationId = TenantOrganizationId::try_from(req.tenant_organization_id)
        .map_err(|e| CarbideError::InvalidArgument(e.to_string()))?;
    let image =
        db::os_image::get(&mut txn, image_id)
            .await
            .map_err(|e| CarbideError::Internal {
                message: e.to_string(),
            })?;
    if image.attributes.tenant_organization_id != tenant {
        return Err(CarbideError::InvalidArgument("os image tenant mismatch".to_string()).into());
    }
    if image.status == OsImageStatus::InProgress {
        return Err(CarbideError::FailedPrecondition("os image busy".to_string()).into());
    }

    db::os_image::delete(&image, &mut txn)
        .await
        .map_err(|e| CarbideError::Internal {
            message: e.to_string(),
        })?;
    txn.commit().await.map_err(|e| CarbideError::Internal {
        message: e.to_string(),
    })?;

    let resp = crate::api::rpc::DeleteOsImageResponse::default();
    Ok(Response::new(resp))
}

pub(crate) async fn update_os_image(
    api: &Api,
    request: Request<crate::api::rpc::OsImageAttributes>,
) -> Result<Response<crate::api::rpc::OsImage>, Status> {
    let mut txn = api.txn_begin().await?;

    let new_attrs: OsImageAttributes = OsImageAttributes::try_from(request.into_inner())
        .map_err(|e| CarbideError::InvalidArgument(e.to_string()))?;
    let image = db::os_image::get(&mut txn, new_attrs.id)
        .await
        .map_err(|e| CarbideError::Internal {
            message: e.to_string(),
        })?;
    if new_attrs.source_url != image.attributes.source_url
        || new_attrs.digest != image.attributes.digest
        || new_attrs.tenant_organization_id != image.attributes.tenant_organization_id
        || new_attrs.create_volume != image.attributes.create_volume
        || new_attrs.rootfs_id != image.attributes.rootfs_id
        || new_attrs.rootfs_label != image.attributes.rootfs_label
        || new_attrs.capacity != image.attributes.capacity
    {
        return Err(CarbideError::InvalidArgument(
            "os_image update read-only attributes changed".into(),
        )
        .into());
    }
    let updated = db::os_image::update(&image, &mut txn, new_attrs)
        .await
        .map_err(|e| CarbideError::Internal {
            message: e.to_string(),
        })?;

    txn.commit().await.map_err(|e| CarbideError::Internal {
        message: e.to_string(),
    })?;

    let resp: crate::api::rpc::OsImage =
        rpc::forge::OsImage::try_from(updated).map_err(|e| CarbideError::Internal {
            message: e.to_string(),
        })?;
    Ok(Response::new(resp))
}

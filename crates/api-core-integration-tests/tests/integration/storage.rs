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

use carbide_test_harness::prelude::{TestHarness, sqlx_test};
use rpc::forge::forge_server::Forge;
use rpc::forge::{
    // StorageClusterAttributes,
    // StoragePoolAttributes,
    OsImageAttributes,
    OsImageStatus,
};
use tonic::Request;
use uuid::Uuid;

#[sqlx_test]
async fn test_create_and_delete_os_image(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = TestHarness::builder(pool).build().await;

    let image_attrs = OsImageAttributes {
        id: Some(rpc::Uuid {
            value: Uuid::new_v4().to_string(),
        }),
        source_url: "https://example.com/image.qcow2".to_string(),
        digest: "sha256:1234567890".to_string(),
        tenant_organization_id: "test-org".to_string(),
        create_volume: false,
        name: Some("test-image".to_string()),
        description: Some("Test OS Image".to_string()),
        auth_type: None,
        auth_token: None,
        rootfs_id: None,
        rootfs_label: None,
        boot_disk: None,
        capacity: Some(1024 * 1024 * 1024), // 1GB
        bootfs_id: None,
        efifs_id: None,
    };

    let request = Request::new(image_attrs.clone());
    let response = env.api().create_os_image(request).await;
    let image = response.expect("Could not create OS image").into_inner();

    assert!(image.attributes.is_some(), "Image attributes should be set");
    assert_eq!(
        image.status,
        OsImageStatus::ImageReady as i32,
        "Initial status should be Ready"
    );

    let image_id = image.attributes.as_ref().unwrap().id.clone().unwrap();
    let delete_request = rpc::forge::DeleteOsImageRequest {
        id: Some(image_id),
        tenant_organization_id: "test-org".to_string(),
    };

    let request = Request::new(delete_request);
    let response = env.api().delete_os_image(request).await;
    let _deletion_result = response.expect("Could not delete OS image").into_inner();

    Ok(())
}

#[sqlx_test]
async fn test_os_image_status_transitions(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = TestHarness::builder(pool).build().await;

    let image_attrs = OsImageAttributes {
        id: Some(rpc::Uuid {
            value: Uuid::new_v4().to_string(),
        }),
        source_url: "https://example.com/image.qcow2".to_string(),
        digest: "sha256:1234567890".to_string(),
        tenant_organization_id: "test-org".to_string(),
        create_volume: false,
        name: Some("test-image".to_string()),
        description: Some("Test OS Image".to_string()),
        auth_type: None,
        auth_token: None,
        rootfs_id: None,
        rootfs_label: None,
        boot_disk: None,
        capacity: Some(1024 * 1024 * 1024), // 1GB
        bootfs_id: None,
        efifs_id: None,
    };

    let request = Request::new(image_attrs.clone());
    let response = env.api().create_os_image(request).await;
    let image = response.expect("Could not create OS image").into_inner();

    assert_eq!(
        image.status,
        OsImageStatus::ImageReady as i32,
        "Initial status should be Ready"
    );

    // Test status transition to InProgress
    let mut updated_attrs = image_attrs.clone();
    updated_attrs.name = Some("in-progress-image".to_string());

    let request = Request::new(updated_attrs);
    let response = env.api().update_os_image(request).await;
    let updated = response.expect("Could not update OS image").into_inner();

    // The status should not change unless the volume is created
    assert_eq!(
        updated.status,
        OsImageStatus::ImageReady as i32,
        "Status should transition to ImageReady"
    );

    Ok(())
}

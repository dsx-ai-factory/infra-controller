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

use std::net::SocketAddr;

use rpc::forge::{
    CreateTenantKeysetRequest, CreateTenantRequest, Metadata, TenantKeysetContent,
    TenantKeysetIdentifier, TenantPublicKey,
};

use crate::api_client;

pub async fn create(
    carbide_api_addrs: &[SocketAddr],
    organization_id: &str,
    name: &str,
) -> eyre::Result<()> {
    tracing::info!("Creating tenant");

    let request = CreateTenantRequest {
        organization_id: organization_id.to_string(),
        metadata: Some(Metadata {
            name: name.to_string(),
            ..Default::default()
        }),
        routing_profile_type: Some("EXTERNAL".to_string()),
    };
    api_client::call(carbide_api_addrs, "CreateTenant", |mut client| async move {
        client.create_tenant(request).await
    })
    .await?;
    tracing::info!(tenant_name = name, "Tenant created",);
    Ok(())
}

pub mod keyset {
    use uuid::Uuid;

    use super::*;

    pub async fn create(
        carbide_api_addrs: &[SocketAddr],
        organization_id: &str,
        id: Uuid,
        public_keys: &[&str],
    ) -> eyre::Result<()> {
        tracing::info!("Creating tenant keyset");

        let request = CreateTenantKeysetRequest {
            keyset_identifier: Some(TenantKeysetIdentifier {
                organization_id: organization_id.to_string(),
                keyset_id: id.to_string(),
            }),
            keyset_content: Some(TenantKeysetContent {
                public_keys: public_keys
                    .iter()
                    .map(|public_key| TenantPublicKey {
                        public_key: (*public_key).to_string(),
                        comment: None,
                    })
                    .collect(),
            }),
            version: "V1".to_string(),
        };
        api_client::call(
            carbide_api_addrs,
            "CreateTenantKeyset",
            |mut client| async move { client.create_tenant_keyset(request).await },
        )
        .await?;
        tracing::info!(
            keyset_id = %id,
            "Tenant keyset created",
        );
        Ok(())
    }
}

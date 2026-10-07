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

use eyre::ContextCompat;
use rpc::forge::{Metadata, VpcCreationRequest, VpcVirtualizationType};

use crate::api_client;

pub async fn create(carbide_api_addrs: &[SocketAddr], tenant_org_id: &str) -> eyre::Result<String> {
    tracing::info!("Creating VPC");

    // Default VPC type is ETV. ETV rejects `routing_profile_type` at the
    // API gate (it's FNN-only -- see `model::vpc::capability`), so this
    // fixture intentionally omits the field. The FNN-specific fixture
    // (`create_fnn`) sets it.
    let vpc_id = create_with_type(carbide_api_addrs, tenant_org_id, "tenant_vpc", None).await?;
    tracing::info!(
        vpc_id = %vpc_id,
        "VPC created",
    );
    Ok(vpc_id)
}

pub async fn create_fnn(
    carbide_api_addrs: &[SocketAddr],
    tenant_org_id: &str,
) -> eyre::Result<String> {
    tracing::info!("Creating FNN VPC");

    let vpc_id = create_with_type(
        carbide_api_addrs,
        tenant_org_id,
        "tenant_vpc_fnn",
        Some((VpcVirtualizationType::Fnn, "EXTERNAL")),
    )
    .await?;
    tracing::info!(
        vpc_id = %vpc_id,
        "FNN VPC created",
    );
    Ok(vpc_id)
}

pub async fn create_flat(
    carbide_api_addrs: &[SocketAddr],
    tenant_org_id: &str,
) -> eyre::Result<String> {
    tracing::info!("Creating Flat VPC");

    // Flat VPCs reject `routing_profile_type` -- there's no NICo-managed
    // data plane to apply a routing profile to.
    let vpc_id = create_with_type(
        carbide_api_addrs,
        tenant_org_id,
        "tenant_vpc_flat",
        Some((VpcVirtualizationType::Flat, "")),
    )
    .await?;
    tracing::info!(
        vpc_id = %vpc_id,
        "Flat VPC created",
    );
    Ok(vpc_id)
}

async fn create_with_type(
    carbide_api_addrs: &[SocketAddr],
    tenant_org_id: &str,
    name: &str,
    virtualization: Option<(VpcVirtualizationType, &str)>,
) -> eyre::Result<String> {
    let request = VpcCreationRequest {
        tenant_organization_id: tenant_org_id.to_string(),
        network_virtualization_type: virtualization.map(|(kind, _)| kind as i32),
        routing_profile_type: virtualization
            .and_then(|(_, profile)| (!profile.is_empty()).then(|| profile.to_string())),
        metadata: Some(Metadata {
            name: name.to_string(),
            ..Default::default()
        }),
        ..Default::default()
    };
    let vpc = api_client::call(carbide_api_addrs, "CreateVpc", |mut client| async move {
        client.create_vpc(request).await
    })
    .await?;

    Ok(vpc
        .id
        .context("CreateVpc response has no VPC ID")?
        .to_string())
}

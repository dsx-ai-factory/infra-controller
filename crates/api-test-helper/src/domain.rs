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
use rpc::dns::CreateDomainRequest;

use crate::api_client;

pub async fn create(carbide_api_addrs: &[SocketAddr], name: &str) -> eyre::Result<String> {
    tracing::info!("Creating domain");

    let request = CreateDomainRequest {
        name: name.to_string(),
    };
    let domain = api_client::call(carbide_api_addrs, "CreateDomain", |mut client| async move {
        client.create_domain(request).await
    })
    .await?;
    let domain_id = domain
        .id
        .context("CreateDomain response has no domain ID")?;
    tracing::info!(
        domain_id = %domain_id,
        "Domain created",
    );
    Ok(domain_id.to_string())
}

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
use rpc::forge::{Metadata, VpcPrefixConfig, VpcPrefixCreationRequest};

use crate::api_client;

pub async fn create(
    carbide_api_addrs: &[SocketAddr],
    vpc_id: &str,
    prefix: &str,
    name: &str,
) -> eyre::Result<String> {
    tracing::info!(prefix, vpc_prefix_name = name, "Creating VPC prefix",);

    let request = VpcPrefixCreationRequest {
        vpc_id: Some(vpc_id.parse()?),
        config: Some(VpcPrefixConfig {
            prefix: prefix.to_string(),
        }),
        metadata: Some(Metadata {
            name: name.to_string(),
            description: format!("VPC prefix for {prefix}"),
            ..Default::default()
        }),
        ..Default::default()
    };
    let vpc_prefix = api_client::call(
        carbide_api_addrs,
        "CreateVpcPrefix",
        |mut client| async move { client.create_vpc_prefix(request).await },
    )
    .await?;
    let prefix_id = vpc_prefix
        .id
        .context("CreateVpcPrefix response has no VPC prefix ID")?;
    tracing::info!(
        vpc_prefix_id = %prefix_id,
        "VPC prefix created",
    );
    Ok(prefix_id.to_string())
}

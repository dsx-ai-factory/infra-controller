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
use std::net::IpAddr;

use ::rpc::forge as rpc;
use ::rpc::forge_tls_client::{self, ApiConfig, ForgeClientConfig};

pub(crate) mod cloud_init;
pub(crate) mod ipxe;
pub(crate) mod metrics;
pub(crate) mod tls;

struct RpcContext;

impl RpcContext {
    async fn get_pxe_instructions(
        arch: rpc::MachineArchitecture,
        client_ip: IpAddr,
        product: Option<String>,
        url: &str,
        client_config: &ForgeClientConfig,
    ) -> Result<rpc::PxeInstructions, String> {
        let api_config = ApiConfig::new(url, client_config);
        let mut client = forge_tls_client::ForgeTlsClient::retry_build(&api_config)
            .await
            .map_err(|err| err.to_string())?;
        let request = tonic::Request::new(rpc::PxeInstructionRequest {
            arch: arch as i32,
            product,
            client_ip: Some(client_ip.to_string()),
            // `interface_id` is deprecated; let Default fill it so we
            // don't have to reference the deprecated field by name.
            ..Default::default()
        });
        client
            .get_pxe_instructions(request)
            .await
            .map(|response| response.into_inner())
            .map_err(|error| {
                format!(
                    "Error fetching PXE instructions for client_ip {client_ip}; Error: {error}."
                )
            })
    }
}

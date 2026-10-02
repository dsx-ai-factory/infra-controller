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

use ::rpc::forge_tls_client::{self, ApiConfig, ForgeClientConfig};
use forge_tls::client_config::ClientCert;
use scout::{CarbideClientError, CarbideClientResult};

use crate::Options;

/// The client TLS config every scout connection to the API uses. Callers that
/// hold a long-lived client of their own build it from here so none of them
/// silently drops the node-auth JWT.
pub(super) fn forge_client_config(config: &Options) -> ForgeClientConfig {
    ForgeClientConfig::new(
        config.root_ca.clone(),
        Some(ClientCert {
            cert_path: config.client_cert.clone(),
            key_path: config.client_key.clone(),
        }),
    )
    // Node-auth (#355): also present a self-signed bearer JWT minted from the
    // client cert's key. Ignored by the API unless [node_auth] is enabled.
    .with_node_jwt()
}

pub(super) async fn create_forge_client(
    config: &Options,
) -> CarbideClientResult<forge_tls_client::ForgeClientT> {
    let client_config = forge_client_config(config);
    let api_config = ApiConfig::new(&config.api, &client_config);

    let client = forge_tls_client::ForgeTlsClient::retry_build(&api_config)
        .await
        .map_err(|err| CarbideClientError::TransportError(err.to_string()))?;
    Ok(client)
}

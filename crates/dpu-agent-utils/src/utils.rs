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
use rpc::forge_tls_client::{ApiConfig, ForgeClientConfig, ForgeClientT, ForgeTlsClient};

// Forge Communication
pub async fn create_forge_client(
    forge_api: &str,
    client_config: &ForgeClientConfig,
) -> Result<ForgeClientT, eyre::Error> {
    match ForgeTlsClient::retry_build(&ApiConfig::new(forge_api, client_config)).await {
        Ok(client) => Ok(client),
        Err(err) => Err(eyre::eyre!(
            "could not connect to forge API server at {}: {err}",
            forge_api
        )),
    }
}

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
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use forge_tls::client_config::ClientCert;
use rpc::carbide_tls_client;
use rpc::carbide_tls_client::{ApiConfig, CarbideClientConfig};
use rpc::forge::CloudInitInstructionsRequest;

use crate::common::{AppState, Machine};
use crate::rpc_error::PxeRequestError;

impl FromRequestParts<AppState> for Machine {
    type Rejection = PxeRequestError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let client_config = CarbideClientConfig::new(
            state.runtime_config.forge_root_ca_path.clone(),
            Some(ClientCert {
                cert_path: state.runtime_config.server_cert_path.clone(),
                key_path: state.runtime_config.server_key_path.clone(),
            }),
        );
        let api_config = ApiConfig::new(&state.runtime_config.internal_api_url, &client_config);

        let mut client = carbide_tls_client::CarbideTlsClient::retry_build(&api_config)
            .await
            .map_err(|err| {
                tracing::error!(
                    error = ?err,
                    url = ?state.runtime_config.internal_api_url,
                    "error connecting to forge api from pxe"
                );
                PxeRequestError::MissingClientConfig
            })?;

        // Note: This does *NOT* look at X-Forwarded-For, due to security issues with the header. We
        // don't currently have use cases for a proxy in front of carbide-pxe... if that changes
        // someday we will need to configure a request extractor that conditionally uses
        // X-Forwarded-For if it's present and falling back on ClientIp if it's not.
        let client_ip = super::client_ip(parts, state).await?;

        client
            .get_cloud_init_instructions(tonic::Request::new(CloudInitInstructionsRequest {
                ip: client_ip.to_string(),
            }))
            .await
            .map(|response| {
                let instructions = response.into_inner();
                if let Some(id) = instructions.metadata.as_ref().and_then(|metadata| {
                    metadata
                        .instance_id
                        .parse::<carbide_uuid::machine::MachineId>()
                        .ok()
                }) {
                    let id_string = id.to_string();
                    let predicted = id.machine_type().is_predicted_host();
                    trace_propagation::MachineTraceAttributes {
                        machine_id: (!predicted).then_some(id_string.as_str()),
                        predicted_id: predicted.then_some(id_string.as_str()),
                        managed_host_id: (!id.machine_type().is_dpu())
                            .then_some(id_string.as_str()),
                        ..Default::default()
                    }
                    .record_on(&tracing::Span::current());
                }
                Machine { instructions }
            })
            .map_err(PxeRequestError::CarbideApiError)
    }
}

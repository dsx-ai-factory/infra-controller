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

use async_trait::async_trait;
use axum::http::{HeaderMap, Uri};
use forge_dpu_fmds_shared::machine_identity::{
    MetaDataIdentityOutcome, MetaDataIdentitySigner, forward_sign_proxy_if_ready,
    sign_machine_identity_with_forge, wait_identity_rate_limit_permit,
};

use crate::state::FmdsState;

#[async_trait]
impl MetaDataIdentitySigner for FmdsState {
    async fn wait_identity_permit(&self) -> Result<(), tonic::Status> {
        let snap = self.machine_identity.load();
        wait_identity_rate_limit_permit(&snap.governor, snap.wait_timeout)
            .await
            .map_err(|_| {
                tonic::Status::resource_exhausted(
                    "timed out waiting for machine-identity rate limit capacity (machine-identity.wait-timeout-secs)",
                )
            })
    }

    async fn machine_identity_response(
        &self,
        uri: &Uri,
        headers: &HeaderMap,
        audiences: Vec<String>,
    ) -> Result<MetaDataIdentityOutcome, tonic::Status> {
        let serving = self.machine_identity.load_full();
        if let Some(resp) = forward_sign_proxy_if_ready(
            serving.sign_proxy_base.as_deref(),
            serving.sign_proxy_http_client.as_ref(),
            uri,
            headers,
        )
        .await
        {
            return Ok(MetaDataIdentityOutcome::HttpProxy(resp));
        }

        let forge_client_config = self.forge_client_config.as_ref().ok_or_else(|| {
            tonic::Status::failed_precondition(
                "forge client TLS is not configured; cannot sign machine identity",
            )
        })?;
        let snap = self.machine_identity.load();
        let resp = sign_machine_identity_with_forge(
            &self.forge_api,
            forge_client_config.as_ref(),
            snap.forge_call_timeout,
            audiences,
        )
        .await?;
        Ok(MetaDataIdentityOutcome::Forge(resp))
    }
}

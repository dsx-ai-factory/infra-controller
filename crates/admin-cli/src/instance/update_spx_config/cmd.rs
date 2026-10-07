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

use super::args::Args;
use crate::cfg::runtime::RuntimeContext;
use crate::errors::CarbideCliResult;
use crate::rpc::ApiClient;

pub(super) async fn update_spx_config(
    api_client: &ApiClient,
    update_request: Args,
    ctx: &RuntimeContext,
) -> CarbideCliResult<()> {
    let unsafe_op_msg = ctx.assert_cloud_unsafe_op_message()?;

    match api_client
        .update_instance_config_with(
            update_request.instance,
            |config| {
                config.spxconfig = Some(update_request.config);
            },
            |_metadata| {},
            Some(unsafe_op_msg.to_string()),
        )
        .await
    {
        Ok(i) => {
            tracing::info!(
                "update-spx-config was successful. Updated instance: {:?}",
                i
            );
        }
        Err(e) => {
            tracing::info!("update-spx-config failed with {} ", e);
        }
    };
    Ok(())
}

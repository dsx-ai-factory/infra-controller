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

use forge_ssh::ssh::copy_bfb_to_bmc_rshim;

use super::args::Args;
use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::rpc::ApiClient;

pub(super) async fn copy_bfb(api_client: &ApiClient, args: Args) -> CarbideCliResult<()> {
    let bmc_ip = args.ssh_args.credentials.bmc_ip_address.ip().to_string();
    let is_bf2 = match api_client.get_explored_endpoints_by_ids(&[bmc_ip]).await {
        Ok(list) => list
            .endpoints
            .first()
            .and_then(|ep| ep.report.as_ref())
            .map(|report| {
                report.systems.first().is_some_and(|s| s.id == "Bluefield")
                    && report.chassis.iter().any(|c| {
                        c.model
                            .as_deref()
                            .unwrap_or_default()
                            .to_lowercase()
                            .contains("bluefield 2")
                    })
            })
            .unwrap_or(false),
        Err(e) => {
            tracing::warn!("Could not query exploration report, defaulting to non-BF2: {e}");
            false
        }
    };

    if is_bf2 {
        return Err(CarbideCliError::GenericError(
            "BlueField-2 DPUs are no longer supported for BFB copy".to_string(),
        ));
    }

    copy_bfb_to_bmc_rshim(
        args.ssh_args.credentials.bmc_ip_address,
        args.ssh_args.credentials.bmc_username,
        args.ssh_args.credentials.bmc_password,
        args.bfb_path,
    )
    .await
    .map_err(|e| CarbideCliError::GenericError(e.to_string()))?;
    Ok(())
}

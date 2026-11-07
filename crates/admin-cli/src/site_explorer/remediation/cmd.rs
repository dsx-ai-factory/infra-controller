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

use ::rpc::forge::PauseExploredEndpointRemediationRequest;

use super::args::Args;
use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::rpc::ApiClient;

pub(super) async fn remediation(api_client: &ApiClient, opts: Args) -> CarbideCliResult<()> {
    if opts.pause {
        api_client
            .0
            .pause_explored_endpoint_remediation(PauseExploredEndpointRemediationRequest {
                ip_address: opts.address.clone(),
                pause: true,
            })
            .await?;
        println!("Remediation paused for endpoint {}", opts.address);
    } else if opts.resume {
        api_client
            .0
            .pause_explored_endpoint_remediation(PauseExploredEndpointRemediationRequest {
                ip_address: opts.address.clone(),
                pause: false,
            })
            .await?;
        println!("Remediation resumed for endpoint {}", opts.address);
    } else {
        return Err(CarbideCliError::RequireOneError("--pause", "--resume"));
    }
    Ok(())
}

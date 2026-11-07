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

use carbide_uuid::machine::HostMachineId;
use rpc::admin_cli::OutputFormat;

use crate::dpf::common::DpfQuery;
use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::rpc::ApiClient;

pub(in crate::dpf) async fn modify_dpf_state(
    query: &DpfQuery,
    _format: OutputFormat, // TODO: Implement json output handling.
    api_client: &ApiClient,
    enabled: bool,
) -> CarbideCliResult<()> {
    let host: HostMachineId = query.try_into()?;

    // Prevent disabling DPF if it was used for ingestion.
    if !enabled {
        let dpf_states = api_client.get_dpf_state(vec![host], 1).await?;
        if dpf_states.iter().any(|s| s.used_for_ingestion) {
            return Err(CarbideCliError::GenericError(
                "Cannot disable DPF: machine was ingested via DPF. \
                 Disabling would leave DPF CRDs in an inconsistent state."
                    .to_string(),
            ));
        }
    }

    api_client.modify_dpf_state(host, enabled).await?;
    println!("DPF state modified for machine {host} with state {enabled} successfully!!");
    Ok(())
}

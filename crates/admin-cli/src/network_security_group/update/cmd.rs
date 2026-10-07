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

use ::rpc::admin_cli::OutputFormat;

use super::args::Args;
use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::network_security_group::common::convert_nsgs_to_table;
use crate::rpc::ApiClient;

/// Update a network security group.
/// On successful update, the details of the
/// group will be displayed.
pub(super) async fn update(
    args: Args,
    output_format: OutputFormat,
    api_client: &ApiClient,
) -> CarbideCliResult<()> {
    let is_json = output_format == OutputFormat::Json;

    let id = args.id;

    let nsg = api_client
        .get_single_network_security_group(id.clone())
        .await?;

    let mut metadata = nsg.metadata.unwrap_or_default();
    let (mut rules, mut stateful_egress) = {
        let nsg = nsg.attributes.unwrap_or_default();
        (nsg.rules, nsg.stateful_egress)
    };

    if let Some(d) = args.description {
        metadata.description = d;
    }

    if let Some(n) = args.name {
        metadata.name = n;
    }

    if let Some(l) = args.labels {
        metadata.labels = serde_json::from_str(&l)?;
    }

    if let Some(r) = args.rules {
        rules = serde_json::from_str(&r)?;
    }

    if let Some(s) = args.stateful_egress {
        stateful_egress = s;
    }

    let nsg = api_client
        .update_network_security_group(
            id,
            args.tenant_organization_id,
            metadata,
            args.version,
            stateful_egress,
            rules,
        )
        .await?;

    if is_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&nsg).map_err(CarbideCliError::JsonError)?
        );
    } else {
        convert_nsgs_to_table(&[nsg], true)?.printstd();
    }

    Ok(())
}

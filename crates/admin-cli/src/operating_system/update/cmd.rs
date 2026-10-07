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

use ::rpc::forge::{IpxeTemplateParameters, UpdateOperatingSystemRequest};

use super::args::Args;
use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::operating_system::common::{str_to_ipxe_template_id, str_to_os_id};
use crate::rpc::ApiClient;

pub(super) async fn update(opts: Args, api_client: &ApiClient) -> CarbideCliResult<()> {
    let id = str_to_os_id(&opts.id)?;
    let ipxe_template_id = opts
        .ipxe_template_id
        .as_deref()
        .map(str_to_ipxe_template_id)
        .transpose()?;

    let ipxe_template_parameters = opts.params.map(|items| IpxeTemplateParameters { items });

    let os = api_client
        .0
        .update_operating_system(UpdateOperatingSystemRequest {
            id: Some(id),
            name: opts.name,
            description: opts.description,
            is_active: opts.is_active,
            allow_override: opts.allow_override,
            phone_home_enabled: opts.phone_home_enabled,
            user_data: opts.user_data,
            ipxe_script: opts.ipxe_script,
            ipxe_template_id,
            ipxe_template_parameters,
            ipxe_template_artifacts: None,
            ipxe_template_definition_hash: None,
        })
        .await
        .map_err(CarbideCliError::from)?;

    println!(
        "Operating system updated: {} (id={})",
        os.name,
        os.id.map(|u| u.to_string()).as_deref().unwrap_or("")
    );
    Ok(())
}

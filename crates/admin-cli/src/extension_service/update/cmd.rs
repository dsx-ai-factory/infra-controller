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

use ::rpc::admin_cli::output::OutputFormat;
use eyre::WrapErr;
use rpc::forge::{DpuExtensionServiceType, ServiceVpcInterfaceRequirements};

use super::super::show::cmd::convert_extension_services_to_table;
use super::args::Args;
use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::rpc::ApiClient;

pub(super) async fn handle_update(
    args: Args,
    output_format: OutputFormat,
    api_client: &ApiClient,
) -> CarbideCliResult<()> {
    let is_json = output_format == OutputFormat::Json;

    let preserve_service_vpc = args.service_vpc_family.is_none() && !args.clear_service_vpc;
    let mut req = args
        .into_request()
        .wrap_err("construct the extension service update request")?;

    if preserve_service_vpc {
        let current = api_client
            .get_extension_service_by_id(req.service_id.clone())
            .await
            .wrap_err("read the current extension service registration")?;
        // Helm deletion retires V1 in the same transaction that marks the registration deleted.
        if current.service_type == DpuExtensionServiceType::DpfHelmChart as i32
            && current.active_versions.is_empty()
        {
            return Err(CarbideCliError::GenericError(format!(
                "extension service not found: {}",
                req.service_id
            )));
        }
        if let Some(expected) = req.if_version_ctr_match
            && expected != current.version_ctr
        {
            return Err(CarbideCliError::GenericError(format!(
                "extension service version counter is {}, expected {expected}",
                current.version_ctr
            )));
        }
        // Bind the copied requirements to the revision that supplied them.
        req.service_vpc_interfaces = Some(ServiceVpcInterfaceRequirements {
            interfaces: current.service_vpc_interfaces,
        });
        req.if_version_ctr_match = Some(current.version_ctr);
    }
    let extension_service = api_client
        .0
        .update_dpu_extension_service(req)
        .await
        .wrap_err("update the extension service registration")?;

    if is_json {
        println!("{}", serde_json::to_string_pretty(&extension_service)?);
    } else {
        convert_extension_services_to_table(&[extension_service]).printstd();
    }

    Ok(())
}

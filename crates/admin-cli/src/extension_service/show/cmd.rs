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
use ::rpc::forge::{DpuExtensionService, DpuExtensionServiceType};
use prettytable::{Table, row};

use super::args::Args;
use crate::errors::CarbideCliResult;
use crate::rpc::ApiClient;

pub(super) async fn handle_show(
    args: Args,
    output_format: OutputFormat,
    api_client: &ApiClient,
    page_size: usize,
) -> CarbideCliResult<()> {
    let is_json = output_format == OutputFormat::Json;

    let services = if let Some(id) = args.id {
        let service = api_client.get_extension_service_by_id(id).await?;
        vec![service]
    } else {
        let service_list = api_client
            .find_extension_services(
                args.service_type.map(|t| t as i32),
                args.service_name,
                args.tenant_organization_id,
                page_size,
            )
            .await?;
        service_list.services
    };

    if is_json {
        println!("{}", serde_json::to_string_pretty(&services)?);
    } else {
        convert_extension_services_to_table(&services).printstd();
    }

    Ok(())
}

pub(in crate::extension_service) fn convert_extension_services_to_table(
    services: &[DpuExtensionService],
) -> Box<Table> {
    let mut table = Table::new();

    table.set_titles(row![
        "Service ID",
        "Name",
        "Type",
        "DPU Target",
        "Tenant Organization ID",
        "Version Counter",
        "Active Versions",
        "Description",
        "Lifecycle State",
        "Created",
        "Updated",
    ]);

    for service in services {
        let service_type_name = DpuExtensionServiceType::try_from(service.service_type)
            .map(|t| t.as_str_name())
            .unwrap_or("Unknown");

        let active_versions = service.active_versions.join(", ");
        let lifecycle_state = service
            .lifecycle_status
            .as_ref()
            .and_then(|status| {
                serde_json::from_str::<serde_json::Value>(&status.state)
                    .ok()
                    .and_then(|state| state.get("state")?.as_str().map(str::to_string))
                    .or_else(|| (!status.state.is_empty()).then(|| status.state.clone()))
            })
            .unwrap_or_else(|| "Unknown".to_string());

        table.add_row(row![
            service.service_id,
            service.service_name,
            service_type_name,
            match service
                .dpu_target
                .and_then(|value| ::rpc::forge::DpuExtensionServiceDpuTarget::try_from(value).ok())
            {
                Some(::rpc::forge::DpuExtensionServiceDpuTarget::Primary) => "primary",
                Some(::rpc::forge::DpuExtensionServiceDpuTarget::AllActive) => "all-active",
                Some(::rpc::forge::DpuExtensionServiceDpuTarget::All) => "all",
                _ => "",
            },
            service.tenant_organization_id,
            service.version_ctr,
            active_versions,
            service.description,
            lifecycle_state,
            service.created,
            service.updated,
        ]);
    }

    table.into()
}

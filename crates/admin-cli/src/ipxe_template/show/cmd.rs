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
use prettytable::{Cell, Row, Table};

use super::args::Args;
use crate::errors::CarbideCliError;
use crate::rpc::ApiClient;

fn visibility_display(visibility: i32) -> &'static str {
    match rpc::forge::IpxeTemplateVisibility::try_from(visibility) {
        Ok(rpc::forge::IpxeTemplateVisibility::Internal) => "internal",
        Ok(rpc::forge::IpxeTemplateVisibility::Public) => "public",
        _ => "unknown",
    }
}

pub(super) async fn handle_show(
    opts: Args,
    format: OutputFormat,
    api_client: &ApiClient,
) -> Result<(), CarbideCliError> {
    if let Some(id) = opts.id.as_deref()
        && !id.is_empty()
    {
        show_one(id, format, api_client).await
    } else {
        list_all(format, api_client).await
    }
}

async fn list_all(format: OutputFormat, api_client: &ApiClient) -> Result<(), CarbideCliError> {
    let result = api_client.0.list_ipxe_templates().await?;

    if format == OutputFormat::Json {
        println!("{}", serde_json::to_string_pretty(&result.templates)?);
    } else if result.templates.is_empty() {
        println!("No iPXE templates found.");
    } else {
        let mut table = Table::new();
        table.set_titles(Row::new(vec![
            Cell::new("ID"),
            Cell::new("Name"),
            Cell::new("Description"),
            Cell::new("Visibility"),
            Cell::new("Required Params"),
            Cell::new("Required Artifacts"),
        ]));

        for tmpl in &result.templates {
            let id_str = tmpl
                .id
                .as_ref()
                .map(|id| id.to_string())
                .unwrap_or_default();
            table.add_row(Row::new(vec![
                Cell::new(&id_str),
                Cell::new(&tmpl.name),
                Cell::new(&tmpl.description),
                Cell::new(visibility_display(tmpl.visibility)),
                Cell::new(&tmpl.required_params.join(", ")),
                Cell::new(&tmpl.required_artifacts.join(", ")),
            ]));
        }

        table.printstd();
    }

    Ok(())
}

async fn show_one(
    id_str: &str,
    format: OutputFormat,
    api_client: &ApiClient,
) -> Result<(), CarbideCliError> {
    let id: carbide_uuid::ipxe_template::IpxeTemplateId = id_str
        .parse()
        .map_err(|_| CarbideCliError::GenericError(format!("invalid template ID: {}", id_str)))?;

    let result = match api_client
        .0
        .get_ipxe_template(rpc::forge::GetIpxeTemplateRequest { id: Some(id) })
        .await
    {
        Ok(tmpl) => tmpl,
        Err(status) if status.code() == tonic::Code::NotFound => {
            return Err(CarbideCliError::GenericError(format!(
                "iPXE template not found: {}",
                id_str
            )));
        }
        Err(err) => return Err(CarbideCliError::from(err)),
    };

    if format == OutputFormat::Json {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        let id_str = result
            .id
            .as_ref()
            .map(|id| id.to_string())
            .unwrap_or_default();
        println!("ID:          {}", id_str);
        println!("Name:        {}", result.name);
        println!("Description: {}", result.description);
        println!("Visibility:  {}", visibility_display(result.visibility));

        if !result.required_params.is_empty() {
            println!("Required params:    {}", result.required_params.join(", "));
        }
        if !result.reserved_params.is_empty() {
            println!("Reserved params:    {}", result.reserved_params.join(", "));
        }
        if !result.required_artifacts.is_empty() {
            println!(
                "Required artifacts: {}",
                result.required_artifacts.join(", ")
            );
        }

        println!("\nTemplate:\n---\n{}\n---", result.template);
    }

    Ok(())
}

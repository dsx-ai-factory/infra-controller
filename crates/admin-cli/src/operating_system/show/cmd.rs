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
use ::rpc::forge::{
    IpxeTemplateArtifactCacheStrategy, OperatingSystemSearchFilter, OperatingSystemType,
};
use prettytable::{Cell, Row, Table};

use super::args::Args;
use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::operating_system::common::{SerializableOs, str_to_os_id};
use crate::rpc::ApiClient;

pub(super) async fn handle_show(
    opts: Args,
    format: OutputFormat,
    api_client: &ApiClient,
) -> CarbideCliResult<()> {
    if let Some(id) = opts.id.as_ref()
        && !id.is_empty()
    {
        show_one(id, format, api_client).await
    } else {
        list_all(opts, format, api_client).await
    }
}

async fn list_all(
    opts: Args,
    format: OutputFormat,
    api_client: &ApiClient,
) -> CarbideCliResult<()> {
    let id_list = api_client
        .0
        .find_operating_system_ids(OperatingSystemSearchFilter {
            tenant_organization_id: opts.org,
        })
        .await?;

    let operating_systems = if id_list.ids.is_empty() {
        vec![]
    } else {
        api_client
            .0
            .find_operating_systems_by_ids(::rpc::forge::OperatingSystemsByIdsRequest {
                ids: id_list.ids,
            })
            .await?
            .operating_systems
    };

    if format == OutputFormat::Json {
        let serializable: Vec<SerializableOs> = operating_systems
            .into_iter()
            .map(SerializableOs::from)
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&serializable).map_err(CarbideCliError::JsonError)?
        );
        return Ok(());
    }

    if operating_systems.is_empty() {
        println!("No operating system definitions found.");
        return Ok(());
    }

    let mut table = Table::new();
    table.set_titles(Row::new(vec![
        Cell::new("ID"),
        Cell::new("Name"),
        Cell::new("Org"),
        Cell::new("Type"),
        Cell::new("Template"),
        Cell::new("Parameters"),
        Cell::new("Artifacts"),
        Cell::new("Active"),
    ]));

    for os in &operating_systems {
        let params_str = if os.ipxe_template_parameters.is_empty() {
            "-".to_string()
        } else {
            os.ipxe_template_parameters
                .iter()
                .map(|p| format!("{}={}", p.name, p.value))
                .collect::<Vec<_>>()
                .join("\n")
        };
        let artifacts_str = if os.ipxe_template_artifacts.is_empty() {
            "-".to_string()
        } else {
            os.ipxe_template_artifacts
                .iter()
                .map(|a| format!("{}: {}", a.name, a.url))
                .collect::<Vec<_>>()
                .join("\n")
        };
        let id_str = os.id.map(|u| u.to_string()).unwrap_or_default();
        table.add_row(Row::new(vec![
            Cell::new(&id_str),
            Cell::new(&os.name),
            Cell::new(os.tenant_organization_id.as_deref().unwrap_or_default()),
            Cell::new(
                OperatingSystemType::try_from(os.r#type)
                    .map(|t| t.as_str_name())
                    .unwrap_or("UNKNOWN"),
            ),
            Cell::new(
                &os.ipxe_template_id
                    .map(|id| id.to_string())
                    .unwrap_or("-".to_string()),
            ),
            Cell::new(&params_str),
            Cell::new(&artifacts_str),
            Cell::new(if os.is_active { "yes" } else { "no" }),
        ]));
    }

    table.printstd();
    Ok(())
}

async fn show_one(
    id_str: &str,
    format: OutputFormat,
    api_client: &ApiClient,
) -> CarbideCliResult<()> {
    let id = str_to_os_id(id_str)?;

    let os = match api_client.0.get_operating_system(id).await {
        Ok(os) => os,
        Err(status) if status.code() == tonic::Code::NotFound => {
            return Err(CarbideCliError::GenericError(format!(
                "Operating system not found: {}",
                id_str
            )));
        }
        Err(err) => return Err(CarbideCliError::from(err)),
    };

    if format == OutputFormat::Json {
        let serializable: SerializableOs = os.into();
        println!(
            "{}",
            serde_json::to_string_pretty(&serializable).map_err(CarbideCliError::JsonError)?
        );
        return Ok(());
    }

    println!(
        "ID:                  {}",
        os.id.map(|u| u.to_string()).as_deref().unwrap_or("")
    );
    println!("Name:                {}", os.name);
    println!(
        "Org:                 {}",
        os.tenant_organization_id.as_deref().unwrap_or_default()
    );
    println!(
        "Type:                {}",
        OperatingSystemType::try_from(os.r#type)
            .map(|t| t.as_str_name().to_string())
            .unwrap_or_else(|_| os.r#type.to_string())
    );
    println!("Status:              {}", os.status);
    println!("Active:              {}", os.is_active);
    println!("Allow Override:      {}", os.allow_override);
    println!("Phone Home Enabled:  {}", os.phone_home_enabled);
    println!("Created:             {}", os.created);
    println!("Updated:             {}", os.updated);

    if let Some(desc) = &os.description {
        println!("Description:         {desc}");
    }
    if let Some(user_data) = &os.user_data {
        println!("User Data:           {user_data}");
    }

    if let Some(script) = &os.ipxe_script {
        println!("\niPXE Script:\n---\n{script}\n---");
    }
    if let Some(tmpl_id) = &os.ipxe_template_id {
        println!("iPXE Template ID:    {tmpl_id}");
    }

    if !os.ipxe_template_parameters.is_empty() {
        println!("\niPXE Parameters:");
        for p in &os.ipxe_template_parameters {
            println!("  {} = {}", p.name, p.value);
        }
    }
    if !os.ipxe_template_artifacts.is_empty() {
        println!("\niPXE Artifacts:");
        for a in &os.ipxe_template_artifacts {
            let cache = match IpxeTemplateArtifactCacheStrategy::try_from(a.cache_strategy) {
                Ok(IpxeTemplateArtifactCacheStrategy::CacheAsNeeded) => "cache_as_needed",
                Ok(IpxeTemplateArtifactCacheStrategy::LocalOnly) => "local_only",
                Ok(IpxeTemplateArtifactCacheStrategy::CachedOnly) => "cached_only",
                Ok(IpxeTemplateArtifactCacheStrategy::RemoteOnly) => "remote_only",
                _ => "cache_as_needed",
            };
            println!("  {}:", a.name);
            println!("    URL:            {}", a.url);
            if let Some(sha) = &a.sha {
                println!("    SHA:            {sha}");
            }
            if let Some(auth_type) = &a.auth_type {
                println!("    Auth Type:      {auth_type}");
            }
            println!("    Cache Strategy: {cache}");
            if let Some(cached_url) = &a.cached_url {
                println!("    Cached URL:      {cached_url}");
            }
        }
    }

    Ok(())
}

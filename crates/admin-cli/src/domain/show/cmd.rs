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

use std::fmt::Write;

use ::rpc::Timestamp;
use ::rpc::admin_cli::OutputFormat;
use carbide_uuid::domain::DomainId;
use prettytable::{Table, row};
use tracing::warn;

use super::args::Args;
use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::rpc::ApiClient;

// timestamp_or_default returns a String representation of
// the given timestamp Option, or, if the Option is None,
// returns a String representation of the provided "default".
//
// A default is provided with the idea that multiple callers
// may want to use a default timestamp as part of a similar
// operation, and it would be strange for multiple sequential
// calls to get different timestamps.
//
// TODO(chet): Consider making default an &Option<Timestamp>,
// and if None, generate a default timestamp when called.
fn timestamp_or_default(ts: &Option<Timestamp>, default: &Timestamp) -> String {
    ts.as_ref().unwrap_or(default).to_string()
}

pub(in crate::domain) fn convert_domain_to_nice_format(
    domain: &::rpc::protos::dns::Domain,
) -> CarbideCliResult<String> {
    let width = 11;
    let mut lines = String::new();

    let timestamp_default = &Timestamp::default();

    let domain_id = domain.id.unwrap_or_default().to_string();
    let domain_created = timestamp_or_default(&domain.created, timestamp_default);
    let domain_updated = timestamp_or_default(&domain.updated, timestamp_default);
    let domain_deleted = timestamp_or_default(&domain.deleted, timestamp_default);
    let default_ttl = domain
        .default_ttl
        .map_or_else(|| "site default".to_string(), |secs| format!("{secs}s"));

    let data = vec![
        ("ID", domain_id.as_str()),
        ("NAME", domain.name.as_str()),
        ("DEFAULT TTL", default_ttl.as_str()),
        ("CREATED", domain_created.as_str()),
        ("UPDATED", domain_updated.as_str()),
        ("DELETED", domain_deleted.as_str()),
    ];
    for (key, value) in data {
        writeln!(&mut lines, "{key:<width$}: {value}")?;
    }

    Ok(lines)
}

fn convert_domain_to_nice_table(domains: ::rpc::protos::dns::DomainList) -> Box<Table> {
    let mut table = Table::new();

    table.set_titles(row!["Id", "Name", "Created",]);

    for domain in domains.domains {
        table.add_row(row![
            domain.id.unwrap_or_default(),
            domain.name,
            domain.created.unwrap_or_default(),
        ]);
    }

    table.into()
}

async fn show_all_domains(json: bool, api_client: &ApiClient) -> CarbideCliResult<()> {
    let domains = api_client.get_domains(None).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&domains)?);
    } else {
        convert_domain_to_nice_table(domains).printstd();
    }
    Ok(())
}

async fn show_domain_information(
    id: DomainId,
    json: bool,
    api_client: &ApiClient,
) -> CarbideCliResult<()> {
    let domains = api_client.get_domains(Some(id)).await?;
    if domains.domains.is_empty() {
        return Err(CarbideCliError::DomainNotFound);
    }
    let domain = &domains.domains[0];

    if json {
        println!("{}", serde_json::to_string_pretty(&domain)?);
    } else {
        println!(
            "{}",
            convert_domain_to_nice_format(domain).unwrap_or_else(|x| x.to_string())
        );
    }
    Ok(())
}

pub(crate) async fn handle_show(
    args: &Args,
    output_format: OutputFormat,
    api_client: &ApiClient,
) -> CarbideCliResult<()> {
    let is_json = output_format == OutputFormat::Json;
    if let (false, Some(domain_id)) = (args.all, args.domain) {
        show_domain_information(domain_id, is_json, api_client).await?;
    } else {
        // TODO(chet): Remove this ~March 2024.
        // Use tracing::warn for this so its both a little more
        // noticeable, and a little more annoying/naggy. If people
        // complain, it means its working.
        if args.all && output_format == OutputFormat::AsciiTable {
            warn!("redundant `--all` with basic `show` is deprecated. just do `domain show`.")
        }
        show_all_domains(is_json, api_client).await?;
    }
    Ok(())
}

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

use ::rpc::admin_cli::OutputFormat;
use ::rpc::forge as forgerpc;
use prettytable::{Table, row};

use super::args::Args;
use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::rpc::ApiClient;

pub(super) async fn show(
    args: Args,
    output_format: OutputFormat,
    api_client: &ApiClient,
    page_size: usize,
) -> CarbideCliResult<()> {
    let is_json = output_format == OutputFormat::Json;
    let identifier: Option<forgerpc::TenantKeysetIdentifier> = (&args).try_into()?;

    if let Some(identifier) = identifier {
        show_keyset_details(identifier, is_json, api_client).await?;
    } else {
        show_keysets(is_json, api_client, page_size, args.tenant_org_id).await?;
    }

    Ok(())
}

async fn show_keysets(
    json: bool,
    api_client: &ApiClient,
    page_size: usize,
    tenant_org_id: Option<String>,
) -> CarbideCliResult<()> {
    let all_keysets = match api_client.get_all_keysets(tenant_org_id, page_size).await {
        Ok(all_vpc_ids) => all_vpc_ids,
        Err(e) => return Err(e),
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&all_keysets)?);
    } else {
        convert_keysets_to_nice_table(all_keysets).printstd();
    }
    Ok(())
}

async fn show_keyset_details(
    identifier: forgerpc::TenantKeysetIdentifier,
    json: bool,
    api_client: &ApiClient,
) -> CarbideCliResult<()> {
    let keysets = api_client.get_one_keyset(identifier).await?;

    if keysets.keyset.len() != 1 {
        return Err(CarbideCliError::GenericError(
            "Unknown Tenant KeySet ID".to_string(),
        ));
    }

    let keysets = &keysets.keyset[0];

    if json {
        println!("{}", serde_json::to_string_pretty(keysets)?);
    } else {
        println!(
            "{}",
            convert_keyset_to_nice_format(keysets).unwrap_or_else(|x| x.to_string())
        );
    }
    Ok(())
}

fn convert_keysets_to_nice_table(keysets: forgerpc::TenantKeySetList) -> Box<Table> {
    let mut table = Table::new();

    table.set_titles(row!["Id", "TenantOrg", "Version", "Keys",]);

    for keyset in keysets.keyset {
        table.add_row(row![
            keyset
                .keyset_identifier
                .as_ref()
                .map(|ki| ki.keyset_id.as_str())
                .unwrap_or_default(),
            keyset
                .keyset_identifier
                .as_ref()
                .map(|ki| ki.organization_id.as_str())
                .unwrap_or_default(),
            keyset.version,
            keyset
                .keyset_content
                .unwrap_or_default()
                .public_keys
                .len()
                .to_string(),
        ]);
    }

    table.into()
}

fn convert_keyset_to_nice_format(keyset: &forgerpc::TenantKeyset) -> CarbideCliResult<String> {
    let width = 25;
    let mut lines = String::new();

    let data = vec![
        (
            "ID",
            keyset
                .keyset_identifier
                .as_ref()
                .map(|ki| ki.keyset_id.as_str())
                .unwrap_or_default(),
        ),
        (
            "TENANT ORG",
            keyset
                .keyset_identifier
                .as_ref()
                .map(|ki| ki.organization_id.as_str())
                .unwrap_or_default(),
        ),
        ("VERSION", keyset.version.as_str()),
    ];

    for (key, value) in data {
        writeln!(&mut lines, "{key:<width$}: {value}")?;
    }

    writeln!(&mut lines, "{:<width$}: ", "KEYS")?;
    let width = 17;
    if keyset
        .keyset_content
        .as_ref()
        .is_none_or(|c| c.public_keys.is_empty())
    {
        writeln!(&mut lines, "\tNONE")?;
    } else if let Some(keyset_content) = keyset.keyset_content.as_ref() {
        for key in &keyset_content.public_keys {
            let data = vec![
                ("PUBLIC", key.public_key.as_str()),
                ("COMMENT", key.comment.as_deref().unwrap_or_default()),
            ];

            for (key, value) in data {
                writeln!(&mut lines, "\t{key:<width$}: {value}")?;
            }
            writeln!(
                &mut lines,
                "\t------------------------------------------------------------"
            )?;
        }
    }

    Ok(lines)
}

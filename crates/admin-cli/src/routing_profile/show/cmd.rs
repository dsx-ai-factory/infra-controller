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

use eyre::{Context, ensure};
use prettytable::{Table, row};
use rpc::admin_cli::OutputFormat;
use rpc::forge::{NamedRoutingProfile, RoutingProfileSearchFilter, RoutingProfilesByNamesRequest};

use super::Args;
use crate::cfg::runtime::RuntimeContext;
use crate::{async_write, async_write_table_as_csv, async_writeln};

pub(super) async fn show(args: Args, ctx: &mut RuntimeContext) -> eyre::Result<()> {
    let explicit_names = !args.names.is_empty();
    let mut names = if explicit_names {
        args.names
    } else {
        ctx.api_client
            .0
            .find_routing_profile_names(RoutingProfileSearchFilter {
                internal: args.internal,
                minimum_access_tier: args.minimum_access_tier,
            })
            .await
            .context("list configured routing profile names")?
            .names
    };
    names.sort();
    names.dedup();
    let detailed = explicit_names && names.len() == 1;
    let mut profiles = Vec::new();
    if !names.is_empty() {
        let page_size = ctx
            .api_client
            .effective_chunk_size(ctx.config.page_size)
            .await
            .context("determine routing profile fetch batch size")?;
        for page in names.chunks(page_size) {
            let response = ctx
                .api_client
                .0
                .find_routing_profiles_by_names(RoutingProfilesByNamesRequest {
                    names: page.to_vec(),
                })
                .await
                .context("fetch configured routing profile definitions")?;
            profiles.extend(response.profiles);
        }
    }
    profiles.sort_by(|left, right| left.name.cmp(&right.name));
    if explicit_names {
        let missing: Vec<_> = names
            .iter()
            .filter(|name| {
                profiles
                    .binary_search_by(|profile| profile.name.as_str().cmp(name.as_str()))
                    .is_err()
            })
            .cloned()
            .collect();
        ensure!(
            missing.is_empty(),
            "routing profiles not found: {}",
            missing.join(", ")
        );
        profiles.retain(|profile| {
            let config = profile.config.as_ref();
            args.internal.is_none_or(|internal| {
                config
                    .and_then(|config| config.internal)
                    .unwrap_or_default()
                    == internal
            }) && args.minimum_access_tier.is_none_or(|minimum| {
                config
                    .and_then(|config| config.access_tier)
                    .unwrap_or_default()
                    >= minimum
            })
        });
    }
    write_profiles(ctx, &profiles, detailed).await
}

async fn write_profiles(
    ctx: &mut RuntimeContext,
    profiles: &[NamedRoutingProfile],
    detailed: bool,
) -> eyre::Result<()> {
    let format = ctx.config.format;
    match format {
        OutputFormat::Json => {
            let text = serde_json::to_string_pretty(profiles)
                .context("serialize routing profiles as JSON")?;
            async_writeln!(&mut ctx.output_file, "{text}")
                .context("write routing profile definitions")?;
        }
        OutputFormat::Yaml => {
            let text =
                serde_yaml::to_string(profiles).context("serialize routing profiles as YAML")?;
            async_write!(&mut ctx.output_file, "{text}")
                .context("write routing profile definitions")?;
        }
        OutputFormat::AsciiTable | OutputFormat::Csv => {
            let mut table = Table::new();
            if detailed {
                table.set_titles(row!["Name", "Property", "Configured value"]);
            } else {
                table.set_titles(row![
                    "Name",
                    "Internal",
                    "Access tier",
                    "Leak default from underlay",
                    "Leak hosts to underlay",
                ]);
            }
            for profile in profiles {
                let config = profile.config.as_ref().ok_or_else(|| {
                    eyre::eyre!("routing profile {} has no configuration", profile.name)
                })?;
                if detailed {
                    if format == OutputFormat::AsciiTable {
                        async_writeln!(&mut ctx.output_file, "{:<36}: {}", "NAME", profile.name)
                            .context("write routing profile name")?;
                    }
                    let value = serde_json::to_value(config)
                        .context("format routing profile configuration")?;
                    let fields = value.as_object().ok_or_else(|| {
                        eyre::eyre!("routing profile configuration is not an object")
                    })?;
                    for (key, value) in fields {
                        let display = if value.is_null() {
                            "unset".to_string()
                        } else {
                            value.to_string()
                        };
                        if format == OutputFormat::Csv {
                            table.add_row(row![profile.name, key, display]);
                        } else {
                            let label = key.replace('_', " ").to_uppercase();
                            async_writeln!(&mut ctx.output_file, "{label:<36}: {display}")
                                .context("write routing profile property")?;
                        }
                    }
                } else {
                    table.add_row(row![
                        profile.name,
                        config
                            .internal
                            .map_or_else(|| "unset".into(), |value| value.to_string()),
                        config
                            .access_tier
                            .map_or_else(|| "unset".into(), |value| value.to_string()),
                        config
                            .leak_default_route_from_underlay
                            .map_or_else(|| "unset".into(), |value| value.to_string()),
                        config
                            .leak_tenant_host_routes_to_underlay
                            .map_or_else(|| "unset".into(), |value| value.to_string()),
                    ]);
                }
            }
            if format == OutputFormat::Csv {
                async_write_table_as_csv!(&mut ctx.output_file, table)
                    .context("write routing profile CSV")?;
            } else if !detailed {
                async_write!(&mut ctx.output_file, "{table}")
                    .context("write routing profile table")?;
            }
        }
    }
    Ok(())
}

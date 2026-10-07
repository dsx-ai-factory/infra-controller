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
use std::borrow::Cow;
use std::fmt::Write;

use ::rpc::admin_cli::OutputFormat;
use ::rpc::forge::{self as forgerpc};
use carbide_uuid::vpc::VpcId;
use prettytable::{Table, row};

use super::args::Args;
use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::rpc::ApiClient;
use crate::{async_write, async_writeln};

pub(crate) trait VpcShowClient {
    async fn list_vpcs(&self, args: Args, page_size: usize) -> CarbideCliResult<forgerpc::VpcList>;

    async fn find_vpcs_by_ids(&self, vpc_ids: Vec<VpcId>) -> CarbideCliResult<forgerpc::VpcList>;
}

impl VpcShowClient for ApiClient {
    async fn list_vpcs(&self, args: Args, page_size: usize) -> CarbideCliResult<forgerpc::VpcList> {
        ApiClient::get_all_vpcs(
            self,
            args.tenant_org_id,
            args.name,
            page_size,
            args.label_key,
            args.label_value,
        )
        .await
    }

    async fn find_vpcs_by_ids(&self, vpc_ids: Vec<VpcId>) -> CarbideCliResult<forgerpc::VpcList> {
        Ok(self.0.find_vpcs_by_ids(vpc_ids).await?)
    }
}

pub(crate) async fn show(
    args: Args,
    output_format: OutputFormat,
    output_file: &mut Box<dyn tokio::io::AsyncWrite + Unpin>,
    api_client: &impl VpcShowClient,
    page_size: usize,
) -> CarbideCliResult<()> {
    let is_json = output_format == OutputFormat::Json;
    if let Some(id) = args.id {
        show_vpc_details(id, is_json, output_file, api_client).await?;
    } else {
        show_vpcs(is_json, output_file, api_client, page_size, args).await?;
    }
    Ok(())
}

async fn show_vpcs(
    json: bool,
    output_file: &mut Box<dyn tokio::io::AsyncWrite + Unpin>,
    api_client: &impl VpcShowClient,
    page_size: usize,
    args: Args,
) -> CarbideCliResult<()> {
    let all_vpcs = api_client.list_vpcs(args, page_size).await?;
    if json {
        async_writeln!(output_file, "{}", serde_json::to_string_pretty(&all_vpcs)?)?;
    } else {
        async_write!(output_file, "{}", convert_vpcs_to_nice_table(all_vpcs))?;
    }
    Ok(())
}

async fn show_vpc_details(
    vpc_id: VpcId,
    json: bool,
    output_file: &mut Box<dyn tokio::io::AsyncWrite + Unpin>,
    api_client: &impl VpcShowClient,
) -> CarbideCliResult<()> {
    let vpcs = api_client.find_vpcs_by_ids(vec![vpc_id]).await?;

    if vpcs.vpcs.len() != 1 {
        return Err(CarbideCliError::GenericError("Unknown VPC ID".to_string()));
    }

    let vpcs = &vpcs.vpcs[0];

    if json {
        async_writeln!(output_file, "{}", serde_json::to_string_pretty(vpcs)?)?;
    } else {
        async_writeln!(
            output_file,
            "{}",
            convert_vpc_to_nice_format(vpcs).unwrap_or_else(|x| x.to_string())
        )?;
    }
    Ok(())
}

fn convert_vpcs_to_nice_table(vpcs: forgerpc::VpcList) -> Box<Table> {
    let mut table = Table::new();

    table.set_titles(row![
        "Id",
        "Name",
        "TenantOrg",
        "Network Security Group",
        "Version",
        "Created",
        "Virt Type",
        "SLAAC Enabled",
        "Labels",
    ]);
    let default_metadata = Default::default();

    for vpc in vpcs.vpcs {
        let metadata = vpc.metadata.as_ref().unwrap_or(&default_metadata);
        let config = vpc.config.unwrap_or_default();
        let virt_type = forgerpc::VpcVirtualizationType::try_from(
            config.network_virtualization_type.unwrap_or_default(),
        )
        .unwrap_or_default()
        .as_str_name()
        .to_string();

        table.add_row(row![
            vpc.id.unwrap_or_default(),
            metadata.name,
            config.tenant_organization_id,
            config.network_security_group_id.unwrap_or_default(),
            vpc.version,
            vpc.created.unwrap_or_default(),
            virt_type,
            format_slaac_enabled(config.slaac_enabled),
            metadata
                .labels
                .iter()
                .map(|label| {
                    let key = &label.key;
                    let value = label.value.as_deref().unwrap_or_default();
                    format!("\"{key}:{value}\"")
                })
                .collect::<Vec<_>>()
                .join(", "),
        ]);
    }

    table.into()
}

pub(in crate::vpc) fn convert_vpc_to_nice_format(vpc: &forgerpc::Vpc) -> CarbideCliResult<String> {
    let width = 25;
    let mut lines = String::new();
    let default_config = Default::default();
    let config = vpc.config.as_ref().unwrap_or(&default_config);
    let allocated_vni = vpc
        .status
        .as_ref()
        .and_then(|status| status.vni)
        .unwrap_or_default();
    let network_virtualization_type = config.network_virtualization_type.unwrap_or_default();
    let routing_profile_overrides = config
        .routing_profile_overrides
        .as_ref()
        .map(serde_json::to_string_pretty)
        .transpose()?
        .unwrap_or_else(|| "None".to_string());
    let effective_routing_profile = vpc
        .status
        .as_ref()
        .and_then(|status| status.effective_routing_profile.as_ref())
        .map(serde_json::to_string_pretty)
        .transpose()?
        .unwrap_or_else(|| "None".to_string());

    let vpc_name = vpc
        .metadata
        .as_ref()
        .map(|x| Cow::Borrowed(x.name.as_str()))
        .unwrap_or("<no name>".into());

    let data: Vec<(&'static str, Cow<str>)> = vec![
        ("ID", vpc.id.unwrap_or_default().to_string().into()),
        ("NAME", vpc_name),
        ("TENANT ORG", config.tenant_organization_id.as_str().into()),
        (
            "NETWORK SECURITY GROUP",
            config
                .network_security_group_id
                .as_deref()
                .unwrap_or_default()
                .into(),
        ),
        ("VERSION", vpc.version.as_str().into()),
        (
            "CREATED",
            vpc.created.unwrap_or_default().to_string().into(),
        ),
        (
            "UPDATED",
            vpc.updated.unwrap_or_default().to_string().into(),
        ),
        (
            "DELETED",
            match vpc.deleted {
                Some(ts) => ts.to_string().into(),
                None => "".into(),
            },
        ),
        (
            "TENANT KEYSET",
            config
                .tenant_keyset_id
                .as_deref()
                .unwrap_or_default()
                .into(),
        ),
        ("VNI", format!("{allocated_vni}").into()),
        (
            "NW VIRTUALIZATION",
            forgerpc::VpcVirtualizationType::try_from(network_virtualization_type)
                .unwrap_or_default()
                .as_str_name()
                .into(),
        ),
        (
            "SLAAC ENABLED",
            format_slaac_enabled(config.slaac_enabled).into(),
        ),
        (
            "ROUTING PROFILE TYPE",
            config
                .routing_profile_type
                .as_deref()
                .unwrap_or_default()
                .into(),
        ),
        (
            "ROUTING PROFILE OVERRIDES",
            routing_profile_overrides.into(),
        ),
        (
            "EFFECTIVE ROUTING PROFILE",
            effective_routing_profile.into(),
        ),
    ];

    for (key, value) in data {
        writeln!(&mut lines, "{key:<width$}: {value}")?;
    }

    Ok(lines)
}

fn format_slaac_enabled(slaac_enabled: Option<bool>) -> &'static str {
    match slaac_enabled {
        Some(true) => "true",
        Some(false) => "false",
        // `None` means Core predates the response field.
        None => "Unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::async_write::CapturedOutput;

    struct FakeVpcShowClient {
        vpcs: forgerpc::VpcList,
    }

    impl VpcShowClient for FakeVpcShowClient {
        async fn list_vpcs(
            &self,
            _args: Args,
            _page_size: usize,
        ) -> CarbideCliResult<forgerpc::VpcList> {
            Ok(self.vpcs.clone())
        }

        async fn find_vpcs_by_ids(
            &self,
            _vpc_ids: Vec<VpcId>,
        ) -> CarbideCliResult<forgerpc::VpcList> {
            Err(CarbideCliError::GenericError(
                "unexpected VPC detail request".to_string(),
            ))
        }
    }

    #[test]
    fn vpc_details_include_slaac_and_routing_profiles() {
        let vpc = forgerpc::Vpc {
            config: Some(forgerpc::VpcConfig {
                tenant_organization_id: "tenant".to_string(),
                slaac_enabled: Some(true),
                routing_profile_type: Some("INTERNAL".to_string()),
                routing_profile_overrides: Some(forgerpc::VpcRoutingProfileOverrides {
                    leak_default_route_from_underlay: Some(false),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            status: Some(forgerpc::VpcStatus {
                effective_routing_profile: Some(forgerpc::VpcEffectiveRoutingProfile {
                    internal: true,
                    access_tier: 2,
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        };

        let display = convert_vpc_to_nice_format(&vpc).expect("VPC display");
        assert!(display.contains("SLAAC ENABLED            : true"));
        assert!(display.contains("ROUTING PROFILE TYPE"));
        assert!(display.contains("INTERNAL"));
        let (_, routing_profiles) = display
            .split_once("ROUTING PROFILE OVERRIDES")
            .expect("routing-profile overrides");
        let (overrides, effective) = routing_profiles
            .split_once("EFFECTIVE ROUTING PROFILE")
            .expect("effective routing profile");
        assert!(overrides.contains("\"leak_default_route_from_underlay\": false"));
        assert!(!overrides.contains("\"internal\""));
        assert!(!overrides.contains("\"access_tier\""));
        assert!(effective.contains("\"internal\": true"));
        assert!(effective.contains("\"access_tier\": 2"));
    }

    #[tokio::test]
    async fn vpc_list_command_includes_slaac_presence_and_empty_cells() {
        let client = FakeVpcShowClient {
            vpcs: forgerpc::VpcList {
                vpcs: [
                    ("enabled", Some(true)),
                    ("disabled", Some(false)),
                    ("older-core", None),
                ]
                .into_iter()
                .map(|(name, slaac_enabled)| forgerpc::Vpc {
                    metadata: Some(forgerpc::Metadata {
                        name: name.to_string(),
                        ..Default::default()
                    }),
                    config: Some(forgerpc::VpcConfig {
                        slaac_enabled,
                        ..Default::default()
                    }),
                    ..Default::default()
                })
                .collect(),
            },
        };

        let mut captured = CapturedOutput::new();
        show(
            Args {
                id: None,
                tenant_org_id: None,
                name: None,
                label_key: None,
                label_value: None,
            },
            OutputFormat::AsciiTable,
            captured.writer(),
            &client,
            100,
        )
        .await
        .expect("VPC list should render");

        let display = String::from_utf8(captured.into_bytes().await).expect("UTF-8 output");
        let cells_for = |name| {
            display
                .lines()
                .find(|line| line.contains(name))
                .unwrap_or_else(|| panic!("missing row for {name}"))
                .trim_matches('|')
                .split('|')
                .map(str::trim)
                .collect::<Vec<_>>()
        };

        let header = cells_for("SLAAC Enabled");
        assert_eq!(header[7], "SLAAC Enabled");
        assert_eq!(cells_for("enabled")[7], "true");
        assert_eq!(cells_for("disabled")[7], "false");
        let older_core = cells_for("older-core");
        assert_eq!(older_core[7], "Unknown");
        assert_eq!(older_core[8], "");
    }
}

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
use std::str::FromStr;

use ::rpc::admin_cli::OutputFormat;
use ::rpc::forge::{self as forgerpc, Vpc, VpcsByIdsRequest};
use carbide_uuid::instance::InstanceId;
use carbide_uuid::machine::MachineId;
use carbide_uuid::network::NetworkSegmentId;
use carbide_uuid::vpc::VpcId;
use prettytable::{Table, row};

use super::args::Args;
use crate::cfg::cli_options::SortField;
use crate::cfg::runtime::RuntimeContext;
use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::rpc::ApiClient;
use crate::{async_write, async_writeln, invalid_machine_id};

async fn convert_instance_to_nice_format(
    api_client: &ApiClient,
    instance: &forgerpc::Instance,
    extrainfo: bool,
) -> CarbideCliResult<String> {
    let width = 25;
    let mut lines = String::new();

    let mut data = vec![
        (
            "ID",
            instance
                .id
                .map(|id| Cow::Owned(id.to_string()))
                .unwrap_or_default(),
        ),
        (
            "MACHINE ID",
            instance
                .machine_id
                .map(|id| Cow::Owned(id.to_string()))
                .unwrap_or_default(),
        ),
        (
            "TENANT ORG",
            instance
                .config
                .as_ref()
                .and_then(|config| config.tenant.as_ref())
                .map(|tenant| Cow::Borrowed(tenant.tenant_organization_id.as_str()))
                .unwrap_or_default(),
        ),
        (
            "TENANT STATE",
            instance
                .status
                .as_ref()
                .and_then(|status| status.tenant.as_ref())
                .and_then(|tenant| forgerpc::TenantState::try_from(tenant.state).ok())
                .map(|state| Cow::Owned(format!("{state:?}")))
                .unwrap_or_default(),
        ),
        (
            "TENANT STATE DETAILS",
            instance
                .status
                .as_ref()
                .and_then(|status| status.tenant.as_ref())
                .map(|tenant| Cow::Borrowed(tenant.state_details.as_str()))
                .unwrap_or_default(),
        ),
        (
            "INSTANCE TYPE ID",
            instance
                .instance_type_id
                .as_ref()
                .map(|id| Cow::Borrowed(id.as_str()))
                .unwrap_or_default(),
        ),
        (
            "CONFIGS SYNCED",
            instance
                .status
                .as_ref()
                .and_then(|status| forgerpc::SyncState::try_from(status.configs_synced).ok())
                .map(|state| Cow::Owned(format!("{state:?}")))
                .unwrap_or_default(),
        ),
        ("CONFIG VERSION", instance.config_version.as_str().into()),
        (
            "NETWORK CONFIG SYNCED",
            instance
                .status
                .as_ref()
                .and_then(|status| status.network.as_ref())
                .and_then(|status| forgerpc::SyncState::try_from(status.configs_synced).ok())
                .map(|state| Cow::Owned(format!("{state:?}")))
                .unwrap_or_default(),
        ),
        (
            "NETWORK CONFIG VERSION",
            instance.network_config_version.as_str().into(),
        ),
    ];

    let instance_os = instance
        .config
        .as_ref()
        .and_then(|config| config.os.as_ref());

    let mut extra_info = vec![
        (
            "IPXE SCRIPT",
            instance_os
                .and_then(|os| match os.variant.as_ref() {
                    Some(::rpc::forge::instance_operating_system_config::Variant::Ipxe(
                        ipxe_os,
                    )) => Some(Cow::Borrowed(ipxe_os.ipxe_script.as_str())),
                    Some(::rpc::forge::instance_operating_system_config::Variant::OsImageId(
                        image,
                    )) => Some(Cow::Owned(format!("OS Image ID: {}", image.value))),
                    Some(
                        ::rpc::forge::instance_operating_system_config::Variant::OperatingSystemId(
                            id,
                        ),
                    ) => Some(Cow::Owned(format!("Operating System ID: {}", id))),
                    None => None,
                })
                .unwrap_or_default(),
        ),
        (
            "USERDATA",
            instance_os
                .and_then(|os| os.user_data.as_ref())
                .map(|ud| ud.as_str().into())
                .unwrap_or_default(),
        ),
        (
            "RUN PROVISIONING ON EVERY BOOT",
            instance_os
                .map(|os| os.run_provisioning_instructions_on_every_boot)
                .unwrap_or_default()
                .to_string()
                .into(),
        ),
        (
            "PHONE HOME ENABLED",
            instance_os
                .map(|os| os.phone_home_enabled)
                .unwrap_or_default()
                .to_string()
                .into(),
        ),
    ];

    if extrainfo {
        data.append(&mut extra_info);
    }

    for (key, value) in data {
        writeln!(&mut lines, "{key:<width$}: {value}")?;
    }

    let width = 25;
    writeln!(&mut lines, "INTERFACES:")?;
    let network_config = instance
        .config
        .as_ref()
        .and_then(|config| config.network.as_ref());
    let if_configs = network_config
        .map(|config| config.interfaces.as_slice())
        .unwrap_or_default();
    let auto_network = network_config.is_some_and(|config| config.auto_config.is_some());
    let if_status = instance
        .status
        .as_ref()
        .and_then(|status| status.network.as_ref())
        .map(|status| status.interfaces.as_slice())
        .unwrap_or_default();

    if if_status.is_empty() {
        writeln!(&mut lines, "\tEMPTY")?;
    } else if !auto_network && if_configs.len() != if_status.len() {
        writeln!(&mut lines, "\tLENGTH MISMATCH")?;
    } else {
        let vpcs: Vec<Option<Vpc>> = if auto_network {
            futures::future::join_all(
                if_status
                    .iter()
                    .filter_map(|s| s.vpc_id)
                    .map(|vpc_id| get_vpc_by_id(api_client, vpc_id)),
            )
            .await
            .into_iter()
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
        } else {
            futures::future::join_all(if_configs.iter().filter_map(|c| c.network_segment_id).map(
                |segment_id| async move {
                    get_vpc_for_interface_network_segment(api_client, segment_id).await
                },
            ))
            .await
            .into_iter()
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
        }
        .collect();
        if !auto_network && if_configs.len() != if_status.len() {
            writeln!(&mut lines, "\tLENGTH MISMATCH")?;
        } else {
            for (idx, status) in if_status.iter().enumerate() {
                let vpc = vpcs.get(idx);
                let if_config = if_configs.get(idx);
                let data: &[(&str, Cow<str>)] = &[
                    (
                        "FUNCTION_TYPE",
                        format!(
                            "{:?}",
                            match status.virtual_function_id {
                                Some(_) => forgerpc::InterfaceFunctionType::Virtual,
                                None => forgerpc::InterfaceFunctionType::Physical,
                            }
                        )
                        .into(),
                    ),
                    (
                        "VF ID",
                        status
                            .virtual_function_id
                            .map(|id| id.to_string().into())
                            .unwrap_or_default(),
                    ),
                    (
                        "SEGMENT ID",
                        if_config
                            .and_then(|c| c.network_segment_id)
                            .unwrap_or_default()
                            .to_string()
                            .into(),
                    ),
                    (
                        "VPC PREFIX ID",
                        match if_config.and_then(|c| c.network_details.as_ref()) {
                            Some(
                                forgerpc::instance_interface_config::NetworkDetails::SegmentId(_),
                            ) => "Segment Based Allocation".into(),
                            Some(
                                forgerpc::instance_interface_config::NetworkDetails::VpcPrefixId(x),
                            ) => x.to_string().into(),
                            Some(forgerpc::instance_interface_config::NetworkDetails::Vpc(_)) => {
                                "Automatic VPC selection".into()
                            }
                            None => "NA".into(),
                        },
                    ),
                    (
                        "REQUESTED VPC ID",
                        match if_config.and_then(|c| c.network_details.as_ref()) {
                            Some(forgerpc::instance_interface_config::NetworkDetails::Vpc(
                                selection,
                            )) => selection
                                .vpc_id
                                .map(|vpc_id| vpc_id.to_string().into())
                                .unwrap_or_else(|| "NA".into()),
                            _ => "NA".into(),
                        },
                    ),
                    (
                        "REQUESTED IP FAMILY",
                        match if_config.and_then(|c| c.network_details.as_ref()) {
                            Some(forgerpc::instance_interface_config::NetworkDetails::Vpc(
                                selection,
                            )) => selection.family_mode().as_str_name().into(),
                            _ => "NA".into(),
                        },
                    ),
                    (
                        "RESOLVED IPV4 PREFIX ID",
                        status
                            .resolved_vpc_prefixes
                            .as_ref()
                            .and_then(|resolved| resolved.ipv4_vpc_prefix_id)
                            .map(|id| id.to_string().into())
                            .unwrap_or_else(|| "NA".into()),
                    ),
                    (
                        "RESOLVED IPV6 PREFIX ID",
                        status
                            .resolved_vpc_prefixes
                            .as_ref()
                            .and_then(|resolved| resolved.ipv6_vpc_prefix_id)
                            .map(|id| id.to_string().into())
                            .unwrap_or_else(|| "NA".into()),
                    ),
                    (
                        "MAC ADDR",
                        status
                            .mac_address
                            .as_ref()
                            .map(|s| s.as_str().into())
                            .unwrap_or_default(),
                    ),
                    ("ADDRESSES", status.addresses.as_slice().join(", ").into()),
                    (
                        "VPC ID",
                        vpc.map(|v| {
                            v.as_ref()
                                .and_then(|v| v.id)
                                .unwrap_or_default()
                                .to_string()
                                .into()
                        })
                        .unwrap_or("<not found>".into()),
                    ),
                    (
                        "VPC NAME",
                        vpc.and_then(|v| v.as_ref().and_then(|v| v.metadata.as_ref()))
                            .map(|v| Cow::Borrowed(v.name.as_str()))
                            .unwrap_or("<not found>".into()),
                    ),
                ];

                for (key, value) in data {
                    writeln!(&mut lines, "\t{key:<width$}: {value}")?;
                }
                writeln!(
                    &mut lines,
                    "\t--------------------------------------------------"
                )?;
            }
        }
    }

    if let Some(ib_config) = instance.config.as_ref().and_then(|c| c.infiniband.as_ref())
        && let Some(ib_status) = instance.status.as_ref().and_then(|s| s.infiniband.as_ref())
    {
        writeln!(&mut lines, "IB INTERFACES:")?;
        writeln!(
            &mut lines,
            "\t{:<width$}: {}",
            "IB CONFIG VERSION", instance.ib_config_version,
        )?;
        writeln!(
            &mut lines,
            "\t{:<width$}: {}",
            "CONFIG SYNCED", ib_status.configs_synced
        )?;
        for (i, interface) in ib_config.ib_interfaces.iter().enumerate() {
            let status = &ib_status.ib_interfaces[i];
            let data: &[(&str, Cow<str>)] = &[
                (
                    "FUNCTION_TYPE",
                    forgerpc::InterfaceFunctionType::try_from(interface.function_type)
                        .ok()
                        .map(|ty| format!("{ty:?}").into())
                        .unwrap_or_else(|| "INVALID".into()),
                ),
                (
                    "VENDOR",
                    interface
                        .vendor
                        .as_ref()
                        .map(|v| v.as_str().into())
                        .unwrap_or_default(),
                ),
                ("DEVICE", interface.device.as_str().into()),
                (
                    "DEVICE INSTANCE",
                    interface.device_instance.to_string().into(),
                ),
                (
                    "VF ID",
                    interface
                        .virtual_function_id
                        .map(|x| x.to_string().into())
                        .unwrap_or_default(),
                ),
                (
                    "PARTITION ID",
                    interface
                        .ib_partition_id
                        .map(|x| x.to_string().into())
                        .unwrap_or_default(),
                ),
                (
                    "PF GUID",
                    status
                        .pf_guid
                        .as_ref()
                        .map(|g| g.as_str().into())
                        .unwrap_or_default(),
                ),
                (
                    "GUID",
                    status
                        .guid
                        .as_ref()
                        .map(|g| g.as_str().into())
                        .unwrap_or_default(),
                ),
                ("LID", status.lid.to_string().into()),
            ];

            for (key, value) in data {
                writeln!(&mut lines, "\t{key:<width$}: {value}")?;
            }
            writeln!(
                &mut lines,
                "\t--------------------------------------------------"
            )?;
        }
    }

    if let Some(nsg_id) = instance
        .config
        .as_ref()
        .and_then(|c| c.network_security_group_id.as_ref())
    {
        writeln!(&mut lines, "NETWORK SECURITY GROUP ID: {nsg_id}")?;
    }

    crate::metadata::write_metadata_in_nice_format(&mut lines, width, instance.metadata.as_ref())?;

    Ok(lines)
}

fn convert_instances_to_nice_table(instances: forgerpc::InstanceList) -> Box<Table> {
    let mut table = Table::new();

    table.set_titles(row![
        "Id",
        "MachineId",
        "TenantOrg",
        "TenantState",
        "InstanceTypeId",
        "ConfigsSynced",
        "IPAddresses",
        "Labels",
    ]);

    for instance in instances.instances {
        let tenant_org = instance
            .config
            .as_ref()
            .and_then(|config| config.tenant.as_ref())
            .map(|tenant| tenant.tenant_organization_id.as_str())
            .unwrap_or_default();

        let labels = crate::metadata::fmt_labels_as_kv_pairs(instance.metadata.as_ref());

        let tenant_state = instance
            .status
            .as_ref()
            .and_then(|status| status.tenant.as_ref())
            .and_then(|tenant| forgerpc::TenantState::try_from(tenant.state).ok())
            .map(|state| format!("{state:?}"))
            .unwrap_or_default();

        let configs_synced = instance
            .status
            .as_ref()
            .and_then(|status| forgerpc::SyncState::try_from(status.configs_synced).ok())
            .map(|state| format!("{state:?}"))
            .unwrap_or_default();

        let instance_addresses: Vec<&str> = instance
            .status
            .as_ref()
            .and_then(|status| status.network.as_ref())
            .map(|network| network.interfaces.as_slice())
            .unwrap_or_default()
            .iter()
            .filter(|x| x.virtual_function_id.is_none())
            .flat_map(|status| status.addresses.iter().map(|addr| addr.as_str()))
            .collect();

        table.add_row(row![
            instance.id.unwrap_or_default(),
            instance
                .machine_id
                .map(|id| id.to_string())
                .unwrap_or_else(invalid_machine_id),
            tenant_org,
            tenant_state,
            instance.instance_type_id.unwrap_or_default(),
            configs_synced,
            instance_addresses.join(","),
            labels.join(", ")
        ]);
    }

    table.into()
}

async fn show_instance_details(
    id: String,
    output_file: &mut Box<dyn tokio::io::AsyncWrite + Unpin>,
    output_format: &OutputFormat,
    api_client: &ApiClient,
    extrainfo: bool,
) -> CarbideCliResult<()> {
    let instance = if let Ok(id) = id.parse::<MachineId>() {
        api_client.0.find_instance_by_machine_id(id).await?
    } else {
        let instance_id = InstanceId::from_str(&id)
            .map_err(|_| CarbideCliError::GenericError("UUID Conversion failed.".to_string()))?;
        match api_client.get_one_instance(instance_id).await {
            Ok(instance) => instance,
            Err(e) => return Err(e),
        }
    };

    if instance.instances.len() != 1 {
        return Err(CarbideCliError::GenericError(
            "Unknown Instance ID".to_string(),
        ));
    }

    let instance = &instance.instances[0];
    match output_format {
        OutputFormat::Json => {
            async_writeln!(output_file, "{}", serde_json::to_string_pretty(instance)?)?;
        }
        OutputFormat::AsciiTable => {
            async_write!(
                output_file,
                "{}",
                convert_instance_to_nice_format(api_client, instance, extrainfo).await?
            )?;
        }
        OutputFormat::Csv => {
            return Err(CarbideCliError::NotImplemented(
                "CSV formatted output".to_string(),
            ));
        }
        OutputFormat::Yaml => {
            return Err(CarbideCliError::NotImplemented(
                "YAML formatted output".to_string(),
            ));
        }
    }
    Ok(())
}

pub(crate) async fn handle_show(args: Args, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
    if args.id.is_empty() {
        let mut all_instances = ctx
            .api_client
            .get_all_instances(
                args.tenant_org_id,
                args.vpc_id,
                args.label_key,
                args.label_value,
                args.instance_type_id,
                ctx.config.page_size,
            )
            .await?;

        match ctx.config.sort_by {
            SortField::PrimaryId => all_instances.instances.sort_by_key(|instance| instance.id),
            SortField::State => all_instances.instances.sort_by(|i1, i2| {
                let tenant_status1 = i1
                    .status
                    .as_ref()
                    .and_then(|status| status.tenant.as_ref())
                    .and_then(|tenant| forgerpc::TenantState::try_from(tenant.state).ok())
                    .map(|state| format!("{state:?}"))
                    .unwrap_or_default();
                let tenant_status2 = i2
                    .status
                    .as_ref()
                    .and_then(|status| status.tenant.as_ref())
                    .and_then(|tenant| forgerpc::TenantState::try_from(tenant.state).ok())
                    .map(|state| format!("{state:?}"))
                    .unwrap_or_default();
                tenant_status1.cmp(&tenant_status2)
            }),
        }
        match ctx.config.format {
            OutputFormat::Json => {
                async_writeln!(
                    ctx.output_file,
                    "{}",
                    serde_json::to_string_pretty(&all_instances)?
                )?;
            }
            OutputFormat::AsciiTable => {
                let table = convert_instances_to_nice_table(all_instances);
                async_write!(ctx.output_file, "{}", table)?;
            }
            OutputFormat::Csv => {
                return Err(CarbideCliError::NotImplemented(
                    "CSV formatted output".to_string(),
                ));
            }
            OutputFormat::Yaml => {
                return Err(CarbideCliError::NotImplemented(
                    "YAML formatted output".to_string(),
                ));
            }
        }
        return Ok(());
    }
    show_instance_details(
        args.id,
        &mut ctx.output_file,
        &ctx.config.format,
        &ctx.api_client,
        args.extrainfo,
    )
    .await?;
    Ok(())
}

async fn get_vpc_for_interface_network_segment(
    api_client: &ApiClient,
    network_segment_id: NetworkSegmentId,
) -> CarbideCliResult<Option<Vpc>> {
    let network_segments = api_client
        .get_segments_by_ids(&[network_segment_id])
        .await?;

    if !network_segments.network_segments.is_empty()
        && let Some(vpc_id) = network_segments.network_segments.first().and_then(|s| {
            #[allow(deprecated)]
            s.config.as_ref().and_then(|c| c.vpc_id).or(s.vpc_id)
        })
    {
        let vpc_ids: Vec<VpcId> = vec![vpc_id];
        Ok(api_client
            .0
            .find_vpcs_by_ids(VpcsByIdsRequest { vpc_ids })
            .await?
            .vpcs
            .into_iter()
            .next())
    } else {
        Ok(None)
    }
}

async fn get_vpc_by_id(api_client: &ApiClient, vpc_id: VpcId) -> CarbideCliResult<Option<Vpc>> {
    Ok(api_client
        .0
        .find_vpcs_by_ids(VpcsByIdsRequest {
            vpc_ids: vec![vpc_id],
        })
        .await?
        .vpcs
        .into_iter()
        .next())
}

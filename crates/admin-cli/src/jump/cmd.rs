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

use std::net::IpAddr;
use std::str::FromStr;

use ::rpc::forge as forgerpc;
use carbide_uuid::machine::MachineId;
use dpa::ShowDpa;
use mac_address::MacAddress;

use super::args::Cmd;
use crate::cfg::runtime::RuntimeContext;
use crate::errors::CarbideCliError;
use crate::{
    compute_allocation, domain, dpa, instance, machine, machine_interfaces, network_segment,
    resource_pool, site_explorer, vpc,
};

pub(super) async fn jump(args: Cmd, ctx: &mut RuntimeContext) -> color_eyre::Result<()> {
    // Is it a machine ID?
    // Grab the machine details.
    if let Ok(machine_id) = args.id.parse::<MachineId>() {
        machine::handle_show(
            machine::ShowMachine {
                machine: Some(machine_id),
                help: None,
                hosts: false,
                all: false,
                dpus: false,
                instance_type_id: None,
                history_count: 5,
            },
            &ctx.config.format,
            &mut ctx.output_file,
            &ctx.api_client,
            ctx.config.page_size,
            &ctx.config.sort_by,
        )
        .await?;

        return Ok(());
    }

    // Is it an IP? Uses FindIpAddress (machine / BMC / static BMC / instance / …).
    if IpAddr::from_str(&args.id).is_ok() {
        let req = forgerpc::FindIpAddressRequest { ip: args.id };

        let resp = ctx.api_client.0.find_ip_address(req).await?;

        // Go through each object that matched the IP search,
        // and perform any more specific searches available for
        // the object type of the owner.   E.g., if it's an IP
        // attached to an instance, get the details of the instance.
        for m in resp.matches {
            let ip_type = match forgerpc::IpType::try_from(m.ip_type) {
                Ok(t) => t,
                Err(err) => {
                    tracing::error!(ip_type = m.ip_type, error = %err, "Invalid IpType");
                    continue;
                }
            };

            let config_format = ctx.config.format;

            use forgerpc::IpType::*;
            match ip_type {
                StaticDataDhcpServer => tracing::info!("DHCP Server"),
                StaticDataRouteServer => tracing::info!("Route Server"),
                RouteServerFromConfigFile => tracing::info!("Route Server from Carbide config"),
                RouteServerFromAdminApi => tracing::info!("Route Server from Admin API"),
                InstanceAddress => {
                    instance::handle_show(
                        instance::ShowInstance {
                            id: m.owner_id.ok_or(CarbideCliError::GenericError(
                                "failed to unwrap owner_id after finding instance for IP"
                                    .to_string(),
                            ))?,
                            extrainfo: true,
                            tenant_org_id: None,
                            vpc_id: None,
                            label_key: None,
                            label_value: None,
                            instance_type_id: None,
                        },
                        ctx,
                    )
                    .await?
                }
                // StaticBmcIp: operator-configured BMC IP (same `jump` target as BmcIp / MachineAddress).
                MachineAddress | BmcIp | StaticBmcIp | LoopbackIp => {
                    machine::handle_show(
                        machine::ShowMachine {
                            machine: Some(
                                m.owner_id
                                    .and_then(|id| id.parse::<MachineId>().ok())
                                    .ok_or(CarbideCliError::GenericError(
                                        "failed to unwrap owner_id after finding machine for IP"
                                            .to_string(),
                                    ))?,
                            ),
                            help: None,
                            hosts: false,
                            all: false,
                            dpus: false,
                            instance_type_id: None,
                            history_count: 5,
                        },
                        &config_format,
                        &mut ctx.output_file,
                        &ctx.api_client,
                        ctx.config.page_size,
                        &ctx.config.sort_by,
                    )
                    .await?;
                }

                ExploredEndpoint => {
                    site_explorer::show_site_explorer_discovered_managed_host(
                        &ctx.api_client,
                        &mut ctx.output_file,
                        config_format,
                        ctx.config.page_size,
                        site_explorer::GetReportMode::Endpoint(
                            site_explorer::EndpointInfo {
                                address: if m.owner_id.is_some() {
                                    m.owner_id
                                } else {
                                    color_eyre::eyre::bail!(CarbideCliError::GenericError(
                                        "IP type is explored-endpoint but returned owner_id is empty".to_string()
                                    ))
                                },
                                erroronly: false,
                                successonly: false,
                                unpairedonly: false,
                                vendor: None,
                            },
                        ),
                    )
                    .await?;
                }

                NetworkSegment => {
                    network_segment::handle_show(
                        network_segment::ShowNetworkSegment {
                            network: Some(
                                m.owner_id
                                    .ok_or(CarbideCliError::GenericError(
                                        "failed to unwrap owner_id after finding network segment for IP".to_string(),
                                    ))?
                                    .parse()?,
                            ),
                            tenant_org_id: None,
                            name: None,
                        },
                        config_format,
                        &ctx.api_client,
                        ctx.config.page_size,
                    )
                    .await?
                }
                ResourcePool => resource_pool::list(Default::default(), &ctx.api_client).await?,
                DpaInterface =>  {
                    dpa::show(
                        &ShowDpa {
                            id: Some(m.owner_id.ok_or(CarbideCliError::GenericError(
                                "failed to unwrap owner_id after dpa interface for IP".to_string(),
                            ))?.parse()?),
                        },
                        config_format,
                        &ctx.api_client,
                        ctx.config.page_size,
                    )
                    .await?
                }
            };
        }

        return Ok(());
    }

    // Is it the UUID of some type of object?
    // Try to identify the type of object and then perform
    // a search for the object's details.  E.g., if it's the
    // UUID of an instance, then get the details of the instance.
    if let Ok(u) = args.id.parse::<uuid::Uuid>() {
        match ctx.api_client.identify_uuid(u).await {
            Ok(o) => match o {
                forgerpc::UuidType::NetworkSegment => {
                    network_segment::handle_show(
                        network_segment::ShowNetworkSegment {
                            network: Some(args.id.parse()?),
                            tenant_org_id: None,
                            name: None,
                        },
                        ctx.config.format,
                        &ctx.api_client,
                        ctx.config.page_size,
                    )
                    .await?
                }
                forgerpc::UuidType::Instance => {
                    instance::handle_show(
                        instance::ShowInstance {
                            id: args.id,
                            extrainfo: true,
                            tenant_org_id: None,
                            vpc_id: None,
                            label_key: None,
                            label_value: None,
                            instance_type_id: None,
                        },
                        ctx,
                    )
                    .await?
                }
                forgerpc::UuidType::MachineInterface => {
                    machine_interfaces::handle_show(
                        machine_interfaces::ShowMachineInterfaces {
                            interface_id: Some(args.id.parse()?),
                            all: false,
                            more: true,
                        },
                        ctx.config.format,
                        &ctx.api_client,
                    )
                    .await?
                }
                forgerpc::UuidType::Vpc => {
                    vpc::show(
                        vpc::ShowVpc {
                            id: Some(args.id.parse()?),
                            tenant_org_id: None,
                            name: None,
                            label_key: None,
                            label_value: None,
                        },
                        ctx.config.format,
                        &mut ctx.output_file,
                        &ctx.api_client,
                        1,
                    )
                    .await?
                }
                forgerpc::UuidType::Domain => {
                    domain::handle_show(
                        &domain::ShowDomain {
                            domain: Some(args.id.parse()?),
                            all: false,
                        },
                        ctx.config.format,
                        &ctx.api_client,
                    )
                    .await?
                }
                forgerpc::UuidType::DpaInterfaceId => {
                    dpa::show(
                        &ShowDpa {
                            id: Some(args.id.parse()?),
                        },
                        ctx.config.format,
                        &ctx.api_client,
                        1,
                    )
                    .await?
                }
                forgerpc::UuidType::ComputeAllocationId => {
                    compute_allocation::show(
                        compute_allocation::ShowComputeAllocation {
                            id: Some(args.id.parse()?),
                            tenant_organization_id: None,
                            name: None,
                            instance_type_id: None,
                        },
                        ctx.config.format,
                        &ctx.api_client,
                        1,
                        true,
                    )
                    .await?
                }
            },
            Err(e) => {
                color_eyre::eyre::bail!(e);
            }
        }

        return Ok(());
    }

    // Is it a MAC?
    // Grab the details for the interface it's associated with.
    if let Ok(m) = MacAddress::from_str(&args.id) {
        match ctx.api_client.identify_mac(m).await {
            Ok((mac_owner, primary_key)) => match mac_owner {
                forgerpc::MacOwner::MachineInterface => {
                    machine_interfaces::handle_show(
                        machine_interfaces::ShowMachineInterfaces {
                            interface_id: Some(primary_key.parse()?),
                            all: false,
                            more: true,
                        },
                        ctx.config.format,
                        &ctx.api_client,
                    )
                    .await?
                }
                forgerpc::MacOwner::ExploredEndpoint => {
                    color_eyre::eyre::bail!(
                        "searching explored-endpoints from MAC not yet implemented"
                    );
                }
                forgerpc::MacOwner::ExpectedMachine => {
                    color_eyre::eyre::bail!(
                        "searching expected-machines from MAC not yet implemented"
                    );
                }
                forgerpc::MacOwner::DpaInterface => {
                    dpa::show(
                        &ShowDpa {
                            id: Some(primary_key.parse()?),
                        },
                        ctx.config.format,
                        &ctx.api_client,
                        1,
                    )
                    .await?
                }
            },
            Err(e) => {
                color_eyre::eyre::bail!(e);
            }
        }

        return Ok(());
    }

    // Is it a serial number?!??!?!
    // Grab the machine ID and look-up the machine.
    if let Ok(machine_id) = ctx.api_client.identify_serial(args.id, false).await {
        machine::handle_show(
            machine::ShowMachine {
                machine: Some(machine_id),
                help: None,
                hosts: false,
                all: false,
                dpus: false,
                instance_type_id: None,
                history_count: 5,
            },
            &ctx.config.format,
            &mut ctx.output_file,
            &ctx.api_client,
            ctx.config.page_size,
            &ctx.config.sort_by,
        )
        .await?;

        return Ok(());
    }

    // Do we have no idea what it is?
    color_eyre::eyre::bail!("unable to determine ID type");
}

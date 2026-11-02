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
use carbide_network::ip::prefix::Ipv4Network;
use carbide_uuid::network::NetworkSegmentId;
use rpc::forge::{
    DeletedFilter, PrefixMatchType, Vpc, VpcPrefixCreationRequest, VpcPrefixSearchQuery,
};
use serde::{Deserialize, Serialize};

use super::args::{Args, NetworkChoice};
use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::rpc::ApiClient;

#[derive(Clone, Debug, Deserialize, Serialize)]
struct DevEnvFileConfig {
    #[serde(default)]
    overlay_networks: Vec<Ipv4Network>,
    /// Networks for HostInband segments on hosts with no DPU. `reserve_first`
    /// is the number of leading IPs to reserve for the gateway and infra.
    #[serde(default)]
    host_inband_networks: Vec<HostInbandNetwork>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct HostInbandNetwork {
    prefix: Ipv4Network,
    #[serde(default)]
    reserve_first: i32,
}

const DEVENV_VPC_NAME: &str = "devenv_tenant_vpc";
const DEVENV_FLAT_VPC_NAME: &str = "devenv_flat_vpc";

async fn get_or_create_vpc(api_client: &ApiClient) -> CarbideCliResult<Vpc> {
    // Get or create VPC with name "devenv_tenant_vpc"
    let vpcs = api_client.get_vpc_by_name(DEVENV_VPC_NAME).await?;
    let vpc = vpcs.vpcs.first().cloned();

    Ok(if let Some(vpc) = vpc {
        vpc
    } else {
        // If VPC does not exists, create new.
        let vpc_id = uuid::Uuid::new_v4().into();
        let vpc = api_client.create_vpc(DEVENV_VPC_NAME, vpc_id).await?;
        println!(
            "Created VPC with ID: {}, name: {}",
            vpc.id.unwrap_or_default(),
            vpc.metadata
                .as_ref()
                .map(|x| x.name.as_str())
                .unwrap_or_default()
        );
        vpc
    })
}

async fn handle_overlay_segment_creation(
    api_client: &ApiClient,
    overlay_networks: &[Ipv4Network],
) -> CarbideCliResult<()> {
    // Overlay network creation request is received.
    // For each overlay_segment, create new segment if not exists.
    let vpc = get_or_create_vpc(api_client).await?;
    for network in overlay_networks {
        let name = format!("devenv_tenant_{network}");
        let network_segment = api_client
            .get_all_segments(None, Some(name.clone()), 2)
            .await?;

        if let Some(ns) = network_segment.network_segments.first() {
            let ns_name = ns
                .metadata
                .as_ref()
                .map(|m| m.name.as_str())
                .unwrap_or_default();

            let prefix_str = ns
                .config
                .as_ref()
                .and_then(|c| c.prefixes.first())
                .map_or("unknown", |p| p.prefix.as_str());

            println!(
                "Found network segment id: {}, name: {} for prefix: {}",
                ns.id.unwrap_or_default(),
                ns_name,
                prefix_str,
            );

            continue;
        }

        let ns_id: NetworkSegmentId = uuid::Uuid::new_v4().into();

        let ns = api_client
            .create_network_segment(
                ns_id,
                vpc.id,
                name,
                network.to_string(),
                network.nth(1).map(|x| x.to_string()),
            )
            .await?;

        let ns_name = ns
            .metadata
            .as_ref()
            .map(|m| m.name.as_str())
            .unwrap_or_default();

        let prefix_str = ns
            .config
            .as_ref()
            .and_then(|c| c.prefixes.first())
            .map_or("unknown", |p| p.prefix.as_str());

        println!(
            "Created network segment id: {}, name: {} for prefix: {}",
            ns.id.unwrap_or_default(),
            ns_name,
            prefix_str,
        );
    }

    Ok(())
}

async fn handle_devenv_config(
    mode: NetworkChoice,
    config: DevEnvFileConfig,
    api_client: &ApiClient,
) -> eyre::Result<()> {
    match mode {
        NetworkChoice::NetworkSegment => {
            if config.overlay_networks.is_empty() {
                eyre::bail!(
                    "mode network-segment selected but overlay_networks is empty in the config"
                );
            }
            handle_overlay_segment_creation(api_client, &config.overlay_networks).await?;
        }
        NetworkChoice::VpcPrefix => {
            if config.overlay_networks.is_empty() {
                eyre::bail!("mode vpc-prefix selected but overlay_networks is empty in the config");
            }
            handle_overlay_vpc_prefix_creation(api_client, &config.overlay_networks).await?;
        }
        NetworkChoice::HostInbandSegment => {
            if config.host_inband_networks.is_empty() {
                eyre::bail!(
                    "mode host-inband-segment selected but host_inband_networks is empty in the config"
                );
            }
            handle_host_inband_segment_creation(api_client, &config.host_inband_networks).await?;
        }
    }
    Ok(())
}

async fn get_or_create_flat_vpc(api_client: &ApiClient) -> CarbideCliResult<Vpc> {
    let vpcs = api_client.get_vpc_by_name(DEVENV_FLAT_VPC_NAME).await?;
    if let Some(vpc) = vpcs.vpcs.first().cloned() {
        return Ok(vpc);
    }
    let vpc_id = uuid::Uuid::new_v4().into();
    let vpc = api_client
        .create_flat_vpc(DEVENV_FLAT_VPC_NAME, vpc_id)
        .await?;
    println!(
        "Created Flat VPC with ID: {}, name: {}",
        vpc.id.unwrap_or_default(),
        vpc.metadata
            .as_ref()
            .map(|x| x.name.as_str())
            .unwrap_or_default()
    );
    Ok(vpc)
}

async fn handle_host_inband_segment_creation(
    api_client: &ApiClient,
    networks: &[HostInbandNetwork],
) -> CarbideCliResult<()> {
    let vpc = get_or_create_flat_vpc(api_client).await?;
    let vpc_id = vpc.id.ok_or_else(|| {
        CarbideCliError::GenericError("Flat VPC missing id after creation".to_string())
    })?;

    for net in networks {
        if net.reserve_first < 0 {
            return Err(CarbideCliError::GenericError(format!(
                "reserve_first must be >= 0 for host_inband network {}",
                net.prefix
            )));
        }
        let prefix = net.prefix;
        let name = format!("devenv_host_inband_{prefix}");
        let existing = api_client
            .get_all_segments(None, Some(name.clone()), 2)
            .await?;

        if let Some(ns) = existing.network_segments.first() {
            println!(
                "Found HostInband segment id: {}, name: {} for prefix: {}",
                ns.id.unwrap_or_default(),
                name,
                prefix,
            );
            continue;
        }

        let ns_id: NetworkSegmentId = uuid::Uuid::new_v4().into();
        let ns = api_client
            .create_host_inband_segment(
                ns_id,
                vpc_id,
                name.clone(),
                prefix.to_string(),
                prefix.nth(1).map(|x| x.to_string()),
                net.reserve_first,
            )
            .await?;

        println!(
            "Created HostInband segment id: {}, name: {} for prefix: {}",
            ns.id.unwrap_or_default(),
            name,
            prefix,
        );
    }
    Ok(())
}

async fn handle_overlay_vpc_prefix_creation(
    api_client: &ApiClient,
    overlay_networks: &[Ipv4Network],
) -> CarbideCliResult<()> {
    let vpc = get_or_create_vpc(api_client).await?;
    for network in overlay_networks {
        let vpc_prefix_name = format!("overlay_prefix_{network}");
        let query = VpcPrefixSearchQuery {
            vpc_id: vpc.id,
            tenant_prefix_id: None,
            site_prefix_id: None,
            name: Some(vpc_prefix_name.clone()),
            prefix_match: Some(network.to_string()),
            prefix_match_type: Some(PrefixMatchType::PrefixExact as i32),
            deleted: DeletedFilter::Exclude as i32,
        };
        let vpc_prefix_ids = api_client
            .0
            .search_vpc_prefixes(query)
            .await
            .map(|response| response.vpc_prefix_ids)?;

        if let Some(prefix) = vpc_prefix_ids.first() {
            // We found prefix with same config.
            println!("Vpc Prefix {prefix}, name: {vpc_prefix_name} for {network} already exists.");
            continue;
        }

        let new_prefix = VpcPrefixCreationRequest {
            id: Some(uuid::Uuid::new_v4().into()),
            prefix: String::new(),
            vpc_id: vpc.id,
            site_prefix_id: None,
            config: Some(rpc::forge::VpcPrefixConfig {
                prefix: network.to_string(),
            }),
            metadata: Some(rpc::forge::Metadata {
                name: vpc_prefix_name,
                description: "Vpc prefix created for overlay network by dev environment setup"
                    .to_string(),
                ..Default::default()
            }),
        };
        let vpc_prefix = api_client.0.create_vpc_prefix(new_prefix).await?;
        let vpc_prefix_name = vpc_prefix
            .metadata
            .as_ref()
            .map(|x| x.name.as_str())
            .unwrap_or("");
        println!(
            "Created Vpc prefix {}, name: {} for network {network}.",
            vpc_prefix.id.unwrap_or_default(),
            vpc_prefix_name
        );
    }
    Ok(())
}

pub(super) async fn apply_devenv_config(
    config: Args,
    api_client: &ApiClient,
) -> Result<(), CarbideCliError> {
    // Read config file.
    if !std::fs::exists(&config.path)? {
        return Err(CarbideCliError::GenericError(
            "Config file does not exists.".to_string(),
        ));
    }

    if let Ok(file_str) = tokio::fs::read_to_string(config.path).await {
        match toml::from_str::<DevEnvFileConfig>(file_str.as_str()) {
            Ok(devenv_config) => {
                if let Err(err) = handle_devenv_config(config.mode, devenv_config, api_client).await
                {
                    tracing::error!("Unable to apply devenv config, error: {err}");
                } else {
                    tracing::info!("Successfully updated dev env config.");
                }
            }
            Err(err) => {
                tracing::error!(
                    "Unable to parse devconfig file, nothing was written to db. Error: {err}; String read: {file_str}"
                );
            }
        }
    } else {
        tracing::info!("No devenv config file found.");
    }
    Ok(())
}

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

use std::collections::HashMap;
use std::sync::Arc;

use askama::Template;
use axum::Json;
use axum::extract::{Path as AxumPath, State as AxumState};
use axum::response::{Html, IntoResponse, Response};
use carbide_api_core::Api;
use carbide_uuid::network::NetworkSegmentId;
use carbide_uuid::vpc::VpcId;
use forgerpc::NetworkSegment;
use hyper::http::StatusCode;
use rpc::forge as forgerpc;
use rpc::forge::forge_server::Forge;

use super::{Base, filters};

#[derive(Template)]
#[template(path = "instance_show.html")]
struct InstanceShow {
    instances: Vec<InstanceDisplay>,
}

struct InstanceDisplay {
    id: String,
    machine_id: String,
    tenant_org: String,
    tenant_state: String,
    configs_synced: String,
    metadata: rpc::forge::Metadata,
    ip_addresses: String,
    num_eth_ifs: usize,
    num_ib_ifs: usize,
    num_keysets: usize,
    num_nvlink_gpus: usize,
}

impl From<forgerpc::Instance> for InstanceDisplay {
    fn from(instance: forgerpc::Instance) -> Self {
        let tenant_org = instance
            .config
            .as_ref()
            .and_then(|config| config.tenant.as_ref())
            .map(|tenant| tenant.tenant_organization_id.clone())
            .unwrap_or_default();

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

        let num_eth_ifs = instance
            .config
            .as_ref()
            .and_then(|config| config.network.as_ref())
            .map(|network| network.interfaces.len())
            .unwrap_or_default();
        let num_ib_ifs = instance
            .config
            .as_ref()
            .and_then(|config| config.infiniband.as_ref())
            .map(|ib| ib.ib_interfaces.len())
            .unwrap_or_default();
        let num_keysets = instance
            .config
            .as_ref()
            .and_then(|config| config.tenant.as_ref())
            .map(|tenant: &rpc::forge::TenantConfig| tenant.tenant_keyset_ids.len())
            .unwrap_or_default();
        let num_nvlink_gpus = instance
            .config
            .as_ref()
            .and_then(|config| config.nvlink.as_ref())
            .map(|nvl| nvl.gpu_configs.len())
            .unwrap_or_default();

        Self {
            id: instance.id.unwrap_or_default().to_string(),
            metadata: instance.metadata.unwrap_or_default(),
            machine_id: instance
                .machine_id
                .map(|id| id.to_string())
                .unwrap_or_else(super::invalid_machine_id),
            tenant_org,
            tenant_state,
            configs_synced,
            ip_addresses: instance_addresses.join(","),
            num_eth_ifs,
            num_ib_ifs,
            num_keysets,
            num_nvlink_gpus,
        }
    }
}

/// List instances
pub(super) async fn show_html(AxumState(state): AxumState<Arc<Api>>) -> Response {
    let out = match fetch_instances(state).await {
        Ok(m) => m,
        Err(err) => {
            tracing::error!(error = %err, "fetch_instances");
            return (StatusCode::INTERNAL_SERVER_ERROR, "Error loading instances").into_response();
        }
    };

    let instances: Vec<InstanceDisplay> = out.instances.into_iter().map(Into::into).collect();
    let tmpl = InstanceShow { instances };
    (StatusCode::OK, Html(tmpl.render().unwrap())).into_response()
}

pub(super) async fn show_all_json(AxumState(state): AxumState<Arc<Api>>) -> Response {
    let out = match fetch_instances(state).await {
        Ok(m) => m,
        Err(err) => {
            tracing::error!(error = %err, "fetch_instances");
            return (StatusCode::INTERNAL_SERVER_ERROR, "Error loading instances").into_response();
        }
    };
    (StatusCode::OK, Json(out)).into_response()
}

async fn fetch_instances(api: Arc<Api>) -> Result<forgerpc::InstanceList, tonic::Status> {
    let request = tonic::Request::new(forgerpc::InstanceSearchFilter::default());

    let instance_ids = api
        .find_instance_ids(request)
        .await?
        .into_inner()
        .instance_ids;

    let mut instances = Vec::new();
    let mut offset = 0;
    while offset != instance_ids.len() {
        const PAGE_SIZE: usize = 100;
        let page_size = PAGE_SIZE.min(instance_ids.len() - offset);
        let next_ids = &instance_ids[offset..offset + page_size];
        let request = tonic::Request::new(forgerpc::InstancesByIdsRequest {
            instance_ids: next_ids.to_vec(),
        });
        let next_instances = api.find_instances_by_ids(request).await?.into_inner();

        instances.extend(next_instances.instances);
        offset += page_size;
    }

    instances.sort_unstable_by(|i1, i2| {
        // Order by name first, and ID second
        let ord = i1
            .metadata
            .as_ref()
            .map(|m| m.name.as_str())
            .unwrap_or_default()
            .cmp(
                i2.metadata
                    .as_ref()
                    .map(|m| m.name.as_str())
                    .unwrap_or_default(),
            );
        if !ord.is_eq() {
            return ord;
        }

        i1.id
            .as_ref()
            .map(|id| id.to_string())
            .cmp(&i2.id.as_ref().map(|id| id.to_string()))
    });

    Ok(forgerpc::InstanceList { instances })
}

#[derive(Template)]
#[template(path = "instance_detail.html")]
struct InstanceDetail {
    id: String,
    machine_id: String,
    tenant_org: String,
    tenant_state: String,
    tenant_state_details: String,
    configs_synced: String,
    network_config_synced: String,
    network_config_version: String,
    ib_config_synced: String,
    ib_config_version: String,
    config_version: String,
    interfaces: Vec<InstanceInterface>,
    ib_interfaces: Vec<InstanceIbInterface>,
    os: InstanceOs,
    keysets: Vec<String>,
    nvlink_gpus: Vec<InstanceNvLinkGpu>,
    nvlink_config_synced: String,
    nvlink_config_version: String,
    metadata: rpc::forge::Metadata,
}

#[derive(Default)]
struct InstanceOs {
    os_id: String,
    ipxe_script: String,
    userdata: String,
    run_provisioning_instructions_on_every_boot: bool,
    phone_home_enabled: bool,
}

struct InstanceInterface {
    function_type: String,
    vf_id: String,
    segment_id: String,
    mac_address: String,
    addresses: String,
    prefixes: String,
    gateways: String,
    vpc_id: String,
    vpc_name: String,
}

struct InstanceIbInterface {
    device: String,
    vendor: String,
    device_instance: u32,
    function_type: String,
    vf_id: String,
    ib_partition_id: String,

    pf_guid: String,
    guid: String,
    lid: u32,
}

struct InstanceNvLinkGpu {
    device_instance: u32,
    device_guid: String,
    logical_partition_id: String,
}

impl From<forgerpc::Instance> for InstanceDetail {
    fn from(instance: forgerpc::Instance) -> Self {
        let interfaces = Vec::new();

        let mut ib_interfaces = Vec::new();
        let ib_if_configs = instance
            .config
            .as_ref()
            .and_then(|config| config.infiniband.as_ref())
            .map(|config| config.ib_interfaces.as_slice())
            .unwrap_or_default();
        let ib_if_status = instance
            .status
            .as_ref()
            .and_then(|status| status.infiniband.as_ref())
            .map(|status: &rpc::InstanceInfinibandStatus| status.ib_interfaces.as_slice())
            .unwrap_or_default();
        if ib_if_configs.len() == ib_if_status.len() {
            for (i, config) in ib_if_configs.iter().enumerate() {
                let status = &ib_if_status[i];
                ib_interfaces.push(InstanceIbInterface {
                    device: config.device.clone(),
                    vendor: config.vendor.clone().unwrap_or_default(),
                    device_instance: config.device_instance,
                    function_type: forgerpc::InterfaceFunctionType::try_from(config.function_type)
                        .ok()
                        .map(|ty| format!("{ty:?}"))
                        .unwrap_or_else(|| "INVALID".to_string()),
                    vf_id: config
                        .virtual_function_id
                        .map(|id| id.to_string())
                        .unwrap_or_default(),
                    ib_partition_id: config
                        .ib_partition_id
                        .as_ref()
                        .map(|id| id.to_string())
                        .unwrap_or_default(),
                    pf_guid: status.pf_guid.clone().unwrap_or_default(),
                    guid: status.guid.clone().unwrap_or_default(),
                    lid: status.lid,
                })
            }
        }

        let mut nvlink_gpus = Vec::new();
        let nvlink_configs = instance
            .config
            .as_ref()
            .and_then(|config| config.nvlink.as_ref())
            .map(|config| config.gpu_configs.as_slice())
            .unwrap_or_default();
        let nvlink_status = instance
            .status
            .as_ref()
            .and_then(|status| status.nvlink.as_ref())
            .map(|status: &rpc::InstanceNvLinkStatus| status.gpu_statuses.as_slice())
            .unwrap_or_default();

        if nvlink_configs.len() == nvlink_status.len() {
            for (i, config) in nvlink_configs.iter().enumerate() {
                let status = &nvlink_status[i];
                nvlink_gpus.push(InstanceNvLinkGpu {
                    device_instance: config.device_instance,
                    device_guid: status.gpu_guid.clone().unwrap_or_default(),
                    logical_partition_id: config
                        .logical_partition_id
                        .unwrap_or_default()
                        .to_string(),
                })
            }
        }

        let os = instance
            .config
            .as_ref()
            .and_then(|config| config.os.as_ref())
            .map(|os| match &os.variant {
                Some(os_variant) => match os_variant {
                    forgerpc::instance_operating_system_config::Variant::Ipxe(ipxe) => InstanceOs {
                        ipxe_script: ipxe.ipxe_script.clone(),
                        userdata: os.user_data.clone().unwrap_or_default(),
                        run_provisioning_instructions_on_every_boot: os
                            .run_provisioning_instructions_on_every_boot,
                        phone_home_enabled: os.phone_home_enabled,
                        ..Default::default()
                    },
                    forgerpc::instance_operating_system_config::Variant::OsImageId(_id) => {
                        InstanceOs {
                            userdata: os.user_data.clone().unwrap_or_default(),
                            run_provisioning_instructions_on_every_boot: os
                                .run_provisioning_instructions_on_every_boot,
                            phone_home_enabled: os.phone_home_enabled,
                            ..Default::default()
                        }
                    }
                    forgerpc::instance_operating_system_config::Variant::OperatingSystemId(id) => {
                        InstanceOs {
                            os_id: id.to_string(),
                            userdata: os.user_data.clone().unwrap_or_default(),
                            run_provisioning_instructions_on_every_boot: os
                                .run_provisioning_instructions_on_every_boot,
                            phone_home_enabled: os.phone_home_enabled,
                            ..Default::default()
                        }
                    }
                },
                None => InstanceOs::default(),
            })
            .unwrap_or_default();

        let keysets = instance
            .config
            .as_ref()
            .and_then(|config| config.tenant.as_ref())
            .map(|tenant| tenant.tenant_keyset_ids.clone())
            .unwrap_or_default();

        Self {
            id: instance.id.map(|id| id.to_string()).unwrap_or_default(),
            machine_id: instance
                .machine_id
                .map(|id| id.to_string())
                .unwrap_or_default(),
            tenant_org: instance
                .config
                .as_ref()
                .and_then(|config| config.tenant.as_ref())
                .map(|tenant| tenant.tenant_organization_id.clone())
                .unwrap_or_default(),
            tenant_state: instance
                .status
                .as_ref()
                .and_then(|status| status.tenant.as_ref())
                .and_then(|tenant| forgerpc::TenantState::try_from(tenant.state).ok())
                .map(|state| format!("{state:?}"))
                .unwrap_or_default(),
            tenant_state_details: instance
                .status
                .as_ref()
                .and_then(|status| status.tenant.as_ref())
                .map(|tenant| tenant.state_details.clone())
                .unwrap_or_default(),
            configs_synced: instance
                .status
                .as_ref()
                .and_then(|status| forgerpc::SyncState::try_from(status.configs_synced).ok())
                .map(|state| format!("{state:?}"))
                .unwrap_or_default(),
            network_config_synced: instance
                .status
                .as_ref()
                .and_then(|status| status.network.as_ref())
                .and_then(|status| forgerpc::SyncState::try_from(status.configs_synced).ok())
                .map(|state| format!("{state:?}"))
                .unwrap_or_default(),
            ib_config_synced: instance
                .status
                .as_ref()
                .and_then(|status| status.infiniband.as_ref())
                .and_then(|status| forgerpc::SyncState::try_from(status.configs_synced).ok())
                .map(|state| format!("{state:?}"))
                .unwrap_or_default(),
            metadata: instance.metadata.unwrap_or_default(),
            network_config_version: instance.network_config_version,
            config_version: instance.config_version,
            ib_config_version: instance.ib_config_version,
            os,
            interfaces,
            ib_interfaces,
            keysets,
            nvlink_gpus,
            nvlink_config_synced: instance
                .status
                .as_ref()
                .and_then(|status| status.nvlink.as_ref())
                .and_then(|status| forgerpc::SyncState::try_from(status.configs_synced).ok())
                .map(|state| format!("{state:?}"))
                .unwrap_or_default(),
            nvlink_config_version: instance.nvlink_config_version,
        }
    }
}

async fn get_network_segments_map_for_instance(
    state: &Api,
    if_configs: &[forgerpc::InstanceInterfaceConfig],
) -> Result<HashMap<NetworkSegmentId, NetworkSegment>, tonic::Status> {
    let network_segment_ids: Vec<NetworkSegmentId> = if_configs
        .iter()
        .filter_map(|iface| iface.network_segment_id)
        .collect();

    let ns_req = tonic::Request::new(forgerpc::NetworkSegmentsByIdsRequest {
        network_segments_ids: network_segment_ids,
        include_history: false,
        include_num_free_ips: false,
    });

    let network_segments = state
        .find_network_segments_by_ids(ns_req)
        .await?
        .into_inner()
        .network_segments;

    let network_segments_map: HashMap<NetworkSegmentId, NetworkSegment> = network_segments
        .into_iter()
        .filter_map(|ns| ns.id.map(|id| (id, ns)))
        .collect();

    Ok(network_segments_map)
}

async fn get_vpc_map_for_instance(
    state: &Api,
    vpc_ids: Vec<VpcId>,
) -> Result<HashMap<VpcId, forgerpc::Vpc>, tonic::Status> {
    let vpc_req = tonic::Request::new(forgerpc::VpcsByIdsRequest { vpc_ids });

    let vpcs = state.find_vpcs_by_ids(vpc_req).await?.into_inner().vpcs;

    let vpc_map: HashMap<VpcId, forgerpc::Vpc> = vpcs
        .into_iter()
        .filter_map(|vpc| vpc.id.map(|id| (id, vpc)))
        .collect();

    Ok(vpc_map)
}

async fn get_interfaces_for_instance_detail(
    state: &Api,
    instance: &forgerpc::Instance,
) -> Result<Vec<InstanceInterface>, tonic::Status> {
    let network_config = instance
        .config
        .as_ref()
        .and_then(|config| config.network.as_ref());
    let if_configs = network_config
        .map(|config| config.interfaces.as_slice())
        .unwrap_or_default();
    let is_auto_network = network_config.is_some_and(|config| config.auto_config.is_some());
    let if_status = instance
        .status
        .as_ref()
        .and_then(|status| status.network.as_ref())
        .map(|status| status.interfaces.as_slice())
        .unwrap_or_default();

    if !is_auto_network && if_configs.len() != if_status.len() {
        return Ok(Vec::new());
    }

    let network_segments_map = if is_auto_network {
        HashMap::new()
    } else {
        get_network_segments_map_for_instance(state, if_configs).await?
    };

    let vpc_ids = if is_auto_network {
        if_status
            .iter()
            .filter_map(|status| status.vpc_id)
            .collect()
    } else {
        network_segments_map
            .values()
            .filter_map(|segment| segment.config.as_ref().and_then(|config| config.vpc_id))
            .collect()
    };

    let vpc_map = get_vpc_map_for_instance(state, vpc_ids).await?;

    let interfaces = if_status
        .iter()
        .enumerate()
        .map(|(index, status)| {
            let interface = if_configs.get(index);
            let function_type = match interface {
                Some(interface) => {
                    forgerpc::InterfaceFunctionType::try_from(interface.function_type)
                        .ok()
                        .map(|interface_type| format!("{interface_type:?}"))
                        .unwrap_or_else(|| "INVALID".to_string())
                }
                None => match status.virtual_function_id {
                    Some(_) => format!("{:?}", forgerpc::InterfaceFunctionType::Virtual),
                    None => format!("{:?}", forgerpc::InterfaceFunctionType::Physical),
                },
            };
            let vpc_id = if is_auto_network {
                status.vpc_id
            } else {
                interface
                    .and_then(|interface| interface.network_segment_id)
                    .and_then(|network_segment_id| network_segments_map.get(&network_segment_id))
                    .and_then(|segment| segment.config.as_ref())
                    .and_then(|config| config.vpc_id)
            };
            let vpc = vpc_id.and_then(|vpc_id| vpc_map.get(&vpc_id));
            let mac_address = status
                .mac_address
                .clone()
                .unwrap_or("<unknown>".to_string());
            InstanceInterface {
                function_type,
                vf_id: status
                    .virtual_function_id
                    .map(|id| id.to_string())
                    .unwrap_or_default(),
                segment_id: interface
                    .map(|interface| interface.network_segment_id.unwrap_or_default().to_string())
                    .unwrap_or_default(),
                mac_address,
                addresses: status.addresses.join(", "),
                prefixes: status.prefixes.join(", "),
                gateways: status.gateways.join(", "),
                vpc_id: vpc
                    .and_then(|vpc| vpc.id)
                    .map(|id| id.to_string())
                    .unwrap_or_default(),
                vpc_name: vpc
                    .map(|vpc| {
                        vpc.metadata
                            .as_ref()
                            .map(|metadata| metadata.name.as_str())
                            .unwrap_or("<no name>")
                    })
                    .unwrap_or_default()
                    .to_string(),
            }
        })
        .collect();

    Ok(interfaces)
}

/// View instance
pub(super) async fn detail(
    AxumState(state): AxumState<Arc<Api>>,
    AxumPath(instance_id): AxumPath<String>,
) -> Response {
    let (show_json, instance_id_string) = match instance_id.strip_suffix(".json") {
        Some(instance_id) => (true, instance_id.to_string()),
        None => (false, instance_id),
    };

    let instance_id = match instance_id_string.parse() {
        Ok(id) => id,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                format!("Invalid Instance ID {instance_id_string}: {e}"),
            )
                .into_response();
        }
    };

    let request = tonic::Request::new(forgerpc::InstancesByIdsRequest {
        instance_ids: vec![instance_id],
    });
    let instance = match state
        .find_instances_by_ids(request)
        .await
        .map(|response| response.into_inner())
    {
        Ok(x) if x.instances.is_empty() => {
            return super::not_found_response(instance_id_string);
        }
        Ok(x) if x.instances.len() != 1 => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!(
                    "Instance list for {instance_id} returned {} instances",
                    x.instances.len()
                ),
            )
                .into_response();
        }
        Ok(mut x) => x.instances.remove(0),
        Err(err) if err.code() == tonic::Code::NotFound => {
            return super::not_found_response(instance_id_string);
        }
        Err(err) => {
            tracing::error!(error = %err, %instance_id, "find_instances");
            return (StatusCode::INTERNAL_SERVER_ERROR, "Error loading instances").into_response();
        }
    };

    if show_json {
        return (StatusCode::OK, Json(instance)).into_response();
    }

    let instance_detail_interfaces = get_interfaces_for_instance_detail(&state, &instance)
        .await
        .unwrap_or_else(|_| Vec::new());
    let mut instance_detail: InstanceDetail = instance.into();
    instance_detail.interfaces = instance_detail_interfaces;
    (StatusCode::OK, Html(instance_detail.render().unwrap())).into_response()
}

impl super::Base for InstanceShow {}
impl super::Base for InstanceDetail {}

#[cfg(test)]
mod tests {
    use carbide_test_harness::TestHarness;

    use super::*;

    #[crate::sqlx_test]
    async fn auto_network_instance_detail_uses_status_interfaces(
        pool: sqlx::PgPool,
    ) -> eyre::Result<()> {
        let test_harness = TestHarness::builder(pool).build().await;
        let vpc_id = test_harness
            .network_controller()
            .create_vpc("auto network instance detail")
            .await;
        let instance = forgerpc::Instance {
            config: Some(forgerpc::InstanceConfig {
                network: Some(forgerpc::InstanceNetworkConfig {
                    interfaces: Vec::new(),
                    auto_config: Some(forgerpc::InstanceNetworkAutoConfig {
                        vpc_id: Some(vpc_id),
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            status: Some(forgerpc::InstanceStatus {
                network: Some(forgerpc::InstanceNetworkStatus {
                    interfaces: vec![
                        forgerpc::InstanceInterfaceStatus {
                            mac_address: Some("02:00:00:00:00:01".to_string()),
                            addresses: vec!["192.0.2.10".to_string(), "2001:db8::10".to_string()],
                            prefixes: vec!["192.0.2.0/24".to_string(), "2001:db8::/64".to_string()],
                            gateways: vec![
                                "192.0.2.1/24".to_string(),
                                "2001:db8::1/64".to_string(),
                            ],
                            vpc_id: Some(vpc_id),
                            ..Default::default()
                        },
                        forgerpc::InstanceInterfaceStatus {
                            virtual_function_id: Some(7),
                            mac_address: Some("02:00:00:00:00:02".to_string()),
                            addresses: vec!["2001:db8::20".to_string()],
                            gateways: vec!["2001:db8::1/64".to_string()],
                            vpc_id: Some(vpc_id),
                            ..Default::default()
                        },
                        forgerpc::InstanceInterfaceStatus {
                            prefixes: vec!["2001:db8:1::/64".to_string()],
                            vpc_id: Some(vpc_id),
                            ..Default::default()
                        },
                    ],
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        };

        let interfaces = get_interfaces_for_instance_detail(test_harness.api(), &instance).await?;

        assert_eq!(interfaces.len(), 3);
        let physical_interface = &interfaces[0];
        assert_eq!(physical_interface.function_type, "Physical");
        assert!(physical_interface.segment_id.is_empty());
        assert_eq!(physical_interface.mac_address, "02:00:00:00:00:01");
        assert_eq!(physical_interface.addresses, "192.0.2.10, 2001:db8::10");
        assert_eq!(physical_interface.prefixes, "192.0.2.0/24, 2001:db8::/64");
        assert_eq!(physical_interface.gateways, "192.0.2.1/24, 2001:db8::1/64");
        assert_eq!(physical_interface.vpc_id, vpc_id.to_string());
        assert_eq!(physical_interface.vpc_name, "auto network instance detail");

        let virtual_interface = &interfaces[1];
        assert_eq!(virtual_interface.function_type, "Virtual");
        assert_eq!(virtual_interface.vf_id, "7");
        assert!(virtual_interface.segment_id.is_empty());
        assert_eq!(virtual_interface.mac_address, "02:00:00:00:00:02");
        assert_eq!(virtual_interface.addresses, "2001:db8::20");
        assert!(virtual_interface.prefixes.is_empty());
        assert_eq!(virtual_interface.gateways, "2001:db8::1/64");
        assert_eq!(virtual_interface.vpc_id, vpc_id.to_string());
        assert_eq!(virtual_interface.vpc_name, "auto network instance detail");

        let mut detail: InstanceDetail = instance.into();
        detail.interfaces = interfaces;
        let html = detail.render()?;
        assert!(html.contains("192.0.2.10, 2001:db8::10"));
        assert!(html.contains("2001:db8::20"));
        assert!(html.contains("<th>IP Prefixes</th><td>192.0.2.0/24, 2001:db8::/64</td>"));
        assert!(html.contains("<th>IP Prefixes</th><td></td>"));
        assert!(html.contains("<th>IP Prefixes</th><td>2001:db8:1::/64</td>"));
        assert!(html.contains("<th>IP Addresses</th><td></td>"));
        assert!(html.contains(&format!("/admin/vpc/{vpc_id}")));
        assert!(!html.contains("href=\"/admin/network-segment/\""));

        Ok(())
    }
}

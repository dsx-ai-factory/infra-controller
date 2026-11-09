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

use carbide_api_core::cfg::file::VmaasConfig;
use carbide_api_core::test_support::default_config;
use carbide_api_core::test_support::network_segment::FIXTURE_TENANT_NETWORK_SEGMENT_GATEWAYS;
use carbide_test_harness::prelude::*;
use carbide_uuid::machine::StableHostMachineId;
use carbide_uuid::network::NetworkSegmentId;
use rpc::forge::{InstanceAllocationRequest, InstanceConfig};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use tonic::IntoRequest;

use super::instance_common::{
    InstanceEnv, default_os_config, default_tenant_config, single_interface_network_config,
    single_interface_network_config_with_vfs,
};

async fn create_vf_segment(env: &InstanceEnv) -> NetworkSegmentId {
    let vpc_id = db::vpc::find_by_segment(&env.harness.api().database_connection, env.segment_id)
        .await
        .unwrap()
        .unwrap()
        .id;
    let network = FIXTURE_TENANT_NETWORK_SEGMENT_GATEWAYS[1];
    let segment = env
        .harness
        .api()
        .create_network_segment(
            rpc::forge::NetworkSegmentCreationRequest {
                id: None,
                mtu: Some(1500),
                name: "TENANT".to_string(),
                prefixes: vec![rpc::forge::NetworkPrefix {
                    id: None,
                    prefix: ipnetwork::IpNetwork::new(network.network(), network.prefix())
                        .unwrap()
                        .to_string(),
                    gateway: Some(network.ip().to_string()),
                    reserve_first: 3,
                    free_ip_count: 0,
                    svi_ip: None,
                    free_ip_count_v2: None,
                    free_ip_count_saturated: false,
                    dhcpv6_link_address: None,
                }],
                subdomain_id: Some(env.domain.id),
                vpc_id: Some(vpc_id),
                segment_type: rpc::forge::NetworkSegmentType::Tenant as i32,
                infer_slaac_eui64_addresses: false,
            }
            .into_request(),
        )
        .await
        .unwrap()
        .into_inner();
    // Get the tenant segment into ready state, as in the shared network fixtures.
    let network_controller = env.harness.network_controller();
    network_controller.run_single_iteration().await;
    network_controller.run_single_iteration().await;
    segment.id.unwrap()
}

#[sqlx_test]
async fn test_instance_with_vf_when_vf_disabled(_: PgPoolOptions, options: PgConnectOptions) {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let mut config = default_config::get();
    config.vmaas_config = Some(VmaasConfig {
        allow_instance_vf: false,
        hbn_reps: None,
        bridging: None,
    });

    let env = InstanceEnv::new(pool, config).await;
    let mh = env.create_managed_host().await;
    let segment_id = vec![env.segment_id, create_vf_segment(&env).await];

    // Create instance configuration
    let config = InstanceConfig::builder()
        .tenant(default_tenant_config())
        .os(default_os_config())
        .network(single_interface_network_config_with_vfs(segment_id));

    // Allocate an instance
    let instance_result = env
        .harness
        .api()
        .allocate_instance(
            InstanceAllocationRequest::builder(false)
                .machine_id(StableHostMachineId::try_from(mh.host.id).unwrap())
                .config(config)
                .metadata(rpc::Metadata {
                    name: "test-disabled-vf".to_string(),
                    description: "Testing instance creation when vf disabled".to_string(),
                    labels: Vec::new(),
                })
                .tonic_request(),
        )
        .await;

    assert!(instance_result.is_err());
}

#[sqlx_test]
async fn test_instance_without_vf_when_vf_disabled(_: PgPoolOptions, options: PgConnectOptions) {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let mut config = default_config::get();
    config.vmaas_config = Some(VmaasConfig {
        allow_instance_vf: false,
        hbn_reps: None,
        bridging: None,
    });

    let env = InstanceEnv::new(pool, config).await;
    let mh = env.create_managed_host().await;
    let segment_id = env.segment_id;

    // Create instance configuration
    let config = InstanceConfig::builder()
        .tenant(default_tenant_config())
        .os(default_os_config())
        .network(single_interface_network_config(segment_id));

    // Allocate an instance
    let instance_result = env
        .harness
        .api()
        .allocate_instance(
            InstanceAllocationRequest::builder(false)
                .machine_id(StableHostMachineId::try_from(mh.host.id).unwrap())
                .config(config)
                .metadata(rpc::Metadata {
                    name: "test-disabled-vf".to_string(),
                    description: "Testing instance creation when vf disabled".to_string(),
                    labels: Vec::new(),
                })
                .tonic_request(),
        )
        .await;

    assert!(instance_result.is_ok());
}

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

use std::sync::Arc;

use carbide_api_core::test_support::network_segment::{
    FIXTURE_ADMIN_NETWORK_SEGMENT_GATEWAY, FIXTURE_HOST_INBAND_NETWORK_SEGMENT_GATEWAY,
    FIXTURE_TENANT_NETWORK_SEGMENT_GATEWAYS, FIXTURE_TENANT_ORG_ID,
    FIXTURE_UNDERLAY_NETWORK_SEGMENT_GATEWAY,
};
use carbide_api_core::test_support::rpc::forge::forge_server::Forge;
use carbide_network_segment_controller::context::NetworkSegmentStateHandlerServices;
use carbide_network_segment_controller::handler::NetworkSegmentStateHandler;
use carbide_network_segment_controller::io::NetworkSegmentStateControllerIO;
use carbide_utils::test_support::test_meter::TestMeter;
use carbide_uuid::vpc::VpcId;
use ipnetwork::IpNetwork;
use state_controller::controller::StateController;
use tokio::sync::Mutex;

use crate::network::segment::TestNetworkSegment;
use crate::{ApiHandle, TestDomain, rpc};

pub struct TestNetworkController {
    api: Arc<ApiHandle>,
    controller: Arc<Mutex<StateController<NetworkSegmentStateControllerIO>>>,
}

impl TestNetworkController {
    pub(crate) fn new(api: Arc<ApiHandle>, processor_id: String, test_meter: &TestMeter) -> Self {
        let controller = StateController::builder()
            .database(
                api.database_connection.clone(),
                api.work_lock_manager_handle(),
            )
            .meter("carbide_machines", test_meter.meter())
            .processor_id(processor_id)
            .services(
                NetworkSegmentStateHandlerServices {
                    db_pool: api.database_connection.clone(),
                }
                .into(),
            )
            .state_handler(Arc::new(NetworkSegmentStateHandler::new(
                chrono::Duration::milliseconds(500),
                api.common_pools().ethernet.pool_vlan_id.clone(),
                api.common_pools().ethernet.pool_vni.clone(),
            )))
            .build_for_manual_iterations(api.cancel_token.clone())
            .expect("Unable to build state controller");
        Self {
            api,
            controller: Mutex::new(controller).into(),
        }
    }

    pub async fn create_underlay_segment(&self, domain: &TestDomain) -> TestNetworkSegment {
        let prefix = IpNetwork::new(
            FIXTURE_UNDERLAY_NETWORK_SEGMENT_GATEWAY.network(),
            FIXTURE_UNDERLAY_NETWORK_SEGMENT_GATEWAY.prefix(),
        )
        .unwrap()
        .to_string();
        let gateway = FIXTURE_UNDERLAY_NETWORK_SEGMENT_GATEWAY.ip();

        let request = rpc::forge::NetworkSegmentCreationRequest {
            id: None,
            mtu: Some(1500),
            name: "UNDERLAY".to_string(),
            prefixes: vec![rpc::forge::NetworkPrefix {
                id: None,
                prefix: prefix.to_string(),
                gateway: Some(gateway.to_string()),
                reserve_first: 3,
                free_ip_count: 0,
                svi_ip: None,
                free_ip_count_v2: None,
                free_ip_count_saturated: false,
            }],
            subdomain_id: Some(domain.id),
            vpc_id: None,
            segment_type: rpc::forge::NetworkSegmentType::Underlay.into(),
            infer_slaac_eui64_addresses: false,
        };

        let segment = self
            .api
            .create_network_segment(tonic::Request::new(request))
            .await
            .expect("Unable to create network segment")
            .into_inner();

        self.run_single_iteration().await;
        self.run_single_iteration().await;

        TestNetworkSegment {
            id: segment
                .id
                .expect("Id must be returned by create_network_segment API request"),
            relay_address: gateway,
        }
    }

    pub async fn create_admin_segment(&self, domain: &TestDomain) -> TestNetworkSegment {
        let prefix = IpNetwork::new(
            FIXTURE_ADMIN_NETWORK_SEGMENT_GATEWAY.network(),
            FIXTURE_ADMIN_NETWORK_SEGMENT_GATEWAY.prefix(),
        )
        .unwrap()
        .to_string();
        let gateway = FIXTURE_ADMIN_NETWORK_SEGMENT_GATEWAY.ip();

        let request = rpc::forge::NetworkSegmentCreationRequest {
            id: None,
            mtu: Some(1500),
            name: "ADMIN".to_string(),
            prefixes: vec![rpc::forge::NetworkPrefix {
                id: None,
                prefix: prefix.to_string(),
                gateway: Some(gateway.to_string()),
                reserve_first: 3,
                free_ip_count: 0,
                svi_ip: None,
                free_ip_count_v2: None,
                free_ip_count_saturated: false,
            }],
            subdomain_id: Some(domain.id),
            vpc_id: None,
            segment_type: rpc::forge::NetworkSegmentType::Admin.into(),
            infer_slaac_eui64_addresses: false,
        };

        let segment = self
            .api
            .create_network_segment(tonic::Request::new(request))
            .await
            .expect("Unable to create network segment")
            .into_inner();

        self.run_single_iteration().await;
        self.run_single_iteration().await;

        TestNetworkSegment {
            id: segment
                .id
                .expect("Id must be returned by create_network_segment API request"),
            relay_address: gateway,
        }
    }

    pub async fn create_host_inband_segment(&self, domain: &TestDomain) -> TestNetworkSegment {
        let prefix = IpNetwork::new(
            FIXTURE_HOST_INBAND_NETWORK_SEGMENT_GATEWAY.network(),
            FIXTURE_HOST_INBAND_NETWORK_SEGMENT_GATEWAY.prefix(),
        )
        .unwrap()
        .to_string();
        let gateway = FIXTURE_HOST_INBAND_NETWORK_SEGMENT_GATEWAY.ip();

        let vpc = self
            .api
            .create_vpc(tonic::Request::new(rpc::forge::VpcCreationRequest {
                tenant_organization_id: FIXTURE_TENANT_ORG_ID.to_string(),
                network_virtualization_type: Some(rpc::forge::VpcVirtualizationType::Flat.into()),
                metadata: Some(rpc::forge::Metadata {
                    name: "HOST_INBAND_FLAT".to_string(),
                    ..Default::default()
                }),
                ..Default::default()
            }))
            .await
            .expect("Unable to create HostInband flat VPC")
            .into_inner();

        let request = rpc::forge::NetworkSegmentCreationRequest {
            id: None,
            mtu: Some(1500),
            name: "HOST_INBAND".to_string(),
            prefixes: vec![rpc::forge::NetworkPrefix {
                id: None,
                prefix: prefix.to_string(),
                gateway: Some(gateway.to_string()),
                reserve_first: 3,
                free_ip_count: 0,
                svi_ip: None,
                free_ip_count_v2: None,
                free_ip_count_saturated: false,
            }],
            subdomain_id: Some(domain.id),
            vpc_id: vpc.id,
            segment_type: rpc::forge::NetworkSegmentType::HostInband.into(),
            infer_slaac_eui64_addresses: false,
        };

        let segment = self
            .api
            .create_network_segment(tonic::Request::new(request))
            .await
            .expect("Unable to create network segment")
            .into_inner();

        self.run_single_iteration().await;
        self.run_single_iteration().await;

        TestNetworkSegment {
            id: segment
                .id
                .expect("Id must be returned by create_network_segment API request"),
            relay_address: gateway,
        }
    }

    pub async fn create_vpc(&self, name: &str) -> VpcId {
        let vpc = self
            .api
            .create_vpc(
                rpc::forge::VpcCreationRequest::builder(FIXTURE_TENANT_ORG_ID)
                    .metadata(rpc::forge::Metadata {
                        name: name.to_string(),
                        ..Default::default()
                    })
                    .tonic_request(),
            )
            .await
            .expect("Unable to create VPC")
            .into_inner();
        vpc.id.expect("Created VPC must have an id")
    }

    pub async fn create_tenant_segment(
        &self,
        domain: &TestDomain,
        vpc_id: VpcId,
    ) -> TestNetworkSegment {
        let network = FIXTURE_TENANT_NETWORK_SEGMENT_GATEWAYS[0];
        let prefix = IpNetwork::new(network.network(), network.prefix())
            .unwrap()
            .to_string();
        let gateway = network.ip();
        let request = rpc::forge::NetworkSegmentCreationRequest {
            id: None,
            mtu: Some(1500),
            name: "TENANT".to_string(),
            prefixes: vec![rpc::forge::NetworkPrefix {
                id: None,
                prefix,
                gateway: Some(gateway.to_string()),
                reserve_first: 3,
                free_ip_count: 0,
                svi_ip: None,
                free_ip_count_v2: None,
                free_ip_count_saturated: false,
            }],
            subdomain_id: Some(domain.id),
            vpc_id: Some(vpc_id),
            segment_type: rpc::forge::NetworkSegmentType::Tenant.into(),
            infer_slaac_eui64_addresses: false,
        };

        let segment = self
            .api
            .create_network_segment(tonic::Request::new(request))
            .await
            .expect("Unable to create tenant network segment")
            .into_inner();

        self.run_single_iteration().await;
        self.run_single_iteration().await;

        TestNetworkSegment {
            id: segment
                .id
                .expect("Id must be returned by create_network_segment API request"),
            relay_address: gateway,
        }
    }

    async fn run_single_iteration(&self) {
        self.controller.lock().await.run_single_iteration().await;
    }
}

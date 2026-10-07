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

// The actual instances are at the moment created via the SQL fixtures
// in the fixtures folder. This file just contains the UUID references
// for those.

use std::net::{IpAddr, Ipv4Addr};

#[cfg(test)]
use ::rpc::forge::forge_server::Forge;
use carbide_uuid::network::NetworkSegmentId;
#[cfg(test)]
use carbide_uuid::vpc::VpcId;
use ipnetwork::IpNetwork;
use lazy_static::lazy_static;

use crate::api::Api;

pub const FIXTURE_TENANT_ORG_ID: &str = "2829bbe3-c169-4cd9-8b2a-19a8b1618a93";

lazy_static! {
    pub static ref FIXTURE_UNDERLAY_NETWORK_SEGMENT_GATEWAY: IpNetwork =
        IpNetwork::new(IpAddr::V4(Ipv4Addr::new(192, 0, 1, 1)), 24).unwrap();
}

lazy_static! {
    pub static ref FIXTURE_ADMIN_NETWORK_SEGMENT_GATEWAY: IpNetwork =
        IpNetwork::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)), 24).unwrap();
}

lazy_static! {
    pub static ref FIXTURE_HOST_INBAND_NETWORK_SEGMENT_GATEWAY: IpNetwork =
        IpNetwork::new(IpAddr::V4(Ipv4Addr::new(192, 0, 3, 1)), 24).unwrap();
}

lazy_static! {
    pub static ref FIXTURE_TENANT_NETWORK_SEGMENT_GATEWAYS: [IpNetwork; 11] = [
        IpNetwork::new(IpAddr::V4(Ipv4Addr::new(192, 0, 4, 1)), 24).unwrap(),
        IpNetwork::new(IpAddr::V4(Ipv4Addr::new(192, 1, 4, 1)), 24).unwrap(),
        IpNetwork::new(IpAddr::V4(Ipv4Addr::new(192, 2, 4, 1)), 24).unwrap(),
        IpNetwork::new(IpAddr::V4(Ipv4Addr::new(192, 3, 4, 1)), 24).unwrap(),
        IpNetwork::new(IpAddr::V4(Ipv4Addr::new(192, 4, 4, 1)), 24).unwrap(),
        IpNetwork::new(IpAddr::V4(Ipv4Addr::new(192, 5, 4, 1)), 24).unwrap(),
        IpNetwork::new(IpAddr::V4(Ipv4Addr::new(192, 6, 4, 1)), 24).unwrap(),
        IpNetwork::new(IpAddr::V4(Ipv4Addr::new(192, 7, 4, 1)), 24).unwrap(),
        IpNetwork::new(IpAddr::V4(Ipv4Addr::new(192, 8, 4, 1)), 24).unwrap(),
        IpNetwork::new(IpAddr::V4(Ipv4Addr::new(192, 9, 4, 1)), 24).unwrap(),
        IpNetwork::new(IpAddr::V4(Ipv4Addr::new(192, 0, 5, 1)), 24).unwrap(),
    ];
}

lazy_static! {
    pub(crate) static ref FIXTURE_HOST_INBAND_NETWORK_SEGMENT_GATEWAY_2: IpNetwork =
        IpNetwork::new(IpAddr::V4(Ipv4Addr::new(192, 0, 6, 1)), 24).unwrap();
}

lazy_static! {
    pub(crate) static ref FIXTURE_HOST_INBAND_NETWORK_SEGMENT_GATEWAY_3: IpNetwork =
        IpNetwork::new(IpAddr::V4(Ipv4Addr::new(192, 0, 7, 1)), 24).unwrap();
}

#[cfg(test)]
pub(crate) async fn create_underlay_network_segment(api: &Api) -> NetworkSegmentId {
    let prefix = IpNetwork::new(
        FIXTURE_UNDERLAY_NETWORK_SEGMENT_GATEWAY.network(),
        FIXTURE_UNDERLAY_NETWORK_SEGMENT_GATEWAY.prefix(),
    )
    .unwrap()
    .to_string();
    let gateway = FIXTURE_UNDERLAY_NETWORK_SEGMENT_GATEWAY.ip().to_string();

    create_network_segment(
        api,
        "UNDERLAY",
        &prefix,  // 192.0.1.0/24
        &gateway, // 192.0.1.1
        rpc::forge::NetworkSegmentType::Underlay,
        None,
        true,
    )
    .await
}

pub async fn create_static_assignments_segment(
    api: &Api,
    subdomain_id: Option<carbide_uuid::domain::DomainId>,
) -> NetworkSegmentId {
    let mut txn = db::Transaction::begin(&api.database_connection)
        .await
        .unwrap();
    crate::db_init::ensure_static_assignments_segment(api, &mut txn, subdomain_id)
        .await
        .unwrap();
    let seg = db::network_segment::static_assignments(&mut txn)
        .await
        .unwrap();
    let prefixes = seg
        .prefixes
        .iter()
        .map(|prefix| prefix.prefix)
        .collect::<Vec<_>>();
    db::dns::ensure_reverse_zones(&prefixes, &mut txn)
        .await
        .unwrap();
    txn.commit().await.unwrap();
    seg.id
}

#[cfg(test)]
pub(crate) async fn create_admin_network_segment(api: &Api) -> NetworkSegmentId {
    let prefix = IpNetwork::new(
        FIXTURE_ADMIN_NETWORK_SEGMENT_GATEWAY.network(),
        FIXTURE_ADMIN_NETWORK_SEGMENT_GATEWAY.prefix(),
    )
    .unwrap()
    .to_string();
    let gateway = FIXTURE_ADMIN_NETWORK_SEGMENT_GATEWAY.ip().to_string();

    create_network_segment(
        api,
        "ADMIN",
        &prefix,  // 192.0.2.0/24
        &gateway, // 192.0.2.1
        rpc::forge::NetworkSegmentType::Admin,
        None,
        true,
    )
    .await
}

#[cfg(test)]
pub(crate) async fn create_host_inband_network_segment(
    api: &Api,
    vpc_id: Option<VpcId>,
) -> NetworkSegmentId {
    let prefix = IpNetwork::new(
        FIXTURE_HOST_INBAND_NETWORK_SEGMENT_GATEWAY.network(),
        FIXTURE_HOST_INBAND_NETWORK_SEGMENT_GATEWAY.prefix(),
    )
    .unwrap()
    .to_string();
    let gateway = FIXTURE_HOST_INBAND_NETWORK_SEGMENT_GATEWAY.ip().to_string();

    create_network_segment(
        api,
        "HOST_INBAND",
        &prefix,  // 192.0.3.0/24
        &gateway, // 192.0.3.1
        rpc::forge::NetworkSegmentType::HostInband,
        vpc_id,
        true,
    )
    .await
}

/// Creates a Flat VPC for the default test tenant and returns its id. Pass the
/// returned id to `create_host_inband_network_segment` when a test needs an
/// explicitly VPC-bound HostInband segment.
#[cfg(test)]
pub(crate) async fn create_default_flat_vpc(api: &Api, name: &str) -> VpcId {
    let request = rpc::forge::VpcCreationRequest::builder(FIXTURE_TENANT_ORG_ID)
        .metadata(rpc::forge::Metadata {
            name: name.to_string(),
            ..Default::default()
        })
        .network_virtualization_type(rpc::forge::VpcVirtualizationType::Flat as i32)
        .tonic_request();
    let vpc = api
        .create_vpc(request)
        .await
        .expect("Unable to create Flat VPC fixture")
        .into_inner();
    vpc.id.expect("Created Flat VPC must have an id")
}

#[cfg(test)]
pub(crate) async fn create_tenant_network_segment(
    api: &Api,
    vpc_id: Option<VpcId>,
    network: IpNetwork,
    name: &str,
    include_subdomain: bool,
) -> NetworkSegmentId {
    let prefix = IpNetwork::new(network.network(), network.prefix())
        .unwrap()
        .to_string();
    let gateway = network.ip().to_string();

    create_network_segment(
        api,
        name,
        &prefix,
        &gateway,
        rpc::forge::NetworkSegmentType::Tenant,
        vpc_id,
        include_subdomain,
    )
    .await
}

#[cfg(test)]
pub(crate) async fn create_network_segment(
    api: &Api,
    name: &str,
    prefix: &str,
    gateway: &str,
    segment_type: rpc::forge::NetworkSegmentType,
    vpc_id: Option<VpcId>,
    include_subdomain: bool,
) -> NetworkSegmentId {
    let subdomain_id = if include_subdomain {
        let request = ::rpc::protos::dns::DomainSearchQuery {
            id: None,
            name: Some("dwrt1.com".to_string()),
        };

        let domain = api
            .find_domain(tonic::Request::new(request))
            .await
            .expect("Unable to find a domain")
            .into_inner()
            .domains
            .first()
            .and_then(|d| d.id)
            .map(::carbide_uuid::domain::DomainId::try_from)
            .unwrap()
            .unwrap();

        Some(domain)
    } else {
        None
    };

    let request = rpc::forge::NetworkSegmentCreationRequest {
        id: None,
        mtu: Some(1500),
        name: name.to_string(),
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
        subdomain_id,
        vpc_id,
        segment_type: segment_type as _,
        infer_slaac_eui64_addresses: false,
    };

    let segment = api
        .create_network_segment(tonic::Request::new(request))
        .await
        .expect("Unable to create network segment")
        .into_inner();
    let segment_id: NetworkSegmentId = segment.id.unwrap();

    segment_id
}

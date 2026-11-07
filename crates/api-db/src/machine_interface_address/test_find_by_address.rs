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

use ipnetwork::IpNetwork;
use mac_address::MacAddress;
use model::address_selection_strategy::AddressSelectionStrategy;
use model::dns::{Domain, NewDomain};
use model::network_prefix::NewNetworkPrefix;
use model::network_segment::{
    NetworkSegmentControllerState, NetworkSegmentType, NewNetworkSegment,
};

use crate as db;

async fn init_dwrt1_domain(txn: &mut sqlx::PgTransaction<'_>) -> db::DatabaseResult<Domain> {
    db::dns::domain::persist(NewDomain::new("dwrt1.com"), txn.as_mut()).await
}

#[crate::sqlx_test]
async fn find_by_address_bmc(pool: sqlx::PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = pool.begin().await?;
    let domain = init_dwrt1_domain(&mut txn).await?;

    let new_ns = NewNetworkSegment {
        name: "PDX01-M01-H14-IPMITOR-01".to_string(),
        subdomain_id: Some(domain.id),
        vpc_id: None,
        mtu: 1490,
        prefixes: vec![NewNetworkPrefix {
            prefix: IpNetwork::V4("192.168.0.0/24".parse().unwrap()),
            gateway: Some(IpAddr::V4("192.168.0.1".parse().unwrap())),
            dhcpv6_link_address: None,
            num_reserved: 14,
        }],
        vlan_id: None,
        vni: None,
        segment_type: NetworkSegmentType::Underlay,
        id: uuid::uuid!("f9860f19-37d5-44f6-b637-84de4648cd39").into(),
        can_stretch: None,
        allocation_strategy: Default::default(),
        infer_slaac_eui64_addresses: false,
    };
    let network_segment =
        db::network_segment::persist(new_ns, &mut txn, NetworkSegmentControllerState::Ready)
            .await?;
    // An interface that isn't attached to a Machine. This is what BMC interfaces are.
    let interface = db::machine_interface::create(
        &mut txn,
        std::slice::from_ref(&network_segment),
        &MacAddress::from_str("ff:ff:ff:ff:ff:ff").unwrap(),
        true,
        AddressSelectionStrategy::NextAvailableIp,
        None,
    )
    .await?;
    let bmc_ip = interface.addresses.iter().find(|x| x.is_ipv4()).copied();
    assert!(bmc_ip.is_some());
    let res = db::machine_interface_address::find_by_address(txn.as_mut(), bmc_ip.unwrap()).await?;
    assert!(res.is_some());
    assert_eq!(res.unwrap().id, interface.id);

    Ok(())
}

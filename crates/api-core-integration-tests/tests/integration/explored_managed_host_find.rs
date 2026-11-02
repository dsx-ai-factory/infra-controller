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

use carbide_test_harness::prelude::*;
use mac_address::MacAddress;
use model::site_explorer::{EndpointExplorationReport, ExploredDpu, ExploredManagedHost};

#[sqlx_test]
async fn test_find_explored_managed_hosts_by_ids(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = TestHarness::builder(pool).build().await;

    let mut txn = env.db_txn().await;
    let mut managed_hosts: Vec<ExploredManagedHost> = Vec::new();
    for i in 1..6 {
        let host_bmc_ip = IpAddr::from_str(format!("141.219.24.{i}").as_str())?;
        let bmc_ip = IpAddr::from_str(format!("10.231.11.{i}").as_str())?;
        let mac_address = MacAddress::from_str(format!("94:6D:AE:5F:09:C{i}").as_str())?;
        managed_hosts.push(ExploredManagedHost {
            host_bmc_ip,
            dpus: vec![ExploredDpu {
                bmc_ip,
                host_pf_mac_address: Some(mac_address),
                host_chassis_id: None,
                report: EndpointExplorationReport::default().into(),
            }],
        });
    }
    db::explored_managed_host::update(&mut txn, &managed_hosts.iter().collect::<Vec<_>>()).await?;
    txn.commit().await?;

    let id_list = env
        .api()
        .find_explored_managed_host_ids(tonic::Request::new(
            ::rpc::site_explorer::ExploredManagedHostSearchFilter {},
        ))
        .await
        .map(|response| response.into_inner())
        .unwrap();
    assert_eq!(id_list.host_ids.len(), 5);

    let request = tonic::Request::new(::rpc::site_explorer::ExploredManagedHostsByIdsRequest {
        host_ids: id_list.host_ids.clone(),
    });

    let host_list = env
        .api()
        .find_explored_managed_hosts_by_ids(request)
        .await
        .map(|response| response.into_inner())
        .unwrap();
    assert_eq!(host_list.managed_hosts.len(), 5);

    // validate we got endpoints with specified ids
    let mut hosts_copy = host_list.managed_hosts;
    for _ in 0..5 {
        let host = hosts_copy.remove(0);
        let host_id = host.host_bmc_ip;
        assert!(id_list.host_ids.contains(&host_id));
    }

    Ok(())
}

// The empty-list and over-max guards for `find_explored_managed_hosts_by_ids` are
// shared API-layer code, proven once across representative RPCs in
// `tests::find_by_ids_guards`.

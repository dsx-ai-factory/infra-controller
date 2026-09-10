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

use carbide_test_harness::prelude::*;
use chrono::Utc;
use model::machine::ManagedHostState;
use rpc::Timestamp;
use rpc::forge::forge_server::Forge;
use rpc::forge::{DpuNetworkStatus, FabricInterfaceData, LinkData};

#[sqlx_test]
async fn test_get_dpu_info_list(pool: PgPool) {
    let env = TestHarness::builder(pool).build().await;
    let domain = env.test_domain().await;
    let network_controller = env.network_controller();
    let underlay_segment = network_controller.create_underlay_segment(&domain).await;
    network_controller.create_admin_segment(&domain).await;
    let site_explorer = env.default_test_site_explorer();
    let mut managed_hosts = Vec::with_capacity(2);
    for _ in 0..2 {
        let (mh, _) = env
            .managed_host_builder(&site_explorer, underlay_segment)
            .build()
            .await;
        mh.first_dpu().discover_oob_iface(underlay_segment).await;
        let mut txn = env.db_txn().await;
        mh.first_dpu()
            .db_machine(&mut txn)
            .await
            .advance_state(&mut txn, ManagedHostState::Ready)
            .await;
        txn.commit().await.unwrap();
        managed_hosts.push(mh);
    }
    let dpu_machine_id_1 = managed_hosts[0].first_dpu().id;
    let dpu_machine_id_2 = managed_hosts[1].first_dpu().id;

    let observed_at = Utc::now();
    let heartbeat_timestamp = Timestamp::from(observed_at);
    let fabric_interface =
        |interface_name: &str, carrier_up: Option<bool>, state: &str| FabricInterfaceData {
            interface_name: interface_name.to_string(),
            link_data: Some(LinkData {
                link_type: Some("ethernet".to_string()),
                state: Some(state.to_string()),
                carrier_up,
                mtu: Some(1500),
                carrier_up_count: None,
                carrier_down_count: None,
            }),
        };

    // Persist a current network status with fabric interface data for one DPU.
    env.api()
        .record_dpu_network_status(tonic::Request::new(DpuNetworkStatus {
            dpu_machine_id: Some(dpu_machine_id_1),
            dpu_agent_version: Some("test".to_string()),
            observed_at: Some(heartbeat_timestamp),
            dpu_health: Some(rpc::health::HealthReport {
                source: "forge-dpu-agent".to_string(),
                triggered_by: None,
                observed_at: None,
                successes: vec![],
                alerts: vec![],
            }),
            network_config_version: None,
            instance_id: None,
            instance_config_version: None,
            instance_network_config_version: None,
            interfaces: vec![],
            network_config_error: None,
            client_certificate_expiry_unix_epoch_secs: None,
            fabric_interfaces: vec![
                fabric_interface("pf0vf0_if_r", Some(false), "down"),
                fabric_interface("pf0dpu0", Some(true), "up"),
                fabric_interface("p0_if", Some(true), "up"),
            ],
            last_dhcp_requests: vec![],
            dpu_extension_service_version: None,
            dpu_extension_services: vec![],
            astra_config_status: None,
        }))
        .await
        .unwrap();

    // Make RPC call to get list of DPU information
    let dpu_list = env
        .api()
        .get_dpu_info_list(tonic::Request::new(rpc::forge::GetDpuInfoListRequest {}))
        .await
        .unwrap()
        .into_inner()
        .dpu_list;

    // Check that the DPU returns list of expected DPU ids
    let mut dpu_ids: Vec<String> = dpu_list.iter().map(|dpu| dpu.id.clone()).collect();
    let mut exp_ids: Vec<String> = vec![dpu_machine_id_1.to_string(), dpu_machine_id_2.to_string()];
    dpu_ids.sort();
    exp_ids.sort();
    assert_eq!(dpu_ids, exp_ids);

    // Check that the DPU returns a list of expected DPU loopback IP addresses
    let exp_dpu_loopback_ip_1 = managed_hosts[0]
        .first_dpu()
        .machine()
        .await
        .loopback_ip()
        .unwrap();
    let exp_dpu_loopback_ip_2 = managed_hosts[1]
        .first_dpu()
        .machine()
        .await
        .loopback_ip()
        .unwrap();

    let mut dpu_loopback_ips: Vec<String> = dpu_list
        .iter()
        .map(|dpu| dpu.loopback_ip.to_string())
        .collect();
    let mut exp_loopback_ips: Vec<String> = vec![
        exp_dpu_loopback_ip_1.to_string(),
        exp_dpu_loopback_ip_2.to_string(),
    ];
    dpu_loopback_ips.sort();
    exp_loopback_ips.sort();
    assert_eq!(dpu_loopback_ips, exp_loopback_ips);

    // Check that operational fields are populated from persisted DPU state.
    let dpu_1 = dpu_list
        .iter()
        .find(|dpu| dpu.id == dpu_machine_id_1.to_string())
        .unwrap();
    let observed_status = dpu_1.observed_status.as_ref().unwrap();
    assert_eq!(
        observed_status
            .os_operational_state
            .as_ref()
            .map(|state| state.state_detail.as_str()),
        Some("Ready")
    );
    assert_eq!(
        observed_status.firmware_version.as_deref(),
        Some("24.42.1000")
    );
    assert_eq!(observed_status.last_heartbeat, Some(heartbeat_timestamp));
    assert_eq!(observed_status.representors.len(), 2);
    let pf0dpu0 = observed_status
        .representors
        .iter()
        .find(|representor| representor.name == "pf0dpu0")
        .unwrap();
    assert_eq!(pf0dpu0.carrier_up, Some(true));
    assert_eq!(pf0dpu0.state.as_deref(), Some("up"));
    let pf0vf0_if_r = observed_status
        .representors
        .iter()
        .find(|representor| representor.name == "pf0vf0_if_r")
        .unwrap();
    assert_eq!(pf0vf0_if_r.carrier_up, Some(false));
    assert_eq!(pf0vf0_if_r.state.as_deref(), Some("down"));
    assert!(
        !observed_status
            .representors
            .iter()
            .any(|representor| representor.name == "p0_if")
    );
}

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

use std::collections::{HashMap, HashSet};

use carbide_ib_fabric::config::IBFabricConfig;
use carbide_ib_fabric::ib::{GetPartitionOptions, IBFabric, IBMtu, IBRateLimit, IBServiceLevel};
use carbide_instrument::testing::MetricsCapture;
use carbide_uuid::machine::MachineId;
use common::api_fixtures::create_managed_host;
use model::ib::{DEFAULT_IB_FABRIC_NAME, IBNetwork, IBQosConf};
use model::ib_partition::PartitionKey;

use crate::tests::common;
use crate::tests::common::api_fixtures::TestEnvOverrides;
use crate::tests::common::api_fixtures::ib_partition::{DEFAULT_TENANT, create_ib_partition};

#[crate::sqlx_test]
async fn monitor_ib_status_and_fix_incorrect_pkey_associations(pool: sqlx::PgPool) {
    let mut config = common::api_fixtures::get_config();
    config.ib_config = Some(IBFabricConfig {
        enabled: true,
        mtu: carbide_ib_fabric::ib::IBMtu(2),
        rate_limit: carbide_ib_fabric::ib::IBRateLimit(10),
        max_partition_per_tenant: 16,
        ..Default::default()
    });

    let env = common::api_fixtures::create_test_env_with_overrides(
        pool,
        TestEnvOverrides::with_config(config),
    )
    .await;

    // Ingest 2 Machines. They should have different GUIDs, different LIDs, and report different IB status
    let (host_machine_id_1, _dpu_machine_id) = create_managed_host(&env).await.into();
    let (host_machine_id_2, _dpu_machine_id) = create_managed_host(&env).await.into();

    let host_machines = [host_machine_id_1, host_machine_id_2];
    let mut guids: HashMap<MachineId, Vec<String>> = HashMap::new();

    let mut active_lids = HashSet::new();

    for host_machine_id in host_machines.iter().copied() {
        println!("Testing host machine {host_machine_id}");
        let rpc_machine_id: MachineId = host_machine_id.into();

        let machine = env.find_machine(&rpc_machine_id).await.remove(0);

        let machine_guids = guids.entry(host_machine_id.into()).or_default();

        let discovery_info = machine
            .status
            .as_ref()
            .unwrap()
            .discovery_info
            .as_ref()
            .unwrap();
        let ib_status = machine
            .status
            .as_ref()
            .unwrap()
            .infiniband
            .clone()
            .expect("IB status is missing");
        assert_eq!(
            discovery_info.infiniband_interfaces.len(),
            ib_status.ib_interfaces.len()
        );

        for ib_iface in discovery_info.infiniband_interfaces.iter() {
            machine_guids.push(ib_iface.guid.clone());
            let iface_status = ib_status
                .ib_interfaces
                .iter()
                .find(|iface| iface.guid() == ib_iface.guid)
                .expect("IB interface with matching GUID was not found");
            assert_eq!(iface_status.fabric_id(), "default");
            assert!(iface_status.lid.is_some());
            assert_ne!(iface_status.lid(), 0xffff_u32);
            assert!(
                !active_lids.contains(&iface_status.lid()),
                "Lid {} is used by multiple interfaces",
                iface_status.lid()
            );
            active_lids.insert(iface_status.lid());
        }

        assert_ne!(ib_status.ib_interfaces.len(), 0);
    }
    assert_eq!(
        env.test_meter
            .parsed_metrics("carbide_ib_monitor_machines_by_port_state_count"),
        vec![(
            "{active_ports=\"6\",total_ports=\"6\"}".to_string(),
            "2".to_string()
        )]
    );
    assert_eq!(
        env.test_meter
            .formatted_metric("carbide_ib_monitor_machines_with_missing_pkeys_count")
            .unwrap(),
        "0"
    );
    assert_eq!(
        env.test_meter
            .formatted_metric("carbide_ib_monitor_machines_with_unexpected_pkeys_count")
            .unwrap(),
        "0"
    );
    assert_eq!(
        env.test_meter
            .formatted_metric("carbide_ib_monitor_machines_with_unknown_pkeys_count")
            .unwrap(),
        "0"
    );

    // Down the first and third interface of host_machine_1 and check
    // whether this gets reflected in the observed status
    let guid1 = guids.get(&host_machine_id_1).unwrap()[0].clone();
    let guid2 = guids.get(&host_machine_id_1).unwrap()[1].clone();
    let guid3 = guids.get(&host_machine_id_1).unwrap()[2].clone();
    let guid4 = guids.get(&host_machine_id_1).unwrap()[3].clone();
    let ib_manager = env.ib_fabric_manager.get_mock_manager().clone();
    ib_manager.set_port_state(&guid1, false);
    ib_manager.set_port_state(&guid3, false);

    // Also bind the 2nd GUID and 4th GUID with a partition behind the scenes
    // Partition 2 has no representation in Forge
    // This should lead to certain metrics being emitted. It also should lead to
    // the misconfiguration being auto-corrected
    let (ib_partition_id1, ib_partition1) = create_ib_partition(
        &env,
        "test_ib_partition".to_string(),
        DEFAULT_TENANT.to_string(),
    )
    .await;
    let pkey1: PartitionKey = ib_partition1
        .status
        .as_ref()
        .unwrap()
        .pkey()
        .parse()
        .unwrap();
    // The allocated pkey is random. Pick a different value from the configured
    // managed ranges instead of using `pkey1 + 1`. The cleanup path calls
    // `is_pkey_in_managed_range`, which checks `start..end`, so the configured
    // end value, such as 100 in the default test config, is out of range.
    let allocated_pkey = u16::from(pkey1);
    let parse_pkey_endpoint = |value: &str| {
        let value = value.trim();

        if let Some(hex) = value
            .strip_prefix("0x")
            .or_else(|| value.strip_prefix("0X"))
        {
            u16::from_str_radix(hex, 16).ok()
        } else {
            value.parse::<u16>().ok()
        }
    };

    let pkey2_value = env
        .config
        .ib_fabrics
        .get(DEFAULT_IB_FABRIC_NAME)
        .into_iter()
        .flat_map(|fabric| fabric.pkeys.iter())
        .filter_map(|range| {
            let start = parse_pkey_endpoint(&range.start)?;
            let end = parse_pkey_endpoint(&range.end)?;
            (start..end).find(|value| *value != allocated_pkey)
        })
        .next()
        .expect("test IB fabric config should contain another managed pkey");
    let pkey2: PartitionKey = pkey2_value.try_into().unwrap();

    let partition1 = IBNetwork {
        pkey: pkey1.into(),
        name: "x".to_string(),
        qos_conf: Some(IBQosConf {
            mtu: IBMtu::default(),
            service_level: IBServiceLevel::default(),
            rate_limit: IBRateLimit::default(),
        }),
        ipoib: false,
        associated_guids: None,
        membership: None,
        // Not implemented yet
        // enable_sharp: false,
        // index0: false,
    };
    let mut partition2 = partition1.clone();
    partition2.pkey = pkey2.into();
    ib_manager
        .bind_ib_ports(partition1, vec![guid2.clone(), guid4.clone()])
        .await
        .unwrap();
    ib_manager
        .bind_ib_ports(partition2, vec![guid2.clone()])
        .await
        .unwrap();
    // Double check that the setting is applied
    let p1 = ib_manager
        .get_ib_network(
            pkey1.into(),
            GetPartitionOptions {
                include_guids_data: true,
                include_qos_conf: true,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        p1.associated_guids,
        Some(HashSet::from_iter([guid2.clone(), guid4.clone()]))
    );
    let p2 = ib_manager
        .get_ib_network(
            pkey2.into(),
            GetPartitionOptions {
                include_guids_data: true,
                include_qos_conf: true,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        p2.associated_guids,
        Some(HashSet::from_iter([guid2.clone()]))
    );

    let ufm_change_metrics = MetricsCapture::start();
    env.ib_fabric_monitor.run_single_iteration().await.unwrap();
    let successful_unbinds = ufm_change_metrics.counter_delta(
        "carbide_ib_monitor_ufm_changes_applied_total",
        &[
            ("fabric", "default"),
            ("operation", "unbind_guid_from_pkey"),
            ("status", "ok"),
        ],
    );
    // Other API tests can drive the same process-global Event concurrently.
    // The Event-level test pins exact counts; this check proves the monitor's
    // reconciliation path reaches the UFM change Event.
    assert!(
        successful_unbinds >= 3.0,
        "expected three successful UFM unbinds, observed {successful_unbinds}"
    );
    drop(ufm_change_metrics);
    assert_eq!(
        env.test_meter
            .formatted_metric("carbide_ib_monitor_machine_ib_status_updates_count")
            .unwrap(),
        "1"
    );
    assert_eq!(
        env.test_meter
            .parsed_metrics("carbide_ib_monitor_machines_by_port_state_count"),
        vec![
            (
                "{active_ports=\"4\",total_ports=\"6\"}".to_string(),
                "1".to_string()
            ),
            (
                "{active_ports=\"6\",total_ports=\"6\"}".to_string(),
                "1".to_string()
            )
        ]
    );
    assert_eq!(
        env.test_meter
            .parsed_metrics("carbide_ib_monitor_machines_by_ports_with_partitions_count"),
        vec![
            ("{ports_with_partitions=\"0\"}".to_string(), "1".to_string()),
            ("{ports_with_partitions=\"2\"}".to_string(), "1".to_string())
        ]
    );
    assert_eq!(
        env.test_meter
            .formatted_metric("carbide_ib_monitor_machines_with_missing_pkeys_count")
            .unwrap(),
        "0"
    );
    assert_eq!(
        env.test_meter
            .formatted_metric("carbide_ib_monitor_machines_with_unexpected_pkeys_count")
            .unwrap(),
        "1"
    );
    assert_eq!(
        env.test_meter
            .formatted_metric("carbide_ib_monitor_machines_with_unknown_pkeys_count")
            .unwrap(),
        "1"
    );
    active_lids.clear();
    for host_machine_id in host_machines.iter().copied() {
        println!("Testing host machine {host_machine_id}");
        let rpc_machine_id: MachineId = host_machine_id.into();

        let machine = env.find_machine(&rpc_machine_id).await.remove(0);

        let discovery_info = machine
            .status
            .as_ref()
            .unwrap()
            .discovery_info
            .as_ref()
            .unwrap();
        let ib_status = machine
            .status
            .as_ref()
            .unwrap()
            .infiniband
            .clone()
            .expect("IB status is missing");
        assert_eq!(
            discovery_info.infiniband_interfaces.len(),
            ib_status.ib_interfaces.len()
        );

        for ib_iface in discovery_info.infiniband_interfaces.iter() {
            let iface_status = ib_status
                .ib_interfaces
                .iter()
                .find(|iface| iface.guid() == ib_iface.guid)
                .expect("IB interface with matching GUID was not found");
            assert_eq!(iface_status.fabric_id(), "default");
            assert!(iface_status.lid.is_some());
            if ib_iface.guid == guid1 || ib_iface.guid == guid3 {
                assert_eq!(iface_status.lid(), 0xffff_u32);
            } else {
                assert_ne!(iface_status.lid(), 0xffff_u32);
                assert!(
                    !active_lids.contains(&iface_status.lid()),
                    "Lid {} is used by multiple interfaces",
                    iface_status.lid()
                );
                active_lids.insert(iface_status.lid());
            }

            let mut associated_pkeys = iface_status
                .associated_pkeys
                .clone()
                .expect("Associated pkeys should be available");
            associated_pkeys.items.sort();
            match &ib_iface.guid {
                guid if guid == &guid2 => {
                    let mut expected_pkeys = vec![pkey1.to_string(), pkey2.to_string()];
                    expected_pkeys.sort();
                    assert_eq!(associated_pkeys.items, expected_pkeys)
                }
                guid if guid == &guid4 => {
                    assert_eq!(associated_pkeys.items, vec![pkey1.to_string()])
                }
                _ => assert!(associated_pkeys.items.is_empty()),
            }

            let mut associated_partition_ids = iface_status
                .associated_partition_ids
                .clone()
                .expect("Associated partition IDs should be available");
            associated_partition_ids.items.sort();
            match &ib_iface.guid {
                guid if guid == &guid2 || guid == &guid4 => assert_eq!(
                    associated_partition_ids.items,
                    vec![ib_partition_id1.to_string()]
                ),
                _ => assert!(associated_partition_ids.items.is_empty()),
            }
        }

        assert_ne!(ib_status.ib_interfaces.len(), 0);
    }

    // After the last run, the unexpected pkeys have been removed
    env.ib_fabric_monitor.run_single_iteration().await.unwrap();
    // Double check that the setting is applied
    let err = ib_manager
        .get_ib_network(
            pkey1.into(),
            GetPartitionOptions {
                include_guids_data: true,
                include_qos_conf: true,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        format!("ufm_path not found: /resources/pkeys/{pkey1}")
    );
    let err = ib_manager
        .get_ib_network(
            pkey2.into(),
            GetPartitionOptions {
                include_guids_data: true,
                include_qos_conf: true,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        format!("ufm_path not found: /resources/pkeys/{pkey2}")
    );
    assert_eq!(
        env.test_meter
            .formatted_metric("carbide_ib_monitor_machine_ib_status_updates_count")
            .unwrap(),
        "1"
    );
    assert_eq!(
        env.test_meter
            .parsed_metrics("carbide_ib_monitor_machines_by_port_state_count"),
        vec![
            (
                "{active_ports=\"4\",total_ports=\"6\"}".to_string(),
                "1".to_string()
            ),
            (
                "{active_ports=\"6\",total_ports=\"6\"}".to_string(),
                "1".to_string()
            )
        ]
    );
    assert_eq!(
        env.test_meter
            .parsed_metrics("carbide_ib_monitor_machines_by_ports_with_partitions_count"),
        vec![("{ports_with_partitions=\"0\"}".to_string(), "2".to_string())]
    );
    assert_eq!(
        env.test_meter
            .formatted_metric("carbide_ib_monitor_machines_with_missing_pkeys_count")
            .unwrap(),
        "0"
    );
    assert_eq!(
        env.test_meter
            .formatted_metric("carbide_ib_monitor_machines_with_unexpected_pkeys_count")
            .unwrap(),
        "0"
    );
    assert_eq!(
        env.test_meter
            .formatted_metric("carbide_ib_monitor_machines_with_unknown_pkeys_count")
            .unwrap(),
        "0"
    );
}

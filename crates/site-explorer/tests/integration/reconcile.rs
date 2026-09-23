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
use std::sync::Arc;

use carbide_site_explorer::config::SiteExplorerConfig;
use carbide_test_harness::prelude::*;
use carbide_test_harness::test_support::network_segment::create_static_assignments_segment;
use mac_address::MacAddress;
use model::expected_machine::{ExpectedMachine, ExpectedMachineData};
use model::metadata::Metadata;
use model::site_explorer::EndpointExplorationReport;

async fn init(pool: &PgPool) -> TestHarness {
    let test_harness = TestHarness::builder(pool.clone()).build().await;
    let domain = test_harness.test_domain().await;
    create_static_assignments_segment(test_harness.api(), Some(domain.id)).await;
    test_harness
}

/// Running `try_preallocate_one` twice for the same (mac, ip) must be a no-op the second time
/// -- no new rows, no errors. This is the steady-state behavior since site-explorer iterates
/// continuously and re-issues the same calls on every pass.
#[sqlx_test]
async fn test_site_explorer_reconcile_is_idempotent(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    init(&pool).await;

    let bmc_mac: MacAddress = "AA:BB:CC:DD:E2:01".parse().unwrap();
    let bmc_ip: IpAddr = "10.99.0.30".parse().unwrap();

    let mut txn = pool.begin().await?;
    db::expected_machine::create(
        &mut txn,
        ExpectedMachine {
            id: None,
            bmc_mac_address: bmc_mac,
            data: ExpectedMachineData {
                serial_number: "reconcile-idem-001".to_string(),
                bmc_ip_address: Some(bmc_ip),
                ..Default::default()
            },
        },
    )
    .await?;
    txn.commit().await?;

    for _ in 0..2 {
        carbide_site_explorer::try_preallocate_one(
            &pool,
            bmc_mac,
            bmc_ip,
            model::machine_interface::InterfaceType::Bmc,
            "expected_machine BMC",
            None,
        )
        .await;
    }

    let mut txn = pool.begin().await?;
    let interfaces = db::machine_interface::find_by_mac_address(&mut *txn, bmc_mac).await?;
    txn.commit().await?;
    assert_eq!(
        interfaces.len(),
        1,
        "two preallocate calls should not create duplicate machine_interface rows"
    );
    assert!(interfaces[0].addresses.contains(&bmc_ip));

    Ok(())
}

/// Site-explorer's reconciliation pass must materialize `ExpectedInterface.fixed_ip`
/// reservations too, not just BMC IPs.
#[sqlx_test]
async fn test_site_explorer_reconcile_preallocates_host_nic_fixed_ip(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let test_harness = init(&pool).await;

    let bmc_mac: MacAddress = "AA:BB:CC:DD:E1:01".parse().unwrap();
    let nic_mac: MacAddress = "AA:BB:CC:DD:E1:02".parse().unwrap();
    let fixed_ip = "10.99.0.20";
    let parsed_fixed_ip: IpAddr = fixed_ip.parse().unwrap();
    let expected_interface = model::expected_machine::ExpectedInterface {
        mac_address: nic_mac,
        nic_type: Some("onboard".into()),
        fixed_ip: Some(parsed_fixed_ip),
        ..Default::default()
    };

    let mut txn = pool.begin().await?;
    db::expected_machine::create(
        &mut txn,
        ExpectedMachine {
            id: None,
            bmc_mac_address: bmc_mac,
            data: ExpectedMachineData {
                serial_number: "reconcile-hostnic-001".to_string(),
                interfaces: vec![expected_interface],
                ..Default::default()
            },
        },
    )
    .await?;
    txn.commit().await?;

    // Drive the production reconciliation entry point so this covers loading
    // `ExpectedMachineData.interfaces` as well as applying its policy.
    let explorer = super::env::test_site_explorer(
        &test_harness,
        SiteExplorerConfig {
            explorations_per_run: 0,
            ..Default::default()
        },
    );
    explorer.run_single_iteration().await?;

    let mut txn = pool.begin().await?;
    let nic_iface = db::machine_interface::find_by_mac_address(&mut *txn, nic_mac).await?;
    txn.commit().await?;
    assert_eq!(
        nic_iface.len(),
        1,
        "host NIC interface should be preallocated"
    );
    assert!(
        nic_iface[0].addresses.contains(&parsed_fixed_ip),
        "preallocated host NIC interface should carry the fixed_ip"
    );
    assert_eq!(
        nic_iface[0].interface_type,
        model::machine_interface::InterfaceType::Data,
        "host NIC preallocation should mark the interface as InterfaceType::Data, not Bmc"
    );

    Ok(())
}

/// A per-entry conflict (e.g., two expected_machines configured with the same static IP -- a
/// genuine operator misconfiguration) must not abort the whole reconciliation pass. The
/// conflicting entry gets logged and skipped; the rest of the iteration continues.
#[sqlx_test]
async fn test_site_explorer_reconcile_tolerates_per_entry_conflicts(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    init(&pool).await;

    let mac_a: MacAddress = "AA:BB:CC:DD:E3:01".parse().unwrap();
    let mac_b: MacAddress = "AA:BB:CC:DD:E3:02".parse().unwrap();
    let mac_c: MacAddress = "AA:BB:CC:DD:E3:03".parse().unwrap();
    let shared_ip: IpAddr = "10.99.0.40".parse().unwrap();
    let ok_ip: IpAddr = "10.99.0.41".parse().unwrap();

    let mut txn = pool.begin().await?;
    // Two machines configured with the SAME bmc_ip_address -- the second will conflict at
    // preallocate time. A third has its own valid IP and should still get preallocated.
    for (mac, ip, sn) in [
        (mac_a, shared_ip, "reconcile-conflict-a"),
        (mac_b, shared_ip, "reconcile-conflict-b"),
        (mac_c, ok_ip, "reconcile-conflict-c"),
    ] {
        db::expected_machine::create(
            &mut txn,
            ExpectedMachine {
                id: None,
                bmc_mac_address: mac,
                data: ExpectedMachineData {
                    serial_number: sn.to_string(),
                    bmc_ip_address: Some(ip),
                    ..Default::default()
                },
            },
        )
        .await?;
    }
    txn.commit().await?;

    // try_preallocate_one swallows per-entry errors -- the conflict between mac_a and mac_b
    // gets logged and the third call still succeeds.
    for (mac, ip) in [(mac_a, shared_ip), (mac_b, shared_ip), (mac_c, ok_ip)] {
        carbide_site_explorer::try_preallocate_one(
            &pool,
            mac,
            ip,
            model::machine_interface::InterfaceType::Bmc,
            "expected_machine BMC",
            None,
        )
        .await;
    }

    let mut txn = pool.begin().await?;
    // Exactly one of {mac_a, mac_b} won the race for the shared IP.
    let a = db::machine_interface::find_by_mac_address(&mut *txn, mac_a).await?;
    let b = db::machine_interface::find_by_mac_address(&mut *txn, mac_b).await?;
    let winners = a.len() + b.len();
    assert_eq!(
        winners, 1,
        "exactly one of the two conflicting MACs should have a preallocated row"
    );
    // The third, non-conflicting entry must still be preallocated.
    let c = db::machine_interface::find_by_mac_address(&mut *txn, mac_c).await?;
    assert_eq!(
        c.len(),
        1,
        "non-conflicting expected_machine should still be preallocated despite the upstream conflict"
    );
    assert!(c[0].addresses.contains(&ok_ip));
    txn.commit().await?;

    Ok(())
}

/// Site-explorer's reconciliation pass must materialize the (nvos_mac, nvos_ip_address)
/// pairing for expected switches, mirroring how it handles `bmc_ip_address` and the
/// host-NIC `fixed_ip` paths. A full iteration must scan the switch BMC but not its
/// preallocated NVOS endpoint, while preserving the NVOS row as `InterfaceType::Data`.
#[sqlx_test]
async fn test_site_explorer_reconcile_preallocates_nvos_ip(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let test_harness = init(&pool).await;

    let bmc_mac: MacAddress = "AA:BB:CC:DD:E4:01".parse().unwrap();
    let nvos_mac: MacAddress = "AA:BB:CC:DD:E4:02".parse().unwrap();
    let bmc_ip: IpAddr = "10.99.0.49".parse().unwrap();
    let nvos_ip: IpAddr = "10.99.0.50".parse().unwrap();

    let mut txn = pool.begin().await?;
    db::expected_switch::create(
        &mut txn,
        model::expected_switch::ExpectedSwitch {
            expected_switch_id: None,
            bmc_mac_address: bmc_mac,
            nvos_mac_addresses: vec![nvos_mac],
            bmc_username: "ADMIN".into(),
            serial_number: "reconcile-nvos-001".into(),
            bmc_password: "PASS".into(),
            nvos_username: None,
            nvos_password: None,
            bmc_ip_address: Some(bmc_ip),
            nvos_ip_address: Some(nvos_ip),
            metadata: Metadata::default(),
            rack_id: None,
            bmc_retain_credentials: None,
        },
    )
    .await?;
    txn.commit().await?;

    // Baseline: no NVOS interface yet.
    let mut txn = pool.begin().await?;
    let before = db::machine_interface::find_by_mac_address(&mut *txn, nvos_mac).await?;
    assert!(
        before.is_empty(),
        "no machine_interface should exist before site-explorer reconciles for {nvos_mac}"
    );
    txn.commit().await?;

    let explorer = super::env::test_site_explorer(
        &test_harness,
        SiteExplorerConfig {
            create_machines: Arc::new(false.into()),
            create_power_shelves: Arc::new(false.into()),
            create_switches: Arc::new(false.into()),
            ..Default::default()
        },
    );
    explorer.insert_endpoints(vec![(bmc_ip, EndpointExplorationReport::default())]);
    explorer.run_single_iteration().await?;

    assert_eq!(
        explorer
            .endpoint_explorer()
            .explore_endpoint_calls
            .lock()
            .unwrap()
            .iter()
            .map(|call| call.ip_address)
            .collect::<Vec<_>>(),
        vec![bmc_ip],
        "site explorer must scan the switch BMC without probing its NVOS endpoint",
    );

    let mut txn = pool.begin().await?;
    let after = db::machine_interface::find_by_mac_address(&mut *txn, nvos_mac).await?;
    assert_eq!(
        after.len(),
        1,
        "expected_switch NVOS should be preallocated for {nvos_mac}"
    );
    assert!(
        after[0].addresses.contains(&nvos_ip),
        "preallocated row should carry {nvos_ip}, got {:?}",
        after[0].addresses,
    );
    assert_eq!(
        after[0].interface_type,
        model::machine_interface::InterfaceType::Data,
        "NVOS IPs should be preallocated with InterfaceType::Data, not Bmc"
    );
    txn.commit().await?;

    Ok(())
}

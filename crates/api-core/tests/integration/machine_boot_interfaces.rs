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

//! `GetMachineBootInterfaces` gathers one machine's boot-interface view from
//! all four stores -- owned interface rows, predictions, the explored endpoint
//! default, and the retained post-deletion pairs -- and reports the effective
//! boot interface, divergence, and desired-state reconciliation. These tests
//! seed the stores for one host and assert the gathered view.

use carbide_test_harness::prelude::*;
use carbide_test_harness::test_support::fixture_config::{
    FixtureDefault as _, ManagedHostConfigExt as _,
};
use mac_address::MacAddress;
use model::expected_machine::{ExpectedInterface, ExpectedMachineData};
use model::network_segment::NetworkSegmentType;
use model::predicted_machine_interface::NewPredictedMachineInterface;
use model::test_support::{DpuConfig, ManagedHostConfig};
use rpc::forge;
use rpc::forge::forge_server::Forge;
use rpc::forge::get_machine_boot_interfaces_response::reconciliation::State as ReconciliationState;

async fn init_with_config(
    pool: PgPool,
    config: ManagedHostConfig,
) -> (TestHarness, TestManagedHost) {
    let env = TestHarness::builder(pool).build().await;
    let domain = env.test_domain().await;
    let network_controller = env.network_controller();
    let underlay_segment = network_controller.create_underlay_segment(&domain).await;
    network_controller.create_admin_segment(&domain).await;
    let site_explorer = env.default_test_site_explorer();
    let (host, _) = env
        .managed_host_builder(&site_explorer, underlay_segment)
        .with_config(config)
        .build()
        .await;
    (env, host)
}

async fn init(pool: PgPool) -> (TestHarness, TestManagedHost) {
    init_with_config(pool, ManagedHostConfig::default().with_dpu_count(1)).await
}

#[sqlx_test]
async fn test_expected_machine_selection_source_survives_ingestion(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let dpu = DpuConfig::default();
    let declared_primary = dpu.host_mac_address;
    let config = ManagedHostConfig::default()
        .with_dpus(vec![dpu])
        .with_expected_machine_data(ExpectedMachineData {
            interfaces: vec![ExpectedInterface {
                mac_address: declared_primary,
                primary: Some(true),
                ..Default::default()
            }],
            ..Default::default()
        });
    let (env, host) = init_with_config(pool, config).await;

    let report = env
        .api()
        .get_machine_boot_interfaces(tonic::Request::new(
            forge::GetMachineBootInterfacesRequest {
                machine_id: Some(host.host.id.into()),
            },
        ))
        .await?
        .into_inner();
    let reconciliation = report
        .reconciliation
        .expect("ingestion should initialize desired boot reconciliation");

    assert_eq!(
        reconciliation.selection_source(),
        forge::BootInterfaceSelectionSource::ExpectedMachine,
    );
    assert_eq!(
        reconciliation
            .desired_boot_interface
            .as_ref()
            .map(|target| target.mac_address.as_str()),
        Some(declared_primary.to_string().as_str()),
    );
    assert!(reconciliation.selection_updated_at.is_some());

    Ok(())
}

#[sqlx_test]
async fn test_get_machine_boot_interfaces_gathers_all_four_stores(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let (env, host) = init(pool).await;

    // A real ingested host gives us owned `machine_interfaces` rows, BMC
    // topology, and explored endpoints -- stores 1 and 3.
    let host_id = host.host.id;

    // Read the owned rows the host ended up with: the primary is the effective
    // boot interface `pick_boot_interface` selects, and we reuse its MAC to
    // seed a retained record (store 4).
    let primary_mac = {
        let mut txn = env.db_txn().await;
        let interfaces = db::machine_interface::find_by_machine_ids(txn.as_mut(), &[host_id])
            .await?
            .remove(&host_id)
            .expect("host should have interface rows");
        txn.rollback().await?;
        interfaces
            .iter()
            .find(|i| i.primary_interface)
            .expect("a DPU host has a primary interface")
            .mac_address
    };
    // A DPU host's primary carries a boot-interface id; seed a known one so the
    // effective-pick contract is asserted against a concrete value -- a regression
    // to no id then fails loudly instead of defaulting the assertion away.
    let primary_boot_id = "NIC.Primary.1-1-1";

    // Store 2: a prediction for this host, flagged primary, on a MAC that is
    // NOT the owned effective pick -- two disagreeing boot-MAC signals, so the
    // view must flag divergence.
    let predicted_mac: MacAddress = "aa:bb:cc:dd:ee:01".parse()?;
    // Store 4: a retained pair on the primary's MAC, aged well past any window.
    // `find_records_by_macs` ignores the window, so the troubleshooting view
    // surfaces it even though `find_by_mac` would hide a stale record.
    {
        let mut txn = env.db_txn().await;
        db::predicted_machine_interface::create(
            NewPredictedMachineInterface {
                machine_id: &host_id,
                mac_address: predicted_mac,
                expected_network_segment_type: NetworkSegmentType::HostInband,
                boot_interface_id: Some("NIC.Predicted.1-1-1".to_string()),
                primary_interface: true,
            },
            txn.as_mut(),
        )
        .await?;

        // Seed the owned primary's boot-interface id so the effective pick has a
        // concrete value to assert.
        db::machine_interface::set_boot_interface_id(primary_mac, primary_boot_id, txn.as_mut())
            .await?;

        // Store 3: give the host's explored BMC endpoint a recorded boot
        // interface so the explored-endpoint store has concrete data to
        // surface. Resolve the BMC IP the same way the handler does (machine ->
        // BMC pairs -> explored endpoint at that address) and set its default to
        // the owned primary -- naming the same boot NIC, so it adds no new
        // distinct boot-MAC signal and leaves the divergence verdict to the
        // conflicting prediction.
        let bmc_ip: std::net::IpAddr =
            db::machine_topology::find_machine_bmc_pairs_by_machine_id(txn.as_mut(), vec![host_id])
                .await?
                .into_iter()
                .find_map(|(_, ip)| ip)
                .expect("the ingested host should have a BMC address")
                .parse()?;
        db::explored_endpoints::set_boot_interface(
            bmc_ip,
            &model::machine_boot_interface::MachineBootInterface {
                mac_address: primary_mac,
                interface_id: primary_boot_id.to_string(),
            },
            txn.as_mut(),
        )
        .await?;

        db::retained_boot_interface::upsert(txn.as_mut(), primary_mac, "NIC.Retained.9-9-9")
            .await?;
        sqlx::query(
            "UPDATE retained_boot_interfaces SET recorded_at = NOW() - INTERVAL '30 days' \
             WHERE mac_address = $1",
        )
        .bind(primary_mac)
        .execute(txn.as_mut())
        .await?;
        txn.commit().await?;
    }

    let report = env
        .api()
        .get_machine_boot_interfaces(tonic::Request::new(
            forge::GetMachineBootInterfacesRequest {
                machine_id: Some(host_id.into()),
            },
        ))
        .await?
        .into_inner();

    assert_eq!(report.machine_id, Some(host_id.into()));

    // The desired-state view names the boot target Site Explorer persisted for
    // this host. The fixture runs Site Explorer but no machine-controller
    // iteration, so the host still sits in its initial ConfigureAstra state.
    let reconciliation = report
        .reconciliation
        .as_ref()
        .expect("an ingested host should have a desired boot interface");
    assert_eq!(
        reconciliation
            .desired_boot_interface
            .as_ref()
            .map(|target| target.mac_address.as_str()),
        Some(primary_mac.to_string().as_str()),
        "reconciliation should report the persisted primary target"
    );
    assert!(
        !reconciliation.desired_version.is_empty(),
        "the desired generation should be reported"
    );
    assert_eq!(
        reconciliation.reconciliation_state(),
        ReconciliationState::Pending,
        "the unverified desired generation should still be pending"
    );
    assert_eq!(
        reconciliation.machine_state, "ConfigureAstra/EnableNics",
        "the managed-host state should explain where reconciliation is waiting"
    );
    assert_eq!(
        reconciliation.selection_source(),
        forge::BootInterfaceSelectionSource::RedfishUefiPci,
        "the fixture's complete UEFI paths should survive ingestion as the exact selection source",
    );
    assert!(
        reconciliation.selection_updated_at.is_some(),
        "a newly recorded selection source should report when it was selected",
    );

    // Store 1: the owned rows include the primary, and the primary is flagged.
    assert!(
        !report.machine_interfaces.is_empty(),
        "owned interface rows should be reported"
    );
    let reported_primary = report
        .machine_interfaces
        .iter()
        .find(|i| i.mac_address == primary_mac.to_string())
        .expect("the primary should appear among owned rows");
    assert!(
        reported_primary.primary_interface,
        "the primary row should carry the primary flag"
    );

    // Store 2: the seeded prediction shows up with its id and primary flag.
    let reported_prediction = report
        .predicted_interfaces
        .iter()
        .find(|p| p.mac_address == predicted_mac.to_string())
        .expect("the seeded prediction should be reported");
    assert!(reported_prediction.primary_interface);
    assert_eq!(
        reported_prediction.boot_interface_id.as_deref(),
        Some("NIC.Predicted.1-1-1"),
        "the prediction's boot interface id should be reported"
    );

    // Store 3: the host's explored BMC endpoint is surfaced with the boot
    // interface we recorded against it.
    let reported_explored = report
        .explored_endpoints
        .iter()
        .find(|e| e.boot_interface_mac.as_deref() == Some(primary_mac.to_string().as_str()))
        .expect("the host's explored endpoint default should be reported");
    assert_eq!(
        reported_explored.boot_interface_id.as_deref(),
        Some(primary_boot_id),
        "the explored endpoint's recorded boot interface id should be reported"
    );

    // Store 4: the stale retained record is surfaced with its recorded_at,
    // proving the un-window-filtered read.
    let reported_retained = report
        .retained_interfaces
        .iter()
        .find(|r| r.mac_address == primary_mac.to_string())
        .expect("the stale retained record should be surfaced");
    assert_eq!(reported_retained.boot_interface_id, "NIC.Retained.9-9-9");
    assert!(
        reported_retained.recorded_at.is_some(),
        "the retained record should carry recorded_at"
    );

    // Effective pick: the owned primary's MAC.
    assert_eq!(
        report.effective_boot_interface_mac.as_deref(),
        Some(primary_mac.to_string().as_str()),
        "the effective boot interface is the owned primary"
    );
    assert_eq!(
        report.effective_boot_interface_id.as_deref(),
        Some(primary_boot_id),
        "the effective boot interface id is the primary row's captured boot-interface id"
    );

    // Divergence: the predicted primary disagrees with the owned pick.
    assert!(
        report.divergent,
        "a predicted primary on a different MAC than the owned pick is a divergence"
    );

    // Every owned row reports its `machine_interfaces` row id, so a candidate
    // can be handed straight to `SetPrimaryInterface`.
    assert!(
        report
            .machine_interfaces
            .iter()
            .all(|i| i.interface_id.is_some()),
        "owned rows should report their interface row ids"
    );

    // The default pick (primary flag masked) names one of the machine's own
    // non-underlay rows. Its exact identity is the selection logic's business
    // (unit-tested in api-model); here we assert the wiring.
    let default_mac = report
        .default_boot_interface
        .as_ref()
        .map(|b| b.mac_address.as_str())
        .expect("a host with non-underlay rows has a default pick");
    let default_row = report
        .machine_interfaces
        .iter()
        .find(|i| i.mac_address == default_mac)
        .expect("the default pick names an owned row");
    assert_ne!(
        default_row.network_segment_type.as_deref(),
        Some(NetworkSegmentType::Underlay.to_string().as_str()),
        "the default pick never lands on an underlay row"
    );

    // The predicted pick is the seeded primary-flagged prediction, id and all.
    let predicted_pick = report
        .predicted_boot_interface
        .as_ref()
        .expect("the predicted pick is reported");
    assert_eq!(
        predicted_pick.mac_address,
        predicted_mac.to_string(),
        "the predicted pick is the declared-primary prediction"
    );
    assert_eq!(
        predicted_pick.interface_id.as_deref(),
        Some("NIC.Predicted.1-1-1"),
        "the predicted pick reports the prediction's captured id"
    );

    Ok(())
}

#[sqlx_test]
async fn test_get_machine_boot_interfaces_agrees_when_only_owned_rows_exist(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let (env, host) = init(pool).await;
    let host_id = host.host.id;

    let report = env
        .api()
        .get_machine_boot_interfaces(tonic::Request::new(
            forge::GetMachineBootInterfacesRequest {
                machine_id: Some(host_id.into()),
            },
        ))
        .await?
        .into_inner();

    // No predictions seeded.
    assert!(report.predicted_interfaces.is_empty());
    // The owned primary is the effective pick.
    assert!(report.effective_boot_interface_mac.is_some());
    // No predictions -> no predicted pick; the owned rows still yield a
    // default (primary-masked) pick.
    assert!(report.predicted_boot_interface.is_none());
    assert!(report.default_boot_interface.is_some());

    // With at most one distinct boot-MAC signal (the owned pick; the explored
    // default, when recorded, names the same boot NIC for a DPU host), the
    // stores do not diverge.
    assert!(
        !report.divergent,
        "a freshly ingested host with no conflicting prediction should not diverge"
    );

    Ok(())
}

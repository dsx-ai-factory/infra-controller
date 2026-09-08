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

// The deprecated fields on `rpc::forge::Machine` must still be read here for
// backwards-compat. See https://github.com/NVIDIA/infra-controller/issues/2793
#![allow(deprecated)]

use std::sync::Arc;
use std::time::Duration;

use carbide_site_explorer::config::SiteExplorerConfig;
use carbide_site_explorer::test_support::{MockEndpointExplorer, TestSiteExplorer};
use carbide_site_explorer::{EndpointExplorationService, SiteExplorer};
use carbide_test_harness::prelude::*;
use carbide_uuid::machine::MachineId;
use model::expected_machine::{ExpectedMachine, ExpectedMachineData};
use model::machine::machine_search_config::MachineSearchConfig;
use model::site_explorer::EndpointExplorationError;
use rpc::forge::forge_server::Forge;
use tonic::Request;

fn site_explorer_config() -> SiteExplorerConfig {
    SiteExplorerConfig {
        enabled: Arc::new(true.into()),
        retained_boot_interface_window: None,
        explorations_per_run: 10,
        concurrent_explorations: 1,
        run_interval: Duration::from_secs(1),
        create_machines: Arc::new(true.into()),
        ..Default::default()
    }
}

fn health_site_explorer_config() -> SiteExplorerConfig {
    SiteExplorerConfig {
        create_power_shelves: Arc::new(true.into()),
        power_shelves_created_per_run: 1,
        create_switches: Arc::new(true.into()),
        switches_created_per_run: 1,
        ..site_explorer_config()
    }
}

fn test_site_explorer(
    test_harness: &TestHarness,
    explorer_config: SiteExplorerConfig,
) -> TestSiteExplorer {
    let endpoint_explorer = Arc::new(MockEndpointExplorer::default());
    let api = test_harness.api();
    let endpoint_exploration_service = Arc::new(EndpointExplorationService::new(
        api.database_connection.clone(),
        endpoint_explorer.clone(),
        Arc::new(api.runtime_config.get_firmware_config()),
    ));
    let site_explorer = SiteExplorer::new(
        api.database_connection.clone(),
        explorer_config,
        test_harness.test_meter.meter(),
        endpoint_exploration_service,
        endpoint_explorer.clone(),
        api.common_pools().clone(),
        api.work_lock_manager_handle(),
        api.runtime_config.rack_profiles.clone(),
        None,
        api.credential_manager().clone(),
    );
    TestSiteExplorer::new(site_explorer, endpoint_explorer)
}

async fn find_machine(
    api: &Api,
    machine_id: MachineId,
) -> Result<rpc::forge::Machine, Box<dyn std::error::Error>> {
    let mut machines = api
        .find_machines_by_ids(Request::new(rpc::forge::MachinesByIdsRequest {
            machine_ids: vec![machine_id],
            include_history: true,
        }))
        .await?
        .into_inner()
        .machines;
    assert_eq!(machines.len(), 1);

    Ok(machines.remove(0))
}

#[sqlx_test]
async fn test_site_explorer_health_report(pool: PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let test_harness = TestHarness::builder(pool.clone())
        .with_resource_pools(ResourcePoolBuilder::default().build())
        .build()
        .await;
    let domain = test_harness.test_domain().await;
    let network_controller = test_harness.network_controller();
    let underlay_segment = network_controller.create_underlay_segment(&domain).await;
    network_controller.create_admin_segment(&domain).await;
    let explorer = test_site_explorer(&test_harness, health_site_explorer_config());

    // Start with successful site explorer iterations to update
    // ExploredEndpoints with valid info and create the managed host.
    // Site Explorer needs to run against BMC IPs on an underlay segment. The
    // old api-core fixture mutated a tenant segment into underlay for this;
    // TestHarness creates an underlay segment directly.
    let (created_host, build_data) = test_harness
        .managed_host_builder(&explorer, underlay_segment)
        .with_dpu_network_status_reported()
        .build()
        .await;
    let host_bmc_ip = build_data.host_bmc_ip();

    let host_machine = find_machine(test_harness.api(), created_host.host.id.into()).await?;
    let alerts = &host_machine.health.as_ref().unwrap().alerts;
    assert!(
        alerts.is_empty(),
        "expected no health alerts after successful exploration, got: {alerts:#?}"
    );

    // Now mark the Machine as unreachable. A health alert should be emitted
    explorer.insert_endpoint_result(
        host_bmc_ip,
        Err(EndpointExplorationError::Unreachable { details: None }),
    );

    explorer.run_single_iteration().await?;

    let host_machine = find_machine(test_harness.api(), created_host.host.id.into()).await?;

    let mut alerts = host_machine.health.as_ref().unwrap().alerts.clone();
    assert_eq!(
        alerts.len(),
        1,
        "expected exactly one health alert after failed exploration, got: {alerts:#?}"
    );
    for alert in alerts.iter_mut() {
        assert!(alert.in_alert_since.is_some());
        alert.in_alert_since = None;
    }
    alerts
        .sort_by(|alert1, alert2| (&alert1.id, &alert1.target).cmp(&(&alert2.id, &alert2.target)));
    assert_eq!(
        alerts,
        vec![rpc::health::HealthProbeAlert {
            id: "BmcExplorationFailure".to_string(),
            target: Some(host_bmc_ip.to_string()),
            in_alert_since: None,
            message: "Endpoint exploration failed: the endpoint was not reachable due to a generic network issue: None"
                .to_string(),
            tenant_message: None,
            classifications: vec!["PreventAllocations".to_string()]
        }]
    );

    let stored_site_explorer_report =
        db::machine::find_one(&pool, &created_host.host.id, MachineSearchConfig::default())
            .await?
            .unwrap()
            .site_explorer_health_report()
            .cloned()
            .unwrap();

    // A steady-state pass with the same endpoint result should not rewrite the
    // site-explorer merge report.
    explorer.run_single_iteration().await?;

    let next_site_explorer_report =
        db::machine::find_one(&pool, &created_host.host.id, MachineSearchConfig::default())
            .await?
            .unwrap()
            .site_explorer_health_report()
            .cloned()
            .unwrap();
    assert_eq!(next_site_explorer_report, stored_site_explorer_report);

    Ok(())
}

#[sqlx_test]
async fn test_site_explorer_machine_audit_state_bulk_lookup_matches_existing_path(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let test_harness = TestHarness::builder(pool.clone()).build().await;
    let domain = test_harness.test_domain().await;
    let network_controller = test_harness.network_controller();
    let underlay_segment = network_controller.create_underlay_segment(&domain).await;
    network_controller.create_admin_segment(&domain).await;
    let explorer = test_site_explorer(&test_harness, site_explorer_config());
    let (created_host, build_data) = test_harness
        .managed_host_builder(&explorer, underlay_segment)
        .build()
        .await;

    let host_bmc_ip = build_data.host_bmc_ip();
    let dpu_bmc_ip = build_data.first_dpu_bmc_ip();
    let unknown_bmc_ip = "192.0.2.10".parse()?;

    let mut site_explorer_report = health_report::HealthReport::empty(
        health_report::HealthReport::SITE_EXPLORER_SOURCE.into(),
    );
    site_explorer_report
        .alerts
        .push(health_report::HealthProbeAlert {
            id: "BmcExplorationFailure".parse().unwrap(),
            target: Some(host_bmc_ip.to_string()),
            in_alert_since: None,
            message: "Endpoint exploration failed".to_string(),
            tenant_message: None,
            classifications: vec![health_report::HealthAlertClassification::prevent_allocations()],
        });

    let mut txn = db::Transaction::begin(&pool).await?;
    db::machine::update_site_explorer_health_report(
        txn.as_pgconn(),
        &created_host.host.id,
        &site_explorer_report,
    )
    .await?;
    txn.commit().await?;

    let bmc_ips = vec![host_bmc_ip, dpu_bmc_ip, unknown_bmc_ip];
    let mut existing_path_states = Vec::new();
    let mut txn = db::Transaction::begin(&pool).await?;
    for bmc_ip in &bmc_ips {
        let Some(machine_id) = db::machine::find_id_by_bmc_ip(txn.as_pgconn(), bmc_ip).await?
        else {
            continue;
        };
        let machine = db::machine::find(
            &mut txn,
            db::ObjectFilter::One(machine_id),
            MachineSearchConfig {
                include_dpus: true,
                include_predicted_host: true,
                ..Default::default()
            },
        )
        .await?
        .into_iter()
        .next()
        .unwrap();
        existing_path_states.push(db::machine::SiteExplorerMachineAuditState {
            bmc_ip: *bmc_ip,
            machine_id,
            site_explorer_health_report: machine.site_explorer_health_report().cloned(),
        });
    }
    txn.commit().await?;

    let mut txn = db::Transaction::begin(&pool).await?;
    let mut bulk_lookup_states =
        db::machine::find_site_explorer_machine_audit_states_by_bmc_ips(&mut txn, &bmc_ips).await?;
    txn.commit().await?;

    existing_path_states.sort_by_key(|state| state.bmc_ip);
    bulk_lookup_states.sort_by_key(|state| state.bmc_ip);

    assert_eq!(bulk_lookup_states, existing_path_states);

    Ok(())
}

/// A Managed Host whose `expected_machines` row is later removed becomes an
/// orphan: `audit_exploration_results` emits an `OrphanManagedHost` health
/// alert on the host's Machine. Re-adding the entry clears the alert on the
/// next iteration.
#[sqlx_test]
async fn test_orphan_managed_host_alert_emitted(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let test_harness = TestHarness::builder(pool.clone()).build().await;
    let domain = test_harness.test_domain().await;
    let network_controller = test_harness.network_controller();
    let underlay_segment = network_controller.create_underlay_segment(&domain).await;
    network_controller.create_admin_segment(&domain).await;
    let explorer = test_site_explorer(&test_harness, site_explorer_config());
    let (created_host, _) = test_harness
        .managed_host_builder(&explorer, underlay_segment)
        .build()
        .await;
    let host_bmc_mac = created_host.host.bmc_mac;
    let chassis_serial = created_host
        .host
        .serial()
        .expect("created host should have a serial number");

    // Orphan the host by deleting its expected_machines entry.
    let mut txn = pool.begin().await?;
    db::expected_machine::delete_by_mac(&mut txn, host_bmc_mac).await?;
    txn.commit().await?;

    // Run an iteration: audit_exploration_results should emit the orphan alert.
    explorer.run_single_iteration().await?;
    let alerts = find_machine(test_harness.api(), created_host.host.id.into())
        .await?
        .health
        .unwrap()
        .alerts;
    assert!(
        alerts.iter().any(|a| a.id == "OrphanManagedHost"),
        "expected OrphanManagedHost alert, got: {alerts:#?}"
    );

    // Re-add the expected_machines entry. The alert should clear next iteration.
    let mut txn = pool.begin().await?;
    db::expected_machine::create(
        &mut txn,
        ExpectedMachine {
            id: None,
            bmc_mac_address: host_bmc_mac,
            data: ExpectedMachineData {
                serial_number: chassis_serial,
                ..Default::default()
            },
        },
    )
    .await?;
    txn.commit().await?;

    explorer.run_single_iteration().await?;
    let alerts = find_machine(test_harness.api(), created_host.host.id.into())
        .await?
        .health
        .unwrap()
        .alerts;
    assert!(
        !alerts.iter().any(|a| a.id == "OrphanManagedHost"),
        "expected no OrphanManagedHost alert after re-adding expected_machines, got: {alerts:#?}"
    );

    Ok(())
}

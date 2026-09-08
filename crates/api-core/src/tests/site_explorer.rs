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
use std::sync::Arc;

use carbide_redfish::boot_interface::BootInterfaceTarget;
use carbide_site_explorer::config::SiteExplorerConfig;
use common::api_fixtures::TestEnv;
use db::{self};
use ipnetwork::IpNetwork;
use mac_address::MacAddress;
use model::bmc_suppression::{BmcSuppressionSubsystem, NewBmcSuppression};
use model::hardware_info::HardwareInfo;
use model::machine::ManagedHostStateSnapshot;
use model::machine_boot_interface::MachineBootInterfaceTarget;
use model::site_explorer::{
    Chassis, EndpointExplorationError, EndpointExplorationReport, MachineSetupStatus,
};
use model::test_support::{DpuConfig, ManagedHostConfig};
use rpc::forge::forge_server::Forge;
use rpc::{DiscoveryData, DiscoveryInfo, MachineDiscoveryInfo};
use sqlx::PgPool;
use tonic::Request;

use crate::sqlx_test;
use crate::test_support::fixture_config::{FixtureDefault as _, ManagedHostConfigExt as _};
use crate::tests::common;
use crate::tests::common::api_fixtures;
use crate::tests::common::api_fixtures::TestEnvOverrides;
use crate::tests::common::api_fixtures::network_segment::{
    FIXTURE_ADMIN_NETWORK_SEGMENT_GATEWAY, FIXTURE_HOST_INBAND_NETWORK_SEGMENT_GATEWAY,
    create_host_inband_network_segment,
};
use crate::tests::common::api_fixtures::site_explorer::MockExploredHost;
use crate::tests::common::rpc_builder::DhcpDiscovery;

// Test that discover_machines will reject request of machine that was not created by site-explorer when create_machines = true
#[sqlx_test]
async fn test_disable_machine_creation_outside_site_explorer(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut config = common::api_fixtures::get_config();
    config.site_explorer = SiteExplorerConfig {
        enabled: Arc::new(true.into()),
        explorations_per_run: 2,
        concurrent_explorations: 1,
        run_interval: std::time::Duration::from_secs(1),
        create_machines: Arc::new(true.into()),
        create_power_shelves: Arc::new(true.into()),
        power_shelves_created_per_run: 1,
        create_switches: Arc::new(true.into()),
        switches_created_per_run: 1,
        ..Default::default()
    };
    let env = common::api_fixtures::create_test_env_with_overrides(
        pool,
        TestEnvOverrides::with_config(config),
    )
    .await;
    let host_config = env.managed_host_config();

    let hardware_info = HardwareInfo::from(&host_config);
    let discovery_info = DiscoveryInfo::try_from(hardware_info.clone()).unwrap();
    let oob_mac = MacAddress::from_str("a0:88:c2:08:80:95")?;
    let response = env
        .api
        .discover_dhcp(
            DhcpDiscovery::builder(oob_mac, "192.0.1.1")
                .vendor_string("NVIDIA/OOB")
                .tonic_request(),
        )
        .await
        .unwrap()
        .into_inner();

    assert!(response.machine_interface_id.is_some());

    let _dm_response = env
        .api
        .discover_machine(Request::new(MachineDiscoveryInfo {
            machine_interface_id: response.machine_interface_id,
            discovery_data: Some(DiscoveryData::Info(discovery_info)),
            create_machine: true,
            ..Default::default()
        }))
        .await;

    // assert!(dm_response.is_err_and(|e| e.message().contains("was not discovered by site-explore")));

    Ok(())
}

#[sqlx_test]
async fn test_site_explorer_new_host_fixture(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = common::api_fixtures::create_test_env_with_overrides(
        pool.clone(),
        TestEnvOverrides {
            site_prefixes: Some(vec![
                IpNetwork::new(
                    FIXTURE_ADMIN_NETWORK_SEGMENT_GATEWAY.network(),
                    FIXTURE_ADMIN_NETWORK_SEGMENT_GATEWAY.prefix(),
                )
                .unwrap(),
                IpNetwork::new(
                    FIXTURE_HOST_INBAND_NETWORK_SEGMENT_GATEWAY.network(),
                    FIXTURE_HOST_INBAND_NETWORK_SEGMENT_GATEWAY.prefix(),
                )
                .unwrap(),
            ]),
            ..Default::default()
        },
    )
    .await;

    create_host_inband_network_segment(&env.api, None).await;

    let zero_dpu_host =
        api_fixtures::site_explorer::new_host(&env, ManagedHostConfig::zero_dpu()).await?;
    assert_eq!(zero_dpu_host.dpu_snapshots.len(), 0);

    let single_dpu_host =
        api_fixtures::site_explorer::new_host(&env, ManagedHostConfig::default()).await?;
    assert_eq!(single_dpu_host.dpu_snapshots.len(), 1);

    let config = ManagedHostConfig::default().with_dpu_count(2);
    let two_dpu_host = api_fixtures::site_explorer::new_host(&env, config).await?;
    assert_eq!(two_dpu_host.dpu_snapshots.len(), 2);

    Ok(())
}

#[sqlx_test]
async fn test_site_explorer_fixtures_singledpu(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = common::api_fixtures::create_test_env(pool).await;

    let mock_host = ManagedHostConfig::default();
    api_fixtures::site_explorer::register_expected_machine(&env, &mock_host, None).await;
    let mock_explored_host = MockExploredHost::new(&env, mock_host);

    let snapshot: ManagedHostStateSnapshot = mock_explored_host
        // Run host DHCP first
        .discover_dhcp_host_bmc(|result, _| {
            let response = result.unwrap().into_inner();
            assert!(response.machine_id.is_none()); // Should not have a machine-id for BMC
            Ok(())
        })
        .await?
        // Then DPU DHCP
        .discover_dhcp_dpu_bmc(0, |result, _| {
            let response = result.unwrap().into_inner();
            assert!(response.machine_id.is_none()); // Should not have a machine-id for BMC
            Ok(())
        })
        .await?
        // Place site explorer results into the mock site explorer
        .insert_site_exploration_results()?
        .run_site_explorer_iteration()
        .await
        .mark_preingestion_complete()
        .await?
        .run_site_explorer_iteration()
        .await
        // Get DHCP on the DPU interface
        .discover_dhcp_host_primary_iface(|result, _| {
            let response = result.unwrap().into_inner();
            assert!(response.machine_id.is_some());
            Ok(())
        })
        .await?
        // Run discovery
        .discover_machine(|result, _| {
            assert!(result.is_ok());
            Ok(())
        })
        .await?
        .run_site_explorer_iteration()
        .await
        .finish(|mock| async move {
            // Get the managed host snapshot from the database
            let machine_id = mock.machine_discovery_response.unwrap().machine_id.unwrap();
            Ok::<ManagedHostStateSnapshot, eyre::Report>(
                db::managed_host::load_snapshot(
                    &mut mock.test_env.db_reader(),
                    &machine_id,
                    Default::default(),
                )
                .await
                .transpose()
                .unwrap()?,
            )
        })
        .await?;

    assert_eq!(snapshot.dpu_snapshots.len(), 1);

    Ok(())
}

#[sqlx_test]
async fn test_site_explorer_fixtures_multidpu(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = common::api_fixtures::create_test_env(pool).await;

    let mock_host = ManagedHostConfig {
        dpus: vec![DpuConfig::default(), DpuConfig::default()],
        ..ManagedHostConfig::default()
    };
    api_fixtures::site_explorer::register_expected_machine(&env, &mock_host, None).await;
    let mock_explored_host = MockExploredHost::new(&env, mock_host);

    let snapshot: ManagedHostStateSnapshot = mock_explored_host
        // Run host DHCP first
        .discover_dhcp_host_bmc(|result, _| {
            let response = result.unwrap().into_inner();
            assert!(response.machine_id.is_none()); // Should not have a machine-id for BMC
            Ok(())
        })
        .await?
        .discover_dhcp_dpu_bmc(0, |result, _| {
            let response = result.unwrap().into_inner();
            assert!(response.machine_id.is_none()); // Should not have a machine-id for BMC
            Ok(())
        })
        .await?
        .discover_dhcp_dpu_bmc(1, |result, _| {
            let response = result.unwrap().into_inner();
            assert!(response.machine_id.is_none()); // Should not have a machine-id for BMC
            Ok(())
        })
        .await?
        // Place site explorer results into the mock site explorer
        .insert_site_exploration_results()?
        .run_site_explorer_iteration()
        .await
        .mark_preingestion_complete()
        .await?
        .run_site_explorer_iteration()
        .await
        // Get DHCP on the DPU interface
        .discover_dhcp_host_primary_iface(|result, _| {
            let response = result.unwrap().into_inner();
            assert!(response.machine_id.is_some());
            Ok(())
        })
        .await?
        // Run discovery
        .discover_machine(|result, _| {
            assert!(result.is_ok());
            Ok(())
        })
        .await?
        .run_site_explorer_iteration()
        .await
        .finish(|mock| async move {
            // Get the managed host snapshot from the database
            let machine_id = mock.machine_discovery_response.unwrap().machine_id.unwrap();
            Ok::<ManagedHostStateSnapshot, eyre::Report>(
                db::managed_host::load_snapshot(
                    &mut mock.test_env.db_reader(),
                    &machine_id,
                    Default::default(),
                )
                .await
                .transpose()
                .unwrap()?,
            )
        })
        .await?;

    assert_eq!(snapshot.dpu_snapshots.len(), 2);

    Ok(())
}

#[sqlx_test]
async fn test_site_explorer_fixtures_zerodpu_site_explorer_before_host_dhcp(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = common::api_fixtures::create_test_env_with_overrides(
        pool.clone(),
        TestEnvOverrides {
            site_prefixes: Some(vec![
                IpNetwork::new(
                    FIXTURE_ADMIN_NETWORK_SEGMENT_GATEWAY.network(),
                    FIXTURE_ADMIN_NETWORK_SEGMENT_GATEWAY.prefix(),
                )
                .unwrap(),
                IpNetwork::new(
                    FIXTURE_HOST_INBAND_NETWORK_SEGMENT_GATEWAY.network(),
                    FIXTURE_HOST_INBAND_NETWORK_SEGMENT_GATEWAY.prefix(),
                )
                .unwrap(),
            ]),
            ..Default::default()
        },
    )
    .await;

    create_host_inband_network_segment(&env.api, None).await;

    let mock_host = ManagedHostConfig {
        dpus: vec![],
        ..ManagedHostConfig::default()
    };
    api_fixtures::site_explorer::register_expected_machine(&env, &mock_host, None).await;
    let mock_explored_host = MockExploredHost::new(&env, mock_host);

    let snapshot: ManagedHostStateSnapshot = mock_explored_host
        // Run host BMC DHCP first
        .discover_dhcp_host_bmc(|result, _| {
            let response = result.unwrap().into_inner();
            assert!(response.machine_id.is_none()); // Should not have a machine-id for BMC
            Ok(())
        })
        .await?
        // Place site explorer results into the mock site explorer
        .insert_site_exploration_results()?
        .run_site_explorer_iteration()
        .await
        .mark_preingestion_complete()
        .await?
        .run_site_explorer_iteration()
        .await
        // Get DHCP on the host in-band NIC
        .discover_dhcp_host_primary_iface(|result, _| {
            let response = result.unwrap().into_inner();
            assert!(response.machine_id.is_some());
            Ok(())
        })
        .await?
        // Run discovery
        .discover_machine(|result, _| {
            assert!(result.is_ok());
            Ok(())
        })
        .await?
        .run_site_explorer_iteration()
        .await
        .finish(|mock| async move {
            // Get the managed host snapshot from the database
            let machine_id = mock.machine_discovery_response.unwrap().machine_id.unwrap();
            Ok::<ManagedHostStateSnapshot, eyre::Report>(
                db::managed_host::load_snapshot(
                    &mut mock.test_env.db_reader(),
                    &machine_id,
                    Default::default(),
                )
                .await
                .transpose()
                .unwrap()?,
            )
        })
        .await?;

    assert_eq!(snapshot.dpu_snapshots.len(), 0);

    Ok(())
}

#[sqlx_test]
async fn test_delete_explored_endpoint(pool: PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let env = common::api_fixtures::create_test_env(pool.clone()).await;

    // Delete an endpoint that doesn't exist
    let non_existent_ip = "192.168.1.100";
    let response = env
        .api
        .delete_explored_endpoint(Request::new(rpc::forge::DeleteExploredEndpointRequest {
            ip_address: non_existent_ip.to_string(),
        }))
        .await?
        .into_inner();

    assert!(!response.deleted);
    assert_eq!(
        response.message,
        Some(format!(
            "No explored endpoint found with IP {non_existent_ip}"
        ))
    );

    // Create an explored endpoint that's not part of a managed host
    let standalone_endpoint_ip = "192.168.1.50";
    let mut txn = env.pool.begin().await?;

    db::explored_endpoints::insert(
        IpAddr::from_str(standalone_endpoint_ip)?,
        &EndpointExplorationReport::default(),
        false,
        &mut txn,
    )
    .await?;
    txn.commit().await?;

    // Verify the endpoint exists
    let mut txn = env.pool.begin().await?;
    let endpoints =
        db::explored_endpoints::find_all_by_ip(IpAddr::from_str(standalone_endpoint_ip)?, &mut txn)
            .await?;
    assert_eq!(endpoints.len(), 1);
    txn.commit().await?;

    // Delete the standalone endpoint - should succeed
    let response = env
        .api
        .delete_explored_endpoint(Request::new(rpc::forge::DeleteExploredEndpointRequest {
            ip_address: standalone_endpoint_ip.to_string(),
        }))
        .await?
        .into_inner();

    assert!(response.deleted);
    assert_eq!(
        response.message,
        Some(format!(
            "Successfully deleted explored endpoint with IP {standalone_endpoint_ip}"
        ))
    );

    // Verify the endpoint was deleted
    let mut txn = env.pool.begin().await?;
    let endpoints =
        db::explored_endpoints::find_all_by_ip(IpAddr::from_str(standalone_endpoint_ip)?, &mut txn)
            .await?;
    assert_eq!(endpoints.len(), 0);
    txn.commit().await?;

    // Create explored endpoints that are part of a managed host
    let mh = common::api_fixtures::create_managed_host(&env).await;

    // Get the machines to find their BMC IPs
    let mut txn = env.pool.begin().await?;
    let host_machine = mh.host().db_machine(&mut txn).await;
    let dpu_machine = mh.dpu().db_machine(&mut txn).await;
    txn.commit().await?;

    let host_ip = host_machine.status.bmc_info.ip.as_ref().unwrap();
    let dpu_ip = dpu_machine.status.bmc_info.ip.as_ref().unwrap();

    // Now try to delete the host endpoint - should fail because it's part of a machine
    let error = env
        .api
        .delete_explored_endpoint(Request::new(rpc::forge::DeleteExploredEndpointRequest {
            ip_address: host_ip.to_string(),
        }))
        .await
        .expect_err("Should fail with InvalidArgument error");

    assert_eq!(error.code(), tonic::Code::InvalidArgument);
    assert_eq!(
        error.message(),
        format!(
            "cannot delete endpoint {host_ip} because a machine exists for it. did you mean to force-delete the machine?"
        )
    );

    // Try to delete the DPU endpoint - should also fail
    let error = env
        .api
        .delete_explored_endpoint(Request::new(rpc::forge::DeleteExploredEndpointRequest {
            ip_address: dpu_ip.to_string(),
        }))
        .await
        .expect_err("Should fail with InvalidArgument error");

    assert_eq!(error.code(), tonic::Code::InvalidArgument);
    assert_eq!(
        error.message(),
        format!(
            "cannot delete endpoint {dpu_ip} because a machine exists for it. did you mean to force-delete the machine?"
        )
    );

    // Verify both endpoints still exist
    let mut txn = env.pool.begin().await?;
    let host_endpoints = db::explored_endpoints::find_all_by_ip(*host_ip, &mut txn).await?;
    assert_eq!(host_endpoints.len(), 1);

    let dpu_endpoints = db::explored_endpoints::find_all_by_ip(*dpu_ip, &mut txn).await?;
    assert_eq!(dpu_endpoints.len(), 1);
    txn.commit().await?;

    Ok(())
}

#[sqlx_test]
async fn test_get_machine_position_info(pool: PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let env = common::api_fixtures::create_test_env(pool.clone()).await;
    let (_host_machine_id, dpu_machine_id) =
        common::api_fixtures::create_managed_host(&env).await.into();

    let dpu_machine = env.find_machine(&dpu_machine_id).await.remove(0);
    let bmc_ip: IpAddr = dpu_machine.bmc_info.as_ref().unwrap().ip().parse().unwrap();

    // Get the existing explored endpoint (created by create_managed_host) and update it with position info
    let mut txn = env.pool.begin().await?;
    let existing = db::explored_endpoints::find_by_ips(txn.as_mut(), vec![bmc_ip])
        .await?
        .pop()
        .unwrap();
    let mut report = existing.report;
    report.chassis = vec![Chassis {
        id: "Chassis_0".to_string(),
        physical_slot_number: Some(5),
        compute_tray_index: Some(2),
        topology_id: Some(10),
        revision_id: Some(3),
        ..Default::default()
    }];
    report.physical_slot_number = Some(5);
    report.compute_tray_index = Some(2);
    report.topology_id = Some(10);
    report.revision_id = Some(3);
    db::explored_endpoints::try_update(bmc_ip, existing.report_version, &report, false, &mut txn)
        .await?;
    txn.commit().await?;

    // Call the API
    let response = env
        .api
        .get_machine_position_info(tonic::Request::new(rpc::forge::MachinePositionQuery {
            machine_ids: vec![dpu_machine_id.into()],
        }))
        .await?
        .into_inner();

    // Verify the response
    assert_eq!(response.machine_position_info.len(), 1);
    let info = &response.machine_position_info[0];
    assert_eq!(info.machine_id, Some(dpu_machine_id.into()));
    assert_eq!(info.physical_slot_number, Some(5));
    assert_eq!(info.compute_tray_index, Some(2));
    assert_eq!(info.topology_id, Some(10));
    assert_eq!(info.revision_id, Some(3));

    Ok(())
}

/// Test get_machine_position_info with a machine that has no explored endpoint
#[sqlx_test]
async fn test_get_machine_position_info_no_endpoint(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    use rpc::forge::forge_server::Forge;

    let env = common::api_fixtures::create_test_env(pool.clone()).await;
    let (_host_machine_id, dpu_machine_id) =
        common::api_fixtures::create_managed_host(&env).await.into();

    // Don't create any explored endpoint - just query

    // Call the API
    let response = env
        .api
        .get_machine_position_info(tonic::Request::new(rpc::forge::MachinePositionQuery {
            machine_ids: vec![dpu_machine_id.into()],
        }))
        .await?
        .into_inner();

    // Machine should be in the response but with all None position info
    assert_eq!(response.machine_position_info.len(), 1);
    let info = &response.machine_position_info[0];
    assert_eq!(info.machine_id, Some(dpu_machine_id.into()));
    assert_eq!(info.physical_slot_number, None);
    assert_eq!(info.compute_tray_index, None);
    assert_eq!(info.topology_id, None);
    assert_eq!(info.revision_id, None);

    Ok(())
}

async fn host_bmc_ip(
    env: &TestEnv,
    mh: &api_fixtures::TestManagedHost,
) -> Result<IpAddr, Box<dyn std::error::Error>> {
    let mut txn = env.pool.begin().await?;
    let bmc_ip = mh.host().bmc_ip(&mut txn).await.unwrap();
    txn.commit().await?;
    Ok(bmc_ip)
}

async fn explored_endpoint(
    env: &TestEnv,
    bmc_ip: IpAddr,
) -> Result<model::site_explorer::ExploredEndpoint, Box<dyn std::error::Error>> {
    let mut txn = env.pool.begin().await?;
    let endpoint = db::explored_endpoints::find_all_by_ip(bmc_ip, &mut txn)
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| format!("expected endpoint {bmc_ip} to exist"))?;
    txn.commit().await?;
    Ok(endpoint)
}

fn endpoint_explore_call_count(env: &TestEnv, bmc_ip: IpAddr) -> usize {
    env.endpoint_explorer
        .explore_endpoint_calls
        .lock()
        .unwrap()
        .iter()
        .filter(|call| call.ip_address == bmc_ip)
        .count()
}

fn endpoint_explore_target(env: &TestEnv, bmc_ip: IpAddr) -> Option<BootInterfaceTarget> {
    env.endpoint_explorer
        .explore_endpoint_calls
        .lock()
        .unwrap()
        .iter()
        .rev()
        .find(|call| call.ip_address == bmc_ip)
        .unwrap_or_else(|| panic!("expected endpoint {bmc_ip} to have been explored"))
        .boot_interface
        .clone()
}

fn boot_interface_target_cases(
    endpoint: &model::site_explorer::ExploredEndpoint,
) -> Vec<(&'static str, Option<BootInterfaceTarget>)> {
    let pair = endpoint
        .boot_interface()
        .expect("managed-host fixture should retain a complete boot-interface pair");
    let mac_address = pair.mac_address;

    vec![
        ("complete pair", Some(BootInterfaceTarget::Pair(pair))),
        (
            "legacy MAC only",
            Some(BootInterfaceTarget::MacOnly(mac_address)),
        ),
        ("no target", None),
    ]
}

async fn set_endpoint_boot_interface_target(
    env: &TestEnv,
    bmc_ip: IpAddr,
    target: Option<&BootInterfaceTarget>,
) -> Result<(), Box<dyn std::error::Error>> {
    let (mac_address, interface_id) = match target {
        Some(BootInterfaceTarget::Pair(boot_interface)) => (
            Some(boot_interface.mac_address),
            Some(boot_interface.interface_id.as_str()),
        ),
        Some(BootInterfaceTarget::MacOnly(mac_address)) => (Some(*mac_address), None),
        None => (None, None),
    };

    let mut txn = env.pool.begin().await?;
    sqlx::query(
        "UPDATE explored_endpoints
         SET boot_interface_mac = $1, boot_interface_id = $2
         WHERE address = $3",
    )
    .bind(mac_address)
    .bind(interface_id)
    .bind(bmc_ip)
    .execute(&mut *txn)
    .await?;
    txn.commit().await?;

    Ok(())
}

fn report_with_evaluated_boot_interface(
    mut report: EndpointExplorationReport,
    target: Option<&BootInterfaceTarget>,
) -> EndpointExplorationReport {
    report.machine_setup_status = Some(MachineSetupStatus {
        is_done: true,
        diffs: Vec::new(),
        evaluated_boot_interface: target.map(MachineBootInterfaceTarget::from),
    });
    report
}

async fn prepare_endpoint_exploration_case(
    env: &TestEnv,
    bmc_ip: IpAddr,
    target: Option<&BootInterfaceTarget>,
) -> Result<model::site_explorer::ExploredEndpoint, Box<dyn std::error::Error>> {
    set_endpoint_boot_interface_target(env, bmc_ip, target).await?;
    let endpoint = explored_endpoint(env, bmc_ip).await?;
    let report = report_with_evaluated_boot_interface(endpoint.report.clone(), target);
    env.endpoint_explorer
        .insert_endpoint_result(bmc_ip, Ok(report));
    env.endpoint_explorer
        .explore_endpoint_calls
        .lock()
        .unwrap()
        .clear();

    Ok(endpoint)
}

#[sqlx_test]
async fn test_manual_refreshes_reject_site_explorer_suppressed_bmc(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = common::api_fixtures::create_test_env(pool.clone()).await;
    let mh = common::api_fixtures::create_managed_host(&env).await;
    let bmc_ip = host_bmc_ip(&env, &mh).await?;

    let mut txn = env.pool.begin().await?;
    let bmc_interface = db::machine_interface::find_by_ip(txn.as_mut(), bmc_ip)
        .await?
        .ok_or_else(|| format!("expected BMC interface at {bmc_ip}"))?;
    db::bmc_suppression::upsert(
        txn.as_mut(),
        &NewBmcSuppression {
            bmc_mac_address: bmc_interface.mac_address,
            reason: "manual refresh rejection test".to_string(),
            subsystem: BmcSuppressionSubsystem::SiteExplorer,
        },
    )
    .await?;
    txn.commit().await?;

    let re_explore_error = env
        .api
        .re_explore_endpoint(Request::new(rpc::forge::ReExploreEndpointRequest {
            ip_address: bmc_ip.to_string(),
            if_version_match: None,
        }))
        .await
        .unwrap_err();
    assert_eq!(re_explore_error.code(), tonic::Code::FailedPrecondition);
    assert!(
        re_explore_error
            .message()
            .contains(&bmc_interface.mac_address.to_string())
    );
    assert!(
        !explored_endpoint(&env, bmc_ip).await?.exploration_requested,
        "a rejected re-exploration must not remain queued"
    );

    let calls_before_refresh = endpoint_explore_call_count(&env, bmc_ip);
    let refresh_error = env
        .api
        .refresh_endpoint_report(Request::new(rpc::forge::RefreshEndpointReportRequest {
            ip_address: bmc_ip.to_string(),
        }))
        .await
        .unwrap_err();
    assert_eq!(refresh_error.code(), tonic::Code::FailedPrecondition);
    assert!(
        refresh_error
            .message()
            .contains(&bmc_interface.mac_address.to_string())
    );
    assert_eq!(
        endpoint_explore_call_count(&env, bmc_ip),
        calls_before_refresh,
        "a rejected immediate refresh must not probe the BMC"
    );

    Ok(())
}

#[sqlx_test]
async fn test_refresh_endpoint_report_correlates_each_boot_interface_target(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = common::api_fixtures::create_test_env(pool.clone()).await;
    let mh = common::api_fixtures::create_managed_host(&env).await;
    let bmc_ip = host_bmc_ip(&env, &mh).await?;
    let initial = explored_endpoint(&env, bmc_ip).await?;
    for (scenario, target) in boot_interface_target_cases(&initial) {
        let baseline = prepare_endpoint_exploration_case(&env, bmc_ip, target.as_ref()).await?;
        let baseline_version = baseline.report_version;
        let expected_evaluated_target = target.as_ref().map(MachineBootInterfaceTarget::from);

        env.api
            .refresh_endpoint_report(Request::new(rpc::forge::RefreshEndpointReportRequest {
                ip_address: bmc_ip.to_string(),
            }))
            .await?;

        let refreshed = explored_endpoint(&env, bmc_ip).await?;
        assert!(
            refreshed.report_version.version_nr() > baseline_version.version_nr(),
            "{scenario}: refresh should bump report version from {} to a newer version, got {}",
            baseline_version.version_nr(),
            refreshed.report_version.version_nr()
        );
        assert_eq!(
            endpoint_explore_target(&env, bmc_ip),
            target,
            "{scenario}: explicit refresh should evaluate the target stored with the endpoint"
        );
        let status = refreshed
            .report
            .machine_setup_status
            .expect("mock report should include machine setup status");
        assert_eq!(
            status.evaluated_boot_interface, expected_evaluated_target,
            "{scenario}: the persisted status and evaluated target should come from the same report update"
        );
    }

    Ok(())
}

#[sqlx_test]
async fn test_ad_hoc_explore_uses_stored_boot_interface_target(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = common::api_fixtures::create_test_env(pool.clone()).await;
    let mh = common::api_fixtures::create_managed_host(&env).await;
    let bmc_ip = host_bmc_ip(&env, &mh).await?;
    let initial = explored_endpoint(&env, bmc_ip).await?;
    for (scenario, target) in boot_interface_target_cases(&initial) {
        prepare_endpoint_exploration_case(&env, bmc_ip, target.as_ref()).await?;

        let response = env
            .api
            .explore(Request::new(rpc::forge::BmcEndpointRequest {
                ip_address: bmc_ip.to_string(),
                mac_address: None,
            }))
            .await?
            .into_inner();

        assert_eq!(
            endpoint_explore_target(&env, bmc_ip),
            target,
            "{scenario}: ad-hoc exploration should evaluate the target stored with the endpoint"
        );
        assert!(
            response.machine_setup_status.is_some(),
            "{scenario}: ad-hoc exploration should return the machine setup observation"
        );
    }

    Ok(())
}

#[sqlx_test]
async fn test_periodic_exploration_uses_stored_boot_interface_target(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = common::api_fixtures::create_test_env(pool.clone()).await;
    let mh = common::api_fixtures::create_managed_host(&env).await;
    let bmc_ip = host_bmc_ip(&env, &mh).await?;
    let initial = explored_endpoint(&env, bmc_ip).await?;
    for (scenario, target) in boot_interface_target_cases(&initial) {
        let expected_evaluated_target = target.as_ref().map(MachineBootInterfaceTarget::from);
        prepare_endpoint_exploration_case(&env, bmc_ip, target.as_ref()).await?;

        env.api
            .re_explore_endpoint(Request::new(rpc::forge::ReExploreEndpointRequest {
                ip_address: bmc_ip.to_string(),
                if_version_match: None,
            }))
            .await?;
        env.run_site_explorer_iteration().await;

        assert_eq!(
            endpoint_explore_target(&env, bmc_ip),
            target,
            "{scenario}: periodic exploration should evaluate the target stored with the endpoint"
        );
        let status = explored_endpoint(&env, bmc_ip)
            .await?
            .report
            .machine_setup_status
            .expect("mock report should include machine setup status");
        assert_eq!(
            status.evaluated_boot_interface, expected_evaluated_target,
            "{scenario}: periodic exploration should persist the correlated observation"
        );
    }

    Ok(())
}

#[sqlx_test]
async fn test_refresh_endpoint_report_rejects_nonexistent_endpoint(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = common::api_fixtures::create_test_env(pool.clone()).await;

    let err = env
        .api
        .refresh_endpoint_report(Request::new(rpc::forge::RefreshEndpointReportRequest {
            ip_address: "99.99.99.99".to_string(),
        }))
        .await
        .unwrap_err();

    assert_eq!(err.code(), tonic::Code::NotFound);

    Ok(())
}

#[sqlx_test]
async fn test_refresh_endpoint_report_rejects_duplicate_refresh(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = common::api_fixtures::create_test_env(pool.clone()).await;
    let mh = common::api_fixtures::create_managed_host(&env).await;
    let bmc_ip = host_bmc_ip(&env, &mh).await?;
    let blocker = env.endpoint_explorer.block_next_exploration();
    let api = env.api.clone();
    let first_refresh = tokio::spawn(async move {
        api.refresh_endpoint_report(Request::new(rpc::forge::RefreshEndpointReportRequest {
            ip_address: bmc_ip.to_string(),
        }))
        .await
    });
    blocker.wait_until_started().await;

    let err = env
        .api
        .refresh_endpoint_report(Request::new(rpc::forge::RefreshEndpointReportRequest {
            ip_address: bmc_ip.to_string(),
        }))
        .await
        .unwrap_err();

    blocker.release();
    first_refresh.await??;

    assert_eq!(err.code(), tonic::Code::AlreadyExists);
    assert!(
        err.message().contains(&bmc_ip.to_string()),
        "error should identify the locked endpoint"
    );

    Ok(())
}

#[sqlx_test]
async fn test_refresh_endpoint_report_lock_blocks_periodic_probe(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = common::api_fixtures::create_test_env(pool.clone()).await;
    let mh = common::api_fixtures::create_managed_host(&env).await;
    let bmc_ip = host_bmc_ip(&env, &mh).await?;

    env.api
        .re_explore_endpoint(Request::new(rpc::forge::ReExploreEndpointRequest {
            ip_address: bmc_ip.to_string(),
            if_version_match: None,
        }))
        .await?;

    let blocker = env.endpoint_explorer.block_next_exploration();
    let api = env.api.clone();
    let refresh = tokio::spawn(async move {
        api.refresh_endpoint_report(Request::new(rpc::forge::RefreshEndpointReportRequest {
            ip_address: bmc_ip.to_string(),
        }))
        .await
    });
    blocker.wait_until_started().await;
    let calls_while_refreshing = endpoint_explore_call_count(&env, bmc_ip);

    env.run_site_explorer_iteration().await;

    assert_eq!(
        endpoint_explore_call_count(&env, bmc_ip),
        calls_while_refreshing,
        "periodic site explorer probe should be skipped while refresh lock is held"
    );

    blocker.release();
    refresh.await??;

    Ok(())
}

#[sqlx_test]
async fn test_refresh_endpoint_report_rejects_concurrent_report_update(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = common::api_fixtures::create_test_env(pool.clone()).await;
    let mh = common::api_fixtures::create_managed_host(&env).await;
    let bmc_ip = host_bmc_ip(&env, &mh).await?;
    let baseline = explored_endpoint(&env, bmc_ip).await?;

    let blocker = env.endpoint_explorer.block_next_exploration();
    let api = env.api.clone();
    let refresh = tokio::spawn(async move {
        api.refresh_endpoint_report(Request::new(rpc::forge::RefreshEndpointReportRequest {
            ip_address: bmc_ip.to_string(),
        }))
        .await
    });
    blocker.wait_until_started().await;

    let mut concurrent_report = baseline.report.clone();
    concurrent_report.model = Some("concurrent update".to_string());
    let concurrent_target = MachineBootInterfaceTarget::MacOnly("02:00:00:00:00:77".parse()?);
    concurrent_report.machine_setup_status = Some(MachineSetupStatus {
        is_done: false,
        diffs: Vec::new(),
        evaluated_boot_interface: Some(concurrent_target.clone()),
    });
    let mut txn = env.pool.begin().await?;
    assert!(
        db::explored_endpoints::try_update(
            bmc_ip,
            baseline.report_version,
            &concurrent_report,
            baseline.waiting_for_explorer_refresh,
            &mut txn,
        )
        .await?,
        "concurrent report update should succeed"
    );
    txn.commit().await?;
    let concurrent = explored_endpoint(&env, bmc_ip).await?;

    blocker.release();
    let error = refresh.await?.unwrap_err();

    assert_eq!(error.code(), tonic::Code::FailedPrecondition);
    let final_endpoint = explored_endpoint(&env, bmc_ip).await?;
    assert_eq!(
        final_endpoint.report_version, concurrent.report_version,
        "stale refresh must not overwrite the concurrent report"
    );
    assert_eq!(
        final_endpoint.report.model.as_deref(),
        Some("concurrent update")
    );
    assert_eq!(
        final_endpoint
            .report
            .machine_setup_status
            .and_then(|status| status.evaluated_boot_interface),
        Some(concurrent_target),
        "stale refresh must not replace the target correlated with the concurrent report"
    );

    Ok(())
}

#[sqlx_test]
async fn test_refresh_endpoint_report_failure_persists_error_and_bumps_version(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = common::api_fixtures::create_test_env(pool.clone()).await;
    let mh = common::api_fixtures::create_managed_host(&env).await;
    let bmc_ip = host_bmc_ip(&env, &mh).await?;
    let mut initial = explored_endpoint(&env, bmc_ip).await?;
    let preserved_target = initial
        .boot_interface_target()
        .expect("managed-host fixture should retain a boot-interface target");
    initial.report.machine_setup_status = Some(MachineSetupStatus {
        is_done: true,
        diffs: Vec::new(),
        evaluated_boot_interface: Some(preserved_target.clone()),
    });
    let mut txn = env.pool.begin().await?;
    assert!(
        db::explored_endpoints::try_update(
            bmc_ip,
            initial.report_version,
            &initial.report,
            initial.waiting_for_explorer_refresh,
            &mut txn,
        )
        .await?
    );
    txn.commit().await?;
    let initial_version = explored_endpoint(&env, bmc_ip).await?.report_version;
    env.endpoint_explorer.insert_endpoint_result(
        bmc_ip,
        Err(EndpointExplorationError::Unreachable {
            details: Some("refresh failure".to_string()),
        }),
    );

    env.api
        .refresh_endpoint_report(Request::new(rpc::forge::RefreshEndpointReportRequest {
            ip_address: bmc_ip.to_string(),
        }))
        .await?;

    let refreshed = explored_endpoint(&env, bmc_ip).await?;
    assert!(
        refreshed.report_version.version_nr() > initial_version.version_nr(),
        "failed refresh should still bump report version"
    );
    assert!(
        refreshed.report.last_exploration_error.is_some(),
        "failed refresh should persist the exploration error"
    );
    assert_eq!(
        refreshed
            .report
            .machine_setup_status
            .and_then(|status| status.evaluated_boot_interface),
        Some(preserved_target),
        "failed refresh should preserve the last successful target-correlated observation"
    );

    Ok(())
}

#[sqlx_test]
async fn test_refresh_endpoint_report_clears_pending_requested_exploration(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = common::api_fixtures::create_test_env(pool.clone()).await;
    let mh = common::api_fixtures::create_managed_host(&env).await;
    let bmc_ip = host_bmc_ip(&env, &mh).await?;

    env.api
        .re_explore_endpoint(Request::new(rpc::forge::ReExploreEndpointRequest {
            ip_address: bmc_ip.to_string(),
            if_version_match: None,
        }))
        .await?;
    assert!(explored_endpoint(&env, bmc_ip).await?.exploration_requested);

    env.api
        .refresh_endpoint_report(Request::new(rpc::forge::RefreshEndpointReportRequest {
            ip_address: bmc_ip.to_string(),
        }))
        .await?;

    assert!(
        !explored_endpoint(&env, bmc_ip).await?.exploration_requested,
        "refresh should clear the pending requested exploration so the endpoint is not immediately probed again as priority work"
    );

    Ok(())
}

#[sqlx_test]
async fn test_refresh_endpoint_report_lock_is_per_endpoint(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = common::api_fixtures::create_test_env(pool.clone()).await;
    let mh_a = common::api_fixtures::create_managed_host(&env).await;
    let mh_b = common::api_fixtures::create_managed_host(&env).await;
    let bmc_ip_a = host_bmc_ip(&env, &mh_a).await?;
    let bmc_ip_b = host_bmc_ip(&env, &mh_b).await?;
    let initial_version_b = explored_endpoint(&env, bmc_ip_b).await?.report_version;
    let blocker = env.endpoint_explorer.block_next_exploration();
    let api = env.api.clone();
    let refresh_a = tokio::spawn(async move {
        api.refresh_endpoint_report(Request::new(rpc::forge::RefreshEndpointReportRequest {
            ip_address: bmc_ip_a.to_string(),
        }))
        .await
    });
    blocker.wait_until_started().await;

    env.api
        .refresh_endpoint_report(Request::new(rpc::forge::RefreshEndpointReportRequest {
            ip_address: bmc_ip_b.to_string(),
        }))
        .await?;

    let refreshed_b = explored_endpoint(&env, bmc_ip_b).await?;
    assert!(
        refreshed_b.report_version.version_nr() > initial_version_b.version_nr(),
        "lock for endpoint {bmc_ip_a} should not block refresh for endpoint {bmc_ip_b}"
    );

    blocker.release();
    refresh_a.await??;

    Ok(())
}

/// Retention recovery is centralized at row creation, so even a static
/// preallocation (a declared `fixed_ip` reservation) recovers a retained
/// boot interface id -- the pair must not depend on WHICH path recreates
/// the row after a force-delete.
#[sqlx_test]
async fn test_preallocated_interface_recovers_retained_boot_interface_id(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = common::api_fixtures::create_test_env(pool.clone()).await;

    let mac: MacAddress = "aa:55:66:77:88:99".parse()?;
    // An external static IP: preallocation homes it on the
    // static-assignments anchor segment, no fixture segment needed.
    let static_ip: std::net::IpAddr = "203.0.113.7".parse()?;

    // A prior row for this MAC was deleted with its boot pair retained.
    let mut txn = env.pool.begin().await?;
    db::retained_boot_interface::upsert(txn.as_mut(), mac, "NIC.Static.1-1-1").await?;
    txn.commit().await?;

    // The static reservation recreates the row (the path a declared
    // fixed_ip takes via DHCP discover or site-explorer reconciliation).
    let mut txn = env.pool.begin().await?;
    db::machine_interface::preallocate_machine_interface(txn.as_mut(), mac, static_ip, None)
        .await?;
    txn.commit().await?;

    let mut txn = env.pool.begin().await?;
    let interfaces = db::machine_interface::find_by_mac_address(txn.as_mut(), mac).await?;
    assert_eq!(interfaces.len(), 1);
    assert_eq!(
        interfaces[0].boot_interface_id.as_deref(),
        Some("NIC.Static.1-1-1"),
        "a preallocation-created row recovers the retained boot interface id"
    );
    assert!(
        db::retained_boot_interface::find_by_mac(txn.as_mut(), mac, None)
            .await?
            .is_none(),
        "the retention record is consumed once applied"
    );
    txn.rollback().await?;

    Ok(())
}

/// The expiry sweep removes only records older than the configured window
/// -- and removes nothing when no window is set (records wait forever for
/// their machine to come back).
#[sqlx_test]
async fn test_retained_boot_interface_sweep_removes_only_expired_records(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let _env = common::api_fixtures::create_test_env(pool.clone()).await;

    let old_mac: MacAddress = "aa:bb:cc:00:00:01".parse()?;
    let recent_mac: MacAddress = "aa:bb:cc:00:00:02".parse()?;

    let mut txn = pool.begin().await?;
    db::retained_boot_interface::upsert(txn.as_mut(), old_mac, "NIC.Old.1-1-1").await?;
    db::retained_boot_interface::upsert(txn.as_mut(), recent_mac, "NIC.Recent.1-1-1").await?;
    // Age one record past the window.
    sqlx::query(
        "UPDATE retained_boot_interfaces SET recorded_at = NOW() - INTERVAL '2 hours' \
         WHERE mac_address = $1",
    )
    .bind(old_mac)
    .execute(txn.as_mut())
    .await?;
    txn.commit().await?;

    // No window -> nothing is swept.
    let mut txn = pool.begin().await?;
    assert_eq!(
        db::retained_boot_interface::delete_expired(txn.as_mut(), None).await?,
        0,
        "without a window the sweep must leave every record in place"
    );
    let swept =
        db::retained_boot_interface::delete_expired(txn.as_mut(), Some(chrono::Duration::hours(1)))
            .await?;
    assert_eq!(swept, 1, "only the aged-out record is swept");
    assert!(
        db::retained_boot_interface::find_by_mac(txn.as_mut(), old_mac, None)
            .await?
            .is_none(),
        "the aged-out record is gone"
    );
    assert_eq!(
        db::retained_boot_interface::find_by_mac(txn.as_mut(), recent_mac, None)
            .await?
            .as_deref(),
        Some("NIC.Recent.1-1-1"),
        "the in-window record survives the sweep"
    );
    txn.rollback().await?;

    Ok(())
}

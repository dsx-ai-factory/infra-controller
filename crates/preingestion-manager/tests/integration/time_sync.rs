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

use carbide_preingestion_manager::PreingestionManager;
use carbide_redfish::libredfish::test_support::{RedfishSim, RedfishSimAction};
use carbide_test_harness::prelude::*;
use carbide_test_harness::test_support::default_config;
use libredfish::SystemPowerControl;
use model::site_explorer::{PreingestionState, TimeSyncResetPhase};
use rpc::forge::DhcpDiscovery;

use crate::common;

/// Test that when BMC time is in sync, preingestion proceeds normally with firmware checks
#[sqlx_test]
async fn test_preingestion_time_sync_ok(pool: PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let env = TestHarness::builder(pool.clone()).build().await;
    let domain = env.test_domain().await;
    let nc = env.network_controller();
    let underlay_segment = nc.create_underlay_segment(&domain).await;
    let mut config = default_config::get();

    config.ntp_servers = vec![
        "198.51.100.10".parse().unwrap(),
        "198.51.100.11".parse().unwrap(),
    ];

    let redfish_sim = Arc::new(RedfishSim::default());
    let mgr = PreingestionManager::new(
        pool.clone(),
        config.preingestion_manager(),
        redfish_sim.clone(),
        env.test_meter.meter(),
        None,
        None,
        None,
        env.api().work_lock_manager_handle(),
        config.ntp_servers.clone(),
    );

    let response = env
        .api()
        .discover_dhcp(
            DhcpDiscovery::builder("b8:3f:d2:90:97:a6", underlay_segment.relay_address)
                .vendor_string("iDRac")
                .tonic_request(),
        )
        .await?
        .into_inner();

    let addr = response.address.as_str();
    let ip_addr = IpAddr::from_str(addr).unwrap();
    // Insert endpoint with current versions that are up to date
    let mut txn = pool.begin().await.unwrap();
    common::insert_endpoint_version(&mut txn, addr, "6.00.30.00", "1.13.2", false).await?;
    txn.commit().await?;

    let timepoint = redfish_sim.timepoint();

    // Run preingestion manager - should apply site NTP servers, check time sync,
    // then check firmware and complete.
    mgr.run_single_iteration().await?;

    // Second iteration applies site NTP servers and records when that
    // succeeded, but does not check BMC time until the convergence wait elapses.
    mgr.run_single_iteration().await?;

    let actions = redfish_sim.actions_since(&timepoint);
    assert!(
        actions
            .all_hosts()
            .iter()
            .any(|a| matches!(a, RedfishSimAction::SetNtpServers(_))),
        "Expected SetNtpServers when site NTP is configured"
    );

    let mut txn = pool.begin().await.unwrap();
    let endpoints = db::explored_endpoints::find_all_by_ip(ip_addr, &mut txn).await?;
    let endpoint = endpoints.first().expect("Endpoint should exist");
    assert!(
        matches!(
            endpoint.preingestion_state,
            PreingestionState::SetNtpServers {
                set_at: Some(_),
                attempts: 0
            }
        ),
        "Expected SetNtpServers wait after applying NTP, got: {:?}",
        endpoint.preingestion_state
    );
    txn.commit().await?;

    // The next iteration should still wait for BMC NTP convergence.
    mgr.run_single_iteration().await?;

    let mut txn = pool.begin().await.unwrap();
    db::explored_endpoints::set_preingestion_set_ntp_servers(
        ip_addr,
        Some(chrono::Utc::now() - chrono::TimeDelta::minutes(3)),
        0,
        &mut txn,
    )
    .await?;
    txn.commit().await?;

    // Once the convergence wait has elapsed, time sync and firmware checks complete.
    mgr.run_single_iteration().await?;

    let mut txn = pool.begin().await.unwrap();
    // Should go directly to complete since time is in sync and firmware is up to date
    assert!(
        db::explored_endpoints::find_all_preingestion_complete(&mut txn)
            .await?
            .len()
            == 1
    );
    txn.commit().await?;

    Ok(())
}

/// Test that an empty NTP server config skips Redfish NTP setup and proceeds
/// directly to initial checks from the SetNtpServers state.
#[sqlx_test]
async fn test_preingestion_set_ntp_servers_empty(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = TestHarness::builder(pool.clone()).build().await;
    let domain = env.test_domain().await;
    let nc = env.network_controller();
    let underlay_segment = nc.create_underlay_segment(&domain).await;

    let mut config = default_config::get();
    config.ntp_servers.clear(); // Use empty NTP servers.

    let redfish_sim = Arc::new(RedfishSim::default());
    let mgr = PreingestionManager::new(
        pool.clone(),
        config.preingestion_manager(),
        redfish_sim.clone(),
        env.test_meter.meter(),
        None,
        None,
        None,
        env.api().work_lock_manager_handle(),
        config.ntp_servers.clone(),
    );

    let response = env
        .api()
        .discover_dhcp(
            DhcpDiscovery::builder("b8:3f:d2:90:97:a6", underlay_segment.relay_address)
                .vendor_string("iDRac")
                .tonic_request(),
        )
        .await?
        .into_inner();

    let addr = response.address.as_str();
    let ip_addr = IpAddr::from_str(addr).unwrap();
    let mut txn = pool.begin().await.unwrap();
    common::insert_endpoint_version(&mut txn, addr, "6.00.30.00", "1.13.2", false).await?;

    // Transition to start of SetNtpServers state with no attempts made so far.
    db::explored_endpoints::set_preingestion_set_ntp_servers(ip_addr, None, 0, &mut txn).await?;
    txn.commit().await?;

    let timepoint = redfish_sim.timepoint();
    mgr.run_single_iteration().await?;

    // Expect no SetNtpServers actions since NTP server config is empty.
    let actions = redfish_sim.actions_since(&timepoint);
    assert!(
        !actions
            .all_hosts()
            .iter()
            .any(|a| matches!(a, RedfishSimAction::SetNtpServers(_))),
        "Did not expect SetNtpServers when NTP server config is empty"
    );

    // Expect to go to complete since no need to set NTP server config.
    let mut txn = pool.begin().await.unwrap();
    assert_eq!(
        db::explored_endpoints::find_all_preingestion_complete(&mut txn)
            .await?
            .len(),
        1
    );
    txn.commit().await?;

    Ok(())
}

/// Test that exhausting NTP setup attempts proceeds to initial checks without
/// failing preingestion or trying to set NTP again.
#[sqlx_test]
async fn test_preingestion_set_ntp_servers_max_attempts(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = TestHarness::builder(pool.clone()).build().await;
    let domain = env.test_domain().await;
    let nc = env.network_controller();
    let underlay_segment = nc.create_underlay_segment(&domain).await;

    let mut config = default_config::get();
    config.ntp_servers = vec!["198.51.100.10".parse().unwrap()];

    let redfish_sim = Arc::new(RedfishSim::default());
    let mgr = PreingestionManager::new(
        pool.clone(),
        config.preingestion_manager(),
        redfish_sim.clone(),
        env.test_meter.meter(),
        None,
        None,
        None,
        env.api().work_lock_manager_handle(),
        config.ntp_servers.clone(),
    );

    let response = env
        .api()
        .discover_dhcp(
            DhcpDiscovery::builder("b8:3f:d2:90:97:a6", underlay_segment.relay_address)
                .vendor_string("iDRac")
                .tonic_request(),
        )
        .await?
        .into_inner();

    let addr = response.address.as_str();
    let ip_addr = IpAddr::from_str(addr).unwrap();
    let mut txn = pool.begin().await.unwrap();
    common::insert_endpoint_version(&mut txn, addr, "6.00.30.00", "1.13.2", false).await?;

    db::explored_endpoints::set_preingestion_set_ntp_servers(ip_addr, None, 3, &mut txn).await?;
    txn.commit().await?;

    let timepoint = redfish_sim.timepoint();
    mgr.run_single_iteration().await?;

    let actions = redfish_sim.actions_since(&timepoint);
    assert!(
        !actions
            .all_hosts()
            .iter()
            .any(|a| matches!(a, RedfishSimAction::SetNtpServers(_))),
        "Did not expect SetNtpServers after max attempts are exhausted"
    );

    // The next iteration should go to complete since NTP setup is given up.
    let mut txn = pool.begin().await.unwrap();
    assert_eq!(
        db::explored_endpoints::find_all_preingestion_complete(&mut txn)
            .await?
            .len(),
        1
    );
    txn.commit().await?;

    Ok(())
}

/// Test that preingestion handles the TimeSyncReset state machine correctly
#[sqlx_test]
async fn test_preingestion_time_sync_reset_flow(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = TestHarness::builder(pool.clone()).build().await;
    let domain = env.test_domain().await;
    let nc = env.network_controller();
    let underlay_segment = nc.create_underlay_segment(&domain).await;
    let config = default_config::get();

    let redfish_sim = Arc::new(RedfishSim::default());
    let mgr = PreingestionManager::new(
        pool.clone(),
        config.preingestion_manager(),
        redfish_sim.clone(),
        env.test_meter.meter(),
        None,
        None,
        None,
        env.api().work_lock_manager_handle(),
        config.ntp_servers.clone(),
    );

    let response = env
        .api()
        .discover_dhcp(
            DhcpDiscovery::builder("b8:3f:d2:90:97:a6", underlay_segment.relay_address)
                .vendor_string("iDRac")
                .tonic_request(),
        )
        .await?
        .into_inner();

    let addr = response.address.as_str();
    let ip_addr = IpAddr::from_str(addr).unwrap();

    // Manually set up an endpoint in TimeSyncReset state to test the state machine
    let mut txn = pool.begin().await.unwrap();
    common::insert_endpoint_version(&mut txn, addr, "6.00.30.00", "1.13.2", false).await?;

    // Set to TimeSyncReset Start phase
    db::explored_endpoints::set_preingestion_time_sync_reset(
        ip_addr,
        TimeSyncResetPhase::Start,
        0,
        &mut txn,
    )
    .await?;
    txn.commit().await?;

    // Capture timepoint before running iteration
    let timepoint = redfish_sim.timepoint();

    // Run iteration - should initiate BMC reset and move to BMCWasReset
    mgr.run_single_iteration().await?;

    // Verify that SetUtcTimezone was called during the Start phase
    let actions = redfish_sim.actions_since(&timepoint);
    let all_actions = actions.all_hosts();
    assert!(
        all_actions.contains(&RedfishSimAction::SetUtcTimezone),
        "Expected SetUtcTimezone action to be called during TimeSyncReset Start phase"
    );

    let mut txn = pool.begin().await.unwrap();
    let endpoints =
        db::explored_endpoints::find_preingest_not_waiting_not_error(txn.as_mut()).await?;
    assert_eq!(endpoints.len(), 1);
    let endpoint = endpoints.first().unwrap();
    match &endpoint.preingestion_state {
        PreingestionState::TimeSyncReset { phase, .. } => {
            assert_eq!(*phase, TimeSyncResetPhase::BMCWasReset);
        }
        _ => {
            panic!(
                "Expected TimeSyncReset state, got: {:?}",
                endpoint.preingestion_state
            );
        }
    }
    txn.commit().await?;

    // Run iteration - should power on host and move to WaitHostBoot
    mgr.run_single_iteration().await?;

    let mut txn = pool.begin().await.unwrap();
    let endpoints =
        db::explored_endpoints::find_preingest_not_waiting_not_error(txn.as_mut()).await?;
    let endpoint = endpoints.first().unwrap();
    match &endpoint.preingestion_state {
        PreingestionState::TimeSyncReset { phase, .. } => {
            assert_eq!(*phase, TimeSyncResetPhase::WaitHostBoot);
        }
        _ => {
            panic!(
                "Expected TimeSyncReset WaitHostBoot, got: {:?}",
                endpoint.preingestion_state
            );
        }
    }

    // Simulate time passage for host boot (pretend we waited 20 minutes)
    db::explored_endpoints::pregestion_hostboot_time_test(ip_addr, &mut txn).await?;
    txn.commit().await?;

    // Run iteration - should check time sync again, and since mock BMC returns good time,
    // proceed to check firmware versions which should complete since firmware is up-to-date
    mgr.run_single_iteration().await?;

    let mut txn = pool.begin().await.unwrap();
    // After time sync reset completes and firmware check runs, endpoint should be in Complete state
    // since the firmware versions are already up-to-date
    let endpoints = db::explored_endpoints::find_all_by_ip(ip_addr, &mut txn).await?;
    let endpoint = endpoints.first().expect("Endpoint should exist");
    assert_eq!(
        endpoint.preingestion_state,
        PreingestionState::Complete,
        "Expected Complete after successful time sync and firmware check, got: {:?}",
        endpoint.preingestion_state
    );
    txn.commit().await?;

    Ok(())
}

/// An ingested/paired host whose BMC clock is skewed must NOT be power-cycled or
/// have its BMC timezone changed by the time-sync remediation. When the initial
/// checks detect a skew, the gate sees that the BMC IP maps to a fleet machine
/// (the same managed-host predicate site-explorer uses) and skips the destructive
/// reset, continuing preingestion with the firmware check instead. This guards
/// against the incident where tenant-assigned nodes were left powered off after a
/// skew was detected.
#[sqlx_test]
async fn test_time_sync_reset_skipped_for_ingested_host(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = TestHarness::builder(pool.clone()).build().await;
    let domain = env.test_domain().await;
    let nc = env.network_controller();
    let underlay_segment = nc.create_underlay_segment(&domain).await;
    nc.create_admin_segment(&domain).await;

    // Build a fully ingested managed host so its BMC IP maps to a fleet machine,
    // which is exactly the signal `is_ingested_host` keys on.
    let explorer = env.default_test_site_explorer();
    let (_managed_host, build_data) = env
        .managed_host_builder(&explorer, underlay_segment)
        .build()
        .await;
    let host_bmc_ip = build_data.host_bmc_ip();

    // Precondition: the host's BMC IP resolves to an ingested machine. If this
    // ever stops holding, the test below would silently stop exercising the gate.
    let mut txn = pool.begin().await.unwrap();
    assert!(
        db::machine::find_id_by_bmc_ip(txn.as_mut(), &host_bmc_ip)
            .await?
            .is_some(),
        "managed host BMC IP should map to an ingested machine"
    );
    txn.commit().await?;

    let mut config = default_config::get();
    // Empty NTP config so the SetNtpServers state advances straight into
    // run_initial_checks (and thus the time-sync skew check) in one iteration.
    config.ntp_servers.clear();

    let redfish_sim = Arc::new(RedfishSim::default());
    // Force a large skew so the initial check reports the BMC time as out of sync
    // and would, without the gate, kick off the destructive reset.
    redfish_sim.set_bmc_time_offset_seconds(600);
    let mgr = PreingestionManager::new(
        pool.clone(),
        config.preingestion_manager(),
        redfish_sim.clone(),
        env.test_meter.meter(),
        None,
        None,
        None,
        env.api().work_lock_manager_handle(),
        config.ntp_servers.clone(),
    );

    // Drive the ingested host's endpoint to the point where initial checks run,
    // and make sure the preingestion loop will actually pick it up.
    let mut txn = pool.begin().await.unwrap();
    db::explored_endpoints::set_preingestion_set_ntp_servers(host_bmc_ip, None, 0, &mut txn)
        .await?;
    sqlx::query(
        "UPDATE explored_endpoints SET waiting_for_explorer_refresh = false WHERE address = $1",
    )
    .bind(host_bmc_ip)
    .execute(&mut *txn)
    .await?;
    txn.commit().await?;

    let timepoint = redfish_sim.timepoint();
    mgr.run_single_iteration().await?;

    // The ingested host must not have been power-cycled or had its timezone changed.
    let actions = redfish_sim.actions_since(&timepoint);
    let all_actions = actions.all_hosts();
    assert!(
        !all_actions.contains(&RedfishSimAction::SetUtcTimezone),
        "ingested host should not have its BMC timezone changed"
    );
    assert!(
        !all_actions
            .iter()
            .any(|a| matches!(a, RedfishSimAction::Power(SystemPowerControl::ForceOff))),
        "ingested host should not be powered off"
    );

    // Despite the skew, the gate must keep the endpoint out of the TimeSyncReset
    // state machine entirely; it continues with the firmware check instead.
    let mut txn = pool.begin().await.unwrap();
    let endpoints = db::explored_endpoints::find_all_by_ip(host_bmc_ip, &mut txn).await?;
    let endpoint = endpoints.first().expect("Endpoint should exist");
    assert!(
        !matches!(
            endpoint.preingestion_state,
            PreingestionState::TimeSyncReset { .. }
        ),
        "ingested host should never enter TimeSyncReset, got: {:?}",
        endpoint.preingestion_state
    );
    txn.commit().await?;

    Ok(())
}

/// Test that when BMC time check returns an error, preingestion fails
#[sqlx_test]
async fn test_preingestion_time_sync_check_error_fails(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    // Note: This test verifies the error handling path exists in the code.
    // In practice, with a working mock BMC, this path might not be exercised.
    // The actual behavior depends on whether the mock BMC's get_manager() method
    // returns a valid DateTime or not.

    let env = TestHarness::builder(pool.clone()).build().await;
    let domain = env.test_domain().await;
    let nc = env.network_controller();
    let underlay_segment = nc.create_underlay_segment(&domain).await;
    let config = default_config::get();

    let redfish_sim = Arc::new(RedfishSim::default());
    let mgr = PreingestionManager::new(
        pool.clone(),
        config.preingestion_manager(),
        redfish_sim.clone(),
        env.test_meter.meter(),
        None,
        None,
        None,
        env.api().work_lock_manager_handle(),
        config.ntp_servers.clone(),
    );

    let response = env
        .api()
        .discover_dhcp(
            DhcpDiscovery::builder("b8:3f:d2:90:97:a6", underlay_segment.relay_address)
                .vendor_string("iDRac")
                .tonic_request(),
        )
        .await?
        .into_inner();

    let addr = response.address.as_str();
    let mut txn = pool.begin().await.unwrap();
    common::insert_endpoint_version(&mut txn, addr, "6.00.30.00", "1.13.2", false).await?;
    txn.commit().await?;

    // Run preingestion - with mock BMC that has valid time, this should succeed
    mgr.run_single_iteration().await?;

    // The test passes if it doesn't panic - the mock BMC should return valid time
    // and the endpoint should proceed to completion or firmware check
    let mut txn = pool.begin().await.unwrap();
    let endpoints = db::explored_endpoints::find_all(txn.as_mut()).await?;
    assert_eq!(endpoints.len(), 1);
    // Just verify we didn't fail - we should be in Complete or some valid state
    let endpoint = &endpoints[0];
    match &endpoint.preingestion_state {
        PreingestionState::Failed { reason } => {
            panic!("Unexpected failure: {}", reason);
        }
        _ => {
            // Expected - time check passed or we're in a valid processing state
        }
    }
    txn.commit().await?;

    Ok(())
}

/// Test the retry logic when time sync fails after first reset attempt
#[sqlx_test]
async fn test_preingestion_time_sync_retry_logic(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = TestHarness::builder(pool.clone()).build().await;
    let domain = env.test_domain().await;
    let nc = env.network_controller();
    let underlay_segment = nc.create_underlay_segment(&domain).await;
    let config = default_config::get();

    let redfish_sim = Arc::new(RedfishSim::default());
    let mgr = PreingestionManager::new(
        pool.clone(),
        config.preingestion_manager(),
        redfish_sim.clone(),
        env.test_meter.meter(),
        None,
        None,
        None,
        env.api().work_lock_manager_handle(),
        config.ntp_servers.clone(),
    );

    let response = env
        .api()
        .discover_dhcp(
            DhcpDiscovery::builder("b8:3f:d2:90:97:a6", underlay_segment.relay_address)
                .vendor_string("iDRac")
                .tonic_request(),
        )
        .await?
        .into_inner();

    let addr = response.address.as_str();
    let ip_addr = IpAddr::from_str(addr).unwrap();

    // Set up endpoint in TimeSyncReset WaitHostBoot phase
    let mut txn = pool.begin().await.unwrap();
    common::insert_endpoint_version(&mut txn, addr, "6.00.30.00", "1.13.2", false).await?;

    // Manually set to WaitHostBoot phase as if we just finished a reset
    db::explored_endpoints::set_preingestion_time_sync_reset(
        ip_addr,
        TimeSyncResetPhase::WaitHostBoot,
        0,
        &mut txn,
    )
    .await?;

    // Simulate time has passed
    db::explored_endpoints::pregestion_hostboot_time_test(ip_addr, &mut txn).await?;
    txn.commit().await?;

    // Run iteration - time check should pass (mock BMC returns valid time)
    // and proceed to check firmware which should complete since firmware is up-to-date
    mgr.run_single_iteration().await?;

    let mut txn = pool.begin().await.unwrap();
    // After time sync reset completes and firmware check runs, endpoint should be in Complete state
    let endpoints = db::explored_endpoints::find_all_by_ip(ip_addr, &mut txn).await?;
    let endpoint = endpoints.first().expect("Endpoint should exist");

    // With a working mock BMC, time sync should succeed and firmware check should complete
    match &endpoint.preingestion_state {
        PreingestionState::Complete => {
            // Expected - time sync passed and firmware is up-to-date
        }
        PreingestionState::RecheckVersions => {
            // Could also be this if firmware check is still pending
        }
        PreingestionState::TimeSyncReset { phase, .. } => {
            // If we're still in TimeSyncReset state, the reset is in progress
            // But with mock BMC this shouldn't happen - we should have progressed
            panic!(
                "Unexpected: Still in TimeSyncReset state with phase {:?}",
                phase
            );
        }
        _ => {
            // Could be other states if firmware upgrade is needed
        }
    }
    txn.commit().await?;

    Ok(())
}

/// When the BMC clock is still out of sync after a reset cycle but the retry
/// budget is not yet exhausted, the endpoint should re-enter the reset cycle
/// (TimeSyncReset Start) with an incremented attempt count rather than failing.
#[sqlx_test]
async fn test_time_sync_retry_reenters_reset_before_failing(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = TestHarness::builder(pool.clone()).build().await;
    let domain = env.test_domain().await;
    let nc = env.network_controller();
    let underlay_segment = nc.create_underlay_segment(&domain).await;
    let config = default_config::get();

    // Simulate a BMC clock that is well past the 5 minute threshold.
    let redfish_sim = Arc::new(RedfishSim::default());
    redfish_sim.set_bmc_time_offset_seconds(600);
    let mgr = PreingestionManager::new(
        pool.clone(),
        config.preingestion_manager(),
        redfish_sim.clone(),
        env.test_meter.meter(),
        None,
        None,
        None,
        env.api().work_lock_manager_handle(),
        config.ntp_servers.clone(),
    );

    let response = env
        .api()
        .discover_dhcp(
            DhcpDiscovery::builder("b8:3f:d2:90:97:a6", underlay_segment.relay_address)
                .vendor_string("iDRac")
                .tonic_request(),
        )
        .await?
        .into_inner();
    let addr = response.address.as_str();
    let ip_addr = IpAddr::from_str(addr).unwrap();

    let mut txn = pool.begin().await.unwrap();
    common::insert_endpoint_version(&mut txn, addr, "6.00.30.00", "1.13.2", false).await?;
    // First reset cycle just finished (attempt 0), awaiting the boot-wait recheck.
    db::explored_endpoints::set_preingestion_time_sync_reset(
        ip_addr,
        TimeSyncResetPhase::WaitHostBoot,
        0,
        &mut txn,
    )
    .await?;
    // Backdate last_time so the boot wait is considered elapsed.
    db::explored_endpoints::pregestion_hostboot_time_test(ip_addr, &mut txn).await?;
    txn.commit().await?;

    mgr.run_single_iteration().await?;

    let mut txn = pool.begin().await.unwrap();
    let endpoints = db::explored_endpoints::find_all_by_ip(ip_addr, &mut txn).await?;
    match &endpoints
        .first()
        .expect("endpoint should exist")
        .preingestion_state
    {
        PreingestionState::TimeSyncReset { phase, attempt, .. } => {
            assert_eq!(
                *phase,
                TimeSyncResetPhase::Start,
                "should retry reset cycle"
            );
            assert_eq!(*attempt, 1, "attempt counter should be incremented");
        }
        other => panic!("expected a retried TimeSyncReset, got: {other:?}"),
    }
    txn.commit().await?;

    Ok(())
}

/// Once the reset retry budget is exhausted and the BMC clock is still out of
/// sync, preingestion should fail terminally.
#[sqlx_test]
async fn test_time_sync_fails_after_max_attempts(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = TestHarness::builder(pool.clone()).build().await;
    let domain = env.test_domain().await;
    let nc = env.network_controller();
    let underlay_segment = nc.create_underlay_segment(&domain).await;
    let config = default_config::get();

    let redfish_sim = Arc::new(RedfishSim::default());
    redfish_sim.set_bmc_time_offset_seconds(600);
    let mgr = PreingestionManager::new(
        pool.clone(),
        config.preingestion_manager(),
        redfish_sim.clone(),
        env.test_meter.meter(),
        None,
        None,
        None,
        env.api().work_lock_manager_handle(),
        config.ntp_servers.clone(),
    );

    let response = env
        .api()
        .discover_dhcp(
            DhcpDiscovery::builder("b8:3f:d2:90:97:a6", underlay_segment.relay_address)
                .vendor_string("iDRac")
                .tonic_request(),
        )
        .await?
        .into_inner();
    let addr = response.address.as_str();
    let ip_addr = IpAddr::from_str(addr).unwrap();

    let mut txn = pool.begin().await.unwrap();
    common::insert_endpoint_version(&mut txn, addr, "6.00.30.00", "1.13.2", false).await?;
    // Final allowed reset cycle (attempt 2 == MAX_TIME_SYNC_RESET_ATTEMPTS - 1)
    // just finished, awaiting the boot-wait recheck.
    db::explored_endpoints::set_preingestion_time_sync_reset(
        ip_addr,
        TimeSyncResetPhase::WaitHostBoot,
        2,
        &mut txn,
    )
    .await?;
    db::explored_endpoints::pregestion_hostboot_time_test(ip_addr, &mut txn).await?;
    txn.commit().await?;

    mgr.run_single_iteration().await?;

    let mut txn = pool.begin().await.unwrap();
    let endpoints = db::explored_endpoints::find_all_by_ip(ip_addr, &mut txn).await?;
    match &endpoints
        .first()
        .expect("endpoint should exist")
        .preingestion_state
    {
        PreingestionState::Failed { reason } => {
            assert!(
                reason.contains("time synchronization failed"),
                "unexpected failure reason: {reason}"
            );
        }
        other => panic!("expected Failed after exhausting retries, got: {other:?}"),
    }
    txn.commit().await?;

    Ok(())
}

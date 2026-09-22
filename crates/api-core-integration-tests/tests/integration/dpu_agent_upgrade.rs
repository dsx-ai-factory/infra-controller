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

use std::time::SystemTime;

use ::rpc::forge as rpc;
use ::rpc::forge::forge_server::Forge;
use carbide_test_harness::prelude::*;
use carbide_test_harness::test_support::fixture_config::{
    FixtureDefault as _, ManagedHostConfigExt as _,
};
use carbide_uuid::machine::DpuMachineId;
use chrono::{Duration, Utc};
use health_report::{HealthAlertClassification, HealthProbeAlert, HealthProbeId};
use model::test_support::ManagedHostConfig;

async fn create_ready_managed_host(env: &TestHarness) -> TestManagedHost {
    let network_controller = env.network_controller();
    let domain = env.test_domain().await;
    let admin_segment = network_controller.create_admin_segment(&domain).await;
    let underlay_segment = network_controller.create_underlay_segment(&domain).await;
    let site_explorer = env.default_test_site_explorer();
    let (mut managed_host, _) = env
        .managed_host_builder(&site_explorer, underlay_segment)
        .with_config(ManagedHostConfig::default().with_dpu_count(1))
        .with_dpu_network_status_reported()
        .build()
        .await;
    managed_host
        .host
        .discover_primary_iface(admin_segment)
        .await;
    managed_host.advance_to_converged_ready().await;
    managed_host
}

async fn init(pool: PgPool) -> (TestHarness, TestManagedHost) {
    let env = TestHarness::builder(pool).build().await;
    let managed_host = create_ready_managed_host(&env).await;
    (env, managed_host)
}

/// Helper: call record_dpu_network_status with an old agent version.
async fn report_old_agent_version(env: &TestHarness, dpu_machine_id: DpuMachineId) {
    env.api()
        .record_dpu_network_status(tonic::Request::new(rpc::DpuNetworkStatus {
            dpu_machine_id: Some(dpu_machine_id),
            dpu_agent_version: Some("v2023.06-rc2-1-gc5c05de3".to_string()),
            observed_at: None,
            dpu_health: Some(::rpc::health::HealthReport {
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
            fabric_interfaces: vec![],
            last_dhcp_requests: vec![],
            dpu_extension_service_version: None,
            dpu_extension_services: vec![],
            astra_config_status: None,
        }))
        .await
        .unwrap();
}

async fn upgrade_needed(env: &TestHarness, dpu_machine_id: DpuMachineId) -> bool {
    env.api()
        .dpu_agent_upgrade_check(tonic::Request::new(rpc::DpuAgentUpgradeCheckRequest {
            machine_id: dpu_machine_id.to_string(),
            current_agent_version: "v2023.06-rc2-1-gc5c05de3".to_string(),
            binary_mtime: Some(SystemTime::now().into()),
            binary_sha: "abc123".to_string(),
        }))
        .await
        .unwrap()
        .into_inner()
        .should_upgrade
}

async fn set_upgrade_policy(env: &TestHarness) {
    env.api()
        .dpu_agent_upgrade_policy_action(tonic::Request::new(rpc::DpuAgentUpgradePolicyRequest {
            new_policy: Some(rpc::AgentUpgradePolicy::UpOnly as i32),
        }))
        .await
        .unwrap();
}

#[sqlx_test]
async fn test_dpf_host_does_not_set_upgrade_flag(db_pool: PgPool) -> Result<(), eyre::Report> {
    let (env, mh) = init(db_pool).await;
    let dpu_id = mh.first_dpu().id;

    set_upgrade_policy(&env).await;

    // Mark the host as DPF-managed (used_for_ingestion = true).
    let mut txn = env.db_txn().await;
    db::machine::mark_machine_ingestion_done_with_dpf(&mut txn, &mh.host.id).await?;
    txn.commit().await?;

    // Even with an old agent version reported, the upgrade flag must NOT be set.
    report_old_agent_version(&env, dpu_id).await;

    assert!(
        !upgrade_needed(&env, dpu_id).await,
        "DPF-managed DPU should never have the agent upgrade flag set"
    );
    Ok(())
}

#[sqlx_test]
async fn test_dpf_transition_clears_stale_upgrade_flag(
    db_pool: PgPool,
) -> Result<(), eyre::Report> {
    let (env, mh) = init(db_pool).await;
    let dpu_id = mh.first_dpu().id;

    set_upgrade_policy(&env).await;

    // While NOT yet DPF-managed, report an old version to set the upgrade flag.
    report_old_agent_version(&env, dpu_id).await;
    assert!(
        upgrade_needed(&env, dpu_id).await,
        "upgrade flag should be set before DPF transition"
    );

    // Transition the host to DPF.
    let mut txn = env.db_txn().await;
    db::machine::mark_machine_ingestion_done_with_dpf(&mut txn, &mh.host.id).await?;
    txn.commit().await?;

    // The next status report should clear the stale flag.
    report_old_agent_version(&env, dpu_id).await;
    assert!(
        !upgrade_needed(&env, dpu_id).await,
        "upgrade flag should be cleared after DPF transition"
    );

    // A second status report must NOT re-set the flag (no redundant DB writes).
    report_old_agent_version(&env, dpu_id).await;
    assert!(
        !upgrade_needed(&env, dpu_id).await,
        "upgrade flag must stay cleared on subsequent status reports"
    );
    Ok(())
}

#[sqlx_test]
async fn test_upgrade_check(db_pool: PgPool) -> Result<(), eyre::Report> {
    let (env, managed_host) = init(db_pool).await;
    let dpu_machine_id = managed_host.first_dpu().id;

    // Set the upgrade policy
    let response = env
        .api()
        .dpu_agent_upgrade_policy_action(tonic::Request::new(rpc::DpuAgentUpgradePolicyRequest {
            new_policy: Some(rpc::AgentUpgradePolicy::UpOnly as i32),
        }))
        .await?
        .into_inner();
    assert_eq!(
        response.active_policy,
        rpc::AgentUpgradePolicy::UpOnly as i32,
        "Policy should be what we set"
    );
    assert!(response.did_change, "Policy should have changed");

    // We'll need to know the current network config version in order to register our
    // forge-dpu-agent version
    let response = env
        .api()
        .get_managed_host_network_config(tonic::Request::new(
            rpc::ManagedHostNetworkConfigRequest {
                dpu_machine_id: Some(dpu_machine_id),
            },
        ))
        .await?
        .into_inner();

    // Report that we're on an old version of the DPU
    // That should trigger marking us for upgrade
    let network_config_version = response.managed_host_config_version.clone();
    env.api()
        .record_dpu_network_status(tonic::Request::new(rpc::DpuNetworkStatus {
            dpu_machine_id: Some(dpu_machine_id),
            // BEGIN This is the important line for this test
            dpu_agent_version: Some("v2023.06-rc2-1-gc5c05de3".to_string()),
            // END
            observed_at: None,
            dpu_health: Some(::rpc::health::HealthReport {
                source: "forge-dpu-agent".to_string(),
                triggered_by: None,
                observed_at: None,
                successes: vec![],
                alerts: vec![],
            }),
            network_config_version: Some(network_config_version.clone()),
            instance_id: None,
            instance_config_version: None,
            instance_network_config_version: None,
            interfaces: vec![rpc::InstanceInterfaceStatusObservation {
                function_type: rpc::InterfaceFunctionType::Physical as i32,
                virtual_function_id: None,
                mac_address: None,
                addresses: vec!["1.2.3.4".to_string()],
                prefixes: vec!["1.2.3.4/32".to_string()],
                gateways: vec!["1.2.3.1".to_string()],
                network_security_group: None,
                internal_uuid: None,
            }],
            network_config_error: None,
            client_certificate_expiry_unix_epoch_secs: None,
            fabric_interfaces: vec![],
            last_dhcp_requests: vec![],
            dpu_extension_service_version: Some("V1-T1".to_string()),
            dpu_extension_services: vec![],
            astra_config_status: None,
        }))
        .await
        .unwrap();

    // Check if we need to upgrade - answer should be yes
    let response = env
        .api()
        .dpu_agent_upgrade_check(tonic::Request::new(rpc::DpuAgentUpgradeCheckRequest {
            machine_id: dpu_machine_id.to_string(),
            current_agent_version: "v2023.06-rc2-1-gc5c05de3".to_string(),
            binary_mtime: Some(SystemTime::now().into()),
            binary_sha: "f86df8a4c022a8e64b5655b0063b3e18107891aefd766df8f34a6e53fda3fde9"
                .to_string(),
        }))
        .await?;
    let resp = response.into_inner();
    assert!(
        resp.should_upgrade,
        "DPU reported old version so should be asked to upgrade"
    );
    let current_version = carbide_version::v!(build_version);
    assert_eq!(
        resp.package_version,
        current_version[1..],
        "Debian package version is our version minus initial 'v'"
    );

    Ok(())
}

#[sqlx_test]
async fn test_dpu_agent_version_staleness(db_pool: PgPool) -> Result<(), eyre::Report> {
    // Set up a 1 day staleness threshold
    let mut runtime_config = carbide_test_harness::test_support::default_config::get();
    runtime_config
        .host_health
        .dpu_agent_version_staleness_threshold = Duration::days(1);
    runtime_config
        .host_health
        .prevent_allocations_on_stale_dpu_agent_version = true;
    let env = TestHarness::builder(db_pool)
        .with_api_builder_fn(move |builder| builder.with_runtime_config(runtime_config.into()))
        .build()
        .await;

    let stale_version = "stale_version";
    let recently_superseded_version = "recently_superseded_version";
    let current_version = carbide_version::v!(build_version);
    let stale_time = Utc::now() - Duration::hours(25);
    let recently_superseded_time = Utc::now() - Duration::hours(23);

    {
        let mut txn = env.db_txn().await;
        db::carbide_version::make_mock_observation(&mut txn, stale_version, Some(stale_time))
            .await?;
        db::carbide_version::make_mock_observation(
            &mut txn,
            recently_superseded_version,
            Some(recently_superseded_time),
        )
        .await?;
        db::carbide_version::make_mock_observation(&mut txn, current_version, None).await?;
        txn.commit().await?;
    }

    let mh = create_ready_managed_host(&env).await;

    // We'll need to know the current network config version in order to register our
    // forge-dpu-agent version
    let response = env
        .api()
        .get_managed_host_network_config(tonic::Request::new(
            rpc::ManagedHostNetworkConfigRequest {
                dpu_machine_id: Some(mh.first_dpu().id),
            },
        ))
        .await?
        .into_inner();

    // Report that we're on a stale version of the dpu agent
    let alert = mh
        .mock_observation_and_get_only_health_alert(
            &env,
            Some(stale_version),
            &response.managed_host_config_version,
        )
        .await
        .expect("Should have caused a health alert");
    assert_eq!(
        alert.message,
        format!(
            "Agent version is {stale_version}, which is out of date since {}",
            stale_time.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
        )
    );
    assert_eq!(alert.target, Some(mh.first_dpu().id.to_string()));
    assert_eq!(
        alert.classifications,
        vec![HealthAlertClassification::prevent_allocations()]
    );
    assert_eq!(alert.id, HealthProbeId::stale_agent_version());

    // Now try with the superseded-but-not-yet-stale version
    assert!(
        mh.mock_observation_and_get_only_health_alert(
            &env,
            Some(recently_superseded_version),
            &response.managed_host_config_version
        )
        .await
        .is_none()
    );

    // Now try with no build number
    let alert = mh
        .mock_observation_and_get_only_health_alert(
            &env,
            None,
            &response.managed_host_config_version,
        )
        .await
        .expect("Should have caused a health alert");
    assert_eq!(alert.message, "Agent version is not known");
    assert_eq!(alert.target, Some(mh.first_dpu().id.to_string()),);
    assert_eq!(
        alert.classifications,
        vec![HealthAlertClassification::prevent_allocations()]
    );
    assert_eq!(alert.id, HealthProbeId::stale_agent_version());

    // Finally, a matching version should be fine
    assert!(
        mh.mock_observation_and_get_only_health_alert(
            &env,
            Some(current_version),
            &response.managed_host_config_version
        )
        .await
        .is_none()
    );

    Ok(())
}

trait TestManagedHostDpuAgentExt {
    async fn mock_observation_and_get_only_health_alert(
        &self,
        test_env: &TestHarness,
        agent_version: Option<&str>,
        managed_host_config_version: &str,
    ) -> Option<HealthProbeAlert>;
}

impl TestManagedHostDpuAgentExt for TestManagedHost {
    async fn mock_observation_and_get_only_health_alert(
        &self,
        test_env: &TestHarness,
        agent_version: Option<&str>,
        managed_host_config_version: &str,
    ) -> Option<HealthProbeAlert> {
        test_env
            .api()
            .record_dpu_network_status(tonic::Request::new(rpc::DpuNetworkStatus {
                dpu_machine_id: Some(self.first_dpu().id),
                dpu_agent_version: agent_version.map(Into::into),
                observed_at: None,
                dpu_health: Some(::rpc::health::HealthReport {
                    source: "forge-dpu-agent".to_string(),
                    triggered_by: None,
                    observed_at: None,
                    successes: vec![],
                    alerts: vec![],
                }),
                network_config_version: Some(managed_host_config_version.to_string()),
                instance_id: None,
                instance_config_version: None,
                instance_network_config_version: None,
                interfaces: vec![rpc::InstanceInterfaceStatusObservation {
                    function_type: rpc::InterfaceFunctionType::Physical as i32,
                    virtual_function_id: None,
                    mac_address: None,
                    addresses: vec!["1.2.3.4".to_string()],
                    prefixes: vec!["1.2.3.4/32".to_string()],
                    gateways: vec!["1.2.3.1".to_string()],
                    network_security_group: None,
                    internal_uuid: None,
                }],
                network_config_error: None,
                client_certificate_expiry_unix_epoch_secs: None,
                fabric_interfaces: vec![],
                last_dhcp_requests: vec![],
                dpu_extension_service_version: Some("V1-T1".to_string()),
                dpu_extension_services: vec![],
                astra_config_status: None,
            }))
            .await
            .unwrap();

        let alerts = test_env
            .api()
            .find_machines_by_ids(tonic::Request::new(rpc::MachinesByIdsRequest {
                machine_ids: vec![self.host.id.into()],
                include_history: false,
            }))
            .await
            .unwrap()
            .into_inner()
            .machines
            .into_iter()
            .next()
            .expect("expected host machine to be found")
            .status
            .unwrap()
            .health
            .expect("expected health report")
            .alerts;

        if alerts.is_empty() {
            None
        } else {
            assert_eq!(
                alerts.len(),
                1,
                "Expected a single alert, got {}",
                alerts.len()
            );
            Some(alerts.into_iter().next().unwrap().try_into().unwrap())
        }
    }
}

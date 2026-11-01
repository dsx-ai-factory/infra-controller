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

//! BFB recovery input validation and persisted target identity.

use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use std::time::Duration;

use carbide_preingestion_manager::PreingestionManager;
use carbide_secrets::SecretsError;
use carbide_secrets::credentials::{
    BmcCredentialType, CredentialKey, CredentialReader, Credentials,
};
use mac_address::MacAddress;
use model::address_selection_strategy::AddressSelectionStrategy;
use model::machine_interface::InterfaceType;
use model::site_explorer::{
    BlueFieldOperatingMode, EndpointExplorationReport, EndpointType, PreingestionState,
};
use model::test_support::DpuConfig;
use rpc::forge::forge_server::Forge;
use tonic::Code;

use crate::test_support::fixture_config::FixtureDefault as _;
use crate::tests::common::api_fixtures::{TestEnv, create_test_env};

const DPU_MAC: &str = "02:00:00:00:84:76";
const HOST_BMC_IP: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 200));

async fn seed_pair(env: &TestEnv) -> IpAddr {
    let mut txn = env.pool.begin().await.unwrap();
    let segments = db::network_segment::admin(&mut txn).await.unwrap();
    let interface = db::machine_interface::create_with_type(
        &mut txn,
        &segments,
        &DPU_MAC.parse().unwrap(),
        false,
        AddressSelectionStrategy::NextAvailableIp,
        InterfaceType::Bmc,
        None,
    )
    .await
    .unwrap();
    let dpu_ip = interface.addresses[0];
    let dpu_report = DpuConfig {
        nic_mode: Some(BlueFieldOperatingMode::Dpu),
        ..DpuConfig::default()
    }
    .into();
    db::explored_endpoints::insert(dpu_ip, &dpu_report, false, &mut txn)
        .await
        .unwrap();
    db::explored_endpoints::insert(
        HOST_BMC_IP,
        &EndpointExplorationReport {
            endpoint_type: EndpointType::Bmc,
            ..Default::default()
        },
        false,
        &mut txn,
    )
    .await
    .unwrap();
    db::explored_endpoints::set_preingestion_complete(HOST_BMC_IP, &mut txn)
        .await
        .unwrap();
    txn.commit().await.unwrap();
    dpu_ip
}

fn request(
    dpu_ip: IpAddr,
    mac: Option<&str>,
    pre_copy_powercycle: bool,
) -> tonic::Request<rpc::forge::CopyBfbToDpuRshimRequest> {
    tonic::Request::new(rpc::forge::CopyBfbToDpuRshimRequest {
        ssh_request: Some(rpc::forge::SshRequest {
            endpoint_request: Some(rpc::forge::BmcEndpointRequest {
                ip_address: dpu_ip.to_string(),
                mac_address: mac.map(str::to_owned),
            }),
        }),
        host_bmc_ip: HOST_BMC_IP.to_string(),
        pre_copy_powercycle,
    })
}

async fn fetch(env: &TestEnv, ip: IpAddr) -> model::site_explorer::ExploredEndpoint {
    db::explored_endpoints::find_by_ips(&env.pool, vec![ip])
        .await
        .unwrap()
        .remove(0)
}

#[crate::sqlx_test]
async fn bfb_recovery_validates_mac_before_enqueue(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let dpu_ip = seed_pair(&env).await;
    for (name, mac, accepted, paused) in [
        ("mismatch", Some("02:00:00:00:84:77"), false, false),
        ("malformed", Some("invalid-mac"), false, true),
        ("matching", Some(DPU_MAC), true, false),
        ("omitted", None, true, false),
    ] {
        let mut txn = env.pool.begin().await.unwrap();
        db::explored_endpoints::set_preingestion_complete(dpu_ip, &mut txn)
            .await
            .unwrap();
        db::explored_endpoints::set_pause_remediation(HOST_BMC_IP, paused, &mut txn)
            .await
            .unwrap();
        txn.commit().await.unwrap();

        let result = env
            .api
            .copy_bfb_to_dpu_rshim(request(dpu_ip, mac, false))
            .await;
        let dpu = fetch(&env, dpu_ip).await;
        let host = fetch(&env, HOST_BMC_IP).await;
        if accepted {
            result.unwrap();
            assert_eq!(
                dpu.preingestion_state,
                PreingestionState::BfbRecoveryNeeded {
                    reason: "Triggered via CLI".to_string(),
                    host_bmc_ip: HOST_BMC_IP,
                    dpu_bmc_mac: Some(DPU_MAC.parse().unwrap()),
                    pre_copy_powercycle: false,
                },
                "{name}"
            );
            assert!(host.pause_remediation, "{name}");
        } else {
            assert_eq!(result.unwrap_err().code(), Code::InvalidArgument, "{name}");
            assert_eq!(
                dpu.preingestion_state,
                PreingestionState::Complete,
                "{name}"
            );
            assert_eq!(host.pause_remediation, paused, "{name}");
        }
    }
}

fn manager(
    env: &TestEnv,
    credential_reader: Option<Arc<dyn CredentialReader>>,
) -> PreingestionManager {
    PreingestionManager::new(
        env.pool.clone(),
        env.config.preingestion_manager(),
        env.redfish_sim.clone(),
        env.test_meter.meter(),
        None,
        None,
        credential_reader,
        env.api.work_lock_manager_handle.clone(),
        env.config.ntp_servers.clone(),
    )
}

#[crate::sqlx_test]
async fn bfb_recovery_rejects_missing_interface(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let dpu_ip = seed_pair(&env).await;
    sqlx::query("DELETE FROM machine_interface_addresses WHERE address = $1")
        .bind(dpu_ip)
        .execute(&env.pool)
        .await
        .unwrap();
    for mac in [Some(DPU_MAC), None] {
        let before = fetch(&env, dpu_ip).await.preingestion_state;
        let error = env
            .api
            .copy_bfb_to_dpu_rshim(request(dpu_ip, mac, false))
            .await
            .unwrap_err();
        assert_eq!(error.code(), Code::InvalidArgument);
        assert!(error.message().contains("no machine interface"));
        assert_eq!(fetch(&env, dpu_ip).await.preingestion_state, before);
        assert!(!fetch(&env, HOST_BMC_IP).await.pause_remediation);
    }
}

#[crate::sqlx_test]
async fn bfb_recovery_rechecks_identity_before_external_actions(pool: sqlx::PgPool) {
    use model::site_explorer::BfbPlatformPowercyclePhase;

    let env = create_test_env(pool).await;
    let dpu_ip = seed_pair(&env).await;
    let original_mac: MacAddress = DPU_MAC.parse().unwrap();
    let replacement_mac: MacAddress = "02:00:00:00:84:77".parse().unwrap();

    let original_interface = db::machine_interface::find_by_ip(&env.pool, dpu_ip)
        .await
        .unwrap()
        .unwrap();
    let mut txn = env.pool.begin().await.unwrap();
    let segments = db::network_segment::admin(&mut txn).await.unwrap();
    let replacement_interface = db::machine_interface::create_with_type(
        &mut txn,
        &segments,
        &replacement_mac,
        false,
        AddressSelectionStrategy::NextAvailableIp,
        InterfaceType::Bmc,
        None,
    )
    .await
    .unwrap();
    sqlx::query("DELETE FROM machine_interface_addresses WHERE interface_id = $1")
        .bind(replacement_interface.id)
        .execute(&mut *txn)
        .await
        .unwrap();
    txn.commit().await.unwrap();

    // Each case reaches a different dispatch/resume boundary. A freshly built
    // manager must consume the persisted identity, not process-local memory.
    for boundary in [
        "copy",
        "pre-cycle",
        "power-off",
        "after-power-cycle",
        "restart",
        "console",
        "legacy",
        "missing-interface",
    ] {
        sqlx::query("UPDATE machine_interface_addresses SET interface_id = $1 WHERE address = $2")
            .bind(original_interface.id)
            .bind(dpu_ip)
            .execute(&env.pool)
            .await
            .unwrap();
        let mut txn = env.pool.begin().await.unwrap();
        db::explored_endpoints::set_preingestion_complete(dpu_ip, &mut txn)
            .await
            .unwrap();
        txn.commit().await.unwrap();
        env.api
            .copy_bfb_to_dpu_rshim(request(
                dpu_ip,
                None,
                boundary == "pre-cycle" || boundary == "power-off",
            ))
            .await
            .unwrap();

        if boundary == "power-off" {
            manager(&env, None).run_single_iteration().await.unwrap();
            let state = fetch(&env, dpu_ip).await.preingestion_state;
            assert!(matches!(
                state,
                PreingestionState::BfbPlatformPowercycle {
                    phase: BfbPlatformPowercyclePhase::PowerOff,
                    ..
                }
            ));
            assert_eq!(state.bfb_target_mac(), Some(original_mac));
        } else if boundary == "after-power-cycle" {
            let mut txn = env.pool.begin().await.unwrap();
            db::explored_endpoints::set_preingestion_bfb_platform_powercycle(
                dpu_ip,
                HOST_BMC_IP,
                BfbPlatformPowercyclePhase::WaitingForDpuBmc,
                false,
                Some(original_mac),
                &mut txn,
            )
            .await
            .unwrap();
            txn.commit().await.unwrap();
        } else if boundary == "restart" {
            let mut txn = env.pool.begin().await.unwrap();
            db::explored_endpoints::set_preingestion_bfb_copy_in_progress(
                dpu_ip,
                HOST_BMC_IP,
                Some(original_mac),
                &mut txn,
            )
            .await
            .unwrap();
            txn.commit().await.unwrap();
            manager(&env, None).run_single_iteration().await.unwrap();
            let state = fetch(&env, dpu_ip).await.preingestion_state;
            assert!(matches!(state, PreingestionState::BfbRecoveryNeeded { .. }));
            assert_eq!(state.bfb_target_mac(), Some(original_mac));
        } else if boundary == "console" {
            let mut txn = env.pool.begin().await.unwrap();
            db::explored_endpoints::set_preingestion_bfb_installation_wait(
                dpu_ip,
                HOST_BMC_IP,
                Some(original_mac),
                &mut txn,
            )
            .await
            .unwrap();
            sqlx::query("UPDATE explored_endpoints SET preingestion_state = jsonb_set(preingestion_state, '{started_at}', to_jsonb(now() - INTERVAL '15 minutes')) WHERE address = $1")
                .bind(dpu_ip).execute(&mut *txn).await.unwrap();
            txn.commit().await.unwrap();
        } else if boundary == "legacy" {
            sqlx::query("UPDATE explored_endpoints SET preingestion_state = preingestion_state - 'dpu_bmc_mac' WHERE address = $1")
                .bind(dpu_ip).execute(&env.pool).await.unwrap();
        }

        if boundary == "missing-interface" {
            sqlx::query("DELETE FROM machine_interface_addresses WHERE address = $1")
                .bind(dpu_ip)
                .execute(&env.pool)
                .await
                .unwrap();
        } else if boundary != "legacy" {
            sqlx::query(
                "UPDATE machine_interface_addresses SET interface_id = $1 WHERE address = $2",
            )
            .bind(replacement_interface.id)
            .bind(dpu_ip)
            .execute(&env.pool)
            .await
            .unwrap();
        }
        let before = env.redfish_sim.timepoint();
        manager(&env, None).run_single_iteration().await.unwrap();
        assert!(
            env.redfish_sim
                .actions_since(&before)
                .all_hosts()
                .is_empty(),
            "{boundary}"
        );
        let dpu = fetch(&env, dpu_ip).await;
        let PreingestionState::Failed { reason } = dpu.preingestion_state else {
            panic!(
                "{boundary}: expected identity failure, got {:?}",
                dpu.preingestion_state
            );
        };
        assert!(
            reason.contains("target identity is missing or changed"),
            "{boundary}: {reason}"
        );
        assert!(
            !fetch(&env, HOST_BMC_IP).await.pause_remediation,
            "{boundary}"
        );
    }
}

struct CredentialRecorder(tokio::sync::mpsc::UnboundedSender<CredentialKey>);

#[async_trait::async_trait]
impl CredentialReader for CredentialRecorder {
    async fn get_credentials(
        &self,
        key: &CredentialKey,
    ) -> Result<Option<Credentials>, SecretsError> {
        self.0.send(key.clone()).unwrap();
        // Stop at the credential boundary; never connect to hardware or write a BFB.
        Ok(None)
    }
}

#[crate::sqlx_test]
async fn bfb_recovery_dispatches_with_bound_credentials(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let dpu_ip = seed_pair(&env).await;
    env.api
        .copy_bfb_to_dpu_rshim(request(dpu_ip, Some(DPU_MAC), false))
        .await
        .unwrap();
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let manager = manager(&env, Some(Arc::new(CredentialRecorder(sender))));
    manager.run_single_iteration().await.unwrap();
    let state = fetch(&env, dpu_ip).await.preingestion_state;
    assert!(matches!(state, PreingestionState::BfbCopyInProgress { .. }));
    assert_eq!(state.bfb_target_mac(), Some(DPU_MAC.parse().unwrap()));
    let key = tokio::time::timeout(Duration::from_secs(5), receiver.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        key,
        CredentialKey::BmcCredentials {
            credential_type: BmcCredentialType::BmcRoot { bmc_mac_address },
        } if bmc_mac_address == DPU_MAC.parse::<MacAddress>().unwrap()
    ));
}

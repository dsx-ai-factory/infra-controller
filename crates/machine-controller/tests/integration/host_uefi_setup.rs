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

//! Host UEFI setup retries client-creation failures and records the credential
//! version selected before dispatch. Saved jobs use their persisted version;
//! older jobs without one finish enrollment without claiming a site version.

use carbide_secrets::credentials::{CredentialKey, Credentials};
use carbide_test_harness::prelude::*;
use carbide_test_harness::test_support::fixture_config::FixtureDefault as _;
use db::credential_rotation::CredentialRotationType::HostUefi;
use db::credential_rotation::device_rotation_status;
use model::machine::{
    LockdownInfo, LockdownMode, LockdownState, MachineState, ManagedHostState, UefiSetupInfo,
    UefiSetupState,
};
use model::test_support::ManagedHostConfig;

use crate::env::Env;

fn uefi_setup_state(state: UefiSetupState) -> ManagedHostState {
    ManagedHostState::HostInit {
        machine_state: MachineState::UefiSetup {
            uefi_setup_info: UefiSetupInfo {
                uefi_password_jid: None,
                credential_version: None,
                uefi_setup_state: state,
            },
        },
    }
}

fn at_uefi_step(state: &ManagedHostState, step: UefiSetupState) -> bool {
    matches!(
        state,
        ManagedHostState::HostInit {
            machine_state: MachineState::UefiSetup { uefi_setup_info },
        } if uefi_setup_info.uefi_setup_state == step
    )
}

async fn setup_host(env: &Env) -> TestManagedHost {
    // Supermicro's device-level error path skips password setup. This lets
    // the retry test distinguish it from a transient client-creation failure.
    let domain = env.test_harness.test_domain().await;
    let network_controller = env.test_harness.network_controller();
    let underlay_segment = network_controller.create_underlay_segment(&domain).await;
    network_controller.create_admin_segment(&domain).await;
    let site_explorer = env.test_harness.default_test_site_explorer();
    env.test_harness
        .managed_host_builder(&site_explorer, underlay_segment)
        .with_config(ManagedHostConfig {
            vendor: Some(bmc_vendor::BMCVendor::Supermicro),
            ..ManagedHostConfig::default()
        })
        .build()
        .await
        .0
}

/// A client-creation failure while setting the ingestion BIOS password is
/// transient: the step must hold in place (not skip ahead to lockdown, which
/// on an untested vendor would permanently ingest the host without a BIOS
/// password), and a later tick must proceed once creation succeeds again.
#[sqlx_test]
async fn set_uefi_password_client_creation_failure_retries_in_place(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut env = Env::builder(pool.clone()).build().await;

    let mh = setup_host(&env).await;

    // The SET step resolves the site-wide host UEFI credential (version 0 on
    // a fresh site) before dispatching.
    env.redfish_sim
        .seed_credential(
            &CredentialKey::host_uefi_site_default(0),
            &Credentials::UsernamePassword {
                username: String::new(),
                password: "uefi-v0".to_string(),
            },
        )
        .await;

    mh.advance_state(uefi_setup_state(UefiSetupState::SetUefiPassword))
        .await;

    // A tick with the credential op failing on client creation must hold in
    // place rather than fall through to the vendor skip.
    env.redfish_sim
        .set_uefi_setup_client_creation_error("bmc unreachable");
    env.run_single_iteration().await;
    assert!(
        at_uefi_step(
            &mh.host.machine().await.state.value,
            UefiSetupState::SetUefiPassword
        ),
        "a transient creation failure must retry the SET step in place, got {:?}",
        mh.host.machine().await.state.value,
    );

    // Once creation succeeds again, the step proceeds normally.
    env.redfish_sim.clear_uefi_setup_client_creation_error();
    env.run_single_iteration().await;
    assert!(
        at_uefi_step(
            &mh.host.machine().await.state.value,
            UefiSetupState::WaitForPasswordJobScheduled
        ),
        "the retried SET step should dispatch and advance, got {:?}",
        mh.host.machine().await.state.value,
    );

    Ok(())
}

#[sqlx_test]
async fn enrollment_keeps_the_version_sent_while_a_new_target_is_published(pool: PgPool) {
    let mut env = Env::builder(pool.clone()).build().await;
    let mh = setup_host(&env).await;
    let mac = mh.host.machine().await.status.bmc_info.mac.unwrap();
    {
        let mut conn = pool.acquire().await.unwrap();
        assert!(
            device_rotation_status(&mut conn, HostUefi, mac)
                .await
                .unwrap()
                .is_none(),
            "the race must exercise first enrollment, not an existing-row no-op"
        );
    }
    let credentials = Credentials::UsernamePassword {
        username: String::new(),
        password: "uefi-v0".to_string(),
    };
    env.redfish_sim
        .seed_credential(&CredentialKey::host_uefi_site_default(0), &credentials)
        .await;
    mh.advance_state(uefi_setup_state(UefiSetupState::SetUefiPassword))
        .await;

    let (started, resume) = env.redfish_sim.pause_next_uefi_setup();
    let publish_target = async {
        let sent = tokio::time::timeout(std::time::Duration::from_secs(30), started)
            .await
            .expect("controller reached UEFI setup")
            .expect("setup dispatched");
        assert_eq!(sent, credentials);
        sqlx::query("UPDATE sitewide_credential_rotation SET target_version = 1 WHERE credential_type = 'host_uefi'")
            .execute(&pool).await.unwrap();
        resume.send(()).unwrap();
    };
    tokio::join!(env.run_single_iteration(), publish_target);

    let saved = mh.host.machine().await.state.value;
    let ManagedHostState::HostInit {
        machine_state: MachineState::UefiSetup { uefi_setup_info },
    } = saved
    else {
        panic!("setup should save its dispatched version")
    };
    assert_eq!(uefi_setup_info.credential_version, Some(0));
    assert_eq!(
        uefi_setup_info.uefi_setup_state,
        UefiSetupState::WaitForPasswordJobScheduled
    );

    for _ in 0..3 {
        env.run_single_iteration().await;
    }
    let machine = mh.host.machine().await;
    assert!(machine.bios_password_set_time.is_some());
    let mut conn = pool.acquire().await.unwrap();
    let status = device_rotation_status(&mut conn, HostUefi, mac)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(status.current_version, Some(0));
    assert_eq!(status.target_version, 1);
    assert!(
        !status.converged,
        "the newer credential still needs to be applied"
    );
}

#[sqlx_test]
async fn saved_setup_records_the_selected_version_or_an_unknown_legacy_version(pool: PgPool) {
    let mut env = Env::builder(pool.clone()).build().await;
    let mh = setup_host(&env).await;
    let mac = mh.host.machine().await.status.bmc_info.mac.unwrap();

    for version in [Some(0), None] {
        // Deserialize the persisted format, including a job saved before the
        // version field existed. Completion must not consult today's target
        // to decide which password that saved job applied.
        let mut saved = serde_json::json!({
            "uefi_password_jid": "password-job",
            "uefi_setup_state": { "state": "waitforpasswordjobcompletion" }
        });
        if let Some(version) = version {
            saved["credential_version"] = serde_json::json!(version);
        }
        let info: UefiSetupInfo = serde_json::from_value(saved).unwrap();
        mh.advance_state(ManagedHostState::HostInit {
            machine_state: MachineState::UefiSetup {
                uefi_setup_info: info,
            },
        })
        .await;
        sqlx::query("UPDATE machines SET bios_password_set_time = NULL WHERE id = $1")
            .bind(mh.host.id)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM device_credential_rotation WHERE device_mac = $1 AND credential_type = 'host_uefi'")
            .bind(mac).execute(&pool).await.unwrap();
        sqlx::query("UPDATE sitewide_credential_rotation SET target_version = 1 WHERE credential_type = 'host_uefi'")
            .execute(&pool).await.unwrap();

        env.redfish_sim.set_job_state_sequence(vec![
            libredfish::JobState::Running,
            libredfish::JobState::Completed,
        ]);
        env.run_single_iteration().await;
        assert!(at_uefi_step(
            &mh.host.machine().await.state.value,
            UefiSetupState::WaitForPasswordJobCompletion
        ));
        env.run_single_iteration().await;

        let machine = mh.host.machine().await;
        let mut conn = pool.acquire().await.unwrap();
        let status = device_rotation_status(&mut conn, HostUefi, mac)
            .await
            .unwrap()
            .expect("a completed job must enroll the host even if its version is unknown");
        assert_eq!(status.current_version, version);
        assert!(!status.converged);
        assert!(machine.bios_password_set_time.is_some());
        assert!(matches!(
            machine.state.value,
            ManagedHostState::HostInit {
                machine_state: MachineState::WaitingForLockdown {
                    lockdown_info: LockdownInfo {
                        state: LockdownState::SetLockdown,
                        mode: LockdownMode::Enable,
                    },
                },
            }
        ));
    }
}

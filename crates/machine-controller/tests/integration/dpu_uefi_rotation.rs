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

//! End-to-end coverage for machine-controller DPU UEFI credential rotation:
//! a staged site-wide `dpu_uefi` target drives a Ready host to
//! converge one of its DPUs through the single-tick
//! `ManagedHostState::RotatingDpuUefi` state (stage -> DPU restart -> record) and
//! back to Ready, converging that DPU's tracked UEFI version. The DPU is a
//! distinct device from its host, keyed by the DPU BMC MAC and applied via a DPU
//! restart, so these tests target the host's first DPU rather than the host
//! itself. As with the host sibling there is no per-device secret: the site-wide
//! versioned credential fully determines the password, so the assertions cover
//! the rotation bookkeeping (target/current version, convergence, quarantine).

use std::sync::Arc;

use carbide_secrets::credentials::{CredentialKey, Credentials};
use carbide_secrets::test_support::credentials::TestCredentialManager;
use carbide_test_harness::prelude::*;
use carbide_test_harness::test_support::fixture_config::FixtureDefault as _;
use chrono::{Duration, Utc};
use db::credential_rotation::{
    CredentialRotationType, device_rotation_status, increment_rotate_attempt,
    record_device_converged, set_next_target_version,
};
use mac_address::MacAddress;
use model::machine::ManagedHostState;
use model::test_support::ManagedHostConfig;

use crate::env::Env;

const DPU_UEFI: CredentialRotationType = CredentialRotationType::DpuUefi;

/// The site-wide DPU UEFI secret at a version. DPU UEFI passwords are uniform per
/// version, so this (not a per-device key) is what the rotation resolves and
/// applies.
fn site_key(version: u32) -> CredentialKey {
    CredentialKey::dpu_uefi_site_default(version)
}

fn uefi_creds(password: &str) -> Credentials {
    Credentials::UsernamePassword {
        username: String::new(),
        password: password.to_string(),
    }
}

/// Build a Ready pool host (whose default config carries one DPU) on the shared
/// Redfish sim and return it plus the first DPU's BMC MAC (the key for DPU UEFI
/// rotation bookkeeping).
async fn ready_host_with_dpu(env: &Env) -> (TestManagedHost, MacAddress) {
    let domain = env.test_harness.test_domain().await;
    let network_controller = env.test_harness.network_controller();
    let underlay_segment = network_controller.create_underlay_segment(&domain).await;
    network_controller.create_admin_segment(&domain).await;
    let site_explorer = env.test_harness.default_test_site_explorer();
    let mh = env
        .test_harness
        .managed_host_builder(&site_explorer, underlay_segment)
        .with_config(ManagedHostConfig::default())
        .build()
        .await
        .0;
    // Model the converged-Ready invariant the controller establishes before it
    // returns a host to Ready (boot interface verified); a plain
    // `advance_state(Ready)` would leave a pending boot-config intent that the
    // Ready handler converges ahead of the rotation entry guards.
    mh.advance_to_converged_ready().await;

    let dpu_mac = mh
        .first_dpu()
        .machine()
        .await
        .status
        .bmc_info
        .mac
        .expect("fixture DPU should have a BMC MAC");
    (mh, dpu_mac)
}

/// Stage a site-wide DPU UEFI rotation to version 1 with the DPU lagging at the
/// v0 baseline: record the device converged at v0, advance the target to 1, and
/// seed the site-wide v1 secret the controller resolves and applies.
async fn stage_lagging_dpu_uefi(
    env: &Env,
    pool: &PgPool,
    dpu_mac: MacAddress,
) -> Result<(), Box<dyn std::error::Error>> {
    {
        let mut conn = pool.acquire().await?;
        record_device_converged(&mut conn, dpu_mac, DPU_UEFI).await?;
        set_next_target_version(&mut conn, DPU_UEFI, 0, serde_json::json!({}))
            .await?
            .expect("target must advance from version 0");
    }
    // The controller resolves the site-wide UEFI credential through the Redfish
    // pool's own store, so seed it there rather than in the API credential store.
    env.redfish_sim
        .seed_credential(&site_key(1), &uefi_creds("dpu-uefi-v1"))
        .await;
    Ok(())
}

/// Advance the controller until the host settles back in Ready, returning the
/// number of iterations taken. Bounded so a wedged FSM fails loudly instead of
/// hanging.
async fn run_until_ready(env: &mut Env, mh: &TestManagedHost) -> usize {
    for iteration in 1..=12 {
        if matches!(mh.host.machine().await.state.value, ManagedHostState::Ready) {
            return iteration - 1;
        }
        env.run_single_iteration().await;
    }
    assert!(
        matches!(mh.host.machine().await.state.value, ManagedHostState::Ready),
        "DPU UEFI rotation FSM did not return to Ready within the iteration budget, got {:?}",
        mh.host.machine().await.state.value,
    );
    12
}

/// A Ready pool host with a DPU whose tracked DPU UEFI version lags a freshly
/// staged site-wide target rotates that DPU on its own: the entry guard promotes
/// the host to `RotatingDpuUefi` for that DPU, the single-tick state stages the
/// change and restarts the DPU, and the device converges to the target version
/// before returning to Ready.
#[sqlx_test]
async fn ready_dpu_converges_uefi_to_site_target(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let cm = Arc::new(TestCredentialManager::default());
    let mut env = Env::builder(pool.clone())
        .with_credential_manager(cm.clone())
        // The passive rotation guard is gated behind the site-wide feature flag.
        .configure_runtime(|c| c.uefi_rotation_enabled = true)
        .build()
        .await;

    let (mh, dpu_mac) = ready_host_with_dpu(&env).await;
    let dpu_machine_id = mh.first_dpu().id;
    stage_lagging_dpu_uefi(&env, &pool, dpu_mac).await?;

    // The device lags the staged target before the controller runs.
    {
        let mut conn = pool.acquire().await?;
        let status = device_rotation_status(&mut conn, DPU_UEFI, dpu_mac)
            .await?
            .expect("device rotation row should exist");
        assert!(
            !status.converged,
            "DPU should lag the staged target before rotation"
        );
    }

    // Iteration 1: Ready observes the lagging DPU and enters RotatingDpuUefi for
    // exactly that DPU.
    env.run_single_iteration().await;
    assert!(
        matches!(
            mh.host.machine().await.state.value,
            ManagedHostState::RotatingDpuUefi { dpu_machine_id: id } if id == dpu_machine_id
        ),
        "expected RotatingDpuUefi for the lagging DPU after the entry guard fires, got {:?}",
        mh.host.machine().await.state.value,
    );

    // Drive the single-tick state (stage -> DPU restart -> record) until the host
    // settles back in Ready.
    run_until_ready(&mut env, &mh).await;

    // The DPU is recorded converged at the target version.
    {
        let mut conn = pool.acquire().await?;
        let status = device_rotation_status(&mut conn, DPU_UEFI, dpu_mac)
            .await?
            .expect("device rotation row should exist");
        assert!(status.converged, "DPU should be converged after rotation");
        assert_eq!(
            status.current_version,
            Some(1),
            "DPU should be recorded at target version 1"
        );
    }

    Ok(())
}

/// With the site-wide feature flag off (the default), a Ready host with a DPU
/// that lags the staged target must NOT rotate the DPU on its own: the passive
/// gate is the fleet kill-switch, so the host stays Ready and the DPU's tracked
/// version is untouched.
#[sqlx_test]
async fn feature_flag_off_suppresses_passive_dpu_uefi_rotation(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let cm = Arc::new(TestCredentialManager::default());
    // No configure_runtime: uefi_rotation_enabled defaults to false.
    let mut env = Env::builder(pool.clone())
        .with_credential_manager(cm.clone())
        .build()
        .await;

    let (mh, dpu_mac) = ready_host_with_dpu(&env).await;
    stage_lagging_dpu_uefi(&env, &pool, dpu_mac).await?;

    // A full sweep must leave the host in Ready: the disabled flag keeps the
    // passive gate from ever promoting it to RotatingDpuUefi.
    env.run_single_iteration().await;
    assert!(
        matches!(mh.host.machine().await.state.value, ManagedHostState::Ready),
        "expected Ready to be preserved while the feature flag is off, got {:?}",
        mh.host.machine().await.state.value,
    );
    {
        let mut conn = pool.acquire().await?;
        let status = device_rotation_status(&mut conn, DPU_UEFI, dpu_mac)
            .await?
            .expect("device rotation row should exist");
        assert!(
            !status.converged,
            "DPU must remain unrotated while the feature flag is off"
        );
    }

    Ok(())
}

/// The operator force-converge escape hatch overrides both the site-wide flag
/// (off here) and the DPU's active backoff quarantine: the targeted DPU rotates
/// its UEFI credential on the next sweep and the one-shot request is cleared
/// afterward.
#[sqlx_test]
async fn force_request_converges_quarantined_dpu_uefi_when_disabled(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let cm = Arc::new(TestCredentialManager::default());
    // Feature flag stays off: only the force request should drive rotation.
    let mut env = Env::builder(pool.clone())
        .with_credential_manager(cm.clone())
        .build()
        .await;

    let (mh, dpu_mac) = ready_host_with_dpu(&env).await;
    let dpu_machine_id = mh.first_dpu().id;
    stage_lagging_dpu_uefi(&env, &pool, dpu_mac).await?;

    // Quarantine the DPU (so the passive gate would skip it even if enabled) and
    // record the operator's force-converge request on the DPU machine row.
    {
        let mut conn = pool.acquire().await?;
        increment_rotate_attempt(
            &mut conn,
            dpu_mac,
            DPU_UEFI,
            "seed backoff",
            Utc::now() + Duration::seconds(3600),
        )
        .await?;
        db::machine::set_uefi_credential_rotation_requested(&mut conn, dpu_machine_id.into())
            .await?;
    }

    // Iteration 1: the force request drives entry into RotatingDpuUefi for the
    // targeted DPU despite the disabled flag and the active quarantine.
    env.run_single_iteration().await;
    assert!(
        matches!(
            mh.host.machine().await.state.value,
            ManagedHostState::RotatingDpuUefi { dpu_machine_id: id } if id == dpu_machine_id
        ),
        "expected RotatingDpuUefi from the force request, got {:?}",
        mh.host.machine().await.state.value,
    );

    // Drive the state until the forced rotation settles in Ready.
    run_until_ready(&mut env, &mh).await;

    // The DPU converged despite its quarantine, and the one-shot request was
    // cleared so it does not re-enter.
    {
        let mut conn = pool.acquire().await?;
        let status = device_rotation_status(&mut conn, DPU_UEFI, dpu_mac)
            .await?
            .expect("device rotation row should exist");
        assert!(status.converged, "forced DPU should be converged");
        assert_eq!(status.current_version, Some(1));
    }
    assert!(
        !mh.first_dpu()
            .machine()
            .await
            .uefi_credential_rotation_requested,
        "the one-shot force request must be cleared once rotation settles"
    );

    Ok(())
}

/// A device-level DPU UEFI change failure (the BIOS rejects the password change)
/// must not wedge the host in `RotatingDpuUefi`: the state quarantines the DPU
/// with backoff, records a password-redacted error, and returns to Ready so a
/// later sweep can retry once the window elapses.
#[sqlx_test]
async fn dpu_uefi_change_failure_quarantines_and_returns_to_ready(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let cm = Arc::new(TestCredentialManager::default());
    let mut env = Env::builder(pool.clone())
        .with_credential_manager(cm.clone())
        .configure_runtime(|c| c.uefi_rotation_enabled = true)
        .build()
        .await;

    let (mh, dpu_mac) = ready_host_with_dpu(&env).await;
    stage_lagging_dpu_uefi(&env, &pool, dpu_mac).await?;

    // Model a BIOS that rejects the change. The error carries the new password so
    // we can assert the recorded rotation error is redacted end to end.
    env.redfish_sim
        .set_uefi_password_change_error("boom dpu-uefi-v1 rejected");

    // Iteration 1: Ready observes the lag and enters RotatingDpuUefi.
    env.run_single_iteration().await;
    assert!(
        matches!(
            mh.host.machine().await.state.value,
            ManagedHostState::RotatingDpuUefi { .. }
        ),
        "expected RotatingDpuUefi after the entry guard fires, got {:?}",
        mh.host.machine().await.state.value,
    );

    // The failing change must land the host back in Ready rather than looping in
    // the state.
    run_until_ready(&mut env, &mh).await;

    // The DPU did not converge; it is quarantined with a recorded, redacted error
    // and an incremented attempt count.
    {
        let mut conn = pool.acquire().await?;
        let status = device_rotation_status(&mut conn, DPU_UEFI, dpu_mac)
            .await?
            .expect("device rotation row should exist");
        assert!(
            !status.converged,
            "a rejected UEFI change must not converge the DPU"
        );
        assert!(
            status.quarantined,
            "a failed attempt must quarantine the DPU with backoff"
        );
        assert!(
            status.rotate_attempts >= 1,
            "a failed attempt must be counted, got {}",
            status.rotate_attempts
        );
        let recorded = status
            .rotate_last_error_redacted
            .expect("a failed attempt must record an error");
        assert!(
            !recorded.contains("dpu-uefi-v1"),
            "the recorded rotation error must be password-redacted, got {recorded:?}"
        );
    }

    Ok(())
}

/// A client-creation failure during the DPU UEFI change (credential store,
/// TCP, or the vendor probe) is transient: the state must hold in place with
/// no quarantine or counted attempt, and a later tick must complete the
/// rotation once creation succeeds again.
#[sqlx_test]
async fn dpu_uefi_client_creation_failure_retries_without_quarantine(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let cm = Arc::new(TestCredentialManager::default());
    let mut env = Env::builder(pool.clone())
        .with_credential_manager(cm.clone())
        .configure_runtime(|c| c.uefi_rotation_enabled = true)
        .build()
        .await;

    let (mh, dpu_mac) = ready_host_with_dpu(&env).await;
    stage_lagging_dpu_uefi(&env, &pool, dpu_mac).await?;

    // Iteration 1: Ready observes the lag and enters RotatingDpuUefi.
    env.run_single_iteration().await;
    assert!(
        matches!(
            mh.host.machine().await.state.value,
            ManagedHostState::RotatingDpuUefi { .. }
        ),
        "expected RotatingDpuUefi after the entry guard fires, got {:?}",
        mh.host.machine().await.state.value,
    );

    // A tick with client creation failing must hold in place: same state, no
    // quarantine, no counted attempt.
    env.redfish_sim
        .set_create_client_error("dpu bmc unreachable");
    env.run_single_iteration().await;
    assert!(
        matches!(
            mh.host.machine().await.state.value,
            ManagedHostState::RotatingDpuUefi { .. }
        ),
        "a transient creation failure must not advance or abort the rotation, got {:?}",
        mh.host.machine().await.state.value,
    );
    {
        let mut conn = pool.acquire().await?;
        let status = device_rotation_status(&mut conn, DPU_UEFI, dpu_mac)
            .await?
            .expect("device rotation row should exist");
        assert!(
            !status.quarantined,
            "a transient creation failure must not quarantine the DPU"
        );
        assert_eq!(
            status.rotate_attempts, 0,
            "a transient creation failure must not count a rotation attempt"
        );
    }

    // Once creation succeeds again, a later iteration retries and the
    // rotation converges normally.
    env.redfish_sim.clear_create_client_error();
    run_until_ready(&mut env, &mh).await;
    {
        let mut conn = pool.acquire().await?;
        let status = device_rotation_status(&mut conn, DPU_UEFI, dpu_mac)
            .await?
            .expect("device rotation row should exist");
        assert!(status.converged, "the retried rotation should converge");
        assert_eq!(status.current_version, Some(1));
    }

    Ok(())
}

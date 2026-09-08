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

//! End-to-end coverage for machine-controller host UEFI credential rotation:
//! a staged site-wide target drives a Ready host through the
//! multi-tick `ManagedHostState::RotatingHostUefi` FSM (set -> job -> power-cycle ->
//! poll -> record) and back to Ready, converging the device's tracked UEFI
//! version. Unlike the BMC sibling there is no per-device secret: the site-wide
//! versioned credential fully determines the password, so these tests assert the
//! rotation bookkeeping (target/current version, convergence, quarantine) rather
//! than a rewritten per-device secret.

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
use model::machine::{ManagedHostState, UefiSetupState};
use model::test_support::ManagedHostConfig;

use crate::env::Env;

const HOST_UEFI: CredentialRotationType = CredentialRotationType::HostUefi;

/// The site-wide host UEFI secret at a version. Host UEFI passwords are uniform
/// per version, so this (not a per-device key) is what the rotation resolves and
/// applies.
fn site_key(version: u32) -> CredentialKey {
    CredentialKey::host_uefi_site_default(version)
}

fn uefi_creds(password: &str) -> Credentials {
    Credentials::UsernamePassword {
        username: String::new(),
        password: password.to_string(),
    }
}

/// Build a Ready pool host on the shared Redfish sim and return its BMC MAC (the
/// key for host UEFI rotation bookkeeping).
async fn ready_host(env: &Env, pool: &PgPool) -> (TestManagedHost, MacAddress) {
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

    // Model a normally-ingested host: NICo stamps `bios_password_set_time` when
    // it sets the BIOS password during ingestion, and the passive host UEFI
    // rotation guard requires that stamp (an unforced rotation only runs where
    // NICo is known to own the password).
    {
        let mut conn = pool.acquire().await.expect("acquire connection");
        db::machine::update_bios_password_set_time(&mh.host.id, &mut conn)
            .await
            .expect("stamp bios_password_set_time");
    }

    let host_mac = mh
        .host
        .machine()
        .await
        .status
        .bmc_info
        .mac
        .expect("fixture host should have a BMC MAC");
    (mh, host_mac)
}

/// Stage a site-wide host UEFI rotation to version 1 with the host lagging at the
/// v0 baseline: record the device converged at v0, advance the target to 1, and
/// seed the site-wide v1 secret the controller resolves and applies.
async fn stage_lagging_host_uefi(
    env: &Env,
    pool: &PgPool,
    host_mac: MacAddress,
) -> Result<(), Box<dyn std::error::Error>> {
    {
        let mut conn = pool.acquire().await?;
        record_device_converged(&mut conn, host_mac, HOST_UEFI).await?;
        set_next_target_version(&mut conn, HOST_UEFI, 0, serde_json::json!({}))
            .await?
            .expect("target must advance from version 0");
    }
    // The controller resolves the site-wide UEFI credential through the Redfish
    // pool's own store, so seed it there rather than in the API credential store.
    env.redfish_sim
        .seed_credential(&site_key(1), &uefi_creds("uefi-v1"))
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
        "host UEFI rotation FSM did not return to Ready within the iteration budget, got {:?}",
        mh.host.machine().await.state.value,
    );
    12
}

/// A Ready pool host whose tracked host UEFI version lags a freshly staged
/// site-wide target rotates on its own: the entry guard promotes it to
/// `RotatingHostUefi`, the multi-tick FSM applies the change and power-cycles the
/// host, and the device converges to the target version before returning to
/// Ready.
#[sqlx_test]
async fn ready_host_converges_uefi_to_site_target(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let cm = Arc::new(TestCredentialManager::default());
    let mut env = Env::builder(pool.clone())
        .with_credential_manager(cm.clone())
        // The passive rotation guard is gated behind the site-wide feature flag.
        .configure_runtime(|c| c.uefi_rotation_enabled = true)
        .build()
        .await;

    let (mh, host_mac) = ready_host(&env, &pool).await;
    stage_lagging_host_uefi(&env, &pool, host_mac).await?;

    // The device lags the staged target before the controller runs.
    {
        let mut conn = pool.acquire().await?;
        let status = device_rotation_status(&mut conn, HOST_UEFI, host_mac)
            .await?
            .expect("device rotation row should exist");
        assert!(
            !status.converged,
            "device should lag the staged target before rotation"
        );
    }

    // Iteration 1: Ready observes the lag and enters the RotatingHostUefi FSM.
    env.run_single_iteration().await;
    assert!(
        matches!(
            mh.host.machine().await.state.value,
            ManagedHostState::RotatingHostUefi { .. }
        ),
        "expected RotatingHostUefi after the entry guard fires, got {:?}",
        mh.host.machine().await.state.value,
    );

    // Drive the remaining multi-tick FSM (set -> schedule -> power-cycle -> poll
    // -> record) until the host settles back in Ready.
    run_until_ready(&mut env, &mh).await;

    // The device is recorded converged at the target version.
    {
        let mut conn = pool.acquire().await?;
        let status = device_rotation_status(&mut conn, HOST_UEFI, host_mac)
            .await?
            .expect("device rotation row should exist");
        assert!(
            status.converged,
            "device should be converged after rotation"
        );
        assert_eq!(
            status.current_version,
            Some(1),
            "device should be recorded at target version 1"
        );
    }

    Ok(())
}

/// With the site-wide feature flag off (the default), a Ready host that lags the
/// staged target must NOT rotate on its own: the passive gate is the fleet
/// kill-switch, so the host stays Ready and its tracked version is untouched.
#[sqlx_test]
async fn feature_flag_off_suppresses_passive_uefi_rotation(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let cm = Arc::new(TestCredentialManager::default());
    // No configure_runtime: uefi_rotation_enabled defaults to false.
    let mut env = Env::builder(pool.clone())
        .with_credential_manager(cm.clone())
        .build()
        .await;

    let (mh, host_mac) = ready_host(&env, &pool).await;
    stage_lagging_host_uefi(&env, &pool, host_mac).await?;

    // A full sweep must leave the lagging host in Ready: the disabled flag keeps
    // the passive gate from ever promoting it to RotatingHostUefi.
    env.run_single_iteration().await;
    assert!(
        matches!(mh.host.machine().await.state.value, ManagedHostState::Ready),
        "expected Ready to be preserved while the feature flag is off, got {:?}",
        mh.host.machine().await.state.value,
    );
    {
        let mut conn = pool.acquire().await?;
        let status = device_rotation_status(&mut conn, HOST_UEFI, host_mac)
            .await?
            .expect("device rotation row should exist");
        assert!(
            !status.converged,
            "device must remain unrotated while the feature flag is off"
        );
    }

    Ok(())
}

/// The operator force-converge escape hatch overrides both the site-wide flag
/// (off here) and the device's active backoff quarantine: the targeted host
/// rotates its UEFI credential on the next sweep and the one-shot request is
/// cleared afterward.
#[sqlx_test]
async fn force_request_converges_quarantined_uefi_when_disabled(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let cm = Arc::new(TestCredentialManager::default());
    // Feature flag stays off: only the force request should drive rotation.
    let mut env = Env::builder(pool.clone())
        .with_credential_manager(cm.clone())
        .build()
        .await;

    let (mh, host_mac) = ready_host(&env, &pool).await;
    let machine_id = mh.host.machine().await.id;
    stage_lagging_host_uefi(&env, &pool, host_mac).await?;

    // Quarantine the device (so the passive gate would skip it even if enabled)
    // and record the operator's force-converge request for this host's UEFI.
    {
        let mut conn = pool.acquire().await?;
        increment_rotate_attempt(
            &mut conn,
            host_mac,
            HOST_UEFI,
            "seed backoff",
            Utc::now() + Duration::seconds(3600),
        )
        .await?;
        db::machine::set_uefi_credential_rotation_requested(&mut conn, machine_id).await?;
    }

    // Iteration 1: the force request drives entry into RotatingHostUefi despite the
    // disabled flag and the active quarantine.
    env.run_single_iteration().await;
    assert!(
        matches!(
            mh.host.machine().await.state.value,
            ManagedHostState::RotatingHostUefi { .. }
        ),
        "expected RotatingHostUefi from the force request, got {:?}",
        mh.host.machine().await.state.value,
    );

    // Drive the remaining FSM ticks until the forced rotation settles in Ready.
    run_until_ready(&mut env, &mh).await;

    // The device converged despite its quarantine, and the one-shot request was
    // cleared so it does not re-enter.
    {
        let mut conn = pool.acquire().await?;
        let status = device_rotation_status(&mut conn, HOST_UEFI, host_mac)
            .await?
            .expect("device rotation row should exist");
        assert!(status.converged, "forced device should be converged");
        assert_eq!(status.current_version, Some(1));
    }
    assert!(
        !mh.host.machine().await.uefi_credential_rotation_requested,
        "the one-shot force request must be cleared once rotation settles"
    );

    Ok(())
}

/// A device-level UEFI change failure (the BIOS rejects the password change)
/// must not wedge the host in `RotatingHostUefi`: the SET step quarantines the device
/// with backoff, records a password-redacted error, and returns to Ready so a
/// later sweep can retry once the window elapses.
#[sqlx_test]
async fn uefi_change_failure_quarantines_and_returns_to_ready(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let cm = Arc::new(TestCredentialManager::default());
    let mut env = Env::builder(pool.clone())
        .with_credential_manager(cm.clone())
        .configure_runtime(|c| c.uefi_rotation_enabled = true)
        .build()
        .await;

    let (mh, host_mac) = ready_host(&env, &pool).await;
    stage_lagging_host_uefi(&env, &pool, host_mac).await?;

    // Model a BIOS that rejects the change. The error carries the new password so
    // we can assert the recorded rotation error is redacted end to end.
    env.redfish_sim
        .set_uefi_password_change_error("boom uefi-v1 rejected");

    // Iteration 1: Ready observes the lag and enters RotatingHostUefi.
    env.run_single_iteration().await;
    assert!(
        matches!(
            mh.host.machine().await.state.value,
            ManagedHostState::RotatingHostUefi { .. }
        ),
        "expected RotatingHostUefi after the entry guard fires, got {:?}",
        mh.host.machine().await.state.value,
    );

    // The failing change must land the host back in Ready rather than looping in
    // the state.
    run_until_ready(&mut env, &mh).await;

    // The device did not converge; it is quarantined with a recorded, redacted
    // error and an incremented attempt count.
    {
        let mut conn = pool.acquire().await?;
        let status = device_rotation_status(&mut conn, HOST_UEFI, host_mac)
            .await?
            .expect("device rotation row should exist");
        assert!(
            !status.converged,
            "a rejected UEFI change must not converge the device"
        );
        assert!(
            status.quarantined,
            "a failed attempt must quarantine the device with backoff"
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
            !recorded.contains("uefi-v1"),
            "the recorded rotation error must be password-redacted, got {recorded:?}"
        );
    }

    Ok(())
}

/// A client-creation failure during the SET step (credential store, TCP, or
/// the vendor probe) is transient: the FSM must hold in place with no
/// quarantine or counted attempt, and a later tick must complete the rotation
/// once creation succeeds again.
#[sqlx_test]
async fn uefi_client_creation_failure_retries_without_quarantine(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let cm = Arc::new(TestCredentialManager::default());
    let mut env = Env::builder(pool.clone())
        .with_credential_manager(cm.clone())
        .configure_runtime(|c| c.uefi_rotation_enabled = true)
        .build()
        .await;

    let (mh, host_mac) = ready_host(&env, &pool).await;
    stage_lagging_host_uefi(&env, &pool, host_mac).await?;

    // Drive the FSM to the SET step with a healthy sim, so the injected
    // failure lands in the dispatch itself rather than an earlier step.
    let at_set_step = |state: &ManagedHostState| {
        matches!(
            state,
            ManagedHostState::RotatingHostUefi { uefi_setup_info }
                if uefi_setup_info.uefi_setup_state == UefiSetupState::SetUefiPassword
        )
    };
    for _ in 0..4 {
        if at_set_step(&mh.host.machine().await.state.value) {
            break;
        }
        env.run_single_iteration().await;
    }
    assert!(
        at_set_step(&mh.host.machine().await.state.value),
        "FSM should reach the SET step, got {:?}",
        mh.host.machine().await.state.value,
    );

    // A tick with client creation failing must hold in place: same step, no
    // quarantine, no counted attempt.
    env.redfish_sim.set_create_client_error("bmc unreachable");
    env.run_single_iteration().await;
    assert!(
        at_set_step(&mh.host.machine().await.state.value),
        "a transient creation failure must not advance or abort the SET step, got {:?}",
        mh.host.machine().await.state.value,
    );
    {
        let mut conn = pool.acquire().await?;
        let status = device_rotation_status(&mut conn, HOST_UEFI, host_mac)
            .await?
            .expect("device rotation row should exist");
        assert!(
            !status.quarantined,
            "a transient creation failure must not quarantine the device"
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
        let status = device_rotation_status(&mut conn, HOST_UEFI, host_mac)
            .await?
            .expect("device rotation row should exist");
        assert!(status.converged, "the retried rotation should converge");
        assert_eq!(status.current_version, Some(1));
    }

    Ok(())
}

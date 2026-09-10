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

//! End-to-end coverage for machine-controller BMC credential rotation:
//! a staged site-wide target drives a Ready host through
//! `ManagedHostState::RotatingBmc` and back to Ready, converging the device and
//! persisting the rotated per-device secret.

use std::sync::Arc;

use carbide_secrets::credentials::{
    BmcCredentialType, CredentialKey, CredentialReader, CredentialWriter, Credentials,
};
use carbide_secrets::test_support::credentials::TestCredentialManager;
use carbide_test_harness::prelude::*;
use carbide_test_harness::test_support::fixture_config::FixtureDefault as _;
use chrono::{Duration, Utc};
use db::credential_rotation::{
    CredentialRotationType, device_rotation_status, increment_rotate_attempt,
    record_device_converged, set_next_target_version,
};
use mac_address::MacAddress;
use model::bmc_suppression::{BmcSuppressionSubsystem, NewBmcSuppression};
use model::machine::ManagedHostState;
use model::test_support::ManagedHostConfig;

use crate::env::Env;

const BMC: CredentialRotationType = CredentialRotationType::Bmc;

/// Stand in for site-explorer acknowledging every pending BMC suppression -- the
/// barrier the rotation gate waits on before changing a credential. The
/// integration harness drives only the machine controller, so a test plays the
/// site-explorer ack pass itself.
async fn ack_all_site_explorer_suppressions(pool: &PgPool) {
    let mut conn = pool.acquire().await.expect("acquire connection");
    let macs: Vec<MacAddress> = db::bmc_suppression::find_all_by_subsystem(
        &mut *conn,
        BmcSuppressionSubsystem::SiteExplorer,
    )
    .await
    .expect("read suppressions")
    .into_iter()
    .map(|s| s.bmc_mac_address)
    .collect();
    db::bmc_suppression::acknowledge_unacknowledged(
        &mut conn,
        &macs,
        BmcSuppressionSubsystem::SiteExplorer,
    )
    .await
    .expect("acknowledge suppressions");
}

fn per_device_key(mac: MacAddress) -> CredentialKey {
    CredentialKey::BmcCredentials {
        credential_type: BmcCredentialType::BmcRoot {
            bmc_mac_address: mac,
        },
    }
}

fn rotate_to_key(version: u32) -> CredentialKey {
    CredentialKey::BmcCredentials {
        credential_type: BmcCredentialType::site_wide_root(version),
    }
}

fn creds(username: &str, password: &str) -> Credentials {
    Credentials::UsernamePassword {
        username: username.to_string(),
        password: password.to_string(),
    }
}

/// Stage a site-wide rotation to version 1 with the host BMC lagging at the v0
/// baseline: seed the per-device "old" secret in both the sim and the store,
/// advance the target, and stage the rotate-to "new" secret.
async fn stage_lagging_bmc(
    pool: &PgPool,
    cm: &TestCredentialManager,
    host_mac: MacAddress,
) -> Result<(), Box<dyn std::error::Error>> {
    cm.set_credentials(&per_device_key(host_mac), &creds("root", "old"))
        .await
        .expect("staging the per-device secret should succeed");
    {
        let mut conn = pool.acquire().await?;
        record_device_converged(&mut conn, host_mac, BMC).await?;
        set_next_target_version(&mut conn, BMC, 0, serde_json::json!({}))
            .await?
            .expect("target must advance from version 0");
    }
    cm.set_credentials(&rotate_to_key(1), &creds("root", "new"))
        .await
        .expect("staging the rotate-to secret should succeed");
    Ok(())
}

/// When the BMC password change lands but persisting the new per-device secret
/// to the store fails, the hardware is AHEAD of the store. The host must hold in
/// `RotatingBmc` (staying non-allocatable and keeping the site-explorer
/// suppression) rather than settling to Ready with a stale stored secret, and it
/// must keep retrying until the store reconciles -- reconciliation is never
/// abandoned.
#[sqlx_test]
async fn store_persist_failure_holds_in_rotating_bmc_until_reconciled(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let cm = Arc::new(TestCredentialManager::default());
    let mut env = Env::builder(pool.clone())
        .with_credential_manager(cm.clone())
        .configure_runtime(|c| c.bmc_rotation_enabled = true)
        .build()
        .await;

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
    mh.advance_to_converged_ready().await;

    let host_mac = mh
        .host
        .machine()
        .await
        .status
        .bmc_info
        .mac
        .expect("fixture host should have a BMC MAC");
    env.redfish_sim.seed_user("root", "old");
    stage_lagging_bmc(&pool, &cm, host_mac).await?;

    // Drive up to the converging tick: enter RotatingBmc, let the gate record the
    // suppression, and acknowledge it.
    env.run_single_iteration().await; // Ready -> RotatingBmc
    env.run_single_iteration().await; // gate records the suppression and waits
    ack_all_site_explorer_suppressions(&pool).await;

    // The store write now fails: the hardware change will land but the persist
    // will not, leaving the BMC ahead of the store.
    cm.set_set_credentials_failure(true);

    // Iteration 3: the password change succeeds on the hardware but the persist
    // fails, so the host holds in RotatingBmc instead of returning to Ready.
    env.run_single_iteration().await;
    assert!(
        matches!(
            mh.host.machine().await.state.value,
            ManagedHostState::RotatingBmc { .. }
        ),
        "a persist failure must hold in RotatingBmc (non-allocatable), got {:?}",
        mh.host.machine().await.state.value,
    );
    // The site-explorer suppression is retained across the hold -- we must not
    // resume probing a BMC whose stored secret still lags the hardware.
    assert!(
        db::bmc_suppression::is_suppressed(&pool, host_mac, BmcSuppressionSubsystem::SiteExplorer)
            .await?,
        "the rotation suppression must be retained while the store lags"
    );
    // The device is neither converged nor quarantined: it stays eligible so the
    // persist is retried every sweep rather than backed off.
    {
        let mut conn = pool.acquire().await?;
        let status = device_rotation_status(&mut conn, BMC, host_mac)
            .await?
            .expect("device rotation row should exist");
        assert!(
            !status.converged,
            "hardware ahead of store is not converged"
        );
        assert!(
            !status.quarantined,
            "a persist failure must not record a backoff"
        );
    }
    // The hardware carries the new password, but the store still lags.
    assert_eq!(
        env.redfish_sim.user_password("root").as_deref(),
        Some("new"),
        "the hardware change must have landed even though the persist failed"
    );
    assert_eq!(
        cm.get_credentials(&per_device_key(host_mac))
            .await
            .expect("reading the per-device secret should succeed")
            .expect("per-device secret should still be set"),
        creds("root", "old"),
        "the stored secret must still lag after the failed persist"
    );

    // Iteration 4: still failing -> still holding (the hold is not a bounded
    // transient retry; it persists until the store reconciles).
    env.run_single_iteration().await;
    assert!(
        matches!(
            mh.host.machine().await.state.value,
            ManagedHostState::RotatingBmc { .. }
        ),
        "the hold must persist across sweeps while the store keeps failing, got {:?}",
        mh.host.machine().await.state.value,
    );

    // The store becomes writable again: the next tick's change-then-verify
    // recovery re-persists and converges, and the host returns to Ready.
    cm.set_set_credentials_failure(false);
    env.run_single_iteration().await;
    assert!(
        matches!(mh.host.machine().await.state.value, ManagedHostState::Ready),
        "expected Ready once the store reconciles, got {:?}",
        mh.host.machine().await.state.value,
    );
    assert!(
        !db::bmc_suppression::is_suppressed(&pool, host_mac, BmcSuppressionSubsystem::SiteExplorer)
            .await?,
        "resume must delete the rotation suppression once reconciled"
    );
    {
        let mut conn = pool.acquire().await?;
        let status = device_rotation_status(&mut conn, BMC, host_mac)
            .await?
            .expect("device rotation row should exist");
        assert!(
            status.converged,
            "device should converge once the store reconciles"
        );
        assert_eq!(status.current_version, Some(1));
    }
    assert_eq!(
        cm.get_credentials(&per_device_key(host_mac))
            .await
            .expect("reading the per-device secret should succeed")
            .expect("per-device secret should still be set"),
        creds("root", "new"),
        "the store is reconciled to the new password once the write succeeds"
    );

    Ok(())
}

/// With the site-wide feature flag off (the default), a Ready host that lags the
/// staged target must NOT rotate on its own: the passive gate is the fleet
/// kill-switch, so the host stays Ready.
#[sqlx_test]
async fn feature_flag_off_suppresses_passive_rotation(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let cm = Arc::new(TestCredentialManager::default());
    // No configure_runtime: bmc_rotation_enabled defaults to false.
    let mut env = Env::builder(pool.clone())
        .with_credential_manager(cm.clone())
        .build()
        .await;

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
    mh.advance_to_converged_ready().await;

    let host_mac = mh
        .host
        .machine()
        .await
        .status
        .bmc_info
        .mac
        .expect("fixture host should have a BMC MAC");
    env.redfish_sim.seed_user("root", "old");
    stage_lagging_bmc(&pool, &cm, host_mac).await?;

    // A full sweep must leave the lagging host in Ready: the disabled flag keeps
    // the passive gate from ever promoting it to RotatingBmc.
    env.run_single_iteration().await;
    assert!(
        matches!(mh.host.machine().await.state.value, ManagedHostState::Ready),
        "expected Ready to be preserved while the feature flag is off, got {:?}",
        mh.host.machine().await.state.value,
    );
    {
        let mut conn = pool.acquire().await?;
        let status = device_rotation_status(&mut conn, BMC, host_mac)
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
/// (off here) and the device's active backoff quarantine: the targeted BMC is
/// rotated on the next sweep and the one-shot request is cleared afterward.
#[sqlx_test]
async fn force_request_converges_quarantined_bmc_when_disabled(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let cm = Arc::new(TestCredentialManager::default());
    // Feature flag stays off: only the force request should drive rotation.
    let mut env = Env::builder(pool.clone())
        .with_credential_manager(cm.clone())
        .build()
        .await;

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
    mh.advance_to_converged_ready().await;

    let host = mh.host.machine().await;
    let machine_id = host.id;
    let host_mac = host
        .status
        .bmc_info
        .mac
        .expect("fixture host should have a BMC MAC");
    env.redfish_sim.seed_user("root", "old");
    stage_lagging_bmc(&pool, &cm, host_mac).await?;

    // Quarantine the device (so the passive gate would skip it even if enabled)
    // and record the operator's force-converge request for this BMC.
    {
        let mut conn = pool.acquire().await?;
        increment_rotate_attempt(
            &mut conn,
            host_mac,
            BMC,
            "seed backoff",
            Utc::now() + Duration::seconds(3600),
        )
        .await?;
        db::machine::set_bmc_credential_rotation_requested(&mut conn, &machine_id).await?;
    }

    // Iteration 1: the force request drives entry into RotatingBmc despite the
    // disabled flag.
    env.run_single_iteration().await;
    assert!(
        matches!(
            mh.host.machine().await.state.value,
            ManagedHostState::RotatingBmc { .. }
        ),
        "expected RotatingBmc from the force request, got {:?}",
        mh.host.machine().await.state.value,
    );

    // Iteration 2: even a forced rotation first gates on site-explorer, so it
    // waits in RotatingBmc until the suppression is acknowledged.
    env.run_single_iteration().await;
    assert!(
        matches!(
            mh.host.machine().await.state.value,
            ManagedHostState::RotatingBmc { .. }
        ),
        "a forced rotation must still wait for site-explorer acknowledgement, got {:?}",
        mh.host.machine().await.state.value,
    );
    ack_all_site_explorer_suppressions(&pool).await;

    // Iteration 3: the forced tick bypasses backoff, converges the device, and
    // returns to Ready.
    env.run_single_iteration().await;
    assert!(
        matches!(mh.host.machine().await.state.value, ManagedHostState::Ready),
        "expected Ready once the forced rotation settles, got {:?}",
        mh.host.machine().await.state.value,
    );

    // The device converged despite its quarantine, and the one-shot request was
    // cleared so it does not re-enter.
    {
        let mut conn = pool.acquire().await?;
        let status = device_rotation_status(&mut conn, BMC, host_mac)
            .await?
            .expect("device rotation row should exist");
        assert!(status.converged, "forced device should be converged");
        assert_eq!(status.current_version, Some(1));
    }
    assert!(
        !mh.host.machine().await.bmc_credential_rotation_requested,
        "the one-shot force request must be cleared once rotation settles"
    );
    let persisted = cm
        .get_credentials(&per_device_key(host_mac))
        .await
        .expect("reading the per-device secret should succeed")
        .expect("per-device secret should still be set");
    assert_eq!(persisted, creds("root", "new"));

    Ok(())
}

/// An operator's decommission suppression on the host BMC must survive a
/// rotation: the gate preserves it (never overwrites its reason) and the resume
/// deletes only the rotation-reason row, so the operator suppression remains.
#[sqlx_test]
async fn rotation_preserves_an_operator_suppression(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let cm = Arc::new(TestCredentialManager::default());
    let mut env = Env::builder(pool.clone())
        .with_credential_manager(cm.clone())
        .configure_runtime(|c| c.bmc_rotation_enabled = true)
        .build()
        .await;

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
    mh.advance_to_converged_ready().await;

    let host_mac = mh
        .host
        .machine()
        .await
        .status
        .bmc_info
        .mac
        .expect("fixture host should have a BMC MAC");
    env.redfish_sim.seed_user("root", "old");
    stage_lagging_bmc(&pool, &cm, host_mac).await?;

    // An operator has independently suppressed this BMC for decommissioning.
    {
        let mut conn = pool.acquire().await?;
        db::bmc_suppression::upsert(
            &mut conn,
            &NewBmcSuppression {
                bmc_mac_address: host_mac,
                subsystem: BmcSuppressionSubsystem::SiteExplorer,
                reason: "decommissioning".to_string(),
            },
        )
        .await?;
    }

    // Drive the gated rotation to completion.
    env.run_single_iteration().await; // Ready -> RotatingBmc
    env.run_single_iteration().await; // gate records the suppression and waits
    ack_all_site_explorer_suppressions(&pool).await;
    env.run_single_iteration().await; // barrier satisfied -> rotate -> Ready
    assert!(
        matches!(mh.host.machine().await.state.value, ManagedHostState::Ready),
        "the rotation should settle back to Ready, got {:?}",
        mh.host.machine().await.state.value,
    );

    // The device rotated, yet the operator's suppression survives with its
    // reason intact -- the resume only removed rotation-owned rows.
    {
        let mut conn = pool.acquire().await?;
        let status = device_rotation_status(&mut conn, BMC, host_mac)
            .await?
            .expect("device rotation row should exist");
        assert!(status.converged, "device should have rotated");
    }
    let operator =
        db::bmc_suppression::find(&pool, host_mac, BmcSuppressionSubsystem::SiteExplorer)
            .await?
            .expect("operator suppression must survive the rotation");
    assert_eq!(
        operator.reason, "decommissioning",
        "the operator's suppression reason must be preserved"
    );

    Ok(())
}

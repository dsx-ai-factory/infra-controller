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

use std::sync::Arc;

use carbide_secrets::credentials::{
    BmcCredentialType, CredentialKey, CredentialReader, CredentialType, CredentialWriter,
    Credentials, NicLockdownIkm,
};
use carbide_test_harness::prelude::*;
use component_manager::component_manager::ComponentManager;
use component_manager::mock::{MockComputeTrayManager, MockNvSwitchManager, MockPowerShelfManager};
use mac_address::MacAddress;
use rpc::forge::{
    CredentialRotationStatusRequest, RotateCredentialRequest, RotationCredentialType,
};
use tonic::Code;

async fn init(pool: PgPool) -> TestHarness {
    TestHarness::builder(pool).build().await
}

fn component_manager_with_nvos_rotation(supported: bool) -> Arc<ComponentManager> {
    let nv_switch = if supported {
        MockNvSwitchManager::default().with_password_rotation_enabled()
    } else {
        MockNvSwitchManager::default()
    };

    Arc::new(ComponentManager::new(
        Arc::new(nv_switch),
        Arc::new(MockPowerShelfManager),
        Arc::new(MockComputeTrayManager),
        false,
        false,
        false,
    ))
}

async fn init_with_nvos_rotation(pool: PgPool) -> TestHarness {
    TestHarness::builder(pool)
        .with_api_builder_fn(|builder| {
            builder.with_component_manager(component_manager_with_nvos_rotation(true))
        })
        .build()
        .await
}

/// Pulls the password out of a stored credential, failing the test if the key
/// is missing or not a username/password pair.
async fn stored_password(manager: &impl CredentialReader, key: &CredentialKey) -> Option<String> {
    manager
        .get_credentials(key)
        .await
        .unwrap()
        .map(|Credentials::UsernamePassword { password, .. }| password)
}

/// A site-wide (empty-username) credential carrying `password`.
fn site_creds(password: &str) -> Credentials {
    Credentials::UsernamePassword {
        username: String::new(),
        password: password.to_string(),
    }
}

async fn add_live_nvos_switch(
    env: &TestHarness,
    index: i32,
    bmc_mac: &str,
    nvos_username: Option<&str>,
    nvos_password: Option<&str>,
) -> MacAddress {
    let switch = env.create_switch(index, index).await;
    let mut txn = env.db_txn().await;

    sqlx::query(
        "INSERT INTO expected_switches \
             (serial_number, bmc_mac_address, bmc_username, bmc_password, \
              nvos_username, nvos_password) \
         VALUES ($1, $2::macaddr, 'admin', 'password', $3, $4)",
    )
    .bind(format!("nvos-preflight-{index}"))
    .bind(bmc_mac)
    .bind(nvos_username)
    .bind(nvos_password)
    .execute(&mut *txn)
    .await
    .unwrap();

    sqlx::query("UPDATE switches SET bmc_mac_address = $1::macaddr WHERE id = $2")
        .bind(bmc_mac)
        .bind(switch.id)
        .execute(&mut *txn)
        .await
        .unwrap();

    txn.commit().await.unwrap();

    bmc_mac.parse().unwrap()
}

#[sqlx_test]
async fn rotate_auto_generates_versioned_secret_and_bumps_target(pool: sqlx::PgPool) {
    let env = init(pool).await;

    // bmc is backfilled at target 0; an auto-generated rotation advances to 1.
    let result = env
        .api()
        .rotate_credential(tonic::Request::new(RotateCredentialRequest {
            credential_type: RotationCredentialType::RotationBmc.into(),
            password: None,
            reason: Some("test rotation".to_string()),
        }))
        .await
        .expect("auto-generated rotation should succeed")
        .into_inner();
    assert_eq!(result.target_version, 1);
    assert!(result.started_at.is_some());

    // The versioned rotate-TO secret exists at v1 and satisfies the policy.
    let versioned = stored_password(
        env.api().credential_manager(),
        &CredentialKey::BmcCredentials {
            credential_type: BmcCredentialType::SiteWideRootVersioned { version: 1 },
        },
    )
    .await
    .expect("v1 secret must be written");
    Credentials::validate_password_strength(&versioned)
        .expect("auto-generated rotation password must satisfy the policy");

    // BMC is table-driven: the unversioned site path is NOT repointed by a
    // rotation. The live version is whatever `target_version` names (now 1), so
    // consumers read the v1 secret above; the unversioned path keeps its v0 value
    // (here, unset in the test env).
    let unversioned = stored_password(
        env.api().credential_manager(),
        &CredentialKey::BmcCredentials {
            credential_type: BmcCredentialType::SiteWideRoot,
        },
    )
    .await;
    assert!(
        unversioned.is_none(),
        "a BMC rotation must not write the unversioned site path (table-driven)"
    );

    // Status now reports target 1 with no devices converged yet (the engine is
    // not part of this change), so the rotation reads as complete-but-empty.
    let status = env
        .api()
        .get_credential_rotation_status(tonic::Request::new(CredentialRotationStatusRequest {
            credential_type: RotationCredentialType::RotationBmc.into(),
            device_mac: None,
        }))
        .await
        .expect("status should succeed")
        .into_inner();
    assert_eq!(status.target_version, 1);
    assert_eq!(status.converged, 0);
    assert_eq!(status.pending, 0);
    assert_eq!(status.quarantined, 0);
    assert!(status.complete);
    assert!(
        status.device.is_none(),
        "a site-wide query must not carry per-device detail"
    );
}

#[sqlx_test]
async fn rotate_supersedes_on_repeat(pool: sqlx::PgPool) {
    let env = init(pool).await;

    for expected in [1, 2, 3] {
        let result = env
            .api()
            .rotate_credential(tonic::Request::new(RotateCredentialRequest {
                credential_type: RotationCredentialType::RotationHostUefi.into(),
                password: None,
                reason: None,
            }))
            .await
            .expect("rotation should succeed")
            .into_inner();
        assert_eq!(
            result.target_version, expected,
            "each rotation advances the target by one"
        );
    }
}

#[sqlx_test]
async fn rotate_with_explicit_password_uses_it_verbatim(pool: sqlx::PgPool) {
    let env = init(pool).await;

    let password = "Str0ng-Explicit-Pw!";
    env.api()
        .rotate_credential(tonic::Request::new(RotateCredentialRequest {
            credential_type: RotationCredentialType::RotationLockdownIkm.into(),
            password: Some(password.to_string()),
            reason: None,
        }))
        .await
        .expect("explicit-password rotation should succeed");

    // Lockdown IKM is version-addressed with no alias; the v1 secret is the
    // explicit value verbatim.
    let versioned = stored_password(
        env.api().credential_manager(),
        &CredentialKey::NicLockdownIkm {
            credential_type: NicLockdownIkm::SiteWide { version: 1 },
        },
    )
    .await
    .expect("v1 lockdown secret must be written");
    assert_eq!(versioned, password);
}

#[sqlx_test]
async fn rotate_rejects_weak_explicit_password(pool: sqlx::PgPool) {
    let env = init(pool).await;

    let err = env
        .api()
        .rotate_credential(tonic::Request::new(RotateCredentialRequest {
            credential_type: RotationCredentialType::RotationHostUefi.into(),
            password: Some("weak".to_string()),
            reason: None,
        }))
        .await
        .expect_err("a weak explicit password must be rejected");
    assert_eq!(err.code(), Code::InvalidArgument);

    // The rejected rotation must not have advanced the target or written a v1.
    let stored = stored_password(
        env.api().credential_manager(),
        &CredentialKey::HostUefiSiteVersioned { version: 1 },
    )
    .await;
    assert!(
        stored.is_none(),
        "a rejected rotation must not write a secret"
    );
}

#[sqlx_test]
async fn first_nvos_rotation_publishes_verified_version_zero_target(pool: sqlx::PgPool) {
    let env = init_with_nvos_rotation(pool).await;

    let password = "Nvos-Site-Target-0!";

    let result = env
        .api()
        .rotate_credential(tonic::Request::new(RotateCredentialRequest {
            credential_type: RotationCredentialType::RotationNvos.into(),
            password: Some(password.to_string()),
            reason: None,
        }))
        .await
        .expect("nvos target staging should succeed")
        .into_inner();

    assert_eq!(result.target_version, 0);

    let versioned = stored_password(
        env.api().credential_manager(),
        &CredentialKey::switch_nvos_site_admin(0),
    )
    .await
    .expect("v0 nvos site target must be written");

    assert_eq!(versioned, password);

    let per_device = stored_password(
        env.api().credential_manager(),
        &CredentialKey::SwitchNvosAdmin {
            bmc_mac_address: "02:00:00:00:00:01".parse().unwrap(),
        },
    )
    .await;

    assert!(
        per_device.is_none(),
        "staging an nvos target must not overwrite per-device switch credentials"
    );

    let status = env
        .api()
        .get_credential_rotation_status(tonic::Request::new(CredentialRotationStatusRequest {
            credential_type: RotationCredentialType::RotationNvos.into(),
            device_mac: None,
        }))
        .await
        .expect("nvos status should succeed")
        .into_inner();

    assert_eq!(status.target_version, 0);
    assert_eq!(status.converged, 0);
    assert_eq!(status.pending, 0);
    assert_eq!(status.quarantined, 0);
    assert!(status.complete);
}

#[sqlx_test]
async fn nvos_rotation_rejects_malformed_expected_credentials(pool: sqlx::PgPool) {
    let env = init_with_nvos_rotation(pool).await;
    let bmc_mac = "02:00:00:00:00:51";

    let stored_mac = add_live_nvos_switch(&env, 1, bmc_mac, Some("admin"), None).await;
    env.api()
        .credential_manager()
        .set_credentials(
            &CredentialKey::SwitchNvosAdmin {
                bmc_mac_address: stored_mac,
            },
            &Credentials::UsernamePassword {
                username: "admin".to_string(),
                password: "stored-password".to_string(),
            },
        )
        .await
        .unwrap();

    let error = env
        .api()
        .rotate_credential(tonic::Request::new(RotateCredentialRequest {
            credential_type: RotationCredentialType::RotationNvos.into(),
            password: Some("Nvos-Site-Target-0!".to_string()),
            reason: None,
        }))
        .await
        .expect_err("malformed expected credentials must block target publication");

    assert_eq!(error.code(), Code::FailedPrecondition);
    assert!(error.message().contains(bmc_mac));

    assert!(
        stored_password(
            env.api().credential_manager(),
            &CredentialKey::switch_nvos_site_admin(0),
        )
        .await
        .is_none()
    );
}

#[sqlx_test]
async fn nvos_rotation_accepts_expected_or_stored_current_credentials(pool: sqlx::PgPool) {
    let env = init_with_nvos_rotation(pool).await;

    add_live_nvos_switch(
        &env,
        1,
        "02:00:00:00:00:52",
        Some("admin"),
        Some("expected-password"),
    )
    .await;

    let stored_mac = add_live_nvos_switch(&env, 2, "02:00:00:00:00:53", None, None).await;
    env.api()
        .credential_manager()
        .set_credentials(
            &CredentialKey::SwitchNvosAdmin {
                bmc_mac_address: stored_mac,
            },
            &Credentials::UsernamePassword {
                username: "admin".to_string(),
                password: "stored-password".to_string(),
            },
        )
        .await
        .unwrap();

    let result = env
        .api()
        .rotate_credential(tonic::Request::new(RotateCredentialRequest {
            credential_type: RotationCredentialType::RotationNvos.into(),
            password: Some("Nvos-Site-Target-0!".to_string()),
            reason: None,
        }))
        .await
        .expect("either current credential source should allow rotation")
        .into_inner();

    assert_eq!(result.target_version, 0);
}

#[sqlx_test]
async fn nvos_rotation_requires_supporting_component_manager(pool: sqlx::PgPool) {
    let env = TestHarness::builder(pool)
        .with_api_builder_fn(|builder| {
            builder.with_component_manager(component_manager_with_nvos_rotation(false))
        })
        .build()
        .await;

    let err = env
        .api()
        .rotate_credential(tonic::Request::new(RotateCredentialRequest {
            credential_type: RotationCredentialType::RotationNvos.into(),
            password: Some("Nvos-Site-Target-0!".to_string()),
            reason: None,
        }))
        .await
        .expect_err("unsupported component-manager backend must block NVOS rotation");

    assert_eq!(err.code(), Code::FailedPrecondition);

    assert_eq!(
        err.message(),
        "NVOS rotation requires a component-manager switch backend that supports password rotation"
    );
}

#[sqlx_test]
async fn nvos_status_before_first_target_is_failed_precondition(pool: sqlx::PgPool) {
    let env = init(pool).await;
    let switch = env.create_switch(1, 1).await;
    let bmc_mac = "02:00:00:00:00:41";
    let mut txn = env.db_txn().await;

    sqlx::query(
        "INSERT INTO expected_switches \
             (serial_number, bmc_mac_address, bmc_username, bmc_password) \
         VALUES ('nvos-status-before-target', $1::macaddr, 'admin', 'password')",
    )
    .bind(bmc_mac)
    .execute(&mut *txn)
    .await
    .unwrap();

    sqlx::query("UPDATE switches SET bmc_mac_address = $1::macaddr WHERE id = $2")
        .bind(bmc_mac)
        .bind(switch.id)
        .execute(&mut *txn)
        .await
        .unwrap();

    txn.commit().await.unwrap();

    for device_mac in [None, Some(bmc_mac.to_string())] {
        let error = env
            .api()
            .get_credential_rotation_status(tonic::Request::new(CredentialRotationStatusRequest {
                credential_type: RotationCredentialType::RotationNvos.into(),
                device_mac,
            }))
            .await
            .expect_err("unpublished NVOS status must report a precondition");

        assert_eq!(error.code(), Code::FailedPrecondition);

        assert_eq!(
            error.message(),
            "no site-wide NVOS credential rotation target has been published"
        );
    }

    let unknown = env
        .api()
        .get_credential_rotation_status(tonic::Request::new(CredentialRotationStatusRequest {
            credential_type: RotationCredentialType::RotationNvos.into(),
            device_mac: Some("02:00:00:00:00:42".to_string()),
        }))
        .await
        .expect_err("unknown switch must remain not found");

    assert_eq!(unknown.code(), Code::NotFound);
}

#[sqlx_test]
async fn rotate_dpu_uefi_writes_versioned_secret(pool: sqlx::PgPool) {
    let env = init(pool).await;

    let result = env
        .api()
        .rotate_credential(tonic::Request::new(RotateCredentialRequest {
            credential_type: RotationCredentialType::RotationDpuUefi.into(),
            password: None,
            reason: None,
        }))
        .await
        .expect("dpu uefi rotation should succeed")
        .into_inner();
    assert_eq!(result.target_version, 1);

    stored_password(
        env.api().credential_manager(),
        &CredentialKey::DpuUefiSiteVersioned { version: 1 },
    )
    .await
    .expect("v1 dpu uefi secret must be written");

    // Table-driven: the unversioned DPU UEFI site-default is NOT repointed by a
    // rotation. uefi_setup resolves the live version from `target_version`, so
    // the unversioned path keeps its v0 value (unset in the test env).
    let unversioned = stored_password(
        env.api().credential_manager(),
        &CredentialKey::DpuUefi {
            credential_type: CredentialType::SiteDefault,
        },
    )
    .await;
    assert!(
        unversioned.is_none(),
        "a DPU UEFI rotation must not write the unversioned site-default (table-driven)"
    );
}

// Crash-recovery / idempotency: a prior attempt that wrote the v{N+1} secret but
// crashed before publishing the target leaves the slot populated. An
// auto-generated retry must *adopt* that staged secret (not overwrite it with a
// freshly generated password) and complete the rotation, so the value devices
// will converge to is stable across the retry.
#[sqlx_test]
async fn rotate_adopts_already_staged_secret_on_retry(pool: sqlx::PgPool) {
    let env = init(pool).await;

    // Simulate the pre-existing v1 secret left by the crashed attempt.
    let prestaged = "PreStaged-Rotate-Pw-1!";
    env.api()
        .credential_manager()
        .set_credentials(
            &CredentialKey::BmcCredentials {
                credential_type: BmcCredentialType::SiteWideRootVersioned { version: 1 },
            },
            &site_creds(prestaged),
        )
        .await
        .unwrap();

    env.api()
        .rotate_credential(tonic::Request::new(RotateCredentialRequest {
            credential_type: RotationCredentialType::RotationBmc.into(),
            password: None,
            reason: None,
        }))
        .await
        .expect("auto-generated retry should adopt the staged secret and succeed");

    // The adopted (not regenerated) value is what the versioned slot now holds.
    let versioned = stored_password(
        env.api().credential_manager(),
        &CredentialKey::BmcCredentials {
            credential_type: BmcCredentialType::SiteWideRootVersioned { version: 1 },
        },
    )
    .await
    .unwrap();
    assert_eq!(
        versioned, prestaged,
        "an auto-generated retry must adopt the staged secret, not overwrite it"
    );
    // BMC is table-driven: the retry must not write the unversioned site path.
    let unversioned = stored_password(
        env.api().credential_manager(),
        &CredentialKey::BmcCredentials {
            credential_type: BmcCredentialType::SiteWideRoot,
        },
    )
    .await;
    assert!(
        unversioned.is_none(),
        "a BMC rotation must not write the unversioned site path (table-driven)"
    );
}

// An explicit-password rotation that lands on a version already staged with a
// *different* password is a genuine conflict (a competing rotation claimed the
// slot) and must be reported, not silently overwritten.
#[sqlx_test]
async fn rotate_explicit_conflicts_with_differently_staged_version(pool: sqlx::PgPool) {
    let env = init(pool).await;

    env.api()
        .credential_manager()
        .set_credentials(
            &CredentialKey::BmcCredentials {
                credential_type: BmcCredentialType::SiteWideRootVersioned { version: 1 },
            },
            &site_creds("Already-Staged-By-Other-1!"),
        )
        .await
        .unwrap();

    let err = env
        .api()
        .rotate_credential(tonic::Request::new(RotateCredentialRequest {
            credential_type: RotationCredentialType::RotationBmc.into(),
            password: Some("My-Explicit-Different-1!".to_string()),
            reason: None,
        }))
        .await
        .expect_err("an explicit password conflicting with a staged version must be rejected");
    assert_eq!(err.code(), Code::FailedPrecondition);

    // The conflicting attempt must not have overwritten the staged secret or
    // advanced the target.
    let versioned = stored_password(
        env.api().credential_manager(),
        &CredentialKey::BmcCredentials {
            credential_type: BmcCredentialType::SiteWideRootVersioned { version: 1 },
        },
    )
    .await
    .unwrap();
    assert_eq!(
        versioned, "Already-Staged-By-Other-1!",
        "a conflicting explicit rotation must not overwrite the staged secret"
    );
    let status = env
        .api()
        .get_credential_rotation_status(tonic::Request::new(CredentialRotationStatusRequest {
            credential_type: RotationCredentialType::RotationBmc.into(),
            device_mac: None,
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(
        status.target_version, 0,
        "a conflicting rotation must not advance the target"
    );
}

// The proto3 zero value (unset family) must be rejected rather than defaulting
// to a destructive BMC rotation.
#[sqlx_test]
async fn rotate_rejects_unspecified_credential_type(pool: sqlx::PgPool) {
    let env = init(pool).await;

    let err = env
        .api()
        .rotate_credential(tonic::Request::new(RotateCredentialRequest {
            credential_type: RotationCredentialType::Unspecified.into(),
            password: None,
            reason: None,
        }))
        .await
        .expect_err("an unspecified credential type must be rejected");
    assert_eq!(err.code(), Code::InvalidArgument);
}

#[sqlx_test]
async fn host_uefi_rotation_writes_versioned_secret_only(pool: sqlx::PgPool) {
    let env = init(pool).await;

    env.api()
        .rotate_credential(tonic::Request::new(RotateCredentialRequest {
            credential_type: RotationCredentialType::RotationHostUefi.into(),
            password: None,
            reason: None,
        }))
        .await
        .expect("host uefi rotation should succeed");

    stored_password(
        env.api().credential_manager(),
        &CredentialKey::HostUefiSiteVersioned { version: 1 },
    )
    .await
    .expect("v1 host uefi secret must be written");

    // Table-driven: the unversioned host UEFI site-default is NOT repointed by a
    // rotation. uefi_setup / clear resolve the live version from the rotation
    // table, so the unversioned path keeps its v0 value (unset in the test env).
    let unversioned = stored_password(
        env.api().credential_manager(),
        &CredentialKey::HostUefi {
            credential_type: CredentialType::SiteDefault,
        },
    )
    .await;
    assert!(
        unversioned.is_none(),
        "a host UEFI rotation must not write the unversioned site-default (table-driven)"
    );
}

/// Inserts a `device_credential_rotation` row at `current_version` (no
/// quarantine) for the per-device status tests.
async fn insert_device_row(pool: &sqlx::PgPool, mac: &str, current_version: i32) {
    sqlx::query(
        "INSERT INTO device_credential_rotation (device_mac, credential_type, current_version) \
         VALUES ($1::macaddr, 'bmc', $2)",
    )
    .bind(mac)
    .bind(current_version)
    .execute(pool)
    .await
    .unwrap();
}

// A device-scoped status report collapses the aggregate counts to the single
// device and carries the per-device detail. A converged device reads complete;
// a device behind the target reads pending.
#[sqlx_test]
async fn status_for_device_reports_single_device_detail(pool: sqlx::PgPool) {
    let env = init(pool).await;

    // Advance bmc to target 1 so a device can sit on either side of it.
    env.api()
        .rotate_credential(tonic::Request::new(RotateCredentialRequest {
            credential_type: RotationCredentialType::RotationBmc.into(),
            password: None,
            reason: None,
        }))
        .await
        .expect("rotation should succeed");

    insert_device_row(&env.api().database_connection, "02:00:00:00:00:01", 1).await;
    insert_device_row(&env.api().database_connection, "02:00:00:00:00:02", 0).await;

    // Converged device: counts collapse to converged=1 and the rotation reads
    // complete for that device.
    let converged = env
        .api()
        .get_credential_rotation_status(tonic::Request::new(CredentialRotationStatusRequest {
            credential_type: RotationCredentialType::RotationBmc.into(),
            device_mac: Some("02:00:00:00:00:01".to_string()),
        }))
        .await
        .expect("device status should succeed")
        .into_inner();
    assert_eq!(converged.target_version, 1);
    assert_eq!(converged.converged, 1);
    assert_eq!(converged.pending, 0);
    assert_eq!(converged.quarantined, 0);
    assert!(converged.complete);
    let detail = converged.device.expect("per-device detail must be present");
    assert_eq!(detail.device_mac, "02:00:00:00:00:01");
    assert_eq!(detail.current_version, Some(1));
    assert!(detail.converged);
    assert!(!detail.quarantined);

    // Device behind the target: pending=1 and not complete.
    let pending = env
        .api()
        .get_credential_rotation_status(tonic::Request::new(CredentialRotationStatusRequest {
            credential_type: RotationCredentialType::RotationBmc.into(),
            device_mac: Some("02:00:00:00:00:02".to_string()),
        }))
        .await
        .expect("device status should succeed")
        .into_inner();
    assert_eq!(pending.converged, 0);
    assert_eq!(pending.pending, 1);
    assert!(!pending.complete);
    let detail = pending.device.expect("per-device detail must be present");
    assert_eq!(detail.current_version, Some(0));
    assert!(!detail.converged);
}

// A quarantined device exercises the handler's quarantine mapping: the counts
// collapse to quarantined=1 (not pending), the MAC is listed, the rotation reads
// incomplete, and the richer detail fields (backoff window, attempts, redacted
// error, in-flight version) are carried through to the proto.
#[sqlx_test]
async fn status_for_quarantined_device_reports_detail(pool: sqlx::PgPool) {
    let env = init(pool).await;

    // Advance bmc to target 1 so the device can sit behind the target.
    env.api()
        .rotate_credential(tonic::Request::new(RotateCredentialRequest {
            credential_type: RotationCredentialType::RotationBmc.into(),
            password: None,
            reason: None,
        }))
        .await
        .expect("rotation should succeed");

    // Behind the target (0 < 1), mid-flight to v1, with a future backoff window
    // and a recorded failed attempt.
    sqlx::query(
        "INSERT INTO device_credential_rotation \
             (device_mac, credential_type, current_version, rotating_to_version, \
              rotate_attempts, rotate_last_error_redacted, rotate_quarantined_until) \
         VALUES ('02:00:00:00:00:07'::macaddr, 'bmc', 0, 1, 2, 'redacted boom', \
                 now() + interval '1 hour')",
    )
    .execute(&env.api().database_connection)
    .await
    .unwrap();

    let status = env
        .api()
        .get_credential_rotation_status(tonic::Request::new(CredentialRotationStatusRequest {
            credential_type: RotationCredentialType::RotationBmc.into(),
            device_mac: Some("02:00:00:00:00:07".to_string()),
        }))
        .await
        .expect("device status should succeed")
        .into_inner();
    assert_eq!(status.converged, 0);
    assert_eq!(status.pending, 0);
    assert_eq!(status.quarantined, 1);
    assert_eq!(
        status.quarantined_device_macs,
        vec!["02:00:00:00:00:07".to_string()]
    );
    assert!(
        !status.complete,
        "a quarantined device must keep the rotation from reading complete"
    );

    let detail = status.device.expect("per-device detail must be present");
    assert_eq!(detail.current_version, Some(0));
    assert_eq!(detail.rotating_to_version, Some(1));
    assert!(!detail.converged);
    assert!(detail.quarantined);
    assert!(
        detail.quarantined_until.is_some(),
        "a quarantined device must report its backoff window"
    );
    assert_eq!(detail.rotate_attempts, 2);
    assert_eq!(detail.last_error.as_deref(), Some("redacted boom"));
}

// A MAC with no rotation record is a NotFound, not a fabricated "pending"
// status, so a mistyped MAC is surfaced rather than silently misreported.
#[sqlx_test]
async fn status_for_unknown_device_mac_is_not_found(pool: sqlx::PgPool) {
    let env = init(pool).await;

    let err = env
        .api()
        .get_credential_rotation_status(tonic::Request::new(CredentialRotationStatusRequest {
            credential_type: RotationCredentialType::RotationBmc.into(),
            device_mac: Some("02:00:00:00:00:ff".to_string()),
        }))
        .await
        .expect_err("an unknown device MAC must be NotFound");
    assert_eq!(err.code(), Code::NotFound);
}

// A malformed MAC is a client error (InvalidArgument), not an internal error.
#[sqlx_test]
async fn status_for_malformed_device_mac_is_invalid_argument(pool: sqlx::PgPool) {
    let env = init(pool).await;

    let err = env
        .api()
        .get_credential_rotation_status(tonic::Request::new(CredentialRotationStatusRequest {
            credential_type: RotationCredentialType::RotationBmc.into(),
            device_mac: Some("not-a-mac".to_string()),
        }))
        .await
        .expect_err("a malformed device MAC must be rejected");
    assert_eq!(err.code(), Code::InvalidArgument);
}

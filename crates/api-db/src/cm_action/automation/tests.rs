// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use model::switch::{ConfigureCertificateState, ConfiguringState, SwitchDecommissioningState};

use super::*;
use crate::{ConditionalWrite, TransactionVending as _};

async fn transition_switch(
    pool: &PgPool,
    id: SwitchId,
    version: ConfigVersion,
    state: &SwitchControllerState,
) -> ConfigVersion {
    let mut txn = pool.txn_begin().await.unwrap();
    let next = version.increment();
    assert_eq!(
        crate::switch::try_update_controller_state(&mut txn, id, version, next, state)
            .await
            .unwrap(),
        ConditionalWrite::Applied(())
    );
    txn.commit().await.unwrap();
    next
}

#[crate::sqlx_test]
async fn switch_cycles_keep_context_and_isolate_replacement(pool: PgPool) {
    let mut txn = pool.txn_begin().await.unwrap();
    let switch = crate::test_support::switch::create_seeded_discovered(
        &mut txn,
        81,
        "automatic correlation",
    )
    .await
    .unwrap();
    txn.commit().await.unwrap();
    let mut version = switch.controller_state.version;
    for (start, waiting) in [
        (
            SwitchControllerState::Configuring {
                config_state: ConfiguringState::ConfigureCertificate {
                    configure_certificate: ConfigureCertificateState::Start,
                },
            },
            SwitchControllerState::Configuring {
                config_state: ConfiguringState::ConfigureCertificate {
                    configure_certificate: ConfigureCertificateState::WaitForComplete {
                        job_id: "certificate-job".into(),
                    },
                },
            },
        ),
        (
            SwitchControllerState::Decommissioning {
                decommissioning_state: SwitchDecommissioningState::FactoryResetNvos,
            },
            SwitchControllerState::Decommissioning {
                decommissioning_state: SwitchDecommissioningState::WaitingForNvosFactoryReset {
                    job_id: "reset-job".into(),
                },
            },
        ),
    ] {
        let origin = TraceId::generate();
        version =
            correlation::scope(origin, transition_switch(&pool, switch.id, version, &start)).await;
        let original_version = version;
        assert_eq!(
            resume_switch(&pool, &switch.id, version, &start, async {
                correlation::current_id()
            })
            .await,
            Some(origin)
        );
        version = transition_switch(&pool, switch.id, version, &waiting).await;
        assert_eq!(
            resume_switch(&pool, &switch.id, version, &waiting, async {
                correlation::current_id()
            })
            .await,
            Some(origin)
        );
        // A stale worker cannot attach a replacement's context to its old snapshot.
        assert_eq!(
            resume_switch(&pool, &switch.id, original_version, &start, async {
                correlation::local_id()
            })
            .await,
            None
        );
        version = transition_switch(&pool, switch.id, version, &SwitchControllerState::Ready).await;
        let replacement = TraceId::generate();
        version = correlation::scope(
            replacement,
            transition_switch(&pool, switch.id, version, &start),
        )
        .await;
        assert_eq!(
            resume_switch(&pool, &switch.id, version, &start, async {
                correlation::current_id()
            })
            .await,
            Some(replacement)
        );
        // Controller-state CAS failures must not retire or replace the active action.
        let mut txn = pool.txn_begin().await.unwrap();
        assert!(matches!(
            crate::switch::try_update_controller_state(
                &mut txn,
                switch.id,
                original_version,
                version.increment(),
                &SwitchControllerState::Ready
            )
            .await
            .unwrap(),
            ConditionalWrite::NotApplied(_)
        ));
        txn.commit().await.unwrap();
        assert_eq!(
            resume_switch(&pool, &switch.id, version, &start, async {
                correlation::current_id()
            })
            .await,
            Some(replacement)
        );
        version = transition_switch(&pool, switch.id, version, &SwitchControllerState::Ready).await;
    }
}

#[crate::sqlx_test]
async fn rack_automatic_context_does_not_create_a_maintenance_request(pool: PgPool) {
    let id = RackId::new("automatic-correlation");
    let mut txn = pool.txn_begin().await.unwrap();
    let rack = crate::rack::create(
        &mut txn,
        &id,
        None,
        &model::rack::RackConfig::default(),
        None,
    )
    .await
    .unwrap();
    txn.commit().await.unwrap();
    let state = RackState::Maintenance {
        maintenance_state: model::rack::RackMaintenanceState::FirmwareUpgrade {
            rack_firmware_upgrade: model::rack::FirmwareUpgradeState::Start,
        },
    };
    let origin = TraceId::generate();
    let next = rack.controller_state.version.increment();
    let mut txn = pool.txn_begin().await.unwrap();
    assert_eq!(
        correlation::scope(
            origin,
            crate::rack::try_update_controller_state(
                &mut txn,
                &id,
                rack.controller_state.version,
                next,
                &state
            )
        )
        .await
        .unwrap(),
        ConditionalWrite::Applied(())
    );
    txn.commit().await.unwrap();
    assert_eq!(
        resume_rack(&pool, &id, next, async { correlation::current_id() }).await,
        Some(origin)
    );
    let config: sqlx::types::Json<model::rack::RackConfig> =
        sqlx::query_scalar("SELECT config FROM racks WHERE id = $1")
            .bind(&id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(config.maintenance_requested.is_none());
    // Generic config reconciliation must retain this live automatic action.
    let mut txn = pool.txn_begin().await.unwrap();
    crate::rack::update(&mut txn, &id, &config).await.unwrap();
    txn.commit().await.unwrap();
    assert_eq!(
        resume_rack(&pool, &id, next, async { correlation::current_id() }).await,
        Some(origin)
    );
}

#[crate::sqlx_test]
async fn journal_failure_does_not_abort_controller_transition(pool: PgPool) {
    let mut txn = pool.txn_begin().await.unwrap();
    let switch =
        crate::test_support::switch::create_seeded_discovered(&mut txn, 82, "failure isolation")
            .await
            .unwrap();
    txn.commit().await.unwrap();
    sqlx::query("ALTER TABLE cm_actions ADD CONSTRAINT reject_automation CHECK (NOT starts_with(request_key, 'automation/'))").execute(&pool).await.unwrap();
    let state = SwitchControllerState::Configuring {
        config_state: ConfiguringState::RotateOsPassword,
    };
    let version =
        transition_switch(&pool, switch.id, switch.controller_state.version, &state).await;
    let mut conn = pool.acquire().await.unwrap();
    let reloaded = crate::switch::find_by_id(&mut *conn, &switch.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(reloaded.controller_state.value, state);
    assert_eq!(reloaded.controller_state.version, version);
    assert!(resume_switch(&pool, &switch.id, version, &state, async { true }).await);
}

#[crate::sqlx_test]
async fn completed_automatic_history_is_bounded(pool: PgPool) {
    let mut txn = pool.txn_begin().await.unwrap();
    let switch =
        crate::test_support::switch::create_seeded_discovered(&mut txn, 83, "history retention")
            .await
            .unwrap();
    txn.commit().await.unwrap();
    let mut version = switch.controller_state.version;
    let state = SwitchControllerState::Configuring {
        config_state: ConfiguringState::RotateOsPassword,
    };
    for _ in 0..23 {
        version = transition_switch(&pool, switch.id, version, &state).await;
        version = transition_switch(&pool, switch.id, version, &SwitchControllerState::Ready).await;
    }
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM cm_actions WHERE switch_id = $1")
        .bind(switch.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 20);
}

#[crate::sqlx_test]
async fn nvos_retry_preserves_origin_without_affecting_firmware_tracking(pool: PgPool) {
    use crate::credential_rotation::CredentialRotationType::Nvos;
    use crate::credential_rotation::{self as rotation};
    let mac = "02:00:00:00:00:91".parse().unwrap();
    let origin = TraceId::generate();
    let mut txn = pool.txn_begin().await.unwrap();
    assert!(matches!(
        rotation::set_initial_target_version(&mut txn, Nvos, serde_json::json!({}))
            .await
            .unwrap(),
        ConditionalWrite::Applied(_)
    ));
    let ConditionalWrite::Applied(attempt) = correlation::scope(
        origin,
        rotation::record_device_rotation_started(&mut txn, mac, Nvos, 0),
    )
    .await
    .unwrap() else {
        panic!("rotation must be staged")
    };
    txn.commit().await.unwrap();
    assert_eq!(
        resume_nvos(&pool, mac, 0, attempt, async { correlation::current_id() }).await,
        Some(origin)
    );
    let mut txn = pool.txn_begin().await.unwrap();
    assert_eq!(
        rotation::record_device_rotation_submitted(&mut txn, mac, Nvos, 0, attempt, "original-job")
            .await
            .unwrap(),
        ConditionalWrite::Applied(())
    );
    let ConditionalWrite::Applied(retry) = rotation::record_device_rotation_retry_started(
        &mut txn,
        mac,
        Nvos,
        0,
        attempt,
        Some("original-job"),
    )
    .await
    .unwrap() else {
        panic!("observation retry must be staged")
    };
    txn.commit().await.unwrap();
    // Firmware save/retention on the same MAC must not retire rotation metadata.
    crate::direct_dispatch_rms_job::save(
        &pool,
        mac,
        crate::direct_dispatch_rms_job::RmsJobKind::FirmwareObject,
        "firmware-job",
    )
    .await
    .unwrap();
    assert_eq!(
        resume_nvos(&pool, mac, 0, retry, async { correlation::current_id() }).await,
        Some(origin)
    );
    assert_eq!(
        resume_nvos(&pool, mac, 0, attempt, async { correlation::local_id() }).await,
        None
    );
    let mut txn = pool.txn_begin().await.unwrap();
    assert_eq!(
        rotation::record_device_rotation_submitted(&mut txn, mac, Nvos, 0, retry, "retry-job")
            .await
            .unwrap(),
        ConditionalWrite::Applied(())
    );
    assert_eq!(
        rotation::record_device_rotation_succeeded(&mut txn, mac, Nvos, 0, retry, "retry-job")
            .await
            .unwrap(),
        ConditionalWrite::Applied(())
    );
    txn.commit().await.unwrap();
    assert_eq!(
        resume_nvos(&pool, mac, 0, retry, async { correlation::local_id() }).await,
        None
    );
    let next_origin = TraceId::generate();
    let mut txn = pool.txn_begin().await.unwrap();
    assert!(matches!(
        rotation::set_next_target_version(&mut txn, Nvos, 0, serde_json::json!({}))
            .await
            .unwrap(),
        ConditionalWrite::Applied(_)
    ));
    let ConditionalWrite::Applied(next) = correlation::scope(
        next_origin,
        rotation::record_device_rotation_started(&mut txn, mac, Nvos, 1),
    )
    .await
    .unwrap() else {
        panic!("next version must be staged")
    };
    txn.commit().await.unwrap();
    assert_eq!(
        resume_nvos(&pool, mac, 1, next, async { correlation::current_id() }).await,
        Some(next_origin)
    );
    assert!(
        crate::direct_dispatch_rms_job::get(
            &pool,
            mac,
            crate::direct_dispatch_rms_job::RmsJobKind::FirmwareObject
        )
        .await
        .unwrap()
        .is_some()
    );
}

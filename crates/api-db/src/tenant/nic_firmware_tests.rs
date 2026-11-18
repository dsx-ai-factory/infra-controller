// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use carbide_libmlx_model::firmware::FirmwareSpec;
use model::nic_firmware::{
    NicFirmwareApproach, NicFirmwareArtifact, NicFirmwareProfile, NicFirmwareProfileConfig,
    NicFirmwareProfileEntry,
};
use sqlx::PgPool;
use sqlx::migrate::Migrator;

use super::*;
use crate::migrations::migrate_before;
use crate::nic_firmware;
use crate::test_support::postgres::wait_for_blocked_query;

/// Ordinary fields make selection writes' preservation behavior observable.
async fn create_tenant(txn: &mut PgConnection) -> Tenant {
    create_and_persist(
        "firmware-tenant".into(),
        Metadata {
            name: "Firmware tenant".into(),
            ..Default::default()
        },
        Some("external".into()),
        txn,
    )
    .await
    .expect("create tenant")
}

/// A catalog entry supplies the real foreign-key target without binding hardware.
async fn create_profile(txn: &mut PgConnection) -> NicFirmwareProfile {
    nic_firmware::create(
        txn,
        &"tenant-profile".parse().unwrap(),
        &NicFirmwareProfileConfig {
            entries: vec![NicFirmwareProfileEntry {
                firmware: FirmwareSpec {
                    part_number: "MCX75310AAS-NEAT".into(),
                    psid: "MT_0000000838".into(),
                    version: "28.43.1014".into(),
                },
                image: NicFirmwareArtifact {
                    url: "https://firmware.invalid/tenant.bin".parse().unwrap(),
                    sha256: "ab".repeat(32),
                },
                device_config: None,
                approach: NicFirmwareApproach::Scout,
            }],
        },
    )
    .await
    .expect("create NIC firmware profile")
}

/// Ordinary metadata and identity-policy writers share the token but do not own
/// the selection column. Their updates must not erase independently stored intent.
#[crate::sqlx_test]
async fn nic_firmware_config_shares_tenant_version_and_preserves_other_fields(pool: PgPool) {
    let mut txn = pool.begin().await.unwrap();
    let tenant = create_tenant(&mut txn).await;
    let profile = create_profile(&mut txn).await;
    txn.commit().await.unwrap();
    let organization_id = &tenant.organization_id;
    assert_eq!(
        find_nic_firmware_config(&pool, organization_id)
            .await
            .unwrap(),
        Some(TenantNicFirmwareConfig {
            organization_id: organization_id.clone(),
            profile_id: None,
            version: tenant.version,
        })
    );

    let mut txn = pool.begin().await.unwrap();
    let selected =
        set_nic_firmware_config(&mut txn, organization_id, Some(&profile.id), tenant.version)
            .await
            .unwrap();
    assert_eq!(
        selected.version.version_nr(),
        tenant.version.version_nr() + 1
    );
    let unchanged = find(organization_id.as_str(), false, &mut txn)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(unchanged.metadata, tenant.metadata);
    assert_eq!(unchanged.routing_profile_type, tenant.routing_profile_type);
    assert_eq!(unchanged.version, selected.version);

    // Ordinary tenant updates and identity-config version increments must keep
    // the separately owned selection while consuming the same tenant token.
    let ordinary = update(
        organization_id.to_string(),
        Metadata {
            name: "Renamed firmware tenant".into(),
            ..Default::default()
        },
        selected.version,
        Some("internal".into()),
        &mut txn,
    )
    .await
    .unwrap();
    increment_version(organization_id.as_str(), &mut txn)
        .await
        .unwrap();
    txn.commit().await.unwrap();
    let mut current = find_nic_firmware_config(&pool, organization_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(current.profile_id, Some(profile.id.clone()));
    assert_eq!(
        current.version.version_nr(),
        ordinary.version.version_nr() + 1
    );

    // Equal version numbers with different timestamps are still stale tokens.
    let stale = format!("V{}-T0", current.version.version_nr())
        .parse()
        .unwrap();
    let mut txn = pool.begin().await.unwrap();
    let error = set_nic_firmware_config(&mut txn, organization_id, None, stale)
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        DatabaseError::ConcurrentModificationError(..)
    ));
    txn.rollback().await.unwrap();
    assert_eq!(
        find_nic_firmware_config(&pool, organization_id)
            .await
            .unwrap(),
        Some(current.clone())
    );

    for (scenario, profile_id) in [
        // Reasserting the current selection still consumes the shared token.
        ("same selection", Some(&profile.id)),
        // Clearing removes the reference without deleting the tenant.
        ("clear selection", None),
        // An already-unset selection must not turn the write into a token no-op.
        ("clear absent selection", None),
    ] {
        let mut txn = pool.begin().await.unwrap();
        let changed =
            set_nic_firmware_config(&mut txn, organization_id, profile_id, current.version)
                .await
                .unwrap();
        assert_eq!(changed.profile_id.as_ref(), profile_id, "{scenario}");
        assert_eq!(
            changed.version.version_nr(),
            current.version.version_nr() + 1,
            "{scenario}"
        );
        let stored_tenant = find(organization_id.as_str(), false, &mut txn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored_tenant.metadata, ordinary.metadata, "{scenario}");
        assert_eq!(
            stored_tenant.routing_profile_type, ordinary.routing_profile_type,
            "{scenario}"
        );
        assert_eq!(stored_tenant.version, changed.version, "{scenario}");
        txn.commit().await.unwrap();
        assert_eq!(
            find_nic_firmware_config(&pool, organization_id)
                .await
                .unwrap(),
            Some(changed.clone()),
            "{scenario}"
        );
        current = changed;
    }

    let missing_tenant: TenantOrganizationId = "missing-tenant".parse().unwrap();
    let missing_profile: NicFirmwareProfileId = "missing-profile".parse().unwrap();
    for (organization_id, profile_id, expected_kind, expected_id) in [
        // Clearing cannot implicitly create a missing tenant.
        (&missing_tenant, None, "tenant", missing_tenant.to_string()),
        // An existing profile must not let selection create a missing tenant.
        (
            &missing_tenant,
            Some(&profile.id),
            "tenant",
            missing_tenant.to_string(),
        ),
        // A missing profile is reported before comparing the tenant's stale token.
        (
            organization_id,
            Some(&missing_profile),
            "NIC firmware profile",
            missing_profile.to_string(),
        ),
        // Profile absence wins even when the tenant is also missing.
        (
            &missing_tenant,
            Some(&missing_profile),
            "NIC firmware profile",
            missing_profile.to_string(),
        ),
    ] {
        let mut txn = pool.begin().await.unwrap();
        let error = set_nic_firmware_config(&mut txn, organization_id, profile_id, stale)
            .await
            .unwrap_err();
        assert!(
            matches!(&error, DatabaseError::NotFoundError { kind, id } if *kind == expected_kind && id == &expected_id),
            "{error:?}"
        );
        txn.rollback().await.unwrap();
    }
    assert_eq!(
        find_nic_firmware_config(&pool, &missing_tenant)
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        find_nic_firmware_config(&pool, organization_id)
            .await
            .unwrap(),
        Some(current)
    );
}

/// Observe the clear waiting on the actual row lock before committing the set;
/// the awakened writer must compare against the winner's complete token.
#[crate::sqlx_test]
async fn concurrent_nic_firmware_writes_preserve_the_committed_tenant_config(pool: PgPool) {
    let mut txn = pool.begin().await.unwrap();
    let tenant = create_tenant(&mut txn).await;
    let profile = create_profile(&mut txn).await;
    txn.commit().await.unwrap();

    let mut winner = pool.begin().await.unwrap();
    let selected = set_nic_firmware_config(
        &mut winner,
        &tenant.organization_id,
        Some(&profile.id),
        tenant.version,
    )
    .await
    .unwrap();
    let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *winner)
        .await
        .unwrap();
    let competing_clear = async {
        let mut txn = pool.begin().await.unwrap();
        let error =
            set_nic_firmware_config(&mut txn, &tenant.organization_id, None, tenant.version)
                .await
                .unwrap_err();
        txn.rollback().await.unwrap();
        error
    };
    let release_winner = async {
        wait_for_blocked_query(&pool, blocker_pid, "SELECT version FROM tenants").await;
        winner.commit().await.unwrap();
    };
    let (error, ()) = tokio::join!(competing_clear, release_winner);
    assert!(matches!(
        error,
        DatabaseError::ConcurrentModificationError(..)
    ));
    assert_eq!(
        find_nic_firmware_config(&pool, &tenant.organization_id)
            .await
            .unwrap(),
        Some(selected)
    );
}

/// A delete that owns the profile lock must make the waiting set return NotFound,
/// rather than reaching the tenant write and surfacing a raw foreign-key error.
#[crate::sqlx_test]
async fn profile_deletion_wins_over_waiting_tenant_selection(pool: PgPool) {
    let mut txn = pool.begin().await.unwrap();
    let tenant = create_tenant(&mut txn).await;
    let profile = create_profile(&mut txn).await;
    txn.commit().await.unwrap();

    let mut winner = pool.begin().await.unwrap();
    nic_firmware::delete(&mut winner, &profile.id, profile.version)
        .await
        .unwrap();
    let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *winner)
        .await
        .unwrap();
    let competing_set = async {
        let mut txn = pool.begin().await.unwrap();
        let error = set_nic_firmware_config(
            &mut txn,
            &tenant.organization_id,
            Some(&profile.id),
            tenant.version,
        )
        .await
        .unwrap_err();
        txn.rollback().await.unwrap();
        error
    };
    let release_winner = async {
        wait_for_blocked_query(
            &pool,
            blocker_pid,
            "SELECT id FROM nic_firmware_profiles WHERE id = $1 FOR SHARE",
        )
        .await;
        winner.commit().await.unwrap();
    };
    let (error, ()) = tokio::join!(competing_set, release_winner);
    assert!(
        matches!(&error, DatabaseError::NotFoundError { kind, id }
            if *kind == "NIC firmware profile" && id == &profile.id.to_string()),
        "{error:?}"
    );
    assert_eq!(
        find_nic_firmware_config(&pool, &tenant.organization_id)
            .await
            .unwrap(),
        Some(TenantNicFirmwareConfig {
            organization_id: tenant.organization_id,
            profile_id: None,
            version: tenant.version,
        })
    );
    assert!(
        nic_firmware::find_by_ids(&pool, &[profile.id])
            .await
            .unwrap()
            .is_empty()
    );
}

/// A tenant selection protects profile existence, but without bound devices it
/// must not freeze the hardware pairs that the profile supports.
#[crate::sqlx_test]
async fn tenant_nic_firmware_reference_allows_profile_edits_but_blocks_deletion_until_clear(
    pool: PgPool,
) {
    let mut txn = pool.begin().await.unwrap();
    let tenant = create_tenant(&mut txn).await;
    let profile = create_profile(&mut txn).await;
    let selected = set_nic_firmware_config(
        &mut txn,
        &tenant.organization_id,
        Some(&profile.id),
        tenant.version,
    )
    .await
    .unwrap();
    let mut replacement = profile.config;
    replacement.entries[0].firmware.psid = "MT_0000000999".into();
    let profile = nic_firmware::update(&mut txn, &profile.id, &replacement, profile.version)
        .await
        .unwrap();
    txn.commit().await.unwrap();

    let mut txn = pool.begin().await.unwrap();
    let error = nic_firmware::delete(&mut txn, &profile.id, profile.version)
        .await
        .unwrap_err();
    assert!(
        matches!(&error, DatabaseError::FailedPrecondition(reason)
        if reason == "profile is selected by a tenant NIC firmware selection"),
        "{error:?}"
    );
    txn.rollback().await.unwrap();

    let mut txn = pool.begin().await.unwrap();
    let cleared =
        set_nic_firmware_config(&mut txn, &tenant.organization_id, None, selected.version)
            .await
            .unwrap();
    nic_firmware::delete(&mut txn, &profile.id, profile.version)
        .await
        .unwrap();
    txn.commit().await.unwrap();
    assert_eq!(cleared.profile_id, None);
    assert_eq!(
        find_nic_firmware_config(&pool, &tenant.organization_id)
            .await
            .unwrap(),
        Some(cleared)
    );
    assert!(
        nic_firmware::find_by_ids(&pool, &[profile.id])
            .await
            .unwrap()
            .is_empty()
    );
}

/// Apply the shipped migration to populated predecessor rows using production's
/// squash-first order, so row preservation and the new reference are tested together.
#[crate::sqlx_test]
async fn tenant_nic_firmware_migration_preserves_existing_rows_and_enforces_profile_reference(
    pool: PgPool,
) {
    const VERSION: i64 = 20261016184101;
    const MIGRATION: &str =
        include_str!("../../migrations/20261016184101_tenant_nic_firmware_profile.sql");
    let migrations = sqlx::migrate!("./migrations");

    migrate_before(&pool, VERSION).await;
    let mut txn = pool.begin().await.unwrap();
    let tenant = create_tenant(&mut txn).await;
    let profile = create_profile(&mut txn).await;
    increment_version(tenant.organization_id.as_str(), &mut txn)
        .await
        .unwrap();
    txn.commit().await.unwrap();
    let before: serde_json::Value =
        sqlx::query_scalar("SELECT to_jsonb(tenants) FROM tenants WHERE organization_id = $1")
            .bind(&tenant.organization_id)
            .fetch_one(&pool)
            .await
            .unwrap();

    let migration = migrations
        .iter()
        .find(|migration| migration.version == VERSION)
        .unwrap();
    assert_eq!(migration.sql.as_ref(), MIGRATION);
    let mut upgrade = Migrator::with_migrations(vec![migration.clone()]);
    upgrade.set_ignore_missing(true);
    upgrade.run(&pool).await.unwrap();

    let after: serde_json::Value = sqlx::query_scalar("SELECT to_jsonb(tenants) - 'nic_firmware_profile_id' FROM tenants WHERE organization_id = $1")
        .bind(&tenant.organization_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(after, before);
    let config = find_nic_firmware_config(&pool, &tenant.organization_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(config.profile_id, None);
    assert_eq!(config.version.version_nr(), 2);
    let profiles = nic_firmware::find_by_ids(&pool, std::slice::from_ref(&profile.id))
        .await
        .unwrap();
    assert_eq!(profiles.len(), 1);
    assert_eq!(profiles[0].config, profile.config);
    assert_eq!(profiles[0].version, profile.version);

    let mut txn = pool.begin().await.unwrap();
    let error = sqlx::query(
        "UPDATE tenants SET nic_firmware_profile_id = 'missing-profile' WHERE organization_id = $1",
    )
    .bind(&tenant.organization_id)
    .execute(&mut *txn)
    .await
    .unwrap_err();
    assert_eq!(
        error.as_database_error().unwrap().constraint(),
        Some("tenants_nic_firmware_profile_id_fkey")
    );
    txn.rollback().await.unwrap();

    let mut txn = pool.begin().await.unwrap();
    let selected = set_nic_firmware_config(
        &mut txn,
        &tenant.organization_id,
        Some(&profile.id),
        config.version,
    )
    .await
    .unwrap();
    txn.commit().await.unwrap();
    assert_eq!(
        find_nic_firmware_config(&pool, &tenant.organization_id)
            .await
            .unwrap(),
        Some(selected)
    );
}

// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Frozen firmware inputs owned by an instance allocation.

use std::collections::HashSet;

use carbide_uuid::instance::InstanceId;
use config_version::ConfigVersion;
use model::instance::nic_firmware::InstanceNicFirmwareSnapshot;
use model::nic_firmware::{NicFirmwareCardSelection, NicFirmwareProfile, NicFirmwareProfileId};
use model::tenant::{TenantNicFirmwareConfig, TenantOrganizationId};
use sqlx::PgConnection;
use sqlx::types::Json;

use crate::{DatabaseError, DatabaseResult};

#[derive(sqlx::FromRow)]
struct CaptureRow {
    id: InstanceId,
    tenant_organization_id: TenantOrganizationId,
    tenant: Option<Json<TenantNicFirmwareConfig>>,
    machine_config_version: ConfigVersion,
    machine_profile_id: Option<NicFirmwareProfileId>,
    #[sqlx(json)]
    card_selections: Vec<NicFirmwareCardSelection>,
    #[sqlx(json)]
    profiles: Vec<NicFirmwareProfile>,
}

/// Captures the complete batch's tenant presence, selections and profile content
/// at one PostgreSQL statement snapshot. Missing tenants remain absent; no tenant
/// registration, site-default lookup or tenant/profile row lock is performed.
///
/// The caller must use an open `READ COMMITTED` transaction and retain the
/// allocation's existing machine row locks through commit, which also serialize
/// machine/card configuration writes. One statement's read snapshot defines the
/// presence and content boundary: commits visible to it are included, and later
/// commits cannot change the returned inputs.
/// Targets are initially empty and must be supplied by the attachment binder
/// before [`persist`]. Results are ordered by instance ID; repeated IDs produce
/// one result, and a missing requested instance returns `NotFoundError`.
pub async fn capture(
    txn: &mut PgConnection,
    instance_ids: &[InstanceId],
) -> DatabaseResult<Vec<(InstanceId, InstanceNicFirmwareSnapshot)>> {
    if instance_ids.is_empty() {
        return Ok(Vec::new());
    }

    // This statement sees either the committed tenant/profile values or the
    // preceding values, including when a tenant is created concurrently.
    // VPC creation locks Tenant before NSG; allocation already holds NSG locks,
    // so taking a Tenant lock here would create the opposite wait order.
    let query = r#"
        SELECT i.id, i.tenant_org AS tenant_organization_id,
            CASE WHEN t.organization_id IS NULL THEN NULL ELSE jsonb_build_object(
                'organization_id', t.organization_id,
                'profile_id', t.nic_firmware_profile_id,
                'version', t.version
            ) END AS tenant,
            m.version AS machine_config_version,
            m.nic_firmware_profile_id AS machine_profile_id,
            cards.selections AS card_selections,
            profiles.content AS profiles
        FROM instances i
        JOIN machines m ON m.id = i.machine_id
        LEFT JOIN tenants t ON t.organization_id = i.tenant_org
        LEFT JOIN LATERAL (
            SELECT COALESCE(jsonb_agg(jsonb_build_object(
                'selector', card.selector,
                'profile_id', card.profile_id
            ) ORDER BY card.selector), '[]'::jsonb) AS selections
            FROM machine_nic_firmware_overrides card
            WHERE card.machine_id = m.id
        ) cards ON true
        LEFT JOIN LATERAL (
            SELECT COALESCE(jsonb_agg(jsonb_build_object(
                'id', profile.id,
                'config', profile.config,
                'version', profile.version
            ) ORDER BY profile.id), '[]'::jsonb) AS content
            FROM nic_firmware_profiles profile
            WHERE profile.id = t.nic_firmware_profile_id
               OR profile.id = m.nic_firmware_profile_id
               OR profile.id IN (
                   SELECT card.profile_id FROM machine_nic_firmware_overrides card
                   WHERE card.machine_id = m.id
               )
        ) profiles ON true
        WHERE i.id = ANY($1)
        ORDER BY i.id
    "#;
    let rows: Vec<CaptureRow> = sqlx::query_as(query)
        .bind(instance_ids)
        .fetch_all(txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;
    let captured_ids: HashSet<_> = rows.iter().map(|row| row.id).collect();
    if let Some(id) = instance_ids.iter().find(|id| !captured_ids.contains(*id)) {
        return Err(DatabaseError::NotFoundError {
            kind: "instance",
            id: id.to_string(),
        });
    }

    Ok(rows
        .into_iter()
        .map(|row| {
            (
                row.id,
                InstanceNicFirmwareSnapshot {
                    tenant_organization_id: row.tenant_organization_id,
                    tenant: row.tenant.map(|tenant| tenant.0),
                    machine_config_version: row.machine_config_version,
                    machine_profile_id: row.machine_profile_id,
                    card_selections: row.card_selections,
                    profiles: row.profiles,
                    targets: Vec::new(),
                },
            )
        })
        .collect())
}

/// Stores an allocation's snapshot once, in its existing allocation transaction.
/// A missing or deleted instance, or one with an existing snapshot, returns
/// `FailedPrecondition`. Ordinary configuration writers do not own this column.
pub async fn persist(
    txn: &mut PgConnection,
    instance_id: InstanceId,
    snapshot: &InstanceNicFirmwareSnapshot,
) -> DatabaseResult<()> {
    let query = "UPDATE instances SET nic_firmware_snapshot = $2
        WHERE id = $1 AND deleted IS NULL AND nic_firmware_snapshot IS NULL
        RETURNING id";
    let updated: Option<InstanceId> = sqlx::query_scalar(query)
        .bind(instance_id)
        .bind(Json(snapshot))
        .fetch_optional(&mut *txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;
    if updated.is_some() {
        return Ok(());
    }

    super::ensure_live_for_config_update(txn, instance_id).await?;
    Err(DatabaseError::FailedPrecondition(format!(
        "instance {instance_id} already has a NIC firmware snapshot"
    )))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use carbide_libmlx_model::firmware::FirmwareSpec;
    use carbide_uuid::machine::{HostMachineId, MachineId, MachineIdSource, MachineType};
    use model::metadata::Metadata;
    use model::nic_firmware::{
        NicFirmwareApproach, NicFirmwareArtifact, NicFirmwareCardSelector,
        NicFirmwareProfileConfig, NicFirmwareProfileEntry,
    };
    use sqlx::PgPool;

    use super::*;
    use crate::test_support::postgres::wait_for_blocked_query;

    /// Creates a minimal persisted allocation so tests exercise the production
    /// instance decoder and real parent/instance relationships.
    async fn create_instance(
        txn: &mut PgConnection,
        index: u8,
        organization_id: &TenantOrganizationId,
    ) -> (InstanceId, HostMachineId) {
        let machine_id: HostMachineId = MachineId::new(
            MachineIdSource::ProductBoardChassisSerial,
            [index; 32],
            MachineType::Host,
        )
        .try_into()
        .unwrap();
        sqlx::query("INSERT INTO machines (id, dpf) VALUES ($1, $2)")
            .bind(machine_id)
            .bind(Json(model::machine::Dpf::default()))
            .execute(&mut *txn)
            .await
            .unwrap();
        let instance_id: InstanceId = uuid::Uuid::from_u128(u128::from(index)).into();
        sqlx::query(
            "INSERT INTO instances (id, machine_id, tenant_org, os_ipxe_script, network_config, nvlink_config)
             VALUES ($1, $2, $3, '#!ipxe', '{\"interfaces\": []}', '{\"gpu_configs\": []}')",
        )
        .bind(instance_id)
        .bind(machine_id)
        .bind(organization_id)
        .execute(txn)
        .await
        .unwrap();
        (instance_id, machine_id)
    }

    /// Creates a real catalog entry whose revision and digest can be changed
    /// independently of the immutable allocation copy.
    async fn create_profile(txn: &mut PgConnection, id: &str) -> NicFirmwareProfile {
        crate::nic_firmware::create(
            txn,
            &id.parse().unwrap(),
            &NicFirmwareProfileConfig {
                entries: vec![NicFirmwareProfileEntry {
                    firmware: FirmwareSpec {
                        part_number: "MCX75310AAS-NEAT".into(),
                        psid: "MT_0000000838".into(),
                        version: "28.43.1014".into(),
                    },
                    image: NicFirmwareArtifact {
                        url: "https://firmware.invalid/image.bin".parse().unwrap(),
                        sha256: "ab".repeat(32),
                    },
                    device_config: None,
                    approach: NicFirmwareApproach::Scout,
                }],
            },
        )
        .await
        .unwrap()
    }

    /// Freezes missing and registered-but-unselected tenant input without converting
    /// legacy NULL into a claim that allocation captured absence.
    #[crate::sqlx_test]
    async fn captured_absence_is_distinct_from_legacy_and_missing_tenants(pool: PgPool) {
        let mut txn = pool.begin().await.unwrap();
        let missing_org: TenantOrganizationId = "missing-tenant".parse().unwrap();
        let tenant = crate::tenant::create_and_persist(
            "registered-tenant".into(),
            Metadata::default(),
            None,
            &mut txn,
        )
        .await
        .unwrap();
        let (legacy_id, _) = create_instance(&mut txn, 1, &missing_org).await;
        let (missing_id, _) = create_instance(&mut txn, 2, &missing_org).await;
        let (unselected_id, _) = create_instance(&mut txn, 3, &tenant.organization_id).await;
        let snapshots = capture(&mut txn, &[unselected_id, missing_id])
            .await
            .unwrap();
        assert_eq!(
            snapshots.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            [missing_id, unselected_id]
        );
        let expected_tenant = TenantNicFirmwareConfig {
            organization_id: tenant.organization_id.clone(),
            profile_id: None,
            version: tenant.version,
        };
        for (scenario, expected_org, expected_tenant, (id, snapshot)) in [
            ("missing tenant", &missing_org, None, &snapshots[0]),
            (
                "unselected tenant",
                &tenant.organization_id,
                Some(expected_tenant),
                &snapshots[1],
            ),
        ] {
            assert_eq!(&snapshot.tenant_organization_id, expected_org, "{scenario}");
            assert_eq!(snapshot.tenant, expected_tenant, "{scenario}");
            assert!(snapshot.machine_profile_id.is_none());
            assert!(snapshot.card_selections.is_empty());
            assert!(snapshot.profiles.is_empty());
            assert!(snapshot.targets.is_empty());
            persist(&mut txn, *id, snapshot).await.unwrap();
        }
        txn.commit().await.unwrap();

        for (id, expected) in snapshots {
            let stored = super::super::find_by_id(&pool, id).await.unwrap().unwrap();
            assert_eq!(stored.nic_firmware_snapshot, Some(expected));
        }
        let legacy = super::super::find_by_id(&pool, legacy_id)
            .await
            .unwrap()
            .unwrap();
        assert!(legacy.nic_firmware_snapshot.is_none());
        assert!(
            crate::tenant::find_nic_firmware_config(&pool, &missing_org)
                .await
                .unwrap()
                .is_none()
        );
    }

    /// Preserves captured content after live references and profiles change,
    /// and prevents a second capture from replacing that history.
    #[crate::sqlx_test]
    async fn captured_profiles_survive_selection_edits_and_catalog_deletion(pool: PgPool) {
        let mut txn = pool.begin().await.unwrap();
        let tenant = crate::tenant::create_and_persist(
            "firmware-tenant".into(),
            Metadata::default(),
            None,
            &mut txn,
        )
        .await
        .unwrap();
        let allocation = create_profile(&mut txn, "allocation").await;
        let machine = create_profile(&mut txn, "machine").await;
        let card_profile = create_profile(&mut txn, "card").await;
        let mut card_config = card_profile.config.clone();
        card_config.entries[0].image.sha256 = "12".repeat(32);
        card_config.entries[0].device_config = Some(NicFirmwareArtifact {
            url: "https://firmware.invalid/card.ini".parse().unwrap(),
            sha256: "34".repeat(32),
        });
        let card_profile = crate::nic_firmware::update(
            &mut txn,
            &card_profile.id,
            &card_config,
            card_profile.version,
        )
        .await
        .unwrap();
        let tenant_config = crate::tenant::set_nic_firmware_config(
            &mut txn,
            &tenant.organization_id,
            Some(&allocation.id),
            tenant.version,
        )
        .await
        .unwrap();
        let (instance_id, machine_id) = create_instance(&mut txn, 4, &tenant.organization_id).await;
        let version = sqlx::query_scalar("SELECT version FROM machines WHERE id = $1")
            .bind(machine_id)
            .fetch_one(&mut *txn)
            .await
            .unwrap();
        crate::nic_firmware::set_machine_config(
            &mut txn,
            &machine_id,
            Some(&machine.id),
            None,
            version,
        )
        .await
        .unwrap();
        // Read the complete stored token rather than generating a new timestamp.
        let selected_version: ConfigVersion =
            sqlx::query_scalar("SELECT version FROM machines WHERE id = $1")
                .bind(machine_id)
                .fetch_one(&mut *txn)
                .await
                .unwrap();
        // Persisted selectors remain meaningful snapshot input when inventory
        // has disappeared; capture must not filter them through observations.
        let card = NicFirmwareCardSelection {
            selector: NicFirmwareCardSelector::Guid("saved-card".into()),
            profile_id: card_profile.id.clone(),
        };
        let query = "INSERT INTO machine_nic_firmware_overrides (machine_id, selector, profile_id) VALUES ($1, $2, $3)";
        sqlx::query(query)
            .bind(machine_id)
            .bind(Json(&card.selector))
            .bind(&card.profile_id)
            .execute(&mut *txn)
            .await
            .unwrap();
        let (_, snapshot) = capture(&mut txn, &[instance_id])
            .await
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(snapshot.tenant, Some(tenant_config.clone()));
        assert_eq!(snapshot.machine_profile_id, Some(machine.id.clone()));
        assert_eq!(snapshot.machine_config_version, selected_version);
        assert_eq!(
            snapshot.profiles,
            [allocation.clone(), card_profile.clone(), machine.clone()]
        );
        assert_eq!(snapshot.card_selections, [card]);
        persist(&mut txn, instance_id, &snapshot).await.unwrap();
        txn.commit().await.unwrap();

        let mut txn = pool.begin().await.unwrap();
        let mut replacement = allocation.config.clone();
        replacement.entries[0].firmware.version = "28.42.1000".into();
        replacement.entries[0].image.sha256 = "cd".repeat(32);
        let changed =
            crate::nic_firmware::update(&mut txn, &allocation.id, &replacement, allocation.version)
                .await
                .unwrap();
        card_config.entries[0].firmware.version = "28.41.1000".into();
        card_config.entries[0]
            .device_config
            .as_mut()
            .unwrap()
            .sha256 = "56".repeat(32);
        let changed_card = crate::nic_firmware::update(
            &mut txn,
            &card_profile.id,
            &card_config,
            card_profile.version,
        )
        .await
        .unwrap();
        crate::tenant::set_nic_firmware_config(
            &mut txn,
            &tenant.organization_id,
            None,
            tenant_config.version,
        )
        .await
        .unwrap();
        crate::nic_firmware::set_machine_config(
            &mut txn,
            &machine_id,
            None,
            Some(&[]),
            snapshot.machine_config_version,
        )
        .await
        .unwrap();
        crate::nic_firmware::delete(&mut txn, &changed.id, changed.version)
            .await
            .unwrap();
        crate::nic_firmware::delete(&mut txn, &changed_card.id, changed_card.version)
            .await
            .unwrap();
        crate::nic_firmware::delete(&mut txn, &machine.id, machine.version)
            .await
            .unwrap();
        let (_, replacement) = capture(&mut txn, &[instance_id])
            .await
            .unwrap()
            .pop()
            .unwrap();
        assert!(replacement.profiles.is_empty());
        let error = persist(&mut txn, instance_id, &replacement)
            .await
            .unwrap_err();
        assert!(
            matches!(error, DatabaseError::FailedPrecondition(reason) if reason.contains("already has"))
        );
        txn.commit().await.unwrap();

        let stored = super::super::find_by_id(&pool, instance_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.nic_firmware_snapshot, Some(snapshot));
    }

    /// Reads complete committed tenant/profile revisions while a concurrent
    /// writer holds their locks, preserving allocation's nonlocking boundary.
    #[crate::sqlx_test]
    async fn capture_reads_committed_tenant_and_profile_content_without_waiting_for_writers(
        pool: PgPool,
    ) {
        // Registration tests absence; an existing selection tests paired tenant
        // and profile revisions. Neither case repeats the setter's validation.
        for (index, scenario, registered) in [
            (5, "missing tenant", false),
            (6, "registered selected tenant", true),
        ] {
            let organization_id: TenantOrganizationId = format!("tenant-{index}").parse().unwrap();
            let mut setup = pool.begin().await.unwrap();
            let profile = create_profile(&mut setup, &format!("profile-{index}")).await;
            let previous_tenant = if registered {
                let tenant = crate::tenant::create_and_persist(
                    organization_id.to_string(),
                    Metadata::default(),
                    None,
                    &mut setup,
                )
                .await
                .unwrap();
                Some(
                    crate::tenant::set_nic_firmware_config(
                        &mut setup,
                        &organization_id,
                        Some(&profile.id),
                        tenant.version,
                    )
                    .await
                    .unwrap(),
                )
            } else {
                None
            };
            let (instance_id, machine_id) =
                create_instance(&mut setup, index, &organization_id).await;
            setup.commit().await.unwrap();

            let mut allocation = pool.begin().await.unwrap();
            sqlx::query("SELECT id FROM machines WHERE id = $1 FOR UPDATE")
                .bind(machine_id)
                .execute(&mut *allocation)
                .await
                .unwrap();
            let mut writer = pool.begin().await.unwrap();
            let tenant_version = match &previous_tenant {
                Some(tenant) => tenant.version,
                None => {
                    crate::tenant::create_and_persist(
                        organization_id.to_string(),
                        Metadata::default(),
                        None,
                        &mut writer,
                    )
                    .await
                    .unwrap()
                    .version
                }
            };
            let selected = crate::tenant::set_nic_firmware_config(
                &mut writer,
                &organization_id,
                Some(&profile.id),
                tenant_version,
            )
            .await
            .unwrap();
            let mut config = profile.config.clone();
            config.entries[0].image.sha256 = "ef".repeat(32);
            let changed =
                crate::nic_firmware::update(&mut writer, &profile.id, &config, profile.version)
                    .await
                    .unwrap();

            // The writer holds tenant and profile locks. Capture must finish
            // with the complete preceding state, including tenant absence.
            let (_, before) = tokio::time::timeout(
                Duration::from_secs(5),
                capture(&mut allocation, &[instance_id]),
            )
            .await
            .expect("capture must not wait for tenant or profile row locks")
            .unwrap()
            .pop()
            .unwrap();
            assert_eq!(before.tenant, previous_tenant, "{scenario}");
            assert_eq!(
                before.profiles,
                if registered {
                    vec![profile]
                } else {
                    Vec::new()
                },
                "{scenario}"
            );
            persist(&mut allocation, instance_id, &before)
                .await
                .unwrap();
            allocation.commit().await.unwrap();
            writer.commit().await.unwrap();

            let mut read = pool.begin().await.unwrap();
            sqlx::query("SELECT id FROM machines WHERE id = $1 FOR UPDATE")
                .bind(machine_id)
                .execute(&mut *read)
                .await
                .unwrap();
            let (_, after) = capture(&mut read, &[instance_id])
                .await
                .unwrap()
                .pop()
                .unwrap();
            assert_eq!(after.tenant, Some(selected), "{scenario}");
            assert_eq!(after.profiles, [changed], "{scenario}");
            read.commit().await.unwrap();
            let stored = super::super::find_by_id(&pool, instance_id)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(stored.nic_firmware_snapshot, Some(before));
        }
    }

    /// Rejects a torn tenant/profile read when an atomic writer commits while
    /// capture waits for the profile relation, protecting the one-statement boundary.
    #[crate::sqlx_test]
    async fn capture_keeps_tenant_selection_and_profile_contents_in_one_snapshot(pool: PgPool) {
        let mut setup = pool.begin().await.unwrap();
        let tenant = crate::tenant::create_and_persist(
            "coherent-tenant".into(),
            Metadata::default(),
            None,
            &mut setup,
        )
        .await
        .unwrap();
        let original = create_profile(&mut setup, "original").await;
        let replacement = create_profile(&mut setup, "replacement").await;
        let old_selection = crate::tenant::set_nic_firmware_config(
            &mut setup,
            &tenant.organization_id,
            Some(&original.id),
            tenant.version,
        )
        .await
        .unwrap();
        let (instance_id, machine_id) =
            create_instance(&mut setup, 8, &tenant.organization_id).await;
        setup.commit().await.unwrap();

        let mut allocation = pool.begin().await.unwrap();
        sqlx::query("SELECT id FROM machines WHERE id = $1 FOR UPDATE")
            .bind(machine_id)
            .execute(&mut *allocation)
            .await
            .unwrap();
        let mut writer = pool.begin().await.unwrap();
        sqlx::query("LOCK TABLE nic_firmware_profiles IN ACCESS EXCLUSIVE MODE")
            .execute(&mut *writer)
            .await
            .unwrap();
        let writer_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *writer)
            .await
            .unwrap();

        let read = async {
            let (_, snapshot) = capture(&mut allocation, &[instance_id])
                .await
                .unwrap()
                .pop()
                .unwrap();
            persist(&mut allocation, instance_id, &snapshot)
                .await
                .unwrap();
            allocation.commit().await.unwrap();
            snapshot
        };
        let change = async {
            // pg_stat_activity can truncate SQL text before the profile join.
            // Match the capture's prefix while still requiring this writer's lock.
            wait_for_blocked_query(&pool, writer_pid, "SELECT i.id, i.tenant_org").await;
            // A split reader has already read the old tenant selection before
            // its profile query blocks. Replacing that selection and editing the
            // old profile together would give that reader a pair that never existed.
            let new_selection = crate::tenant::set_nic_firmware_config(
                &mut writer,
                &tenant.organization_id,
                Some(&replacement.id),
                old_selection.version,
            )
            .await
            .unwrap();
            let mut config = original.config.clone();
            config.entries[0].image.sha256 = "cd".repeat(32);
            crate::nic_firmware::update(&mut writer, &original.id, &config, original.version)
                .await
                .unwrap();
            writer.commit().await.unwrap();
            new_selection
        };
        let (snapshot, new_selection) = tokio::time::timeout(Duration::from_secs(10), async {
            tokio::join!(read, change)
        })
        .await
        .expect("capture and writer must finish after the observed relation lock wait");

        // Planning and execution can acquire relation locks at different times.
        // Either committed snapshot is valid; mixing their fields is not.
        let complete_old =
            snapshot.tenant.as_ref() == Some(&old_selection) && snapshot.profiles == [original];
        let complete_new =
            snapshot.tenant.as_ref() == Some(&new_selection) && snapshot.profiles == [replacement];
        assert!(
            complete_old || complete_new,
            "torn firmware inputs: {snapshot:?}"
        );
        let stored = super::super::find_by_id(&pool, instance_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.nic_firmware_snapshot, Some(snapshot));
    }

    /// Clears allocation selection context during release and proves a deleted
    /// instance cannot regain a snapshot through the capture writer.
    #[crate::sqlx_test]
    async fn release_clears_selection_context_and_prevents_recapture(pool: PgPool) {
        let mut txn = pool.begin().await.unwrap();
        let organization_id = "released-tenant".parse().unwrap();
        let (instance_id, _) = create_instance(&mut txn, 7, &organization_id).await;
        let (_, snapshot) = capture(&mut txn, &[instance_id])
            .await
            .unwrap()
            .pop()
            .unwrap();
        persist(&mut txn, instance_id, &snapshot).await.unwrap();
        super::super::mark_as_deleted(instance_id, &mut txn)
            .await
            .unwrap();
        let error = persist(&mut txn, instance_id, &snapshot).await.unwrap_err();
        assert!(
            matches!(error, DatabaseError::FailedPrecondition(reason) if reason.contains("being deleted"))
        );
        txn.commit().await.unwrap();

        let stored = super::super::find_by_id(&pool, instance_id)
            .await
            .unwrap()
            .unwrap();
        assert!(stored.deleted.is_some());
        assert!(stored.nic_firmware_snapshot.is_none());
    }
}

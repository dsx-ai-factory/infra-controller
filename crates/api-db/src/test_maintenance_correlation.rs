// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Persistence and old-writer compatibility of maintenance log correlation.

use trace_propagation::correlation::{TraceId, scope};

use crate::ConditionalWrite;

#[crate::sqlx_test]
async fn machine_maintenance_trace_correlation(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let trace_id: TraceId = "1234567890abcdef1234567890abcdef".parse()?;
    let mut txn = pool.begin().await?;
    let id: carbide_uuid::machine::HostMachineId =
        "fm100htes3rn1npvbtm5qd57dkilaag7ljugl1llmm7rfuq1ov50i0rpl30".parse()?;
    crate::machine::create(
        txn.as_mut(),
        None,
        &id.into(),
        model::machine::ManagedHostState::Ready,
        None,
        2,
    )
    .await?;
    scope(
        trace_id,
        crate::machine::set_machine_maintenance_requested(
            txn.as_mut(),
            id,
            "first",
            model::machine::MachineMaintenanceOperation::PowerOn,
        ),
    )
    .await?;
    txn.commit().await?;

    // A new connection reloads the same ID after the initiating scope has ended.
    let mut connection = pool.acquire().await?;
    let original = crate::machine::find_one(&mut *connection, &id, Default::default())
        .await?
        .unwrap()
        .machine_maintenance_requested
        .unwrap();
    assert_eq!(
        crate::cm_action::trace_id(&pool, original.action_id).await?,
        Some(trace_id)
    );
    let raw: serde_json::Value =
        sqlx::query_scalar("SELECT machine_maintenance_requested FROM machines WHERE id = $1")
            .bind(id)
            .fetch_one(&mut *connection)
            .await?;
    assert!(
        raw.get("action_id").is_none(),
        "pending JSON must remain compatible with old writers"
    );
    let legacy: model::machine::MachineMaintenanceRequest = serde_json::from_value(raw.clone())?;
    assert_eq!(
        original, legacy,
        "correlation must not change request identity"
    );
    let admitted: model::machine::MachineMaintenanceRequest =
        serde_json::from_value(serde_json::to_value(&original)?)?;
    assert_eq!(
        admitted.action_id, original.action_id,
        "admitted controller state retains correlation"
    );

    // An old writer replaces the request without creating an action record.
    let mut replacement = raw;
    replacement["initiator"] = "legacy replacement".into();
    replacement["requested_at"] = serde_json::to_value(chrono::Utc::now())?;
    sqlx::query("UPDATE machines SET machine_maintenance_requested = $1 WHERE id = $2")
        .bind(replacement)
        .bind(id)
        .execute(&mut *connection)
        .await?;
    let replacement = crate::machine::find_one(&mut *connection, &id, Default::default())
        .await?
        .unwrap()
        .machine_maintenance_requested
        .unwrap();
    assert_eq!(
        replacement.action_id, None,
        "stale correlation cannot attach to replacement work"
    );
    assert!(matches!(
        crate::machine::clear_machine_maintenance_requested(&mut connection, id, &original).await?,
        ConditionalWrite::NotApplied(_)
    ));

    let next_id = TraceId::generate();
    scope(
        next_id,
        crate::machine::set_machine_maintenance_requested(
            &mut connection,
            id,
            "next",
            model::machine::MachineMaintenanceOperation::PowerOn,
        ),
    )
    .await?;
    let next = crate::machine::find_one(&mut *connection, &id, Default::default())
        .await?
        .unwrap()
        .machine_maintenance_requested
        .unwrap();
    assert_eq!(
        crate::cm_action::trace_id(&pool, next.action_id).await?,
        Some(next_id)
    );
    assert!(matches!(
        crate::machine::clear_machine_maintenance_requested(&mut connection, id, &replacement)
            .await?,
        ConditionalWrite::NotApplied(_)
    ));
    assert_eq!(
        crate::machine::clear_machine_maintenance_requested(&mut connection, id, &next).await?,
        ConditionalWrite::Applied(())
    );
    let cleared: bool = sqlx::query_scalar(
        "SELECT machine_maintenance_requested IS NULL FROM machines WHERE id = $1",
    )
    .bind(id)
    .fetch_one(&mut *connection)
    .await?;
    assert!(cleared);
    Ok(())
}

#[crate::sqlx_test]
async fn switch_maintenance_trace_correlation(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let trace_id: TraceId = "1234567890abcdef1234567890abcdef".parse()?;
    let mut txn = pool.begin().await?;
    let id = crate::test_support::switch::create_seeded_discovered(
        txn.as_mut(),
        78,
        "trace correlation",
    )
    .await?
    .id;
    scope(
        trace_id,
        crate::switch::set_switch_maintenance_requested(
            txn.as_mut(),
            id,
            "first",
            model::switch::SwitchMaintenanceOperation::PowerOn,
        ),
    )
    .await?;
    txn.commit().await?;

    // A new connection reloads the same ID after the initiating scope has ended.
    let mut connection = pool.acquire().await?;
    let original = crate::switch::find_by_id(&mut connection, &id)
        .await?
        .unwrap()
        .switch_maintenance_requested
        .unwrap();
    assert_eq!(
        crate::cm_action::trace_id(&pool, original.action_id).await?,
        Some(trace_id)
    );
    let raw: serde_json::Value =
        sqlx::query_scalar("SELECT switch_maintenance_requested FROM switches WHERE id = $1")
            .bind(id)
            .fetch_one(&mut *connection)
            .await?;
    assert!(
        raw.get("action_id").is_none(),
        "pending JSON must remain compatible with old writers"
    );
    let legacy: model::switch::SwitchMaintenanceRequest = serde_json::from_value(raw.clone())?;
    assert_eq!(
        original, legacy,
        "correlation must not change request identity"
    );
    let admitted: model::switch::SwitchMaintenanceRequest =
        serde_json::from_value(serde_json::to_value(&original)?)?;
    assert_eq!(
        admitted.action_id, original.action_id,
        "admitted controller state retains correlation"
    );

    // An old writer replaces the request without creating an action record.
    let mut replacement = raw;
    replacement["initiator"] = "legacy replacement".into();
    replacement["requested_at"] = serde_json::to_value(chrono::Utc::now())?;
    sqlx::query("UPDATE switches SET switch_maintenance_requested = $1 WHERE id = $2")
        .bind(replacement)
        .bind(id)
        .execute(&mut *connection)
        .await?;
    let replacement = crate::switch::find_by_id(&mut connection, &id)
        .await?
        .unwrap()
        .switch_maintenance_requested
        .unwrap();
    assert_eq!(
        replacement.action_id, None,
        "stale correlation cannot attach to replacement work"
    );
    assert!(matches!(
        crate::switch::clear_switch_maintenance_requested(&mut connection, id, &original).await?,
        ConditionalWrite::NotApplied(_)
    ));

    let next_id = TraceId::generate();
    scope(
        next_id,
        crate::switch::set_switch_maintenance_requested(
            &mut connection,
            id,
            "next",
            model::switch::SwitchMaintenanceOperation::PowerOn,
        ),
    )
    .await?;
    let next = crate::switch::find_by_id(&mut connection, &id)
        .await?
        .unwrap()
        .switch_maintenance_requested
        .unwrap();
    assert_eq!(
        crate::cm_action::trace_id(&pool, next.action_id).await?,
        Some(next_id)
    );
    assert!(matches!(
        crate::switch::clear_switch_maintenance_requested(&mut connection, id, &replacement)
            .await?,
        ConditionalWrite::NotApplied(_)
    ));
    assert_eq!(
        crate::switch::clear_switch_maintenance_requested(&mut connection, id, &next).await?,
        ConditionalWrite::Applied(())
    );
    let cleared: bool = sqlx::query_scalar(
        "SELECT switch_maintenance_requested IS NULL FROM switches WHERE id = $1",
    )
    .bind(id)
    .fetch_one(&mut *connection)
    .await?;
    assert!(cleared);
    Ok(())
}

#[crate::sqlx_test]
async fn power_shelf_maintenance_trace_correlation(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let trace_id: TraceId = "1234567890abcdef1234567890abcdef".parse()?;
    let mut txn = pool.begin().await?;
    let id = crate::test_support::power_shelf::create_seeded(txn.as_mut(), 78, "trace correlation")
        .await?
        .id;
    scope(
        trace_id,
        crate::power_shelf::set_power_shelf_maintenance_requested(
            txn.as_mut(),
            id,
            "first",
            model::power_shelf::PowerShelfMaintenanceOperation::PowerOn,
        ),
    )
    .await?;
    txn.commit().await?;

    // A new connection reloads the same ID after the initiating scope has ended.
    let mut connection = pool.acquire().await?;
    let original = crate::power_shelf::find_by_id(&mut connection, &id)
        .await?
        .unwrap()
        .power_shelf_maintenance_requested
        .unwrap();
    assert_eq!(
        crate::cm_action::trace_id(&pool, original.action_id).await?,
        Some(trace_id)
    );
    let raw: serde_json::Value = sqlx::query_scalar(
        "SELECT power_shelf_maintenance_requested FROM power_shelves WHERE id = $1",
    )
    .bind(id)
    .fetch_one(&mut *connection)
    .await?;
    assert!(
        raw.get("action_id").is_none(),
        "pending JSON must remain compatible with old writers"
    );
    let legacy: model::power_shelf::PowerShelfMaintenanceRequest =
        serde_json::from_value(raw.clone())?;
    assert_eq!(
        original, legacy,
        "correlation must not change request identity"
    );
    let admitted: model::power_shelf::PowerShelfMaintenanceRequest =
        serde_json::from_value(serde_json::to_value(&original)?)?;
    assert_eq!(
        admitted.action_id, original.action_id,
        "admitted controller state retains correlation"
    );

    // An old writer replaces the request without creating an action record.
    let mut replacement = raw;
    replacement["initiator"] = "legacy replacement".into();
    replacement["requested_at"] = serde_json::to_value(chrono::Utc::now())?;
    sqlx::query("UPDATE power_shelves SET power_shelf_maintenance_requested = $1 WHERE id = $2")
        .bind(replacement)
        .bind(id)
        .execute(&mut *connection)
        .await?;
    let replacement = crate::power_shelf::find_by_id(&mut connection, &id)
        .await?
        .unwrap()
        .power_shelf_maintenance_requested
        .unwrap();
    assert_eq!(
        replacement.action_id, None,
        "stale correlation cannot attach to replacement work"
    );
    assert!(matches!(
        crate::power_shelf::clear_power_shelf_maintenance_requested(&mut connection, id, &original)
            .await?,
        ConditionalWrite::NotApplied(_)
    ));

    let next_id = TraceId::generate();
    scope(
        next_id,
        crate::power_shelf::set_power_shelf_maintenance_requested(
            &mut connection,
            id,
            "next",
            model::power_shelf::PowerShelfMaintenanceOperation::PowerOn,
        ),
    )
    .await?;
    let next = crate::power_shelf::find_by_id(&mut connection, &id)
        .await?
        .unwrap()
        .power_shelf_maintenance_requested
        .unwrap();
    assert_eq!(
        crate::cm_action::trace_id(&pool, next.action_id).await?,
        Some(next_id)
    );
    assert!(matches!(
        crate::power_shelf::clear_power_shelf_maintenance_requested(
            &mut connection,
            id,
            &replacement
        )
        .await?,
        ConditionalWrite::NotApplied(_)
    ));
    assert_eq!(
        crate::power_shelf::clear_power_shelf_maintenance_requested(&mut connection, id, &next)
            .await?,
        ConditionalWrite::Applied(())
    );
    let cleared: bool = sqlx::query_scalar(
        "SELECT power_shelf_maintenance_requested IS NULL FROM power_shelves WHERE id = $1",
    )
    .bind(id)
    .fetch_one(&mut *connection)
    .await?;
    assert!(cleared);
    Ok(())
}

// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Log correlation for Core-initiated operations, using the existing action journal.

use std::future::Future;

use carbide_uuid::rack::RackId;
use carbide_uuid::switch::SwitchId;
use config_version::ConfigVersion;
use model::rack::RackState;
use model::switch::SwitchControllerState;
use sqlx::{PgConnection, PgPool};
use trace_propagation::correlation::{self, TraceId};
use uuid::Uuid;

use super::{Metadata, Target};
use crate::{DatabaseError, DatabaseResult};

#[cfg(test)]
mod tests;

#[derive(Clone, Copy)]
enum Kind {
    RackMaintenance,
    SwitchConfiguration,
    SwitchDecommissioning,
}

impl Kind {
    fn prefix(self) -> &'static str {
        match self {
            Self::RackMaintenance => "automation/rack-maintenance/",
            Self::SwitchConfiguration => "automation/switch-configuration/",
            Self::SwitchDecommissioning => "automation/switch-decommissioning/",
        }
    }

    fn switch(state: &SwitchControllerState) -> Option<Self> {
        match state {
            SwitchControllerState::Configuring { .. } => Some(Self::SwitchConfiguration),
            SwitchControllerState::Decommissioning { .. } => Some(Self::SwitchDecommissioning),
            _ => None,
        }
    }
}

pub(super) fn live_predicate(target: Target<'_>) -> &'static str {
    match target {
        Target::Rack(_) => {
            "starts_with(a.request_key, 'automation/rack-maintenance/') AND controller_state->>'state' = 'maintenance' AND config->>'maintenance_requested' IS NULL"
        }
        Target::Switch(_) => {
            "(starts_with(a.request_key, 'automation/switch-configuration/') AND controller_state->>'state' = 'configuring') OR (starts_with(a.request_key, 'automation/switch-decommissioning/') AND controller_state->>'state' = 'decommissioning')"
        }
        Target::Machine(_) | Target::PowerShelf(_) => "false",
    }
}

async fn advance(
    txn: &mut PgConnection,
    target: Target<'_>,
    kind: Option<Kind>,
) -> DatabaseResult<()> {
    let (_, column, id, _) = target.parts();
    let prefix = kind.map(Kind::prefix).unwrap_or("no-active-automation/");
    let retire = format!(
        "UPDATE cm_actions SET completed_at = statement_timestamp() WHERE {column} = $1 AND completed_at IS NULL AND starts_with(request_key, 'automation/') AND NOT starts_with(request_key, $2)"
    );
    sqlx::query(sqlx::AssertSqlSafe(retire.as_str()))
        .bind(&id)
        .bind(prefix)
        .execute(&mut *txn)
        .await
        .map_err(|error| DatabaseError::query(&retire, error))?;
    if kind.is_some() {
        let query = format!(
            "SELECT EXISTS(SELECT 1 FROM cm_actions WHERE {column} = $1 AND completed_at IS NULL AND starts_with(request_key, $2))"
        );
        let exists: bool = sqlx::query_scalar(sqlx::AssertSqlSafe(query.as_str()))
            .bind(&id)
            .bind(prefix)
            .fetch_one(&mut *txn)
            .await
            .map_err(|error| DatabaseError::query(&query, error))?;
        if !exists {
            let trace_id = correlation::local_id().unwrap_or_else(TraceId::generate);
            super::create(
                txn,
                target,
                &format!("{prefix}{}", Uuid::new_v4()),
                trace_id,
            )
            .await?;
        }
    }
    super::reconcile(txn, target).await
}

// The owner row is already locked by the successful controller-state CAS.
// Isolate journal errors in a savepoint so optional logging cannot roll back it.
async fn record_transition(txn: &mut PgConnection, target: Target<'_>, kind: Option<Kind>) {
    let result = async {
        let mut savepoint = crate::Transaction::begin_inner(txn).await?;
        match advance(&mut savepoint, target, kind).await {
            Ok(()) => savepoint.commit().await,
            Err(error) => {
                savepoint.rollback().await?;
                Err(error)
            }
        }
    }
    .await;
    if let Err(error) = result {
        tracing::warn!(error = %error, "failed to retain automatic operation log context");
    }
}

pub(crate) async fn rack_transition(
    txn: &mut PgConnection,
    rack_id: &RackId,
    state: &RackState,
    has_request: bool,
) {
    let kind = (matches!(state, RackState::Maintenance { .. }) && !has_request)
        .then_some(Kind::RackMaintenance);
    record_transition(txn, Target::Rack(rack_id), kind).await;
}

pub(crate) async fn switch_transition(
    txn: &mut PgConnection,
    switch_id: &SwitchId,
    state: &SwitchControllerState,
) {
    record_transition(txn, Target::Switch(switch_id), Kind::switch(state)).await;
}

async fn trace_id(
    pool: &PgPool,
    target: Target<'_>,
    version: ConfigVersion,
    kind: Kind,
) -> DatabaseResult<Option<TraceId>> {
    let (table, column, id, _) = target.parts();
    let query = format!(
        "SELECT a.metadata FROM cm_actions a JOIN {table} r ON r.id = a.{column} WHERE r.id = $1 AND r.controller_state_version = $2 AND a.completed_at IS NULL AND starts_with(a.request_key, $3) ORDER BY a.created_at DESC, a.id DESC LIMIT 1"
    );
    let metadata: Option<sqlx::types::Json<Metadata>> =
        sqlx::query_scalar(sqlx::AssertSqlSafe(query.as_str()))
            .bind(id)
            .bind(version)
            .bind(kind.prefix())
            .fetch_optional(pool)
            .await
            .map_err(|error| DatabaseError::query(&query, error))?;
    Ok(metadata.map(|value| value.0.trace_id))
}

fn resume<F: Future>(
    pool: &PgPool,
    target: Target<'_>,
    version: ConfigVersion,
    kind: Option<Kind>,
    future: F,
) -> impl Future<Output = F::Output> {
    let future = Box::pin(future);
    async move {
        let id = if let Some(kind) = kind {
            match trace_id(pool, target, version, kind).await {
                Ok(id) => id,
                Err(error) => {
                    tracing::warn!(error = %error, "failed to restore automatic operation log context");
                    None
                }
            }
        } else {
            None
        };
        correlation::resume(id, future).await
    }
}

/// Restore an automatic rack cycle's ID without changing on-demand maintenance.
/// Legacy cycles without journal metadata execute without restored correlation.
pub fn resume_rack<F: Future>(
    pool: &PgPool,
    rack_id: &RackId,
    version: ConfigVersion,
    future: F,
) -> impl Future<Output = F::Output> {
    resume(
        pool,
        Target::Rack(rack_id),
        version,
        Some(Kind::RackMaintenance),
        future,
    )
}

/// Restore initial switch configuration or decommissioning for this exact snapshot.
/// Other switch states and missing/invalid context retain ordinary execution.
pub fn resume_switch<F: Future>(
    pool: &PgPool,
    switch_id: &SwitchId,
    version: ConfigVersion,
    state: &SwitchControllerState,
    future: F,
) -> impl Future<Output = F::Output> {
    resume(
        pool,
        Target::Switch(switch_id),
        version,
        Kind::switch(state),
        future,
    )
}

async fn record_nvos(
    txn: &mut PgConnection,
    mac: mac_address::MacAddress,
    version: Option<i32>,
    start: bool,
) -> DatabaseResult<()> {
    let prefix = version.map(|version| format!("automation/nvos-password/{version}/"));
    sqlx::query("UPDATE cm_actions SET completed_at = statement_timestamp() WHERE bmc_mac = $1 AND completed_at IS NULL AND starts_with(request_key, 'automation/nvos-password/') AND ($2::text IS NULL OR starts_with(request_key, $2))")
        .bind(mac).bind(if start { None } else { prefix.as_deref() }).execute(&mut *txn).await
        .map_err(|error| DatabaseError::query("retire NVOS rotation correlation", error))?;
    if let Some(prefix) = prefix.filter(|_| start) {
        let trace_id = correlation::local_id().unwrap_or_else(TraceId::generate);
        sqlx::query(
            "INSERT INTO cm_actions (id, bmc_mac, request_key, metadata) VALUES ($1, $2, $3, $4)",
        )
        .bind(Uuid::new_v4())
        .bind(mac)
        .bind(format!("{prefix}{}", Uuid::new_v4()))
        .bind(sqlx::types::Json(Metadata { trace_id }))
        .execute(&mut *txn)
        .await
        .map_err(|error| DatabaseError::query("create NVOS rotation correlation", error))?;
    }
    sqlx::query("DELETE FROM cm_actions WHERE bmc_mac = $1 AND starts_with(request_key, 'automation/nvos-password/') AND completed_at IS NOT NULL AND id NOT IN (SELECT id FROM cm_actions WHERE bmc_mac = $1 AND starts_with(request_key, 'automation/nvos-password/') AND completed_at IS NOT NULL ORDER BY completed_at DESC, created_at DESC, id DESC LIMIT 20)")
        .bind(mac).execute(txn).await
        .map_err(|error| DatabaseError::query("trim NVOS rotation correlation", error))?;
    Ok(())
}

pub(crate) async fn nvos_transition(
    txn: &mut PgConnection,
    mac: mac_address::MacAddress,
    version: Option<i32>,
    start: bool,
) {
    let result = async {
        let mut savepoint = crate::Transaction::begin_inner(txn).await?;
        match record_nvos(&mut savepoint, mac, version, start).await {
            Ok(()) => savepoint.commit().await,
            Err(error) => {
                savepoint.rollback().await?;
                Err(error)
            }
        }
    }
    .await;
    if let Err(error) = result {
        tracing::warn!(%mac, error = %error, "failed to retain NVOS rotation log context");
    }
}

/// Restore the staged NVOS rotation's ID for its exact target and attempt.
/// Observation retries retain the original ID; missing metadata never blocks work.
pub fn resume_nvos<F: Future>(
    pool: &PgPool,
    mac: mac_address::MacAddress,
    target: i32,
    attempt: i32,
    future: F,
) -> impl Future<Output = F::Output> {
    let future = Box::pin(future);
    async move {
        let result: DatabaseResult<Option<sqlx::types::Json<Metadata>>> = sqlx::query_scalar("SELECT a.metadata FROM cm_actions a WHERE a.bmc_mac = $1 AND a.completed_at IS NULL AND starts_with(a.request_key, $2) AND EXISTS (SELECT 1 FROM device_credential_rotation r WHERE r.device_mac = $1 AND r.credential_type = 'nvos' AND r.rotating_to_version = $3 AND r.rotate_attempts = $4) ORDER BY a.created_at DESC, a.id DESC LIMIT 1")
            .bind(mac).bind(format!("automation/nvos-password/{target}/")).bind(target).bind(attempt)
            .fetch_optional(pool).await.map_err(|error| DatabaseError::query("NVOS rotation log context", error));
        let id = match result {
            Ok(metadata) => metadata.map(|value| value.0.trace_id),
            Err(error) => {
                tracing::warn!(%mac, error = %error, "failed to restore NVOS rotation log context");
                None
            }
        };
        correlation::resume(id, future).await
    }
}

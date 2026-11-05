/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

//! Atomic Domain admission and versioned lifecycle persistence.

use std::collections::HashSet;

use carbide_uuid::rack::RackGroupId;
use config_version::ConfigVersion;
use model::controller_outcome::PersistentStateHandlerOutcome;
use model::metadata::Metadata;
use model::nvlink_domain::{
    NvLinkDomain, NvLinkDomainComponentType, NvLinkDomainConfig, NvLinkDomainState,
};
use sqlx::PgConnection;
use uuid::Uuid;

use crate::db_read::DbReader;
use crate::{ConditionalWrite, ControllerStateNotCurrent, DatabaseError, DatabaseResult};

/// Create a Domain and claim all members in the caller's transaction.
/// Roll back the transaction on failure, including membership conflicts.
async fn create(
    txn: &mut PgConnection,
    id: Uuid,
    config: &NvLinkDomainConfig,
    metadata: &Metadata,
    managed: bool,
) -> DatabaseResult<()> {
    metadata
        .validate(false)
        .map_err(|error| DatabaseError::InvalidArgument(error.to_string()))?;
    if config.domain_profile_id.trim().is_empty() {
        return Err(DatabaseError::InvalidArgument(
            "domain profile ID must not be empty".into(),
        ));
    }
    let unique: HashSet<_> = config.members.iter().collect();
    if unique.len() != config.members.len() {
        return Err(DatabaseError::InvalidArgument(
            "domain members must not contain duplicates".into(),
        ));
    }
    for role in [
        NvLinkDomainComponentType::Compute,
        NvLinkDomainComponentType::Switch,
    ] {
        if !config
            .members
            .iter()
            .any(|member| member.component_type == role)
        {
            return Err(DatabaseError::InvalidArgument(
                "a domain requires both compute and switch trays".into(),
            ));
        }
    }

    let query = "SELECT racks FROM expected_rack_groups WHERE rack_group_id = $1 FOR SHARE";
    let racks: Option<sqlx::types::Json<Vec<model::expected_rack_group::ExpectedRackGroupRack>>> =
        sqlx::query_scalar(query)
            .bind(&config.rack_group_id)
            .fetch_optional(&mut *txn)
            .await
            .map_err(|error| DatabaseError::query(query, error))?;
    let racks = racks.ok_or_else(|| DatabaseError::NotFoundError {
        kind: "expected_rack_group",
        id: config.rack_group_id.to_string(),
    })?;
    for member in &config.members {
        if member.tray_index < 0 {
            return Err(DatabaseError::InvalidArgument(
                "tray index must be non-negative".into(),
            ));
        }
        if !racks.0.iter().any(|rack| rack.rack_id == member.rack_id) {
            return Err(DatabaseError::InvalidArgument(format!(
                "rack {} is not in rack group {}",
                member.rack_id, config.rack_group_id,
            )));
        }
    }

    let version = ConfigVersion::initial();
    let query = "INSERT INTO nvlink_domains (
        id, rack_group_id, domain_profile_id, metadata_name, metadata_description,
        metadata_labels, version, controller_state, controller_state_version, managed
    ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $7, $9)";
    sqlx::query(query)
        .bind(id)
        .bind(&config.rack_group_id)
        .bind(&config.domain_profile_id)
        .bind(&metadata.name)
        .bind(&metadata.description)
        .bind(sqlx::types::Json(&metadata.labels))
        .bind(version)
        .bind(sqlx::types::Json(NvLinkDomainState::Discovering))
        .bind(managed)
        .execute(&mut *txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;

    // Every writer takes overlapping member claims in the same order.
    let query = "INSERT INTO nvlink_domain_members (domain_id, component_type, rack_id, tray_index)
        SELECT $1, member.component_type, member.rack_id, member.tray_index
        FROM jsonb_to_recordset($2) AS member(component_type text, rack_id text, tray_index integer)
        ORDER BY member.rack_id, member.component_type, member.tray_index";
    sqlx::query(query)
        .bind(id)
        .bind(sqlx::types::Json(&config.members))
        .execute(txn)
        .await
        .map_err(|error| {
            if error
                .as_database_error()
                .is_some_and(|error| error.constraint() == Some("nvlink_domain_members_pkey"))
            {
                DatabaseError::AlreadyFoundError {
                    kind: "NVLink domain member",
                    id: "one or more trays are already claimed".into(),
                }
            } else {
                DatabaseError::query(query, error)
            }
        })?;
    Ok(())
}

/// List stable IDs in UUID order, including Domains whose teardown is pending.
async fn find_ids(
    reader: impl DbReader<'_>,
    rack_group_id: Option<&RackGroupId>,
) -> DatabaseResult<Vec<Uuid>> {
    let query = "SELECT id FROM nvlink_domains
        WHERE ($1::varchar IS NULL OR rack_group_id = $1) ORDER BY id";
    sqlx::query_scalar(query)
        .bind(rack_group_id)
        .fetch_all(reader)
        .await
        .map_err(|error| DatabaseError::query(query, error))
}

/// Read configuration, member claims, and progress from one database snapshot.
async fn find_by_ids(
    reader: impl DbReader<'_>,
    ids: &[Uuid],
) -> DatabaseResult<Vec<NvLinkDomain>> {
    let query = "SELECT d.*,
        COALESCE((SELECT jsonb_agg(jsonb_build_object(
            'component_type', m.component_type, 'rack_id', m.rack_id, 'tray_index', m.tray_index)
            ORDER BY m.rack_id, m.component_type, m.tray_index)
            FROM nvlink_domain_members m WHERE m.domain_id = d.id), '[]'::jsonb) AS members
        FROM nvlink_domains d WHERE d.id = ANY($1) ORDER BY d.id";
    sqlx::query_as(query)
        .bind(ids)
        .fetch_all(reader)
        .await
        .map_err(|error| DatabaseError::query(query, error))
}

/// Mark a Domain for asynchronous deletion without releasing its members.
/// Repeated requests preserve the original deletion time and controller progress.
async fn request_deletion(txn: &mut PgConnection, id: Uuid) -> DatabaseResult<()> {
    let query = "SELECT version FROM nvlink_domains WHERE id = $1 FOR UPDATE";
    let version: Option<ConfigVersion> = sqlx::query_scalar(query)
        .bind(id)
        .fetch_optional(&mut *txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;
    let version = version.ok_or_else(|| DatabaseError::NotFoundError {
        kind: "nvlink_domain",
        id: id.to_string(),
    })?;
    let query = "UPDATE nvlink_domains SET deleted = now(), updated = now(), version = $2
        WHERE id = $1 AND deleted IS NULL";
    sqlx::query(query)
        .bind(id)
        .bind(version.increment())
        .execute(txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;
    Ok(())
}

/// Persist controller progress only if both configuration and state still match.
/// Missing rows and either stale token return `NotApplied`; no fields change.
async fn try_update_controller_state(
    txn: &mut PgConnection,
    domain: &NvLinkDomain,
    state: &NvLinkDomainState,
    outcome: &PersistentStateHandlerOutcome,
) -> DatabaseResult<ConditionalWrite<ConfigVersion, ControllerStateNotCurrent>> {
    let next_version = domain.controller_state.version.increment();
    let query = "UPDATE nvlink_domains SET controller_state = $4,
        controller_state_version = $5, controller_state_outcome = $6, updated = now()
        WHERE id = $1 AND version = $2 AND controller_state_version = $3";
    let result = sqlx::query(query)
        .bind(domain.id)
        .bind(domain.version)
        .bind(domain.controller_state.version)
        .bind(sqlx::types::Json(state))
        .bind(next_version)
        .bind(sqlx::types::Json(outcome))
        .execute(txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;
    Ok(if result.rows_affected() == 1 {
        ConditionalWrite::Applied(next_version)
    } else {
        ConditionalWrite::NotApplied(ControllerStateNotCurrent)
    })
}

#[cfg(test)]
mod tests;

#[derive(sqlx::FromRow)]
struct MemberCandidate {
    member: sqlx::types::Json<model::nvlink_domain::NvLinkDomainMember>,
    compute_id: Option<carbide_uuid::machine::MachineId>,
    switch_id: Option<carbide_uuid::switch::SwitchId>,
    racks: sqlx::types::Json<Vec<model::expected_rack_group::ExpectedRackGroupRack>>,
}

/// Resolve every member against one inventory snapshot.
/// Undiscovered trays remain present with no target. Ambiguous identities,
/// non-host machines, and devices outside the declared RackGroup are errors.
async fn resolve_members(
    reader: impl DbReader<'_>,
    id: Uuid,
) -> DatabaseResult<Vec<model::nvlink_domain::NvLinkDomainMemberResolution>> {
    use model::nvlink_domain::{NvLinkDomainMemberResolution, NvLinkDomainTray};

    let query = "SELECT to_jsonb(member) AS member, candidate.compute_id,
        candidate.switch_id, g.racks
        FROM nvlink_domain_members member
        JOIN nvlink_domains d ON d.id = member.domain_id
        JOIN expected_rack_groups g ON g.rack_group_id = d.rack_group_id
        LEFT JOIN LATERAL (
            SELECT machine.id AS compute_id, NULL::varchar AS switch_id
            FROM machines machine
            WHERE member.component_type = 'Compute' AND machine.rack_id = member.rack_id AND machine.tray_index = member.tray_index
            UNION ALL
            SELECT NULL::varchar, s.id FROM switches s
            WHERE member.component_type = 'Switch' AND s.deleted IS NULL AND s.rack_id = member.rack_id AND s.tray_index = member.tray_index
        ) candidate ON true
        WHERE d.id = $1 ORDER BY member.rack_id, member.component_type, member.tray_index, candidate.compute_id, candidate.switch_id";
    let candidates: Vec<MemberCandidate> = sqlx::query_as(query)
        .bind(id)
        .fetch_all(reader)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;
    if candidates.is_empty() {
        return Err(DatabaseError::NotFoundError {
            kind: "nvlink_domain",
            id: id.to_string(),
        });
    }
    let mut seen = HashSet::new();
    candidates
        .into_iter()
        .map(|candidate| {
            if !seen.insert(candidate.member.0.clone()) {
                return Err(DatabaseError::FailedPrecondition(format!(
                    "domain {id} has an ambiguous member mapping"
                )));
            }
            let tray = match (candidate.compute_id, candidate.switch_id) {
                (Some(machine_id), None) if machine_id.machine_type().is_host() => {
                    Some(NvLinkDomainTray::Compute(machine_id))
                }
                (None, Some(switch_id)) => Some(NvLinkDomainTray::Switch(switch_id)),
                (None, None) => None,
                _ => {
                    return Err(DatabaseError::FailedPrecondition(format!(
                        "domain {id} member does not resolve to a compute or switch tray"
                    )));
                }
            };
            if !candidate
                .racks
                .0
                .iter()
                .any(|rack| rack.rack_id == candidate.member.0.rack_id)
            {
                return Err(DatabaseError::FailedPrecondition(format!(
                    "domain {id} member position is outside its rack group"
                )));
            }
            Ok(NvLinkDomainMemberResolution {
                member: candidate.member.0,
                tray,
            })
        })
        .collect()
}

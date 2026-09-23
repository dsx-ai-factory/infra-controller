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

//! Database operations for SitePrefixes.
//!
//! Explicit result columns keep this table's queries working across column
//! additions. Cached wildcard statements otherwise fail with PostgreSQL's
//! "cached plan must not change result type".

use std::collections::HashMap;

use carbide_uuid::site_prefix::SitePrefixId;
use carbide_uuid::vpc::VpcPrefixId;
use chrono::{DateTime, Utc};
use config_version::ConfigVersion;
use ipnetwork::IpNetwork;
use model::controller_outcome::PersistentStateHandlerOutcome;
use model::machine::{LoadSnapshotOptions, ManagedHostStateSnapshot};
use model::site_prefix::{
    NewSitePrefix, NewTenantManagedSitePrefix, PrefixMatch, RetireTenantManagedSitePrefix,
    SitePrefix, SitePrefixAuthority, SitePrefixLifecycleState, SitePrefixRoutingScope,
    SitePrefixSearchFilter, UpdateSitePrefixMetadata,
};
use model::tenant::TenantOrganizationId;
use sqlx::{PgConnection, QueryBuilder};

use crate::db_read::DbReader;
use crate::machine::MachineNetworkConfigNotCurrent;
use crate::{ConditionalWrite, ControllerStateNotCurrent, DatabaseError, DatabaseResult};

const TENANT_PREFIX_EXCLUSION: &str = "site_prefixes_tenant_prefix_excl";
const TENANT_ADMISSION_CHECK: &str = "site_prefixes_tenant_admission_check";
const OPERATOR_MANAGED_PREFIX_UNIQUE: &str = "site_prefixes_operator_managed_prefix_key";
// `Configured` describes the config-file source here. Keep the key stable so
// every reconciler uses the same advisory lock.
const CONFIGURED_RECONCILE_LOCK: &str = "site_prefixes:configured_reconcile";

fn tenant_prefix_overlap_error(prefix: &IpNetwork) -> DatabaseError {
    DatabaseError::InvalidArgument(format!(
        "site prefix {prefix} overlaps another site prefix owned by the same tenant"
    ))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CreateDisposition {
    Created,
    Existing,
}

#[derive(Clone, Debug)]
pub struct CreateResult {
    pub site_prefix: SitePrefix,
    pub disposition: CreateDisposition,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VpcPrefixSitePrefixLineageAmbiguity {
    pub vpc_prefix_id: VpcPrefixId,
    pub candidate_site_prefix_ids: Vec<SitePrefixId>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct VpcPrefixSitePrefixLineageReport {
    pub assigned_vpc_prefix_ids: Vec<VpcPrefixId>,
    pub missing_vpc_prefix_ids: Vec<VpcPrefixId>,
    pub ambiguous: Vec<VpcPrefixSitePrefixLineageAmbiguity>,
}

impl VpcPrefixSitePrefixLineageReport {
    /// Reports whether startup may safely accept this lineage result.
    ///
    /// Ambiguous operator lineage is always unsafe. Missing lineage is unsafe
    /// only while operator roots are configured, because a rootless VpcPrefix
    /// remains valid when that legacy admission boundary is disabled.
    pub fn is_safe_for_startup(&self, require_parent_for_every_vpc_prefix: bool) -> bool {
        self.ambiguous.is_empty()
            && (!require_parent_for_every_vpc_prefix || self.missing_vpc_prefix_ids.is_empty())
    }
}

async fn lock_site_prefix_id(
    site_prefix_id: SitePrefixId,
    txn: &mut PgConnection,
) -> DatabaseResult<()> {
    let query = r#"
        SELECT pg_advisory_xact_lock(
            hashtextextended('site_prefixes:id:' || $1, 0)
        )
    "#;
    sqlx::query(query)
        .bind(site_prefix_id.to_string())
        .execute(txn)
        .await
        .map(|_| ())
        .map_err(|error| DatabaseError::query(query, error))
}

/// Runs tenant prefix namespace decisions serially per tenant.
///
/// SitePrefix writes use this lock so concurrent overlaps reach the exclusion
/// constraint instead of deadlocking each other. Attachment checks take the
/// matching shared lock so a concurrent first insert cannot be missed.
async fn lock_tenant_prefix_writes(
    tenant_organization_id: &str,
    txn: &mut PgConnection,
) -> DatabaseResult<()> {
    let query = r#"
        SELECT pg_advisory_xact_lock(
            hashtextextended('site_prefixes:tenant:' || $1, 0)
        )
    "#;
    sqlx::query(query)
        .bind(tenant_organization_id)
        .execute(txn)
        .await
        .map(|_| ())
        .map_err(|error| DatabaseError::query(query, error))
}

/// Prevents tenant-managed SitePrefix writes from changing the namespace
/// while a legacy VpcPrefix request checks whether it must name an exact root.
///
/// Multiple attachment checks may run together, while a tenant-managed create
/// takes the corresponding exclusive advisory lock. Existing-row retirement
/// is still serialized by the row lock taken by the following lookup.
pub async fn lock_tenant_site_prefix_attachments(
    tenant_organization_id: &str,
    txn: &mut PgConnection,
) -> DatabaseResult<()> {
    let query = r#"
        SELECT pg_advisory_xact_lock_shared(
            hashtextextended('site_prefixes:tenant:' || $1, 0)
        )
    "#;
    sqlx::query(query)
        .bind(tenant_organization_id)
        .execute(txn)
        .await
        .map(|_| ())
        .map_err(|error| DatabaseError::query(query, error))
}

async fn insert(value: NewSitePrefix, txn: &mut PgConnection) -> DatabaseResult<SitePrefix> {
    value.validate()?;

    let version = ConfigVersion::initial();
    let query = r#"
        INSERT INTO site_prefixes (
            id,
            prefix,
            authority,
            tenant_organization_id,
            routing_scope,
            lifecycle_state,
            name,
            description,
            labels,
            version
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9::jsonb, $10)
        RETURNING id, prefix, authority, tenant_organization_id, routing_scope,
            lifecycle_state, name, description, labels, version, created_at, updated_at
    "#;

    let site_prefix: SitePrefix = sqlx::query_as(query)
        .bind(value.id)
        .bind(value.config.prefix)
        .bind(value.status.authority)
        .bind(&value.config.tenant_organization_id)
        .bind(value.config.routing_scope)
        .bind(value.status.lifecycle_state)
        .bind(&value.metadata.name)
        .bind(&value.metadata.description)
        .bind(sqlx::types::Json(&value.metadata.labels))
        .bind(version)
        .fetch_one(&mut *txn)
        .await
        .map_err(|error| {
            let constraint = match &error {
                sqlx::Error::Database(database_error) => database_error.constraint(),
                _ => None,
            };

            match constraint {
                Some(TENANT_PREFIX_EXCLUSION) => tenant_prefix_overlap_error(&value.config.prefix),
                Some(OPERATOR_MANAGED_PREFIX_UNIQUE) => DatabaseError::AlreadyFoundError {
                    kind: "operator-managed site prefix",
                    id: value.config.prefix.to_string(),
                },
                // Supported tenant writers validate before insert. Preserve
                // an actionable error for direct or future writers caught by
                // the database's defense-in-depth policy constraint.
                Some(TENANT_ADMISSION_CHECK) => DatabaseError::InvalidArgument(format!(
                    "site prefix {} does not satisfy the tenant address policy",
                    value.config.prefix
                )),
                _ => DatabaseError::query(query, error),
            }
        })?;

    crate::state_history::persist(
        txn,
        crate::state_history::StateHistoryTableId::SitePrefix,
        &site_prefix.id,
        &site_prefix.status.lifecycle_state,
        site_prefix.version,
    )
    .await?;

    Ok(site_prefix)
}

/// Creates one tenant-managed SitePrefix or returns the current resource
/// already using the caller's ID when its immutable fields match. Create
/// metadata is not reapplied on an idempotent retry; callers use the metadata
/// update API for that mutable state.
pub async fn create_tenant_managed(
    value: NewTenantManagedSitePrefix,
    quota_limit: u32,
    txn: &mut PgConnection,
) -> DatabaseResult<CreateResult> {
    value.validate()?;

    lock_site_prefix_id(value.id, txn).await?;
    lock_tenant_prefix_writes(value.tenant_organization_id.as_str(), txn).await?;

    if crate::tenant::find(value.tenant_organization_id.as_str(), false, txn)
        .await?
        .is_none()
    {
        return Err(DatabaseError::NotFoundError {
            kind: "tenant",
            id: value.tenant_organization_id.to_string(),
        });
    }

    if let Some(existing) = find_by_id_for_update(txn, value.id).await? {
        let immutable_fields_match = existing.status.authority
            == SitePrefixAuthority::TenantManaged
            && existing.config.tenant_organization_id.as_ref()
                == Some(&value.tenant_organization_id)
            && existing.config.prefix == value.prefix
            && existing.config.routing_scope == SitePrefixRoutingScope::DatacenterOnly;

        if immutable_fields_match {
            return Ok(CreateResult {
                site_prefix: existing,
                disposition: CreateDisposition::Existing,
            });
        }

        return Err(DatabaseError::AlreadyFoundError {
            kind: "site prefix",
            id: value.id.to_string(),
        });
    }

    let used = count_tenant_managed(&mut *txn, &value.tenant_organization_id).await?;
    if used >= quota_limit {
        return Err(DatabaseError::TenantSitePrefixQuotaExceeded {
            used,
            limit: quota_limit,
        });
    }

    let overlap_query = r#"
        SELECT EXISTS (
            SELECT 1
            FROM site_prefixes
            WHERE authority = $1
              AND tenant_organization_id = $2
              AND prefix && $3
        )
    "#;
    let overlaps: bool = sqlx::query_scalar(overlap_query)
        .bind(SitePrefixAuthority::TenantManaged)
        .bind(&value.tenant_organization_id)
        .bind(value.prefix)
        .fetch_one(&mut *txn)
        .await
        .map_err(|error| DatabaseError::query(overlap_query, error))?;
    if overlaps {
        return Err(tenant_prefix_overlap_error(&value.prefix));
    }

    let site_prefix = insert(value.into_new_site_prefix(), txn).await?;
    Ok(CreateResult {
        site_prefix,
        disposition: CreateDisposition::Created,
    })
}

async fn lock_configured_reconciliation(txn: &mut PgConnection) -> DatabaseResult<()> {
    let query = "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))";
    sqlx::query(query)
        .bind(CONFIGURED_RECONCILE_LOCK)
        .execute(txn)
        .await
        .map(|_| ())
        .map_err(|error| DatabaseError::query(query, error))
}

/// Prevents configured SitePrefix reconciliation from changing the operator
/// namespace while a legacy VpcPrefix request resolves its exact root.
///
/// This namespace lock covers the zero-row case that row locks cannot protect.
pub async fn lock_operator_managed_site_prefix_attachments(
    txn: &mut PgConnection,
) -> DatabaseResult<()> {
    let query = "SELECT pg_advisory_xact_lock_shared(hashtextextended($1, 0))";
    sqlx::query(query)
        .bind(CONFIGURED_RECONCILE_LOCK)
        .execute(txn)
        .await
        .map(|_| ())
        .map_err(|error| DatabaseError::query(query, error))
}

/// Reconciles config-file site fabric prefixes into operator-managed rows.
///
/// The canonical CIDR is the current configuration identity. Existing rows are
/// reactivated in place, new rows receive a generated ID, and absent rows move
/// to Deleting so later lifecycle work can decide when final removal is safe.
pub async fn reconcile_configured(
    txn: &mut PgConnection,
    configured_prefixes: &[IpNetwork],
) -> DatabaseResult<()> {
    lock_configured_reconciliation(txn).await?;

    let mut configured_prefixes: Vec<IpNetwork> = configured_prefixes
        .iter()
        .map(|prefix| NewSitePrefix::configured(*prefix).config.prefix)
        .collect();
    configured_prefixes.sort_by_cached_key(ToString::to_string);
    configured_prefixes.dedup();

    let find_query = "SELECT id, prefix, authority, tenant_organization_id, routing_scope,
        lifecycle_state, name, description, labels, version, created_at, updated_at
        FROM site_prefixes WHERE authority = $1 FOR UPDATE";
    let stored: Vec<SitePrefix> = sqlx::query_as(find_query)
        .bind(SitePrefixAuthority::OperatorManaged)
        .fetch_all(&mut *txn)
        .await
        .map_err(|error| DatabaseError::query(find_query, error))?;
    let mut stored: HashMap<IpNetwork, SitePrefix> = stored
        .into_iter()
        .map(|site_prefix| (site_prefix.config.prefix, site_prefix))
        .collect();

    for prefix in configured_prefixes {
        let desired = NewSitePrefix::configured(prefix);
        desired.validate()?;

        match stored.remove(&prefix) {
            Some(current)
                if current.config == desired.config
                    && current.metadata == desired.metadata
                    && current.status == desired.status => {}
            Some(current) => {
                let next_version = current.version.increment();
                let query = r#"
                    UPDATE site_prefixes
                    SET routing_scope = $1,
                        lifecycle_state = $2,
                        name = $3,
                        description = $4,
                        labels = $5::jsonb,
                        version = $6,
                        updated_at = now()
                    WHERE id = $7
                "#;
                sqlx::query(query)
                    .bind(desired.config.routing_scope)
                    .bind(desired.status.lifecycle_state)
                    .bind(&desired.metadata.name)
                    .bind(&desired.metadata.description)
                    .bind(sqlx::types::Json(&desired.metadata.labels))
                    .bind(next_version)
                    .bind(current.id)
                    .execute(&mut *txn)
                    .await
                    .map_err(|error| DatabaseError::query(query, error))?;

                if current.status.lifecycle_state != desired.status.lifecycle_state {
                    crate::state_history::persist(
                        txn,
                        crate::state_history::StateHistoryTableId::SitePrefix,
                        &current.id,
                        &desired.status.lifecycle_state,
                        next_version,
                    )
                    .await?;
                }
            }
            None => {
                insert(desired, &mut *txn).await?;
            }
        }
    }

    for current in stored.into_values() {
        if current.status.lifecycle_state == SitePrefixLifecycleState::Deleting {
            continue;
        }

        let query = r#"
            UPDATE site_prefixes
            SET lifecycle_state = $1,
                version = $2,
                updated_at = now()
            WHERE id = $3
        "#;
        let next_version = current.version.increment();
        sqlx::query(query)
            .bind(SitePrefixLifecycleState::Deleting)
            .bind(next_version)
            .bind(current.id)
            .execute(&mut *txn)
            .await
            .map_err(|error| DatabaseError::query(query, error))?;

        crate::state_history::persist(
            txn,
            crate::state_history::StateHistoryTableId::SitePrefix,
            &current.id,
            &SitePrefixLifecycleState::Deleting,
            next_version,
        )
        .await?;
    }

    Ok(())
}

/// Locks and returns one SitePrefix for a caller that is about to mutate it.
pub async fn find_by_id_for_update(
    txn: &mut PgConnection,
    site_prefix_id: SitePrefixId,
) -> DatabaseResult<Option<SitePrefix>> {
    let query = "SELECT id, prefix, authority, tenant_organization_id, routing_scope,
        lifecycle_state, name, description, labels, version, created_at, updated_at
        FROM site_prefixes WHERE id = $1 FOR UPDATE";
    sqlx::query_as(query)
        .bind(site_prefix_id)
        .fetch_optional(txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))
}

/// Locks one SitePrefix while a caller validates and attaches a VpcPrefix.
///
/// The shared lock allows concurrent attachments but conflicts with the
/// exclusive lock used by tenant retirement and operator reconciliation. The
/// caller must keep its transaction open through VpcPrefix persistence.
pub async fn find_by_id_for_vpc_prefix_attachment(
    txn: &mut PgConnection,
    site_prefix_id: SitePrefixId,
) -> DatabaseResult<Option<SitePrefix>> {
    let query = "SELECT id, prefix, authority, tenant_organization_id, routing_scope,
        lifecycle_state, name, description, labels, version, created_at, updated_at
        FROM site_prefixes WHERE id = $1 FOR SHARE";
    sqlx::query_as(query)
        .bind(site_prefix_id)
        .fetch_optional(txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))
}

/// Locks every operator-managed SitePrefix containing a legacy VpcPrefix.
///
/// Results are ordered deterministically, but the caller must require exactly
/// one match. In particular, it must not guess between nested operator roots.
/// Lifecycle state is deliberately not filtered so Core can reject a more
/// specific or otherwise matching root that has begun retirement.
pub async fn find_legacy_operator_managed_for_vpc_prefix_attachment(
    txn: &mut PgConnection,
    prefix: IpNetwork,
) -> DatabaseResult<Vec<SitePrefix>> {
    let query = r#"
        SELECT id, prefix, authority, tenant_organization_id, routing_scope,
            lifecycle_state, name, description, labels, version, created_at, updated_at
        FROM site_prefixes
        WHERE authority = $1
          AND prefix >>= $2
        ORDER BY masklen(prefix) DESC, id
        FOR SHARE
    "#;
    sqlx::query_as(query)
        .bind(SitePrefixAuthority::OperatorManaged)
        .bind(prefix)
        .fetch_all(txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))
}

/// Locks tenant-managed SitePrefixes containing a legacy VpcPrefix request.
///
/// Legacy callers must select tenant-managed address space explicitly so Core
/// can enforce ownership and VPC virtualization rules. Callers use only the
/// presence of rows and must not expose another tenant's SitePrefix details.
pub async fn find_containing_tenant_managed_for_vpc_prefix_attachment(
    txn: &mut PgConnection,
    prefix: IpNetwork,
    tenant_organization_id: &str,
) -> DatabaseResult<Vec<SitePrefix>> {
    let query = r#"
        SELECT id, prefix, authority, tenant_organization_id, routing_scope,
            lifecycle_state, name, description, labels, version, created_at, updated_at
        FROM site_prefixes
        WHERE authority = $1
          AND prefix >>= $2
          AND tenant_organization_id = $3
        ORDER BY id
        FOR SHARE
    "#;
    sqlx::query_as(query)
        .bind(SitePrefixAuthority::TenantManaged)
        .bind(prefix)
        .bind(tenant_organization_id)
        .fetch_all(txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))
}

/// Returns VpcPrefixes that still lack an exact SitePrefix relationship.
///
/// This read-only check is used by listen-only API replicas. The authoritative
/// startup path performs the unique-parent backfill before replicas may serve.
pub async fn find_unassigned_vpc_prefix_site_prefix_ids(
    db: impl DbReader<'_>,
) -> DatabaseResult<Vec<VpcPrefixId>> {
    let query = r#"
        SELECT id
        FROM network_vpc_prefixes
        WHERE site_prefix_id IS NULL
        ORDER BY id
    "#;
    sqlx::query_scalar(query)
        .fetch_all(db)
        .await
        .map_err(|error| DatabaseError::query(query, error))
}

/// Returns unassigned VpcPrefixes that have a historical operator parent.
///
/// Listen-only replicas use this narrower audit when no operator roots are
/// configured. It still blocks rows that an authoritative replica must
/// backfill or reject, without rejecting valid rootless VpcPrefixes.
pub async fn find_unassigned_vpc_prefix_ids_with_operator_parent_candidates(
    db: impl DbReader<'_>,
) -> DatabaseResult<Vec<VpcPrefixId>> {
    let query = r#"
        -- With no configured roots, unrelated rootless prefixes remain valid.
        -- Audit only rows that an authoritative Core must assign or reject.
        SELECT network_vpc_prefixes.id
        FROM network_vpc_prefixes
        WHERE network_vpc_prefixes.site_prefix_id IS NULL
          AND EXISTS (
              SELECT 1
              FROM site_prefixes
              WHERE site_prefixes.authority = $1
                -- Retiring roots can still own legacy children awaiting backfill.
                AND site_prefixes.lifecycle_state IN ($2, $3)
                AND site_prefixes.prefix >>= network_vpc_prefixes.prefix
          )
        ORDER BY network_vpc_prefixes.id
    "#;
    sqlx::query_scalar(query)
        .bind(SitePrefixAuthority::OperatorManaged)
        .bind(SitePrefixLifecycleState::Ready)
        .bind(SitePrefixLifecycleState::Deleting)
        .fetch_all(db)
        .await
        .map_err(|error| DatabaseError::query(query, error))
}

/// Backfills exact SitePrefix lineage for legacy VpcPrefixes and audits rows
/// that cannot be assigned safely.
///
/// This is intended to run in the same startup transaction immediately after
/// operator-managed SitePrefix reconciliation. It locks every unassigned row,
/// assigns only rows with one containing operator-managed parent, and leaves
/// missing or ambiguous rows unchanged for the caller to interpret under the
/// configured-root policy. Re-running it is safe: successfully assigned rows
/// are no longer selected.
pub async fn backfill_vpc_prefix_site_prefix_lineage(
    txn: &mut PgConnection,
) -> DatabaseResult<VpcPrefixSitePrefixLineageReport> {
    let lock_query = r#"
        SELECT id, prefix
        FROM network_vpc_prefixes
        WHERE site_prefix_id IS NULL
        ORDER BY id
        FOR UPDATE
    "#;
    let unassigned_vpc_prefixes: Vec<(VpcPrefixId, IpNetwork)> = sqlx::query_as(lock_query)
        .fetch_all(&mut *txn)
        .await
        .map_err(|error| DatabaseError::query(lock_query, error))?;
    if unassigned_vpc_prefixes.is_empty() {
        return Ok(VpcPrefixSitePrefixLineageReport::default());
    }

    let vpc_prefix_ids: Vec<VpcPrefixId> = unassigned_vpc_prefixes
        .iter()
        .map(|(vpc_prefix_id, _)| *vpc_prefix_id)
        .collect();
    let candidate_query = r#"
        SELECT network_vpc_prefixes.id, site_prefixes.id
        FROM network_vpc_prefixes
        INNER JOIN site_prefixes
            ON site_prefixes.prefix >>= network_vpc_prefixes.prefix
        WHERE network_vpc_prefixes.id = ANY($1)
          AND site_prefixes.authority = $2
          AND site_prefixes.lifecycle_state IN ($3, $4)
        ORDER BY network_vpc_prefixes.id, site_prefixes.id
    "#;
    let candidates: Vec<(VpcPrefixId, SitePrefixId)> = sqlx::query_as(candidate_query)
        .bind(&vpc_prefix_ids)
        .bind(SitePrefixAuthority::OperatorManaged)
        .bind(SitePrefixLifecycleState::Ready)
        .bind(SitePrefixLifecycleState::Deleting)
        .fetch_all(&mut *txn)
        .await
        .map_err(|error| DatabaseError::query(candidate_query, error))?;
    let candidates_by_vpc_prefix = candidates.into_iter().fold(
        HashMap::<VpcPrefixId, Vec<SitePrefixId>>::new(),
        |mut candidates, (vpc_prefix_id, site_prefix_id)| {
            candidates
                .entry(vpc_prefix_id)
                .or_default()
                .push(site_prefix_id);
            candidates
        },
    );

    let update_query = r#"
        UPDATE network_vpc_prefixes
        SET site_prefix_id = $1
        WHERE id = $2
          AND site_prefix_id IS NULL
        RETURNING id
    "#;
    let mut report = VpcPrefixSitePrefixLineageReport::default();
    for (vpc_prefix_id, _) in unassigned_vpc_prefixes {
        let Some(candidate_site_prefix_ids) = candidates_by_vpc_prefix.get(&vpc_prefix_id) else {
            report.missing_vpc_prefix_ids.push(vpc_prefix_id);
            continue;
        };

        let [site_prefix_id] = candidate_site_prefix_ids.as_slice() else {
            report.ambiguous.push(VpcPrefixSitePrefixLineageAmbiguity {
                vpc_prefix_id,
                candidate_site_prefix_ids: candidate_site_prefix_ids.clone(),
            });
            continue;
        };

        let assigned_vpc_prefix_id: VpcPrefixId = sqlx::query_scalar(update_query)
            .bind(site_prefix_id)
            .bind(vpc_prefix_id)
            .fetch_one(&mut *txn)
            .await
            .map_err(|error| DatabaseError::query(update_query, error))?;
        report.assigned_vpc_prefix_ids.push(assigned_vpc_prefix_id);
    }

    Ok(report)
}

/// Returns whether persisted operator-managed roots require legacy VpcPrefix
/// lineage handling, including roots removed from the current configuration.
pub async fn operator_managed_prefixes_exist(db: impl DbReader<'_>) -> DatabaseResult<bool> {
    // Use persisted inventory so an empty configuration cannot skip historical lineage checks.
    let query = "SELECT EXISTS (SELECT 1 FROM site_prefixes WHERE authority = $1)";
    sqlx::query_scalar(query)
        .bind(SitePrefixAuthority::OperatorManaged)
        .fetch_one(db)
        .await
        .map_err(|error| DatabaseError::query(query, error))
}

/// Returns operator-managed roots that still require routing isolation.
///
/// Configured-root reconciliation retains removed roots in `Deleting` while
/// their children drain. Soft-deleted VpcPrefixes may remain referenced by
/// generated segments, while VPC-attached direct NetworkPrefixes remain until
/// their segment is hard-deleted. Callers preserve routing isolation until the
/// last child of either kind is physically removed. Legacy rows without exact
/// lineage conservatively retain every containing operator root rather than
/// guessing between nested roots.
pub async fn find_operator_managed_prefixes_with_retained_vpc_prefixes(
    db: impl DbReader<'_>,
) -> DatabaseResult<Vec<IpNetwork>> {
    let query = r#"
        -- Keep routing coverage through soft deletion; either kind of child can
        -- still supply tenant routes until its owning row is physically deleted.
        SELECT site_prefix.prefix
        FROM site_prefixes AS site_prefix
        WHERE site_prefix.authority = $1
          AND site_prefix.lifecycle_state IN ($2, $3)
          AND (
              EXISTS (
                  SELECT 1
                  FROM network_vpc_prefixes AS vpc_prefix
                  -- Exact lineage takes precedence over geometric containment.
                  WHERE vpc_prefix.site_prefix_id = site_prefix.id
                     OR (
                         -- Legacy rows lack lineage, so retain every containing
                         -- operator root without guessing between nested roots.
                         vpc_prefix.site_prefix_id IS NULL
                         AND site_prefix.prefix >>= vpc_prefix.prefix
                     )
              )
              OR EXISTS (
                  -- Direct tenant segments have no VpcPrefix parent. Keep their
                  -- coverage while draining, but exclude infrastructure segments.
                  SELECT 1
                  FROM network_prefixes AS network_prefix
                  JOIN network_segments AS network_segment
                    ON network_segment.id = network_prefix.segment_id
                  WHERE network_prefix.vpc_prefix_id IS NULL
                    AND network_segment.vpc_id IS NOT NULL
                    AND site_prefix.prefix >>= network_prefix.prefix
              )
          )
        ORDER BY site_prefix.prefix
    "#;
    sqlx::query_scalar(query)
        .bind(SitePrefixAuthority::OperatorManaged)
        .bind(SitePrefixLifecycleState::Ready)
        .bind(SitePrefixLifecycleState::Deleting)
        .fetch_all(db)
        .await
        .map_err(|error| DatabaseError::query(query, error))
}

/// Counts every tenant-managed row for one tenant. Rows remain in this count
/// through `Deleting` because their address space is still reserved.
pub async fn count_tenant_managed(
    db: impl DbReader<'_>,
    tenant_organization_id: &TenantOrganizationId,
) -> DatabaseResult<u32> {
    let query = r#"
        SELECT count(*)
        FROM site_prefixes
        WHERE authority = $1
          AND tenant_organization_id = $2
    "#;
    let used: i64 = sqlx::query_scalar(query)
        .bind(SitePrefixAuthority::TenantManaged)
        .bind(tenant_organization_id)
        .fetch_one(db)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;

    u32::try_from(used)
        .map_err(|_| DatabaseError::internal(format!("invalid SitePrefix count {used}")))
}

/// Returns distinct retained tenant prefixes in every lifecycle state.
/// Deleting roots remain protected until their lifecycle finishes removal.
/// Order is unspecified; callers that need stable output must sort or compact it.
pub async fn find_tenant_prefixes(db: impl DbReader<'_>) -> DatabaseResult<Vec<IpNetwork>> {
    let query = "SELECT DISTINCT prefix FROM site_prefixes WHERE authority = $1";
    sqlx::query_scalar(query)
        .bind(SitePrefixAuthority::TenantManaged)
        .fetch_all(db)
        .await
        .map_err(|error| DatabaseError::query(query, error))
}

/// Returns tenant quota use for the owners present in one inventory response.
pub async fn count_tenant_managed_by_organizations(
    db: impl DbReader<'_>,
    tenant_organization_ids: &[TenantOrganizationId],
) -> DatabaseResult<HashMap<String, u32>> {
    if tenant_organization_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let tenant_organization_ids: Vec<&str> = tenant_organization_ids
        .iter()
        .map(TenantOrganizationId::as_str)
        .collect();
    let query = r#"
        SELECT tenant_organization_id, count(*)
        FROM site_prefixes
        WHERE authority = $1
          AND tenant_organization_id = ANY($2)
        GROUP BY tenant_organization_id
    "#;
    let counts: Vec<(String, i64)> = sqlx::query_as(query)
        .bind(SitePrefixAuthority::TenantManaged)
        .bind(&tenant_organization_ids)
        .fetch_all(db)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;

    counts
        .into_iter()
        .map(|(tenant_organization_id, used)| {
            u32::try_from(used)
                .map(|used| (tenant_organization_id, used))
                .map_err(|_| DatabaseError::internal(format!("invalid SitePrefix count {used}")))
        })
        .collect()
}

/// Updates only the caller-controlled metadata on a tenant-managed SitePrefix.
pub async fn update_tenant_metadata(
    value: &UpdateSitePrefixMetadata,
    expected_version: ConfigVersion,
    txn: &mut PgConnection,
) -> DatabaseResult<SitePrefix> {
    value.metadata.validate(true)?;

    let next_version = expected_version.increment();
    let query = r#"
        UPDATE site_prefixes
        SET name = $1,
            description = $2,
            labels = $3::jsonb,
            version = $4,
            updated_at = now()
        WHERE id = $5
          AND tenant_organization_id = $6
          AND authority = $7
          AND lifecycle_state <> $8
          AND version = $9
        RETURNING id, prefix, authority, tenant_organization_id, routing_scope,
            lifecycle_state, name, description, labels, version, created_at, updated_at
    "#;
    sqlx::query_as(query)
        .bind(&value.metadata.name)
        .bind(&value.metadata.description)
        .bind(sqlx::types::Json(&value.metadata.labels))
        .bind(next_version)
        .bind(value.id)
        .bind(&value.tenant_organization_id)
        .bind(SitePrefixAuthority::TenantManaged)
        .bind(SitePrefixLifecycleState::Deleting)
        .bind(expected_version)
        .fetch_one(txn)
        .await
        .map_err(|error| match error {
            sqlx::Error::RowNotFound => DatabaseError::ConcurrentModificationError(
                "site prefix",
                expected_version.to_string(),
            ),
            error => DatabaseError::query(query, error),
        })
}

/// Records retirement intent without releasing the row, quota slot, or CIDR.
pub async fn retire_tenant_managed(
    value: &RetireTenantManagedSitePrefix,
    current: &SitePrefix,
    txn: &mut PgConnection,
) -> DatabaseResult<SitePrefix> {
    if current.id != value.id {
        return Err(DatabaseError::InvalidArgument(format!(
            "retirement request ID {} does not match locked SitePrefix {}",
            value.id, current.id
        )));
    }
    if current.status.authority != SitePrefixAuthority::TenantManaged {
        return Err(DatabaseError::FailedPrecondition(
            "operator-managed SitePrefixes cannot be retired through the tenant API".to_string(),
        ));
    }
    if current.config.tenant_organization_id.as_ref() != Some(&value.tenant_organization_id) {
        return Err(DatabaseError::FailedPrecondition(
            "the SitePrefix is not owned by the requested tenant".to_string(),
        ));
    }
    if current.status.lifecycle_state == SitePrefixLifecycleState::Deleting {
        return Ok(current.clone());
    }

    let next_version = current.version.increment();
    let query = r#"
        UPDATE site_prefixes
        SET lifecycle_state = $1,
            version = $2,
            updated_at = now()
        WHERE id = $3
          AND tenant_organization_id = $4
          AND authority = $5
          AND version = $6
        RETURNING id, prefix, authority, tenant_organization_id, routing_scope,
            lifecycle_state, name, description, labels, version, created_at, updated_at
    "#;
    let site_prefix: SitePrefix = sqlx::query_as(query)
        .bind(SitePrefixLifecycleState::Deleting)
        .bind(next_version)
        .bind(value.id)
        .bind(&value.tenant_organization_id)
        .bind(SitePrefixAuthority::TenantManaged)
        .bind(current.version)
        .fetch_one(&mut *txn)
        .await
        .map_err(|error| match error {
            sqlx::Error::RowNotFound => DatabaseError::ConcurrentModificationError(
                "site prefix",
                current.version.to_string(),
            ),
            error => DatabaseError::query(query, error),
        })?;

    crate::state_history::persist(
        txn,
        crate::state_history::StateHistoryTableId::SitePrefix,
        &site_prefix.id,
        &site_prefix.status.lifecycle_state,
        site_prefix.version,
    )
    .await?;

    Ok(site_prefix)
}

pub async fn find_ids(
    db: impl DbReader<'_>,
    filter: SitePrefixSearchFilter,
) -> DatabaseResult<Vec<SitePrefixId>> {
    let SitePrefixSearchFilter {
        tenant_organization_id,
        authority,
        routing_scope,
        lifecycle_state,
        prefix_match,
    } = filter;

    let mut query = QueryBuilder::new("SELECT id FROM site_prefixes WHERE true");

    if let Some(tenant_organization_id) = tenant_organization_id {
        query.push(" AND tenant_organization_id = ");
        query.push_bind(tenant_organization_id);
    }
    if let Some(authority) = authority {
        query.push(" AND authority = ");
        query.push_bind(authority);
    }
    if let Some(routing_scope) = routing_scope {
        query.push(" AND routing_scope = ");
        query.push_bind(routing_scope);
    }
    if let Some(lifecycle_state) = lifecycle_state {
        query.push(" AND lifecycle_state = ");
        query.push_bind(lifecycle_state);
    }
    if let Some(prefix_match) = prefix_match {
        match prefix_match {
            PrefixMatch::Exact(prefix) => {
                query.push(" AND prefix = ");
                query.push_bind(prefix);
            }
            PrefixMatch::Contains(prefix) => {
                query.push(" AND prefix >>= ");
                query.push_bind(prefix);
            }
            PrefixMatch::ContainedBy(prefix) => {
                query.push(" AND prefix <<= ");
                query.push_bind(prefix);
            }
        }
    }

    query.push(" ORDER BY id");
    query
        .build_query_as()
        .fetch_all(db)
        .await
        .map_err(|error| DatabaseError::query(query.sql(), error))
}

pub async fn find_by_ids(
    db: impl DbReader<'_>,
    site_prefix_ids: &[SitePrefixId],
) -> DatabaseResult<Vec<SitePrefix>> {
    let query = "SELECT id, prefix, authority, tenant_organization_id, routing_scope,
        lifecycle_state, name, description, labels, version, created_at, updated_at
        FROM site_prefixes WHERE id = ANY($1) ORDER BY id";
    sqlx::query_as(query)
        .bind(site_prefix_ids)
        .fetch_all(db)
        .await
        .map_err(|error| DatabaseError::query(query, error))
}

/// `find_isolation_hosts` selects hosts using
/// [`ManagedHostStateSnapshot::needs_site_prefix_isolation`].
/// When the caller's site policy does not require isolation, no hosts are selected.
pub async fn find_isolation_hosts(
    txn: &mut PgConnection,
    isolation_required: bool,
) -> DatabaseResult<Vec<ManagedHostStateSnapshot>> {
    if !isolation_required {
        return Ok(Vec::new());
    }

    let mut hosts = crate::managed_host::load_all(txn, LoadSnapshotOptions::default())
        .await?
        .into_iter()
        .filter(ManagedHostStateSnapshot::needs_site_prefix_isolation)
        .collect::<Vec<_>>();
    // Use the same machine lock order for every prefix's initial request.
    hosts.sort_unstable_by_key(|host| host.host_snapshot.id);
    Ok(hosts)
}

/// Returns the initial protection request time, or None for an unrequested or
/// missing root. Once recorded, this time does not change during readiness.
pub async fn isolation_requested_at(
    txn: &mut PgConnection,
    site_prefix_id: SitePrefixId,
) -> DatabaseResult<Option<DateTime<Utc>>> {
    let query = "SELECT isolation_requested_at FROM site_prefixes WHERE id = $1";
    sqlx::query_scalar(query)
        .bind(site_prefix_id)
        .fetch_optional(txn)
        .await
        .map(Option::flatten)
        .map_err(|error| DatabaseError::query(query, error))
}

/// `request_isolation` requests the complete protection configuration once for
/// a tenant SitePrefix. The caller holds the routing lock and the SitePrefix
/// row lock, and commits the host/DPU version changes with the returned request
/// time. Repeated calls return that time without generating more versions.
/// `isolation_required` is the caller's site policy decision; false records the
/// request with no receivers, allowing the normal readiness transition.
///
/// `NotApplied` means a host disappeared or changed network version. The
/// caller must roll back all writes and retry from fresh snapshots.
pub async fn request_isolation(
    txn: &mut PgConnection,
    site_prefix: &SitePrefix,
    isolation_required: bool,
) -> DatabaseResult<ConditionalWrite<DateTime<Utc>, MachineNetworkConfigNotCurrent>> {
    if site_prefix.status.authority != SitePrefixAuthority::TenantManaged
        || site_prefix.status.lifecycle_state != SitePrefixLifecycleState::Provisioning
    {
        return Err(DatabaseError::FailedPrecondition(
            "only a provisioning tenant SitePrefix can request isolation".to_string(),
        ));
    }

    if let Some(requested_at) = isolation_requested_at(txn, site_prefix.id).await? {
        return Ok(ConditionalWrite::Applied(requested_at));
    }

    for host in find_isolation_hosts(txn, isolation_required).await? {
        if let ConditionalWrite::NotApplied(reason) = crate::machine::try_update_network_config(
            txn,
            &host.host_snapshot.id,
            host.host_snapshot.network_config.version,
            &host.host_snapshot.network_config.value,
        )
        .await?
        {
            return Ok(ConditionalWrite::NotApplied(reason));
        }
    }

    // This is progress within Provisioning, not a public lifecycle or metadata
    // change. Its transaction includes every group update, so a restart can
    // resume the acknowledgement wait without resetting any host's target.
    // Use request completion time, not the transaction's earlier start time.
    let query = "UPDATE site_prefixes SET isolation_requested_at = clock_timestamp() \
        WHERE id = $1 RETURNING isolation_requested_at";
    let requested_at = sqlx::query_scalar(query)
        .bind(site_prefix.id)
        .fetch_one(txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;
    Ok(ConditionalWrite::Applied(requested_at))
}

/// `try_mark_ready` applies a controller readiness decision only while the same
/// tenant root is still Provisioning and its initial protection request exists.
/// Missing, retired, or concurrently changed roots return `NotApplied`.
pub async fn try_mark_ready(
    txn: &mut PgConnection,
    site_prefix_id: SitePrefixId,
    old_version: ConfigVersion,
    new_version: ConfigVersion,
) -> DatabaseResult<ConditionalWrite<(), ControllerStateNotCurrent>> {
    let query = r#"
        UPDATE site_prefixes
        SET lifecycle_state = $1, version = $2, updated_at = now()
        WHERE id = $3 AND version = $4 AND authority = $5
          AND lifecycle_state = $6 AND isolation_requested_at IS NOT NULL
    "#;
    let result = sqlx::query(query)
        .bind(SitePrefixLifecycleState::Ready)
        .bind(new_version)
        .bind(site_prefix_id)
        .bind(old_version)
        .bind(SitePrefixAuthority::TenantManaged)
        .bind(SitePrefixLifecycleState::Provisioning)
        .execute(txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;
    Ok(if result.rows_affected() == 1 {
        ConditionalWrite::Applied(())
    } else {
        ConditionalWrite::NotApplied(ControllerStateNotCurrent)
    })
}

/// `update_controller_state_outcome` stores the last readiness handler result
/// without changing the SitePrefix's public lifecycle or optimistic version.
pub async fn update_controller_state_outcome(
    txn: &mut PgConnection,
    site_prefix_id: SitePrefixId,
    outcome: PersistentStateHandlerOutcome,
) -> DatabaseResult<()> {
    let query = "UPDATE site_prefixes SET controller_state_outcome = $1 WHERE id = $2";
    sqlx::query(query)
        .bind(sqlx::types::Json(outcome))
        .bind(site_prefix_id)
        .execute(txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;
    Ok(())
}

#[cfg(test)]
mod tests {

    use model::metadata::Metadata;
    use model::site_prefix::NewTenantManagedSitePrefix;
    use sqlx::Connection;

    use super::*;

    fn tenant_managed(prefix: &str, tenant_organization_id: &str) -> NewTenantManagedSitePrefix {
        NewTenantManagedSitePrefix {
            id: SitePrefixId::new(),
            prefix: prefix.parse().unwrap(),
            tenant_organization_id: tenant_organization_id.parse().unwrap(),
            metadata: Metadata {
                name: prefix.to_string(),
                ..Metadata::default()
            },
        }
    }

    async fn create_tenant(
        pool: &sqlx::PgPool,
        tenant_organization_id: &str,
    ) -> Result<(), DatabaseError> {
        let mut txn = crate::Transaction::begin(pool).await?;
        crate::tenant::create_and_persist(
            tenant_organization_id.to_string(),
            Metadata {
                name: tenant_organization_id.to_string(),
                ..Metadata::default()
            },
            None,
            txn.as_pgconn(),
        )
        .await?;
        txn.commit().await?;
        Ok(())
    }

    async fn create(
        pool: &sqlx::PgPool,
        value: NewTenantManagedSitePrefix,
        quota_limit: u32,
    ) -> Result<CreateResult, DatabaseError> {
        let mut txn = crate::Transaction::begin(pool).await?;
        let result = create_tenant_managed(value, quota_limit, txn.as_pgconn()).await?;
        txn.commit().await?;
        Ok(result)
    }

    async fn history(
        pool: &sqlx::PgPool,
        site_prefix_id: SitePrefixId,
    ) -> Result<Vec<model::state_history::StateHistoryRecord>, DatabaseError> {
        let mut txn = crate::Transaction::begin(pool).await?;
        crate::state_history::for_object(
            txn.as_pgconn(),
            crate::state_history::StateHistoryTableId::SitePrefix,
            &site_prefix_id,
        )
        .await
    }

    #[crate::sqlx_test]
    async fn site_prefix_queries_survive_added_columns(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        create_tenant(&pool, "tenant-a").await?;
        let mut api_connection = pool.acquire().await?;
        exercise_site_prefix_queries(&mut api_connection, "10.72.0.0/24".parse()?).await?;
        assert!(api_connection.cached_statements_size() > 0);

        let mut migration_connection = pool.acquire().await?;
        let mut migration = migration_connection.begin().await?;
        sqlx::raw_sql(
            "SET LOCAL lock_timeout = '5s';
             ALTER TABLE site_prefixes
                 ADD COLUMN test_added_timestamp timestamptz,
                 ADD COLUMN test_added_outcome jsonb;",
        )
        .execute(&mut *migration)
        .await?;
        migration.commit().await?;

        exercise_site_prefix_queries(&mut api_connection, "10.73.0.0/24".parse()?).await?;
        Ok(())
    }

    async fn exercise_site_prefix_queries(
        connection: &mut PgConnection,
        prefix: IpNetwork,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut txn = connection.begin().await?;
        reconcile_configured(&mut txn, &[prefix]).await?;
        let [operator]: [SitePrefix; 1] =
            find_legacy_operator_managed_for_vpc_prefix_attachment(&mut txn, prefix)
                .await?
                .try_into()
                .expect("one containing operator prefix");
        assert_eq!(operator.config.prefix, prefix);
        assert_eq!(operator.config.tenant_organization_id, None);
        assert_eq!(
            operator.status.authority,
            SitePrefixAuthority::OperatorManaged
        );
        assert_eq!(
            operator.status.lifecycle_state,
            SitePrefixLifecycleState::Ready
        );

        // The first pass retires its tenant root, which still counts toward quota.
        let created =
            create_tenant_managed(tenant_managed(&prefix.to_string(), "tenant-a"), 2, &mut txn)
                .await?
                .site_prefix;
        assert_eq!(created.config.prefix, prefix);
        assert_eq!(
            created.config.tenant_organization_id,
            Some("tenant-a".parse()?)
        );
        assert_eq!(created.status.authority, SitePrefixAuthority::TenantManaged);
        assert_eq!(
            created.status.lifecycle_state,
            SitePrefixLifecycleState::Provisioning
        );

        let metadata = Metadata {
            name: "updated cached prefix".to_string(),
            description: "cached SitePrefix query".to_string(),
            labels: HashMap::from([("test".to_string(), "column addition".to_string())]),
        };
        let updated = update_tenant_metadata(
            &UpdateSitePrefixMetadata {
                id: created.id,
                tenant_organization_id: "tenant-a".parse()?,
                metadata: metadata.clone(),
                if_version_match: Some(created.version),
            },
            created.version,
            &mut txn,
        )
        .await?;
        assert_eq!(updated.metadata, metadata);

        for (name, rows) in [
            (
                "find_by_id_for_update",
                vec![
                    find_by_id_for_update(&mut txn, created.id)
                        .await?
                        .expect("created tenant prefix"),
                ],
            ),
            (
                "find_by_id_for_vpc_prefix_attachment",
                vec![
                    find_by_id_for_vpc_prefix_attachment(&mut txn, created.id)
                        .await?
                        .expect("created tenant prefix"),
                ],
            ),
            (
                "find_containing_tenant_managed_for_vpc_prefix_attachment",
                find_containing_tenant_managed_for_vpc_prefix_attachment(
                    &mut txn, prefix, "tenant-a",
                )
                .await?,
            ),
            ("find_by_ids", find_by_ids(&mut *txn, &[created.id]).await?),
        ] {
            assert_eq!(rows.as_slice(), std::slice::from_ref(&updated), "{name}");
        }

        let retired = retire_tenant_managed(
            &RetireTenantManagedSitePrefix {
                id: created.id,
                tenant_organization_id: "tenant-a".parse()?,
            },
            &updated,
            &mut txn,
        )
        .await?;
        assert_eq!(
            retired.status.lifecycle_state,
            SitePrefixLifecycleState::Deleting
        );
        assert_eq!(retired.config, updated.config);
        assert_eq!(retired.metadata, metadata);
        txn.commit().await?;

        assert_eq!(find_by_ids(connection, &[created.id]).await?, vec![retired]);
        Ok(())
    }

    #[crate::sqlx_test]
    async fn configured_reconciliation_preserves_identity_and_tenant_rows(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        create_tenant(&pool, "tenant-a").await?;

        let configured_prefix: IpNetwork = "10.0.0.0/8".parse()?;
        let equivalent_noncanonical_prefix: IpNetwork = "10.1.2.3/8".parse()?;
        let other_configured_prefix: IpNetwork = "192.168.0.0/16".parse()?;

        let mut txn = pool.begin().await?;
        reconcile_configured(
            &mut txn,
            &[
                configured_prefix,
                equivalent_noncanonical_prefix,
                other_configured_prefix,
            ],
        )
        .await?;
        txn.commit().await?;

        let mut txn = pool.begin().await?;
        let tenant_prefix =
            create_tenant_managed(tenant_managed("10.0.0.0/8", "tenant-a"), 8, &mut txn)
                .await?
                .site_prefix;
        txn.commit().await?;

        let configured_ids = find_ids(
            &pool,
            SitePrefixSearchFilter {
                authority: Some(SitePrefixAuthority::OperatorManaged),
                ..Default::default()
            },
        )
        .await?;
        assert_eq!(configured_ids.len(), 2);
        let configured_rows = find_by_ids(&pool, &configured_ids).await?;
        let configured_before = configured_rows
            .iter()
            .find(|site_prefix| site_prefix.config.prefix == configured_prefix)
            .unwrap()
            .clone();
        let configured_id = configured_before.id;
        assert_eq!(history(&pool, configured_id).await?.len(), 1);

        let mut txn = pool.begin().await?;
        reconcile_configured(&mut txn, &[other_configured_prefix, configured_prefix]).await?;
        txn.commit().await?;

        let rows = find_by_ids(&pool, &[configured_id, tenant_prefix.id]).await?;
        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows.iter()
                .find(|site_prefix| site_prefix.id == configured_id)
                .unwrap(),
            &configured_before
        );
        assert_eq!(
            rows.iter()
                .find(|site_prefix| site_prefix.id == tenant_prefix.id)
                .unwrap(),
            &tenant_prefix
        );
        assert_eq!(history(&pool, configured_id).await?.len(), 1);

        let mut txn = pool.begin().await?;
        reconcile_configured(&mut txn, &[]).await?;
        txn.commit().await?;

        let rows = find_by_ids(&pool, &[configured_id, tenant_prefix.id]).await?;
        assert!(rows.iter().any(|site_prefix| {
            site_prefix.id == configured_id
                && site_prefix.status.lifecycle_state == SitePrefixLifecycleState::Deleting
        }));
        assert!(rows.iter().any(|site_prefix| {
            site_prefix.id == tenant_prefix.id
                && site_prefix.status.lifecycle_state == SitePrefixLifecycleState::Provisioning
        }));
        assert_eq!(
            rows.iter()
                .find(|site_prefix| site_prefix.id == tenant_prefix.id)
                .unwrap(),
            &tenant_prefix
        );
        assert_eq!(history(&pool, configured_id).await?.len(), 2);

        let mut txn = pool.begin().await?;
        reconcile_configured(&mut txn, &[configured_prefix]).await?;
        txn.commit().await?;

        let row = find_by_ids(&pool, &[configured_id]).await?.pop().unwrap();
        assert_eq!(row.id, configured_id);
        assert_eq!(row.status.lifecycle_state, SitePrefixLifecycleState::Ready);
        assert_eq!(history(&pool, configured_id).await?.len(), 3);

        Ok(())
    }

    #[crate::sqlx_test]
    async fn duplicate_operator_managed_prefix_reports_its_authority(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let prefix: IpNetwork = "10.0.0.0/8".parse()?;

        let mut txn = pool.begin().await?;
        insert(NewSitePrefix::configured(prefix), &mut txn).await?;
        txn.commit().await?;

        let mut txn = pool.begin().await?;
        let error = insert(NewSitePrefix::configured(prefix), &mut txn)
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            DatabaseError::AlreadyFoundError { kind, id }
                if kind == "operator-managed site prefix" && id == prefix.to_string()
        ));
        txn.rollback().await?;

        Ok(())
    }

    #[crate::sqlx_test]
    async fn different_tenants_and_operator_can_reuse_a_prefix(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        create_tenant(&pool, "tenant-a").await?;
        create_tenant(&pool, "tenant-b").await?;

        let prefix: IpNetwork = "10.0.0.0/8".parse()?;
        let mut txn = pool.begin().await?;
        reconcile_configured(&mut txn, &[prefix]).await?;
        let tenant_a = create_tenant_managed(tenant_managed("10.0.0.0/8", "tenant-a"), 8, &mut txn)
            .await?
            .site_prefix;
        let tenant_b = create_tenant_managed(tenant_managed("10.0.0.0/8", "tenant-b"), 8, &mut txn)
            .await?
            .site_prefix;
        txn.commit().await?;

        let rows = find_by_ids(&pool, &[tenant_a.id, tenant_b.id]).await?;
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].config.prefix, rows[1].config.prefix);
        assert_ne!(
            rows[0].config.tenant_organization_id,
            rows[1].config.tenant_organization_id
        );

        Ok(())
    }

    #[crate::sqlx_test]
    async fn configured_reconciliation_supports_existing_address_families_and_cidr_edits(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let public_prefix: IpNetwork = "203.0.113.0/24".parse()?;
        let nested_prefix: IpNetwork = "203.0.113.0/25".parse()?;
        let ipv6_prefix: IpNetwork = "2001:db8::/32".parse()?;
        let replacement_prefix: IpNetwork = "198.51.100.0/24".parse()?;

        let mut txn = pool.begin().await?;
        reconcile_configured(&mut txn, &[public_prefix, nested_prefix, ipv6_prefix]).await?;
        txn.commit().await?;

        let original_ids = find_ids(
            &pool,
            SitePrefixSearchFilter {
                authority: Some(SitePrefixAuthority::OperatorManaged),
                ..Default::default()
            },
        )
        .await?;
        let original_rows = find_by_ids(&pool, &original_ids).await?;
        assert_eq!(original_rows.len(), 3);
        assert!(original_rows.iter().any(|row| row.config.prefix.is_ipv6()));
        let public_id = original_rows
            .iter()
            .find(|row| row.config.prefix == public_prefix)
            .unwrap()
            .id;

        let mut txn = pool.begin().await?;
        reconcile_configured(&mut txn, &[replacement_prefix]).await?;
        txn.commit().await?;

        let all_ids = find_ids(&pool, SitePrefixSearchFilter::default()).await?;
        let all_rows = find_by_ids(&pool, &all_ids).await?;
        assert_eq!(all_rows.len(), 4);
        assert!(all_rows.iter().any(|row| {
            row.id == public_id && row.status.lifecycle_state == SitePrefixLifecycleState::Deleting
        }));
        let replacement = all_rows
            .iter()
            .find(|row| row.config.prefix == replacement_prefix)
            .unwrap();
        assert_ne!(replacement.id, public_id);
        assert_eq!(
            replacement.status.lifecycle_state,
            SitePrefixLifecycleState::Ready
        );

        let mut txn = pool.begin().await?;
        reconcile_configured(&mut txn, &[replacement_prefix, public_prefix]).await?;
        txn.commit().await?;

        let public = find_by_ids(&pool, &[public_id]).await?.pop().unwrap();
        assert_eq!(public.id, public_id);
        assert_eq!(
            public.status.lifecycle_state,
            SitePrefixLifecycleState::Ready
        );

        Ok(())
    }

    #[crate::sqlx_test]
    async fn concurrent_configured_reconciliation_reuses_one_row(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        async fn reconcile(pool: sqlx::PgPool, prefix: IpNetwork) -> DatabaseResult<()> {
            let mut txn = crate::Transaction::begin(&pool).await?;
            reconcile_configured(txn.as_pgconn(), &[prefix]).await?;
            txn.commit().await
        }

        let prefix: IpNetwork = "10.0.0.0/8".parse()?;
        let (first, second) = tokio::join!(
            reconcile(pool.clone(), prefix),
            reconcile(pool.clone(), prefix),
        );
        first?;
        second?;

        let ids = find_ids(&pool, SitePrefixSearchFilter::default()).await?;
        assert_eq!(ids.len(), 1);

        Ok(())
    }

    #[crate::sqlx_test]
    async fn same_tenant_overlap_is_rejected_concurrently(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        create_tenant(&pool, "tenant-a").await?;

        async fn insert(
            pool: sqlx::PgPool,
            value: NewTenantManagedSitePrefix,
        ) -> Result<SitePrefix, DatabaseError> {
            let mut txn = crate::Transaction::begin(&pool).await?;
            match create_tenant_managed(value, 8, txn.as_pgconn()).await {
                Ok(result) => {
                    txn.commit().await?;
                    Ok(result.site_prefix)
                }
                Err(error) => Err(error),
            }
        }

        let (first, second) = tokio::join!(
            insert(pool.clone(), tenant_managed("10.0.0.0/8", "tenant-a")),
            insert(pool.clone(), tenant_managed("10.1.0.0/16", "tenant-a")),
        );

        assert_eq!(usize::from(first.is_ok()) + usize::from(second.is_ok()), 1);
        let error = first.err().or_else(|| second.err()).unwrap();
        assert!(
            matches!(error, DatabaseError::InvalidArgument(_)),
            "unexpected overlap error: {error:?}"
        );

        Ok(())
    }

    #[crate::sqlx_test]
    async fn tenant_create_is_idempotent_and_serializes_identical_retries(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        create_tenant(&pool, "tenant-a").await?;

        let value = tenant_managed("10.0.0.0/24", "tenant-a");
        let site_prefix_id = value.id;
        let (first, second) = tokio::join!(
            create(&pool, value.clone(), 8),
            create(&pool, value.clone(), 8),
        );
        let first = first?;
        let second = second?;

        assert_eq!(first.site_prefix.id, site_prefix_id);
        assert_eq!(second.site_prefix.id, site_prefix_id);
        assert_eq!(
            [first.disposition, second.disposition]
                .into_iter()
                .filter(|result| *result == CreateDisposition::Created)
                .count(),
            1
        );
        assert_eq!(
            [first.disposition, second.disposition]
                .into_iter()
                .filter(|result| *result == CreateDisposition::Existing)
                .count(),
            1
        );
        assert_eq!(history(&pool, site_prefix_id).await?.len(), 1);

        let ids = find_ids(&pool, SitePrefixSearchFilter::default()).await?;
        assert_eq!(ids, vec![site_prefix_id]);

        let mut conflicting = value.clone();
        conflicting.prefix = "10.0.1.0/24".parse()?;
        let error = create(&pool, conflicting, 8).await.unwrap_err();
        assert!(matches!(error, DatabaseError::AlreadyFoundError { .. }));

        // A real retry is resolved before quota admission, so lowering the
        // limit cannot make an existing resource disappear behind an error.
        let retry = create(&pool, value, 0).await?;
        assert_eq!(retry.disposition, CreateDisposition::Existing);
        assert_eq!(retry.site_prefix.id, site_prefix_id);

        Ok(())
    }

    #[crate::sqlx_test]
    async fn concurrent_distinct_creates_cannot_overrun_tenant_quota(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        create_tenant(&pool, "tenant-a").await?;

        let first_value = tenant_managed("10.0.0.0/24", "tenant-a");
        let second_value = tenant_managed("10.0.1.0/24", "tenant-a");
        let first_id = first_value.id;
        let second_id = second_value.id;
        let (first, second) = tokio::join!(
            create(&pool, first_value, 1),
            create(&pool, second_value, 1),
        );

        let (winner_id, loser_id, error) = match (first, second) {
            (Ok(first), Err(error)) => (first.site_prefix.id, second_id, error),
            (Err(error), Ok(second)) => (second.site_prefix.id, first_id, error),
            results => panic!("expected one admitted prefix and one quota error: {results:?}"),
        };
        assert!(matches!(
            error,
            DatabaseError::TenantSitePrefixQuotaExceeded { used: 1, limit: 1 }
        ));
        assert_eq!(count_tenant_managed(&pool, &"tenant-a".parse()?).await?, 1);
        assert_eq!(history(&pool, winner_id).await?.len(), 1);
        assert!(history(&pool, loser_id).await?.is_empty());

        Ok(())
    }

    #[crate::sqlx_test]
    async fn tenant_quota_counts_every_retained_lifecycle_state(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        create_tenant(&pool, "tenant-a").await?;
        create_tenant(&pool, "tenant-b").await?;

        let states = [
            SitePrefixLifecycleState::Provisioning,
            SitePrefixLifecycleState::Ready,
            SitePrefixLifecycleState::Error,
            SitePrefixLifecycleState::Deleting,
        ];
        for (index, state) in states.into_iter().enumerate() {
            let value = tenant_managed(&format!("10.0.{index}.0/24"), "tenant-a");
            let site_prefix = create(&pool, value, 4).await?.site_prefix;
            sqlx::query("UPDATE site_prefixes SET lifecycle_state = $1 WHERE id = $2")
                .bind(state)
                .bind(site_prefix.id)
                .execute(&pool)
                .await?;
        }

        assert_eq!(count_tenant_managed(&pool, &"tenant-a".parse()?).await?, 4);
        let error = create(&pool, tenant_managed("10.0.4.0/24", "tenant-a"), 4)
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            DatabaseError::TenantSitePrefixQuotaExceeded { used: 4, limit: 4 }
        ));

        // Quota and overlap are tenant-scoped; tenant B can reuse tenant A's
        // first root and starts with its own count.
        create(&pool, tenant_managed("10.0.0.0/24", "tenant-b"), 4).await?;
        assert_eq!(count_tenant_managed(&pool, &"tenant-b".parse()?).await?, 1);

        Ok(())
    }

    #[crate::sqlx_test]
    async fn metadata_update_and_retirement_preserve_immutable_identity(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        create_tenant(&pool, "tenant-a").await?;

        let create_value = tenant_managed("192.168.0.0/24", "tenant-a");
        let created = create(&pool, create_value.clone(), 8).await?.site_prefix;
        let updated_metadata = Metadata {
            name: "updated prefix".to_string(),
            description: "metadata update".to_string(),
            labels: HashMap::from([("env".to_string(), "test".to_string())]),
        };
        let update = UpdateSitePrefixMetadata {
            id: created.id,
            tenant_organization_id: "tenant-a".parse()?,
            metadata: updated_metadata.clone(),
            if_version_match: Some(created.version),
        };

        let mut txn = pool.begin().await?;
        let updated = update_tenant_metadata(&update, created.version, &mut txn).await?;
        txn.commit().await?;
        assert_eq!(updated.metadata, updated_metadata);
        assert_eq!(updated.config, created.config);
        assert_eq!(updated.status, created.status);
        assert_ne!(updated.version, created.version);
        assert_eq!(history(&pool, created.id).await?.len(), 1);

        let mut txn = pool.begin().await?;
        let stale_error = update_tenant_metadata(&update, created.version, &mut txn)
            .await
            .unwrap_err();
        assert!(matches!(
            stale_error,
            DatabaseError::ConcurrentModificationError("site prefix", _)
        ));
        txn.rollback().await?;

        let retry = create(&pool, create_value.clone(), 8).await?;
        assert_eq!(retry.disposition, CreateDisposition::Existing);
        assert_eq!(retry.site_prefix.metadata, updated_metadata);

        let retire = RetireTenantManagedSitePrefix {
            id: created.id,
            tenant_organization_id: "tenant-a".parse()?,
        };
        let mut txn = pool.begin().await?;
        let current = find_by_id_for_update(&mut txn, created.id).await?.unwrap();
        let deleting = retire_tenant_managed(&retire, &current, &mut txn).await?;
        txn.commit().await?;
        assert_eq!(
            deleting.status.lifecycle_state,
            SitePrefixLifecycleState::Deleting
        );
        assert_ne!(deleting.version, updated.version);

        let mut txn = pool.begin().await?;
        let current = find_by_id_for_update(&mut txn, created.id).await?.unwrap();
        let retry = retire_tenant_managed(&retire, &current, &mut txn).await?;
        txn.commit().await?;
        assert_eq!(retry.version, deleting.version);
        assert_eq!(count_tenant_managed(&pool, &"tenant-a".parse()?).await?, 1);

        // Retirement keeps the CIDR reserved even when the tenant has quota
        // for another resource.
        let overlap_error = create(&pool, tenant_managed("192.168.0.0/24", "tenant-a"), 8)
            .await
            .unwrap_err();
        assert!(matches!(
            overlap_error,
            DatabaseError::InvalidArgument(message)
                if message.contains("overlaps another site prefix")
        ));

        let history = history(&pool, created.id).await?;
        assert_eq!(history.len(), 2);
        let history_states = history
            .iter()
            .map(|record| serde_json::from_str(&record.state))
            .collect::<Result<Vec<SitePrefixLifecycleState>, _>>()?;
        assert_eq!(
            history_states,
            [
                SitePrefixLifecycleState::Provisioning,
                SitePrefixLifecycleState::Deleting
            ]
        );

        // Retirement keeps the row and its identity reserved. A repeated
        // create therefore returns the current Deleting representation.
        let retry = create(&pool, create_value, 0).await?;
        assert_eq!(retry.disposition, CreateDisposition::Existing);
        assert_eq!(retry.site_prefix, deleting);

        Ok(())
    }

    #[crate::sqlx_test]
    async fn tenant_overlap_constraint_uses_public_btree_gist(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let extension_schema: String = sqlx::query_scalar(
            r#"
                SELECT namespace.nspname
                FROM pg_extension extension
                JOIN pg_namespace namespace
                    ON namespace.oid = extension.extnamespace
                WHERE extension.extname = 'btree_gist'
            "#,
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(extension_schema, "public");

        let constraint_definition: String = sqlx::query_scalar(
            r#"
                SELECT pg_get_constraintdef(oid)
                FROM pg_constraint
                WHERE conname = 'site_prefixes_tenant_prefix_excl'
            "#,
        )
        .fetch_one(&pool)
        .await?;
        assert!(constraint_definition.contains("EXCLUDE USING gist"));
        assert!(constraint_definition.contains("tenant_organization_id WITH ="));
        assert!(constraint_definition.contains("prefix inet_ops WITH &&"));
        assert!(constraint_definition.contains("authority = 'tenant_managed'"));

        Ok(())
    }

    #[crate::sqlx_test]
    async fn legacy_attachment_returns_every_operator_managed_candidate(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let broad_prefix: IpNetwork = "172.16.0.0/12".parse()?;
        let specific_prefix: IpNetwork = "172.16.0.0/16".parse()?;
        let vpc_prefix: IpNetwork = "172.16.1.0/24".parse()?;

        let mut txn = pool.begin().await?;
        reconcile_configured(&mut txn, &[broad_prefix, specific_prefix]).await?;
        let specific_id: SitePrefixId =
            sqlx::query_scalar("SELECT id FROM site_prefixes WHERE prefix = $1")
                .bind(specific_prefix)
                .fetch_one(&mut *txn)
                .await?;
        sqlx::query("UPDATE site_prefixes SET lifecycle_state = $1 WHERE id = $2")
            .bind(SitePrefixLifecycleState::Deleting)
            .bind(specific_id)
            .execute(&mut *txn)
            .await?;

        let candidates =
            find_legacy_operator_managed_for_vpc_prefix_attachment(&mut txn, vpc_prefix).await?;
        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].config.prefix, specific_prefix);
        assert_eq!(candidates[1].config.prefix, broad_prefix);
        assert_eq!(
            candidates[0].status.lifecycle_state,
            SitePrefixLifecycleState::Deleting
        );

        txn.commit().await?;
        Ok(())
    }

    /// Proves startup lineage repair assigns only a unique historical root,
    /// while preserving the distinct missing and ambiguous policy outcomes.
    #[crate::sqlx_test]
    async fn lineage_backfill_assigns_only_one_unambiguous_operator_parent(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let unique_root: IpNetwork = "10.0.0.0/8".parse()?;
        let ambiguous_broad_root: IpNetwork = "172.16.0.0/12".parse()?;
        let ambiguous_specific_root: IpNetwork = "172.16.0.0/16".parse()?;
        let unique_prefix: IpNetwork = "10.1.0.0/24".parse()?;
        let ambiguous_prefix: IpNetwork = "172.16.1.0/24".parse()?;
        let missing_prefix: IpNetwork = "192.0.2.0/24".parse()?;

        let mut txn = pool.begin().await?;
        reconcile_configured(
            &mut txn,
            &[unique_root, ambiguous_broad_root, ambiguous_specific_root],
        )
        .await?;

        let operator_roots: Vec<(SitePrefixId, IpNetwork)> =
            sqlx::query_as("SELECT id, prefix FROM site_prefixes ORDER BY id")
                .fetch_all(&mut *txn)
                .await?;
        let unique_root_id = operator_roots
            .iter()
            .find_map(|(id, prefix)| (*prefix == unique_root).then_some(*id))
            .unwrap();
        let mut ambiguous_root_ids: Vec<SitePrefixId> = operator_roots
            .iter()
            .filter_map(|(id, prefix)| {
                [ambiguous_broad_root, ambiguous_specific_root]
                    .contains(prefix)
                    .then_some(*id)
            })
            .collect();
        ambiguous_root_ids.sort();

        // A retiring operator root remains a valid parent for historical
        // lineage. New attachment is rejected separately by Core.
        sqlx::query("UPDATE site_prefixes SET lifecycle_state = $1 WHERE id = $2")
            .bind(SitePrefixLifecycleState::Deleting)
            .bind(unique_root_id)
            .execute(&mut *txn)
            .await?;

        let vpc_id = carbide_uuid::vpc::VpcId::new();
        sqlx::query(
            "INSERT INTO vpcs (id, name, organization_id, version) VALUES ($1, $2, $3, $4)",
        )
        .bind(vpc_id)
        .bind("site-prefix-lineage-backfill")
        .bind("tenant-a")
        .bind(ConfigVersion::initial())
        .execute(&mut *txn)
        .await?;

        let unique_vpc_prefix_id = VpcPrefixId::new();
        let ambiguous_vpc_prefix_id = VpcPrefixId::new();
        let missing_vpc_prefix_id = VpcPrefixId::new();
        for (id, prefix, name) in [
            (unique_vpc_prefix_id, unique_prefix, "unique"),
            (ambiguous_vpc_prefix_id, ambiguous_prefix, "ambiguous"),
            (missing_vpc_prefix_id, missing_prefix, "missing"),
        ] {
            sqlx::query(
                "INSERT INTO network_vpc_prefixes (id, prefix, name, vpc_id) VALUES ($1, $2, $3, $4)",
            )
            .bind(id)
            .bind(prefix)
            .bind(name)
            .bind(vpc_id)
            .execute(&mut *txn)
            .await?;
        }

        // Before repair, the broad audit sees every unassigned prefix, while
        // the root-aware audit excludes the unrelated rootless prefix.
        let mut initially_unassigned =
            find_unassigned_vpc_prefix_site_prefix_ids(&mut *txn).await?;
        initially_unassigned.sort();
        let mut expected_initially_unassigned = vec![
            unique_vpc_prefix_id,
            ambiguous_vpc_prefix_id,
            missing_vpc_prefix_id,
        ];
        expected_initially_unassigned.sort();
        assert_eq!(initially_unassigned, expected_initially_unassigned);
        let mut initially_with_parent_candidates =
            find_unassigned_vpc_prefix_ids_with_operator_parent_candidates(&mut *txn).await?;
        initially_with_parent_candidates.sort();
        let mut expected_with_parent_candidates =
            vec![unique_vpc_prefix_id, ambiguous_vpc_prefix_id];
        expected_with_parent_candidates.sort();
        assert_eq!(
            initially_with_parent_candidates,
            expected_with_parent_candidates
        );

        // Repair assigns the unique parent but leaves missing and ambiguous
        // lineage distinguishable for startup policy enforcement.
        let report = backfill_vpc_prefix_site_prefix_lineage(&mut txn).await?;
        assert_eq!(report.assigned_vpc_prefix_ids, vec![unique_vpc_prefix_id]);
        assert_eq!(report.missing_vpc_prefix_ids, vec![missing_vpc_prefix_id]);
        assert_eq!(
            report.ambiguous,
            vec![VpcPrefixSitePrefixLineageAmbiguity {
                vpc_prefix_id: ambiguous_vpc_prefix_id,
                candidate_site_prefix_ids: ambiguous_root_ids.clone(),
            }]
        );
        assert!(
            !report.is_safe_for_startup(false),
            "ambiguous lineage must block startup even without configured roots"
        );

        // Missing lineage alone is accepted only when the configured operator
        // boundary is disabled; nonempty configuration remains strict.
        let missing_only_report = VpcPrefixSitePrefixLineageReport {
            missing_vpc_prefix_ids: vec![missing_vpc_prefix_id],
            ..Default::default()
        };
        assert!(missing_only_report.is_safe_for_startup(false));
        assert!(!missing_only_report.is_safe_for_startup(true));

        // Verify the unique relationship was persisted and unresolved rows
        // were not assigned by guessing.
        let assignments: Vec<(VpcPrefixId, Option<SitePrefixId>)> =
            sqlx::query_as("SELECT id, site_prefix_id FROM network_vpc_prefixes ORDER BY id")
                .fetch_all(&mut *txn)
                .await?;
        assert!(assignments.contains(&(unique_vpc_prefix_id, Some(unique_root_id))));
        assert!(assignments.contains(&(ambiguous_vpc_prefix_id, None)));
        assert!(assignments.contains(&(missing_vpc_prefix_id, None)));

        let mut still_unassigned = find_unassigned_vpc_prefix_site_prefix_ids(&mut *txn).await?;
        still_unassigned.sort();
        let mut expected_still_unassigned = vec![ambiguous_vpc_prefix_id, missing_vpc_prefix_id];
        expected_still_unassigned.sort();
        assert_eq!(still_unassigned, expected_still_unassigned);
        assert_eq!(
            find_unassigned_vpc_prefix_ids_with_operator_parent_candidates(&mut *txn).await?,
            vec![ambiguous_vpc_prefix_id],
            "listen-only startup must ignore only the unrelated rootless prefix"
        );

        // Re-running repair remains idempotent and reports the same blockers.
        let repeated_report = backfill_vpc_prefix_site_prefix_lineage(&mut txn).await?;
        assert!(repeated_report.assigned_vpc_prefix_ids.is_empty());
        assert_eq!(
            repeated_report.missing_vpc_prefix_ids,
            vec![missing_vpc_prefix_id]
        );
        assert_eq!(
            repeated_report.ambiguous,
            vec![VpcPrefixSitePrefixLineageAmbiguity {
                vpc_prefix_id: ambiguous_vpc_prefix_id,
                candidate_site_prefix_ids: ambiguous_root_ids,
            }]
        );

        txn.commit().await?;
        Ok(())
    }

    /// Proves ambiguous predecessor lineage retains every containing operator
    /// root, because choosing one nested root could withdraw required isolation.
    #[crate::sqlx_test]
    async fn unassigned_vpc_prefix_retains_all_containing_operator_roots(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        create_tenant(&pool, "tenant-a").await?;

        let broad_root: IpNetwork = "10.0.0.0/8".parse()?;
        let specific_root: IpNetwork = "10.1.0.0/16".parse()?;
        let unrelated_root: IpNetwork = "192.0.2.0/24".parse()?;
        let mut txn = pool.begin().await?;

        // Reconstruct persisted predecessor roots after the target config has
        // removed its final root.
        reconcile_configured(&mut txn, &[broad_root, specific_root, unrelated_root]).await?;
        reconcile_configured(&mut txn, &[]).await?;
        assert!(operator_managed_prefixes_exist(&mut *txn).await?);

        let vpc_id = carbide_uuid::vpc::VpcId::new();
        sqlx::query(
            "INSERT INTO vpcs (id, name, organization_id, version) VALUES ($1, $2, $3, $4)",
        )
        .bind(vpc_id)
        .bind("ambiguous retained lineage")
        .bind("tenant-a")
        .bind(ConfigVersion::initial())
        .execute(&mut *txn)
        .await?;
        let vpc_prefix_id = VpcPrefixId::new();
        sqlx::query(
            "INSERT INTO network_vpc_prefixes (id, prefix, name, vpc_id) VALUES ($1, $2, $3, $4)",
        )
        .bind(vpc_prefix_id)
        .bind("10.1.2.0/24".parse::<IpNetwork>()?)
        .bind("ambiguous predecessor child")
        .bind(vpc_id)
        .execute(&mut *txn)
        .await?;

        // Both containing roots remain fail-closed; an unrelated root does not.
        let mut retained =
            find_operator_managed_prefixes_with_retained_vpc_prefixes(&mut *txn).await?;
        retained.sort_by_cached_key(ToString::to_string);
        let mut expected = vec![broad_root, specific_root];
        expected.sort_by_cached_key(ToString::to_string);
        assert_eq!(retained, expected);

        // Hard deletion ends route retention, while persisted deleting roots
        // continue to require startup lineage preflight.
        sqlx::query("DELETE FROM network_vpc_prefixes WHERE id = $1")
            .bind(vpc_prefix_id)
            .execute(&mut *txn)
            .await?;
        assert!(
            find_operator_managed_prefixes_with_retained_vpc_prefixes(&mut *txn)
                .await?
                .is_empty()
        );
        assert!(operator_managed_prefixes_exist(&mut *txn).await?);

        txn.commit().await?;
        Ok(())
    }

    /// Retains operator coverage through child soft deletion, while exact tenant
    /// lineage keeps a containing operator root from claiming unrelated children.
    #[crate::sqlx_test]
    async fn operator_roots_remain_visible_until_vpc_prefixes_are_hard_deleted(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // Keep one operator root ready and retire another while a tenant owns nested space.
        create_tenant(&pool, "tenant-a").await?;

        let ready_root: IpNetwork = "10.0.0.0/8".parse()?;
        let retiring_root: IpNetwork = "172.16.0.0/12".parse()?;
        let operator_root_covering_tenant_child: IpNetwork = "192.168.0.0/15".parse()?;
        let mut txn = pool.begin().await?;
        reconcile_configured(
            &mut txn,
            &[
                ready_root,
                retiring_root,
                operator_root_covering_tenant_child,
            ],
        )
        .await?;
        reconcile_configured(&mut txn, &[ready_root, operator_root_covering_tenant_child]).await?;
        let tenant_root =
            create_tenant_managed(tenant_managed("192.168.0.0/16", "tenant-a"), 8, &mut txn)
                .await?
                .site_prefix;
        sqlx::query("UPDATE site_prefixes SET lifecycle_state = $1 WHERE id = $2")
            .bind(SitePrefixLifecycleState::Ready)
            .bind(tenant_root.id)
            .execute(&mut *txn)
            .await?;

        let roots: Vec<(SitePrefixId, IpNetwork)> =
            sqlx::query_as("SELECT id, prefix FROM site_prefixes ORDER BY prefix")
                .fetch_all(&mut *txn)
                .await?;
        let root_id = |prefix| {
            roots
                .iter()
                .find_map(|(id, stored)| (*stored == prefix).then_some(*id))
                .unwrap()
        };

        let vpc_id = carbide_uuid::vpc::VpcId::new();
        // Persist explicit parent links so containment cannot substitute for ownership.
        sqlx::query(
            "INSERT INTO vpcs (id, name, organization_id, version) VALUES ($1, $2, $3, $4)",
        )
        .bind(vpc_id)
        .bind("operator-root-retention")
        .bind("tenant-a")
        .bind(ConfigVersion::initial())
        .execute(&mut *txn)
        .await?;

        let ready_child_id = VpcPrefixId::new();
        let retiring_child_id = VpcPrefixId::new();
        let tenant_child_id = VpcPrefixId::new();
        for (id, prefix, site_prefix_id) in [
            // A ready operator root still needs isolation for its own child.
            (ready_child_id, "10.1.0.0/24", root_id(ready_root)),
            // Retirement cannot withdraw coverage while its child still exists.
            (retiring_child_id, "172.16.1.0/24", root_id(retiring_root)),
            // A tenant-owned child must not retain a geometrically containing operator root.
            (tenant_child_id, "192.168.1.0/24", tenant_root.id),
        ] {
            sqlx::query(
                "INSERT INTO network_vpc_prefixes (id, prefix, name, vpc_id, site_prefix_id) \
                 VALUES ($1, $2, $3, $4, $5)",
            )
            .bind(id)
            .bind(prefix.parse::<IpNetwork>()?)
            .bind(prefix)
            .bind(vpc_id)
            .bind(site_prefix_id)
            .execute(&mut *txn)
            .await?;
        }

        // Only exact operator ownership should contribute roots at this point.
        let mut retained =
            find_operator_managed_prefixes_with_retained_vpc_prefixes(&mut *txn).await?;
        retained.sort_by_cached_key(ToString::to_string);
        let mut expected = vec![ready_root, retiring_root];
        expected.sort_by_cached_key(ToString::to_string);
        assert_eq!(
            retained, expected,
            "exact tenant-managed lineage must not geometrically retain a covering operator root"
        );

        // Soft deletion leaves the row available to generated segments that are draining.
        sqlx::query("UPDATE network_vpc_prefixes SET deleted = now() WHERE id = $1")
            .bind(retiring_child_id)
            .execute(&mut *txn)
            .await?;
        let mut retained =
            find_operator_managed_prefixes_with_retained_vpc_prefixes(&mut *txn).await?;
        retained.sort_by_cached_key(ToString::to_string);
        assert_eq!(
            retained, expected,
            "soft deletion must retain isolation while generated segments can still reference the child"
        );

        // Physical deletion is the point where the retired root may disappear.
        sqlx::query("DELETE FROM network_vpc_prefixes WHERE id = $1")
            .bind(retiring_child_id)
            .execute(&mut *txn)
            .await?;
        assert_eq!(
            find_operator_managed_prefixes_with_retained_vpc_prefixes(&mut *txn).await?,
            vec![ready_root]
        );

        txn.commit().await?;
        Ok(())
    }
}

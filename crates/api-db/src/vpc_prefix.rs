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

use std::collections::HashMap;

use carbide_network::ip::IdentifyAddressFamily;
pub use carbide_uuid::vpc::{VpcId, VpcPrefixId};
use config_version::ConfigVersion;
use ipnetwork::IpNetwork;
use model::DeletedFilter;
use model::controller_outcome::PersistentStateHandlerOutcome;
use model::network_prefix::NetworkPrefix;
use model::network_segment::NetworkSegmentType;
use model::site_prefix::SitePrefixAuthority;
use model::vpc_prefix::{
    DeleteVpcPrefix, NewVpcPrefix, UpdateVpcPrefix, VpcPrefix, VpcPrefixControllerState,
    VpcPrefixSearch,
};
use sqlx::{FromRow, PgConnection, QueryBuilder, Row};

use super::{ColumnInfo, DatabaseError, ObjectColumnFilter};
use crate::vpc::increment_vpc_version;

async fn network_prefix_occupancy_by_vpc_prefix_id(
    vpc_prefix_ids: &[VpcPrefixId],
    txn: &mut PgConnection,
) -> Result<HashMap<VpcPrefixId, Vec<IpNetwork>>, DatabaseError> {
    if vpc_prefix_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let query = r#"
        SELECT vpc_prefix.id, network_prefix.prefix
        FROM network_vpc_prefixes AS vpc_prefix
        INNER JOIN network_prefixes AS network_prefix
            ON network_prefix.prefix && vpc_prefix.prefix
        WHERE vpc_prefix.id = ANY($1)
          AND (
              network_prefix.vpc_prefix_id = vpc_prefix.id
              OR network_prefix.vpc_prefix_id IS NULL
          )
        ORDER BY vpc_prefix.id, network_prefix.id
    "#;
    let occupied_prefixes: Vec<(VpcPrefixId, IpNetwork)> = sqlx::query_as(query)
        .bind(vpc_prefix_ids)
        .fetch_all(&mut *txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;

    Ok(occupied_prefixes.into_iter().fold(
        HashMap::<VpcPrefixId, Vec<IpNetwork>>::new(),
        |mut occupancy, (vpc_prefix_id, prefix)| {
            occupancy.entry(vpc_prefix_id).or_default().push(prefix);
            occupancy
        },
    ))
}

/// Returns whether SLAAC is enabled for each requested VPC.
///
/// VPCs marked for deletion are included so existing prefix allocations keep
/// the policy that was in effect when they were created.
async fn slaac_enabled_by_vpc_id(
    vpc_ids: &[VpcId],
    txn: &mut PgConnection,
) -> Result<HashMap<VpcId, bool>, DatabaseError> {
    if vpc_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let query = "SELECT id, slaac_enabled FROM vpcs WHERE id = ANY($1)";
    let vpc_modes: Vec<(VpcId, bool)> = sqlx::query_as(query)
        .bind(vpc_ids)
        .fetch_all(txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;

    Ok(vpc_modes.into_iter().collect())
}

async fn update_stats(
    prefixes: &mut [VpcPrefix],
    txn: &mut PgConnection,
) -> Result<(), DatabaseError> {
    let vpc_prefix_ids: Vec<VpcPrefixId> = prefixes.iter().map(|prefix| prefix.id).collect();
    let vpc_ids: Vec<VpcId> = prefixes.iter().map(|prefix| prefix.vpc_id).collect();
    let network_prefix_occupancy =
        network_prefix_occupancy_by_vpc_prefix_id(&vpc_prefix_ids, txn).await?;
    let slaac_enabled_by_vpc = slaac_enabled_by_vpc_id(&vpc_ids, txn).await?;

    for vpc_prefix in prefixes {
        let occupied_prefixes = network_prefix_occupancy
            .get(&vpc_prefix.id)
            .map(Vec::as_slice)
            .unwrap_or_default();

        let slaac_enabled = slaac_enabled_by_vpc
            .get(&vpc_prefix.vpc_id)
            .copied()
            .ok_or_else(|| DatabaseError::Internal {
                message: format!(
                    "VPC prefix {} references missing VPC {}",
                    vpc_prefix.id, vpc_prefix.vpc_id
                ),
            })?;
        let allocation_prefix_len = model::vpc::instance_prefix_len(
            vpc_prefix.config.prefix.address_family(),
            slaac_enabled,
        );
        let vpc_prefix_len = vpc_prefix.config.prefix.prefix();
        let total = if model::vpc::vpc_prefix_can_allocate_interface_prefix(
            vpc_prefix.config.prefix.address_family(),
            vpc_prefix_len,
            allocation_prefix_len,
        ) {
            1u128 << u32::from(allocation_prefix_len - vpc_prefix_len)
        } else {
            0
        };
        let occupied = crate::network_prefix::occupied_prefix_count(
            vpc_prefix.config.prefix,
            allocation_prefix_len,
            occupied_prefixes.iter().copied(),
        );
        let available = total.saturating_sub(occupied);

        // Legacy IPv4-only stats (kept for backwards compatibility).
        if vpc_prefix.config.prefix.is_ipv4() {
            vpc_prefix.status.total_31_segments = u32::try_from(total).unwrap_or(u32::MAX);
            vpc_prefix.status.available_31_segments = u32::try_from(available).unwrap_or(u32::MAX);
        }

        // Capacity follows both the address family and the VPC mode: /31 for
        // IPv4, /64 for SLAAC IPv6, and /127 for stateful IPv6. Compute it
        // without enumerating prefixes. Results larger than the protobuf's u64
        // fields are capped at u64::MAX.
        vpc_prefix.status.total_linknet_segments = u64::try_from(total).unwrap_or(u64::MAX);
        vpc_prefix.status.available_linknet_segments = u64::try_from(available).unwrap_or(u64::MAX);
    }

    Ok(())
}

// Get a list of prefixes matching a filter on the ID column.
pub async fn get_by_id<'a, C>(
    txn: &mut PgConnection,
    filter: ObjectColumnFilter<'a, C>,
    deleted_filter: DeletedFilter,
) -> Result<Vec<VpcPrefix>, DatabaseError>
where
    C: ColumnInfo<'a, TableType = VpcPrefix>,
{
    let mut query =
        super::FilterableQueryBuilder::new("SELECT * FROM network_vpc_prefixes").filter(&filter);
    match deleted_filter {
        DeletedFilter::Exclude => {
            query.push(" AND deleted IS NULL");
        }
        DeletedFilter::Only => {
            query.push(" AND deleted IS NOT NULL");
        }
        DeletedFilter::Include => {}
    }
    let mut container = query
        .build_query_as()
        .fetch_all(&mut *txn)
        .await
        .map_err(|e| DatabaseError::query(query.sql(), e))?;

    update_stats(&mut container, txn).await?;
    Ok(container)
}

/// Loads explicit VPC-prefix selections for allocation validation.
///
/// Deleted rows are deliberately included so the caller can distinguish an
/// unknown prefix from one that became unavailable through soft deletion.
/// This discovery query does not lock rows; allocation locks one candidate at
/// a time through [`lock_for_allocation`].
pub async fn get_for_allocation_by_ids(
    txn: &mut PgConnection,
    vpc_prefix_ids: &[VpcPrefixId],
) -> Result<Vec<VpcPrefix>, DatabaseError> {
    let query = r#"
        SELECT *
        FROM network_vpc_prefixes
        -- Omit a deletion predicate so allocation validation can distinguish
        -- deleted prefixes from unknown IDs.
        WHERE id = ANY($1)
        ORDER BY id
    "#;
    sqlx::query_as(query)
        .bind(vpc_prefix_ids)
        .fetch_all(&mut *txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))
}

/// Loads active VPC prefixes for automatic allocation planning.
///
/// Rows are returned in stable VPC/ID order so caller grouping preserves
/// ascending IDs within each `(vpc_id, family)` lock group. Callers must freeze
/// this result rather than re-ranking it using mutable capacity statistics.
/// Prefix length eligibility depends on the owning VPC's allocation mode and
/// must be applied by the caller after loading that configuration.
pub async fn find_allocation_candidates(
    txn: &mut PgConnection,
    vpc_ids: &[VpcId],
) -> Result<Vec<VpcPrefix>, DatabaseError> {
    let query = r#"
        SELECT *
        FROM network_vpc_prefixes
        WHERE vpc_id = ANY($1)
          -- Soft-deleted prefixes are not eligible automatic candidates.
          AND deleted IS NULL
        -- Preserve ascending candidate IDs within each VPC/family lock group.
        ORDER BY vpc_id, id
    "#;
    sqlx::query_as(query)
        .bind(vpc_ids)
        .fetch_all(&mut *txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))
}

/// Locks and re-reads one allocation candidate.
///
/// Returning `None` means the prefix was deleted after candidate discovery.
/// This helper does not open a transaction. Callers that need the row lock
/// beyond this query must invoke it within one; releasing a nested savepoint
/// retains the lock until the containing transaction ends.
pub async fn lock_for_allocation(
    txn: &mut PgConnection,
    vpc_prefix_id: VpcPrefixId,
) -> Result<Option<VpcPrefix>, DatabaseError> {
    let query = r#"
        SELECT *
        FROM network_vpc_prefixes
        WHERE id = $1
          -- Deletion can race discovery, so re-check it while taking the row lock.
          AND deleted IS NULL
        -- Serialize allocations that share this candidate's persisted cursor.
        FOR NO KEY UPDATE
    "#;
    sqlx::query_as(query)
        .bind(vpc_prefix_id)
        .fetch_optional(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))
}

// Find the prefixes associated with a VPC.
pub async fn find_by_vpc(
    txn: &mut PgConnection,
    vpc_id: VpcId,
) -> Result<Vec<VpcPrefix>, DatabaseError> {
    let query = "SELECT * FROM network_vpc_prefixes WHERE vpc_id=$1 \
            AND deleted IS NULL \
            ORDER BY prefix";
    let mut container = sqlx::query_as(query)
        .bind(vpc_id)
        .fetch_all(&mut *txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    update_stats(&mut container, txn).await?;
    Ok(container)
}

// Find all prefixes associated with any VPC in the list.
pub async fn find_by_vpcs(
    txn: &mut PgConnection,
    vpc_ids: &Vec<VpcId>,
) -> Result<Vec<VpcPrefix>, DatabaseError> {
    let query = "SELECT * FROM network_vpc_prefixes WHERE vpc_id=ANY($1) \
                AND deleted IS NULL \
                ORDER BY prefix";
    sqlx::query_as(query)
        .bind(vpc_ids)
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))
}

// Update last used prefix.
pub async fn update_last_used_prefix(
    txn: &mut PgConnection,
    vpc_prefix_id: &VpcPrefixId,
    last_used_prefix: IpNetwork,
) -> Result<(), DatabaseError> {
    let query = "UPDATE network_vpc_prefixes SET last_used_prefix=$1 WHERE id=$2 AND deleted IS NULL RETURNING *";
    sqlx::query_as::<_, VpcPrefix>(query)
        .bind(last_used_prefix)
        .bind(vpc_prefix_id)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(())
}

// Search for VPC prefixes by the VPC they're in, name, or a match against
// the prefix (or some combination). Returns just the IDs.
pub async fn search(
    txn: &mut PgConnection,
    search: VpcPrefixSearch,
) -> Result<Vec<VpcPrefixId>, DatabaseError> {
    let VpcPrefixSearch {
        vpc_id,
        site_prefix_id,
        name,
        prefix_match,
        deleted_filter,
    } = search;

    let mut query = QueryBuilder::new("SELECT id FROM network_vpc_prefixes WHERE true");

    match deleted_filter {
        DeletedFilter::Exclude => {
            query.push(" AND deleted IS NULL");
        }
        DeletedFilter::Only => {
            query.push(" AND deleted IS NOT NULL");
        }
        DeletedFilter::Include => {}
    }

    if let Some(vpc_id) = vpc_id {
        query.push(" AND vpc_id=");
        query.push_bind(vpc_id);
    }

    if let Some(site_prefix_id) = site_prefix_id {
        query.push(" AND site_prefix_id=");
        query.push_bind(site_prefix_id);
    }

    if let Some(name) = name {
        query.push(" AND name=");
        query.push_bind(name);
    }

    if let Some(prefix_match) = prefix_match {
        use model::vpc_prefix::PrefixMatch::*;
        match prefix_match {
            Exact(prefix) => {
                query.push(" AND prefix=");
                query.push_bind(prefix);
            }
            Contains(prefix) => {
                query.push(" AND prefix>>=");
                query.push_bind(prefix);
            }
            ContainedBy(prefix) => {
                query.push(" AND prefix<<=");
                query.push_bind(prefix);
            }
        }
    }

    query
        .build_query_as()
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(query.sql(), e))
}

#[derive(Clone, Copy)]
pub struct IdColumn;
impl ColumnInfo<'_> for IdColumn {
    type TableType = VpcPrefix;
    type ColumnType = VpcPrefixId;

    fn column_name(&self) -> &'static str {
        "id"
    }
}

pub async fn persist(
    value: NewVpcPrefix,
    expected_vpc_version: ConfigVersion,
    txn: &mut PgConnection,
) -> Result<VpcPrefix, DatabaseError> {
    let initial_version = ConfigVersion::initial();
    let initial_state = VpcPrefixControllerState::Provisioning;

    let insert_query = "INSERT INTO network_vpc_prefixes (
                id,
                prefix,
                name,
                labels,
                description,
                vpc_id,
                site_prefix_id,
                controller_state,
                controller_state_version)
            VALUES ($1, $2, $3, $4::json, $5, $6, $7, $8::json, $9)
            RETURNING *";
    let vpc_prefix: VpcPrefix = match sqlx::query_as(insert_query)
        .bind(value.id)
        .bind(value.config.prefix)
        .bind(&value.metadata.name)
        .bind(sqlx::types::Json(&value.metadata.labels))
        .bind(&value.metadata.description)
        .bind(value.vpc_id)
        .bind(value.site_prefix_id)
        .bind(sqlx::types::Json(&initial_state))
        .bind(initial_version)
        .fetch_one(&mut *txn)
        .await
    {
        Ok(vpc_prefix) => vpc_prefix,
        Err(sqlx::Error::Database(error))
            if error.constraint() == Some("network_vpc_prefixes_globally_unique") =>
        {
            return Err(DatabaseError::InvalidArgument(format!(
                "The requested VPC prefix ({}) overlaps an existing or deleting VPC prefix",
                value.config.prefix
            )));
        }
        Err(e) => return Err(DatabaseError::query(insert_query, e)),
    };

    crate::state_history::persist(
        txn,
        crate::state_history::StateHistoryTableId::VpcPrefix,
        &vpc_prefix.id,
        &initial_state,
        initial_version,
    )
    .await?;

    increment_vpc_version(txn, value.vpc_id, expected_vpc_version).await?;

    Ok(vpc_prefix)
}

/// Checks for existing or deleting VPC prefixes using any of the address space.
pub async fn probe(
    network: IpNetwork,
    txn: &mut PgConnection,
) -> Result<Vec<VpcPrefix>, DatabaseError> {
    // Include soft-deleted rows because the global exclusion constraint still reserves them.
    let query = "SELECT * FROM network_vpc_prefixes WHERE prefix && $1";
    sqlx::query_as(query)
        .bind(network)
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))
}

/// `AttachedSegmentPrefix` identifies a `NetworkPrefix` linked directly to a
/// `NetworkSegment` that has a VPC.
#[derive(Debug)]
pub struct AttachedSegmentPrefix {
    /// The `NetworkSegment` belongs to this VPC.
    pub vpc_id: VpcId,
    /// `VpcPrefix` creation uses this `NetworkSegment` type to decide whether
    /// it may adopt the prefix.
    pub segment_type: NetworkSegmentType,
    /// This is the `NetworkPrefix` stored directly on the `NetworkSegment`.
    pub prefix: NetworkPrefix,
}

/// `probe_segment_prefixes` finds direct `NetworkPrefix` records on attached
/// `NetworkSegment` records that overlap `network`.
///
/// Soft-deleted segments remain visible because their `NetworkPrefix` rows stay
/// in the database until final deletion. A `NetworkPrefix` linked to a
/// `VpcPrefix` is checked through that `VpcPrefix` instead, so this query does
/// not return it.
pub async fn probe_segment_prefixes(
    network: IpNetwork,
    txn: &mut PgConnection,
) -> Result<Vec<AttachedSegmentPrefix>, DatabaseError> {
    let query = "SELECT ns.vpc_id AS vpc_id, ns.network_segment_type, np.* \
            FROM network_prefixes np \
            INNER JOIN network_segments ns ON np.segment_id = ns.id \
            WHERE np.prefix && $1 \
              AND ns.vpc_id IS NOT NULL \
              AND np.vpc_prefix_id IS NULL";

    sqlx::query(query)
        .bind(network)
        .try_map(|row| {
            Ok(AttachedSegmentPrefix {
                vpc_id: row.try_get("vpc_id")?,
                segment_type: row.try_get("network_segment_type")?,
                prefix: NetworkPrefix::from_row(&row)?,
            })
        })
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))
}

pub async fn update(
    update: &UpdateVpcPrefix,
    txn: &mut PgConnection,
) -> Result<VpcPrefix, DatabaseError> {
    let query = "UPDATE network_vpc_prefixes SET name=$1, labels=$2::json, description=$3 WHERE id=$4 AND deleted IS NULL RETURNING *";
    sqlx::query_as(query)
        .bind(&update.metadata.name)
        .bind(sqlx::types::Json(&update.metadata.labels))
        .bind(&update.metadata.description)
        .bind(update.id)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))
    // Note that if/when we add support for prefix resizing, we will need to
    // call increment_vpc_version() here.
}

/// Marks an active VPC prefix for asynchronous deletion and bumps the parent VPC version.
pub async fn mark_as_deleted(
    value: &DeleteVpcPrefix,
    expected_vpc_version: ConfigVersion,
    txn: &mut PgConnection,
) -> Result<VpcPrefixId, DatabaseError> {
    // Mark the prefix deleted while keeping its address space reserved for the controller.
    let query =
        "UPDATE network_vpc_prefixes SET deleted=NOW() WHERE id=$1 AND deleted IS NULL RETURNING *";
    let deleted_prefix: VpcPrefix = sqlx::query_as(query)
        .bind(value.id)
        .fetch_one(&mut *txn)
        .await
        .map_err(|e| match e {
            sqlx::Error::RowNotFound => DatabaseError::NotFoundError {
                kind: "vpc_prefix",
                id: value.id.to_string(),
            },
            e => DatabaseError::query(query, e),
        })?;

    increment_vpc_version(txn, deleted_prefix.vpc_id, expected_vpc_version).await?;

    Ok(deleted_prefix.id)
}

/// Hard-deletes a VPC prefix after the controller has drained all dependencies.
pub async fn final_delete(
    vpc_prefix_id: VpcPrefixId,
    txn: &mut PgConnection,
) -> Result<VpcPrefixId, DatabaseError> {
    // Remove the terminal row without bumping the parent VPC version again.
    let query = "DELETE FROM network_vpc_prefixes WHERE id=$1 RETURNING id";
    let deleted_id = sqlx::query_as::<_, VpcPrefixId>(query)
        .bind(vpc_prefix_id)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(deleted_id)
}

/// Updates the controller-owned VPC prefix state if the version still matches.
pub async fn try_update_controller_state(
    txn: &mut PgConnection,
    vpc_prefix_id: VpcPrefixId,
    expected_version: ConfigVersion,
    new_version: ConfigVersion,
    new_state: &VpcPrefixControllerState,
) -> Result<bool, DatabaseError> {
    // Use optimistic locking so concurrent controller attempts cannot overwrite each other.
    let query = "UPDATE network_vpc_prefixes SET controller_state_version=$1, controller_state=$2::json WHERE id=$3 AND controller_state_version=$4 RETURNING id";
    let result = sqlx::query_as::<_, VpcPrefixId>(query)
        .bind(new_version)
        .bind(sqlx::types::Json(new_state))
        .bind(vpc_prefix_id)
        .bind(expected_version)
        .fetch_optional(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(result.is_some())
}

/// Stores the result of the most recent VPC prefix controller handling attempt.
pub async fn update_controller_state_outcome(
    txn: &mut PgConnection,
    vpc_prefix_id: VpcPrefixId,
    outcome: PersistentStateHandlerOutcome,
) -> Result<(), DatabaseError> {
    // Persist the outcome separately from state so waits/errors remain visible.
    let query = "UPDATE network_vpc_prefixes SET controller_state_outcome=$1::json WHERE id=$2";
    sqlx::query(query)
        .bind(sqlx::types::Json(outcome))
        .bind(vpc_prefix_id)
        .execute(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    Ok(())
}

/// Counts network-prefix rows that still reference a VPC prefix.
pub async fn count_network_prefixes_by_vpc_prefix_id(
    txn: &mut PgConnection,
    vpc_prefix_id: &VpcPrefixId,
) -> Result<usize, DatabaseError> {
    // The controller uses this durable dependency as the deletion drain gate.
    let query = "SELECT count(*) FROM network_prefixes WHERE vpc_prefix_id=$1";
    let (network_prefix_count,): (i64,) = sqlx::query_as(query)
        .bind(vpc_prefix_id)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(network_prefix_count.max(0) as usize)
}

/// Reports whether one VPC retains address space from a tenant-managed root.
///
/// Soft-deleted VpcPrefixes remain relevant until their controller completes
/// physical deletion, so this check deliberately includes them.
pub async fn has_tenant_managed_site_prefix(
    txn: &mut PgConnection,
    vpc_id: VpcId,
) -> Result<bool, DatabaseError> {
    let query = r#"
        SELECT EXISTS (
            SELECT 1
            FROM network_vpc_prefixes AS vpc_prefix
            INNER JOIN site_prefixes AS site_prefix
                ON site_prefix.id = vpc_prefix.site_prefix_id
            WHERE vpc_prefix.vpc_id = $1
              AND site_prefix.authority = $2
        )
    "#;
    sqlx::query_scalar(query)
        .bind(vpc_id)
        .bind(SitePrefixAuthority::TenantManaged)
        .fetch_one(txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))
}

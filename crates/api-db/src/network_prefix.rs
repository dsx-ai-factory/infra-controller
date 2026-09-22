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

//! Database operations for network prefixes.
//!
//! Explicit result columns keep this table's queries working across column
//! additions. Cached wildcard statements otherwise fail with PostgreSQL's
//! "cached plan must not change result type".

use std::net::IpAddr;

use carbide_uuid::network::{NetworkPrefixId, NetworkSegmentId};
use carbide_uuid::vpc::{VpcId, VpcPrefixId};
use ipnetwork::IpNetwork;
use model::network_prefix::{NetworkPrefix, NewNetworkPrefix};
use sqlx::PgConnection;

use super::DatabaseError;
use crate::db_read::DbReader;

fn ip_to_u128(ip: IpAddr) -> u128 {
    match ip {
        IpAddr::V4(ip) => u128::from(u32::from(ip)),
        IpAddr::V6(ip) => u128::from(ip),
    }
}

/// Converts overlapping address ranges into inclusive child-prefix indexes.
///
/// Broad and narrow ranges both occupy every generated child prefix they
/// touch. The result is sorted but intentionally not merged so the allocator
/// can scan it directly and capacity reporting can union it without expanding
/// large IPv6 ranges one child prefix at a time.
pub fn occupied_prefix_intervals(
    parent: IpNetwork,
    child_prefix_length: u8,
    occupied_prefixes: impl IntoIterator<Item = IpNetwork>,
) -> Vec<(u128, u128)> {
    let max_bits = if parent.is_ipv4() { 32u32 } else { 128u32 };
    debug_assert!(u32::from(child_prefix_length) <= max_bits);
    let child_size = 1u128 << (max_bits - u32::from(child_prefix_length));
    let parent_start = ip_to_u128(parent.network());
    let parent_end = ip_to_u128(parent.broadcast());
    let is_ipv6 = parent.is_ipv6();

    let mut intervals: Vec<(u128, u128)> = occupied_prefixes
        .into_iter()
        .filter(|prefix| prefix.is_ipv6() == is_ipv6)
        .filter_map(|prefix| {
            let occupied_start = ip_to_u128(prefix.network()).max(parent_start);
            let occupied_end = ip_to_u128(prefix.broadcast()).min(parent_end);
            (occupied_start <= occupied_end).then_some((
                (occupied_start - parent_start) / child_size,
                (occupied_end - parent_start) / child_size,
            ))
        })
        .collect();
    intervals.sort_unstable();
    intervals
}

/// Counts the union of child-prefix indexes occupied by persisted ranges.
pub fn occupied_prefix_count(
    parent: IpNetwork,
    child_prefix_length: u8,
    occupied_prefixes: impl IntoIterator<Item = IpNetwork>,
) -> u128 {
    let intervals = occupied_prefix_intervals(parent, child_prefix_length, occupied_prefixes);
    let Some(&(first_start, first_end)) = intervals.first() else {
        return 0;
    };

    let mut occupied = 0u128;
    let mut current_start = first_start;
    let mut current_end = first_end;
    for &(start, end) in &intervals[1..] {
        if start <= current_end.saturating_add(1) {
            current_end = current_end.max(end);
            continue;
        }
        occupied += current_end - current_start + 1;
        current_start = start;
        current_end = end;
    }
    occupied + current_end - current_start + 1
}

#[derive(Clone, Copy)]
pub struct SegmentIdColumn;

impl super::ColumnInfo<'_> for SegmentIdColumn {
    type TableType = NetworkPrefix;
    type ColumnType = NetworkSegmentId;

    fn column_name(&self) -> &'static str {
        "segment_id"
    }
}

/// Returns every network prefix that overlaps `prefix`.
///
/// The global exclusion constraint limits this to one row today. Returning all
/// matches keeps callers correct when that constraint is scoped for eligible
/// isolated VPCs.
pub async fn containing_prefix(
    txn: impl DbReader<'_>,
    prefix: &str,
) -> Result<Vec<NetworkPrefix>, DatabaseError> {
    let query = "SELECT id, segment_id, prefix, gateway, dhcpv6_link_address,
            num_reserved, vpc_prefix_id, vpc_prefix, svi_ip
        FROM network_prefixes
        WHERE prefix && $1::inet
        ORDER BY segment_id, prefix";
    let container = sqlx::query_as(query)
        .bind(prefix)
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    Ok(container)
}

/// Fetch network prefixes that occupy address space for allocation from one VPC prefix.
///
/// Prefixes explicitly associated with another VPC prefix do not consume this
/// parent's capacity. Unparented prefixes remain global occupancy until the
/// global NetworkPrefix exclusion constraint is relaxed by #3892; otherwise
/// allocation could repeatedly select a prefix that persistence must reject.
pub async fn find_allocation_occupancy(
    txn: impl DbReader<'_>,
    vpc_prefix_id: VpcPrefixId,
    vpc_prefix: IpNetwork,
) -> Result<Vec<NetworkPrefix>, DatabaseError> {
    let query = r#"
        SELECT np.id, np.segment_id, np.prefix, np.gateway, np.dhcpv6_link_address,
               np.num_reserved, np.vpc_prefix_id, np.vpc_prefix, np.svi_ip
        FROM network_prefixes np
        WHERE np.prefix && $1::cidr
          AND (np.vpc_prefix_id = $2 OR np.vpc_prefix_id IS NULL)
    "#;
    sqlx::query_as(query)
        .bind(vpc_prefix)
        .bind(vpc_prefix_id)
        .fetch_all(txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))
}

// Search for specific prefix
pub async fn find(
    txn: &mut PgConnection,
    uuid: NetworkPrefixId,
) -> Result<NetworkPrefix, DatabaseError> {
    let query = "SELECT id, segment_id, prefix, gateway, dhcpv6_link_address,
            num_reserved, vpc_prefix_id, vpc_prefix, svi_ip
        FROM network_prefixes WHERE id=$1";
    sqlx::query_as(query)
        .bind(uuid)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))
}

/*
 * Return a list of `NetworkPrefix`es for a segment.
 */
pub async fn find_by<'a, C: super::ColumnInfo<'a, TableType = NetworkPrefix>>(
    txn: &mut PgConnection,
    filter: super::ObjectColumnFilter<'a, C>,
) -> Result<Vec<NetworkPrefix>, DatabaseError> {
    let mut query = super::FilterableQueryBuilder::new(
        "SELECT id, segment_id, prefix, gateway, dhcpv6_link_address,
            num_reserved, vpc_prefix_id, vpc_prefix, svi_ip
        FROM network_prefixes",
    )
    .filter(&filter);

    query
        .build_query_as()
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(query.sql(), e))
}

/// Return the persisted prefixes for configured network definitions.
///
/// `network_def.segment_id` is the durable link between a config declaration
/// and the segment it originally created or unambiguously backfilled. Looking
/// up prefixes through that link preserves the existing config-drift contract:
/// a changed declaration does not make startup act on a CIDR that was never
/// persisted.
pub async fn find_persisted_for_network_definitions(
    txn: impl DbReader<'_>,
    network_definition_names: &[String],
) -> Result<Vec<IpNetwork>, DatabaseError> {
    if network_definition_names.is_empty() {
        return Ok(Vec::new());
    }

    let query = "SELECT np.prefix
                 FROM network_prefixes np
                 INNER JOIN network_def nd ON nd.segment_id = np.segment_id
                 INNER JOIN network_segments ns ON ns.id = nd.segment_id
                 WHERE nd.name = ANY($1)
                   AND ns.deleted IS NULL";
    sqlx::query_scalar(query)
        .bind(network_definition_names)
        .fetch_all(txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))
}

// Return a list of network segment prefixes that are associated with this
// VPC but are _not_ associated with a VPC prefix.
pub async fn find_by_vpc(
    txn: &mut PgConnection,
    vpc_id: VpcId,
) -> Result<Vec<NetworkPrefix>, DatabaseError> {
    let query = "SELECT np.id, np.segment_id, np.prefix, np.gateway, np.dhcpv6_link_address, \
            np.num_reserved, np.vpc_prefix_id, np.vpc_prefix, np.svi_ip \
            FROM network_prefixes np \
            INNER JOIN network_segments ns ON np.segment_id = ns.id \
            WHERE np.vpc_prefix_id IS NULL AND ns.vpc_id = $1 ORDER BY ns.created";

    let prefixes = sqlx::query_as(query)
        .bind(vpc_id)
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    Ok(prefixes)
}

// Return a list of network segment prefixes that are associated with any VPC in the list
// but are _not_ associated with a VPC prefix.
pub async fn find_by_vpcs(
    txn: &mut PgConnection,
    vpc_ids: &Vec<VpcId>,
) -> Result<Vec<NetworkPrefix>, DatabaseError> {
    let query = "SELECT np.id, np.segment_id, np.prefix, np.gateway, np.dhcpv6_link_address,
            np.num_reserved, np.vpc_prefix_id, np.vpc_prefix, np.svi_ip
            FROM network_prefixes np
            INNER JOIN network_segments ns ON np.segment_id = ns.id
            WHERE np.vpc_prefix_id IS NULL AND ns.vpc_id = ANY($1) ORDER BY ns.created";

    let prefixes = sqlx::query_as(query)
        .bind(vpc_ids)
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(prefixes)
}

/*
 * Create a prefix for a given segment id.
 *
 * Since this function will perform muliple inserts() it wraps the actions in a sub-transaction
 * and rolls it back if any of the inserts fail and wont leave half of them written.
 *
 * # Parameters
 *
 * txn: An in-progress transaction on a connection pool
 * segment: The UUID of a network segment, must already exist and be visible to this
 * transaction
 * prefixes: A slice of the `NewNetworkPrefix` to create.
 */
pub async fn create_for(
    txn: &mut PgConnection,
    segment_id: &NetworkSegmentId,
    prefixes: &[NewNetworkPrefix],
) -> Result<Vec<NetworkPrefix>, DatabaseError> {
    let mut inner_transaction = crate::Transaction::begin_inner(txn).await?;

    // https://github.com/launchbadge/sqlx/issues/294
    //
    // No way to insert multiple rows easily.  This is more readable than some hack to save
    // tiny amounts of time.
    //
    let mut inserted_prefixes: Vec<NetworkPrefix> = Vec::with_capacity(prefixes.len());
    let query = "INSERT INTO network_prefixes (segment_id, prefix, gateway, dhcpv6_link_address, num_reserved)
            VALUES ($1::uuid, $2::cidr, $3::inet, $4::inet, $5::integer)
            RETURNING id, segment_id, prefix, gateway, dhcpv6_link_address,
                num_reserved, vpc_prefix_id, vpc_prefix, svi_ip";
    for prefix in prefixes {
        let new_prefix: NetworkPrefix = sqlx::query_as(query)
            .bind(segment_id)
            .bind(prefix.prefix)
            .bind(prefix.gateway)
            .bind(prefix.dhcpv6_link_address)
            .bind(prefix.num_reserved)
            .fetch_one(inner_transaction.as_pgconn())
            .await
            .map_err(|e| DatabaseError::query(query, e))?;

        inserted_prefixes.push(new_prefix);
    }

    inner_transaction.commit().await?;

    Ok(inserted_prefixes)
}

pub async fn delete_for_segment(
    segment_id: NetworkSegmentId,
    txn: &mut PgConnection,
) -> Result<(), DatabaseError> {
    let query = "DELETE FROM network_prefixes WHERE segment_id=$1::uuid RETURNING id";
    sqlx::query_as::<_, NetworkPrefixId>(query)
        .bind(segment_id)
        .fetch_all(txn)
        .await
        .map(|_| ())
        .map_err(|e| DatabaseError::query(query, e))
}

// Update the VPC prefix for this segment prefix using the values
// from the specified vpc_prefix.
pub async fn set_vpc_prefix(
    value: &mut NetworkPrefix,
    txn: &mut PgConnection,
    vpc_prefix_id: &VpcPrefixId,
    prefix: &IpNetwork,
) -> Result<(), DatabaseError> {
    let query = "UPDATE network_prefixes SET vpc_prefix_id=$1, vpc_prefix=$2 WHERE id=$3
        RETURNING id, segment_id, prefix, gateway, dhcpv6_link_address,
            num_reserved, vpc_prefix_id, vpc_prefix, svi_ip";
    let network_prefix = sqlx::query_as::<_, NetworkPrefix>(query)
        .bind(vpc_prefix_id)
        .bind(prefix)
        .bind(value.id)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    value.vpc_prefix_id = network_prefix.vpc_prefix_id;
    value.vpc_prefix = network_prefix.vpc_prefix;

    Ok(())
}

// Update the SVI IP.
pub async fn set_svi_ip(
    txn: &mut PgConnection,
    prefix_id: NetworkPrefixId,
    svi_ip: &IpAddr,
) -> Result<(), DatabaseError> {
    let query = "UPDATE network_prefixes SET svi_ip=$1::inet WHERE id=$2
        RETURNING id, segment_id, prefix, gateway, dhcpv6_link_address,
            num_reserved, vpc_prefix_id, vpc_prefix, svi_ip";
    sqlx::query_as::<_, NetworkPrefix>(query)
        .bind(svi_ip)
        .bind(prefix_id)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use config_version::ConfigVersion;

    use super::*;

    #[crate::sqlx_test]
    async fn allocation_occupancy_uses_exact_parent_and_global_unparented_prefixes(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let target_vpc_id = VpcId::new();
        let other_vpc_id = VpcId::new();
        let target_parent_id = VpcPrefixId::new();
        let sibling_parent_id = VpcPrefixId::new();
        let parent: IpNetwork = "10.70.0.0/24".parse()?;
        let version = ConfigVersion::initial();
        let mut txn = pool.begin().await?;

        for (vpc_id, name) in [(target_vpc_id, "target VPC"), (other_vpc_id, "other VPC")] {
            sqlx::query("INSERT INTO vpcs (id, name, version) VALUES ($1, $2, $3)")
                .bind(vpc_id)
                .bind(name)
                .bind(version)
                .execute(&mut *txn)
                .await?;
        }

        // Global VpcPrefix overlap is relaxed by a later task. Reconstruct
        // that future state in this isolated test database so this query's
        // exact-parent behavior is protected now.
        sqlx::query(
            "ALTER TABLE network_vpc_prefixes DROP CONSTRAINT network_vpc_prefixes_globally_unique",
        )
        .execute(&mut *txn)
        .await?;
        for (vpc_prefix_id, name) in [
            (target_parent_id, "target parent"),
            (sibling_parent_id, "sibling parent"),
        ] {
            sqlx::query(
                "INSERT INTO network_vpc_prefixes (id, prefix, name, vpc_id) VALUES ($1, $2, $3, $4)",
            )
            .bind(vpc_prefix_id)
            .bind(parent)
            .bind(name)
            .bind(target_vpc_id)
            .execute(&mut *txn)
            .await?;
        }

        let cases = [
            (
                "10.70.0.0/31",
                Some(target_vpc_id),
                Some((target_parent_id, parent)),
                true,
            ),
            (
                "10.70.0.2/31",
                Some(target_vpc_id),
                Some((sibling_parent_id, parent)),
                false,
            ),
            ("10.70.0.4/31", Some(target_vpc_id), None, true),
            ("10.70.0.6/31", Some(other_vpc_id), None, true),
            ("10.70.0.8/31", None, None, true),
        ];
        let mut expected = Vec::new();
        for (prefix, vpc_id, parent_association, is_expected) in cases {
            let segment_id = NetworkSegmentId::new();
            sqlx::query(
                "INSERT INTO network_segments (id, name, vpc_id, version) VALUES ($1, $2, $3, $4)",
            )
            .bind(segment_id)
            .bind(prefix)
            .bind(vpc_id)
            .bind(version)
            .execute(&mut *txn)
            .await?;
            let network_prefix: NetworkPrefix = sqlx::query_as(
                r#"
                    INSERT INTO network_prefixes (
                        segment_id,
                        prefix,
                        vpc_prefix_id,
                        vpc_prefix
                    )
                    VALUES ($1, $2, $3, $4)
                    RETURNING *
                "#,
            )
            .bind(segment_id)
            .bind(prefix.parse::<IpNetwork>()?)
            .bind(parent_association.map(|(id, _)| id))
            .bind(parent_association.map(|(_, prefix)| prefix))
            .fetch_one(&mut *txn)
            .await?;
            if is_expected {
                expected.push(network_prefix.id);
            }
        }

        let occupancy = find_allocation_occupancy(&mut *txn, target_parent_id, parent).await?;
        let mut actual: Vec<NetworkPrefixId> =
            occupancy.into_iter().map(|prefix| prefix.id).collect();
        actual.sort();
        expected.sort();
        assert_eq!(actual, expected);

        txn.rollback().await?;
        Ok(())
    }
}

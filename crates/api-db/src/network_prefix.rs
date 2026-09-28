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

/// Returns whether a constraint reports a NetworkPrefix overlap conflict.
/// Core uses the same names for client errors and bounded allocation retries.
pub fn is_overlap_constraint(constraint: Option<&str>) -> bool {
    matches!(
        constraint,
        Some(
            "network_prefixes_prefix_excl"
                | "network_prefixes_global_prefix_excl"
                | "network_prefixes_scoped_prefix_excl"
        )
    )
}

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
/// A global row on either side makes the range occupied. Only children in a
/// different stored VPC scope can be ignored. Keep this predicate aligned with
/// VPC-prefix capacity accounting.
pub async fn find_allocation_occupancy(
    txn: impl DbReader<'_>,
    vpc_prefix_id: VpcPrefixId,
    vpc_prefix: IpNetwork,
) -> Result<Vec<NetworkPrefix>, DatabaseError> {
    let query = r#"
        SELECT np.id, np.segment_id, np.prefix, np.gateway, np.dhcpv6_link_address,
               np.num_reserved, np.vpc_prefix_id, np.vpc_prefix, np.svi_ip
        FROM network_prefixes np
        JOIN network_vpc_prefixes vp ON vp.id = $2
        WHERE np.prefix && $1::cidr
          AND (np.overlap_vpc_id IS NULL OR vp.overlap_vpc_id IS NULL
               OR np.overlap_vpc_id = vp.overlap_vpc_id)
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

/// `create_for` inserts all prefixes for an existing segment atomically.
/// An exact parent associates every inserted prefix with that VPC prefix.
/// Only non-stretched Tenant segments in the parent's VPC inherit its scope;
/// direct segments and unparented prefixes remain global.
pub async fn create_for(
    txn: &mut PgConnection,
    segment_id: &NetworkSegmentId,
    prefixes: &[NewNetworkPrefix],
    vpc_prefix_id: Option<VpcPrefixId>,
) -> Result<Vec<NetworkPrefix>, DatabaseError> {
    let mut inner_transaction = crate::Transaction::begin_inner(txn).await?;

    // https://github.com/launchbadge/sqlx/issues/294
    //
    // No way to insert multiple rows easily.  This is more readable than some hack to save
    // tiny amounts of time.
    //
    let mut inserted_prefixes: Vec<NetworkPrefix> = Vec::with_capacity(prefixes.len());
    let query = r#"
        INSERT INTO network_prefixes
            (segment_id, prefix, gateway, dhcpv6_link_address, num_reserved,
             vpc_prefix_id, vpc_prefix, overlap_vpc_id)
        SELECT ns.id, $2::cidr, $3::inet, $4::inet, $5::integer, vp.id, vp.prefix,
            CASE WHEN ns.network_segment_type = 'tenant'
                       AND ns.can_stretch = false AND ns.vpc_id = vp.vpc_id
                 THEN vp.overlap_vpc_id ELSE NULL END
        FROM network_segments ns
        LEFT JOIN network_vpc_prefixes vp ON vp.id = $6
        WHERE ns.id = $1 AND ($6::uuid IS NULL OR vp.id IS NOT NULL)
        RETURNING id, segment_id, prefix, gateway, dhcpv6_link_address,
            num_reserved, vpc_prefix_id, vpc_prefix, svi_ip
    "#;
    for prefix in prefixes {
        let new_prefix: NetworkPrefix = sqlx::query_as(query)
            .bind(segment_id)
            .bind(prefix.prefix)
            .bind(prefix.gateway)
            .bind(prefix.dhcpv6_link_address)
            .bind(prefix.num_reserved)
            .bind(vpc_prefix_id)
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

/// Associates a segment prefix with its exact VPC prefix.
/// Adoption preserves the prefix's stored global scope; generated children
/// instead receive their parent and scope together in `create_for`.
pub async fn set_vpc_prefix(
    value: &mut NetworkPrefix,
    txn: &mut PgConnection,
    vpc_prefix_id: &VpcPrefixId,
    prefix: &IpNetwork,
) -> Result<(), DatabaseError> {
    let query = r#"
        UPDATE network_prefixes AS np
        SET vpc_prefix_id = $1, vpc_prefix = $2
        WHERE np.id = $3
        RETURNING np.id, np.segment_id, np.prefix, np.gateway, np.dhcpv6_link_address,
            np.num_reserved, np.vpc_prefix_id, np.vpc_prefix, np.svi_ip
    "#;
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

    #[test]
    fn overlap_constraint_names_share_one_error_classification() {
        carbide_test_support::value_scenarios!(run = is_overlap_constraint;
            "overlap constraints" {
                Some("network_prefixes_prefix_excl") => true,
                Some("network_prefixes_global_prefix_excl") => true,
                Some("network_prefixes_scoped_prefix_excl") => true,
            }
            "other database failures" {
                Some("network_prefix_family") => false,
                None => false,
            }
        );
    }

    #[crate::sqlx_test]
    async fn allocation_and_capacity_include_global_children_and_match_stored_scope(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let target_vpc_id = VpcId::new();
        let other_vpc_id = VpcId::new();
        let target_parent_id = VpcPrefixId::new();
        let sibling_parent_id = VpcPrefixId::new();
        let parent: IpNetwork = "10.70.0.0/24".parse()?;
        let version = ConfigVersion::initial();
        let mut txn = pool.begin().await?;

        sqlx::query(
            "INSERT INTO tenants (organization_id, organization_name, version)
                     VALUES ('occupancy', 'occupancy', $1)",
        )
        .bind(version)
        .execute(&mut *txn)
        .await?;
        let root: carbide_uuid::site_prefix::SitePrefixId = sqlx::query_scalar(
            "INSERT INTO site_prefixes
             (id, prefix, authority, tenant_organization_id, routing_scope, lifecycle_state, name, version)
             VALUES ($1, '10.70.0.0/16', 'tenant_managed', 'occupancy', 'datacenter_only',
                     'ready', 'occupancy', $2) RETURNING id",
        )
        .bind(carbide_uuid::site_prefix::SitePrefixId::new())
        .bind(version)
        .fetch_one(&mut *txn)
        .await?;

        for (vpc_id, name) in [(target_vpc_id, "target VPC"), (other_vpc_id, "other VPC")] {
            sqlx::query(
                "INSERT INTO vpcs (id, name, version, organization_id, network_virtualization_type)
                         VALUES ($1, $2, $3, 'occupancy', 'fnn')",
            )
            .bind(vpc_id)
            .bind(name)
            .bind(version)
            .execute(&mut *txn)
            .await?;
        }

        for (vpc_prefix_id, vpc_id, name) in [
            (target_parent_id, target_vpc_id, "target parent"),
            (sibling_parent_id, other_vpc_id, "other VPC parent"),
        ] {
            sqlx::query(
                "INSERT INTO network_vpc_prefixes (id, prefix, name, vpc_id, site_prefix_id, overlap_vpc_id)
                 VALUES ($1, $2, $3, $4, $5, $4)",
            )
            .bind(vpc_prefix_id)
            .bind(parent)
            .bind(name)
            .bind(vpc_id)
            .bind(root)
            .execute(&mut *txn)
            .await?;
        }

        struct Case {
            prefix: &'static str,
            vpc_id: Option<VpcId>,
            parent_id: Option<VpcPrefixId>,
            scope: Option<VpcId>,
            occupies_target: bool,
        }
        let cases = [
            Case {
                prefix: "10.70.0.0/31",
                vpc_id: Some(target_vpc_id),
                parent_id: Some(target_parent_id),
                scope: Some(target_vpc_id),
                occupies_target: true,
            },
            Case {
                prefix: "10.70.0.2/31",
                vpc_id: Some(other_vpc_id),
                parent_id: Some(sibling_parent_id),
                scope: Some(other_vpc_id),
                occupies_target: false,
            },
            // An older writer can attach a global child to a scoped parent.
            Case {
                prefix: "10.70.0.4/31",
                vpc_id: Some(other_vpc_id),
                parent_id: Some(sibling_parent_id),
                scope: None,
                occupies_target: true,
            },
            Case {
                prefix: "10.70.0.6/31",
                vpc_id: Some(other_vpc_id),
                parent_id: None,
                scope: None,
                occupies_target: true,
            },
            Case {
                prefix: "10.70.0.8/31",
                vpc_id: None,
                parent_id: None,
                scope: None,
                occupies_target: true,
            },
        ];
        let mut expected = Vec::new();
        for case in cases {
            let segment_id = NetworkSegmentId::new();
            sqlx::query(
                "INSERT INTO network_segments (id, name, vpc_id, version, network_segment_type, can_stretch)
                 VALUES ($1, $2, $3, $4, $5, false)",
            )
            .bind(segment_id)
            .bind(case.prefix)
            .bind(case.vpc_id)
            .bind(version)
            .bind(if case.vpc_id.is_some() {
                model::network_segment::NetworkSegmentType::Tenant
            } else {
                model::network_segment::NetworkSegmentType::HostInband
            })
            .execute(&mut *txn)
            .await?;
            let network_prefix: NetworkPrefix = sqlx::query_as(
                r#"
                    INSERT INTO network_prefixes (
                        segment_id,
                        prefix,
                        vpc_prefix_id,
                        vpc_prefix,
                        overlap_vpc_id
                    )
                    VALUES ($1, $2, $3, $4, $5)
                    RETURNING *
                "#,
            )
            .bind(segment_id)
            .bind(case.prefix.parse::<IpNetwork>()?)
            .bind(case.parent_id)
            .bind(case.parent_id.map(|_| parent))
            .bind(case.scope)
            .fetch_one(&mut *txn)
            .await?;
            if case.occupies_target {
                expected.push(network_prefix.id);
            }
        }

        let occupancy = find_allocation_occupancy(&mut *txn, target_parent_id, parent).await?;
        let occupied =
            occupied_prefix_count(parent, 31, occupancy.iter().map(|prefix| prefix.prefix));
        let mut actual: Vec<NetworkPrefixId> = occupancy.iter().map(|prefix| prefix.id).collect();
        actual.sort();
        expected.sort();
        assert_eq!(actual, expected);
        let loaded = crate::vpc_prefix::get_by_id(
            &mut txn,
            crate::ObjectColumnFilter::One(crate::vpc_prefix::IdColumn, &target_parent_id),
            model::DeletedFilter::Exclude,
        )
        .await?
        .pop()
        .unwrap();
        assert_eq!(occupied, 4);
        assert_eq!(
            loaded.status.available_linknet_segments,
            128 - u64::try_from(occupied)?
        );

        // The query must also honor a globally scoped allocation parent. Clear
        // its child first to preserve the exact-parent scope foreign key.
        sqlx::query("UPDATE network_prefixes SET overlap_vpc_id = NULL WHERE vpc_prefix_id = $1")
            .bind(target_parent_id)
            .execute(&mut *txn)
            .await?;
        sqlx::query("UPDATE network_vpc_prefixes SET overlap_vpc_id = NULL WHERE id = $1")
            .bind(target_parent_id)
            .execute(&mut *txn)
            .await?;
        let occupancy = find_allocation_occupancy(&mut *txn, target_parent_id, parent).await?;
        assert_eq!(occupancy.len(), 5);
        let loaded = crate::vpc_prefix::get_by_id(
            &mut txn,
            crate::ObjectColumnFilter::One(crate::vpc_prefix::IdColumn, &target_parent_id),
            model::DeletedFilter::Exclude,
        )
        .await?
        .pop()
        .unwrap();
        assert_eq!(loaded.status.available_linknet_segments, 123);

        txn.rollback().await?;
        Ok(())
    }

    #[crate::sqlx_test]
    async fn simulator_segment_sql_checks_both_tables_and_preserves_cloned_fields(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let script = include_str!("../../../helm-prereqs/setup-machine-a-tron.sh");
        let template = script
            .split_once("created=\"$(psql_q \"")
            .unwrap()
            .1
            .split_once("\")\"")
            .unwrap()
            .0;
        let version = ConfigVersion::initial();
        let template_id: NetworkSegmentId = sqlx::query_scalar(
            "INSERT INTO network_segments
             (name, version, network_segment_type, mtu, vlan_id, can_stretch, allocation_strategy)
             VALUES ('template', $1, 'underlay', 8765, 123, false, 'static') RETURNING id",
        )
        .bind(version)
        .fetch_one(&pool)
        .await?;
        sqlx::query(
            "INSERT INTO network_prefixes (segment_id, prefix) VALUES ($1, '10.71.1.0/24')",
        )
        .bind(template_id)
        .execute(&pool)
        .await?;
        sqlx::query(
            "INSERT INTO tenants (organization_id, organization_name, version)
                     VALUES ('simulator', 'simulator', $1)",
        )
        .bind(version)
        .execute(&pool)
        .await?;
        let root: carbide_uuid::site_prefix::SitePrefixId = sqlx::query_scalar(
            "INSERT INTO site_prefixes
             (id, prefix, authority, tenant_organization_id, routing_scope, lifecycle_state, name, version)
             VALUES ($1, '10.71.0.0/16', 'tenant_managed', 'simulator', 'datacenter_only',
                     'ready', 'simulator', $2) RETURNING id",
        )
        .bind(carbide_uuid::site_prefix::SitePrefixId::new())
        .bind(version)
        .fetch_one(&pool)
        .await?;
        let vpc_id = VpcId::new();
        sqlx::query(
            "INSERT INTO vpcs (id, name, version, organization_id, network_virtualization_type)
                     VALUES ($1, 'simulator conflict', $2, 'simulator', 'fnn')",
        )
        .bind(vpc_id)
        .bind(version)
        .execute(&pool)
        .await?;
        // A scoped parent does not share the global NetworkPrefix exclusion.
        sqlx::query(
            "INSERT INTO network_vpc_prefixes (prefix, name, vpc_id, overlap_vpc_id, site_prefix_id)
             VALUES ('10.71.2.0/24', 'scoped conflict', $1, $1, $2)",
        ).bind(vpc_id).bind(root).execute(&pool).await?;

        // VPC-less simulator networks may still overlap a global VPC prefix.
        sqlx::query(
            "INSERT INTO network_vpc_prefixes (prefix, name, vpc_id)
             VALUES ('10.71.5.0/24', 'global overlap', $1),
                    ('10.71.6.0/24', 'attached global overlap', $1)",
        )
        .bind(vpc_id)
        .execute(&pool)
        .await?;

        let admin_template_id: NetworkSegmentId = sqlx::query_scalar(
            "INSERT INTO network_segments (name, version, network_segment_type, vpc_id)
             VALUES ('admin template', $1, 'admin', $2) RETURNING id",
        )
        .bind(version)
        .bind(vpc_id)
        .fetch_one(&pool)
        .await?;
        sqlx::query(
            "INSERT INTO network_prefixes (segment_id, prefix) VALUES ($1, '10.71.8.0/24')",
        )
        .bind(admin_template_id)
        .execute(&pool)
        .await?;

        struct Case {
            name: &'static str,
            segment_type: &'static str,
            prefix: &'static str,
            gateway: &'static str,
            expected_rows: i64,
            fails: bool,
        }
        for case in [
            Case {
                name: "segment conflict",
                segment_type: "underlay",
                prefix: "10.71.1.0/24",
                gateway: "10.71.1.1",
                expected_rows: 0,
                fails: false,
            },
            Case {
                name: "parent conflict",
                segment_type: "underlay",
                prefix: "10.71.2.0/24",
                gateway: "10.71.2.1",
                expected_rows: 0,
                fails: false,
            },
            Case {
                name: "created segment",
                segment_type: "underlay",
                prefix: "10.71.3.0/24",
                gateway: "10.71.3.1",
                expected_rows: 1,
                fails: false,
            },
            Case {
                name: "failed prefix",
                segment_type: "underlay",
                prefix: "10.71.4.0/24",
                gateway: "10.71.5.1",
                expected_rows: 0,
                fails: true,
            },
            Case {
                name: "global parent compatibility",
                segment_type: "underlay",
                prefix: "10.71.5.0/24",
                gateway: "10.71.5.1",
                expected_rows: 1,
                fails: false,
            },
            Case {
                name: "attached global parent compatibility",
                segment_type: "admin",
                prefix: "10.71.6.0/24",
                gateway: "10.71.6.1",
                expected_rows: 1,
                fails: false,
            },
        ] {
            let query = template
                .replace("${name}", case.name)
                .replace("${typ}", case.segment_type)
                .replace("${pfx}", case.prefix)
                .replace("${gw}", case.gateway)
                .replace("${rsv}", "2");
            let mut connection = pool.acquire().await?;
            // The shipped SQL and every replacement are fixed test literals;
            // no caller-provided text reaches this statement.
            let result = sqlx::raw_sql(sqlx::AssertSqlSafe(query))
                .execute(&mut *connection)
                .await;
            if case.fails {
                let error = result.expect_err("the gateway outside the prefix must fail");
                assert_eq!(
                    error
                        .as_database_error()
                        .and_then(|error| error.constraint()),
                    Some("gateway_within_network")
                );
                sqlx::query("ROLLBACK").execute(&mut *connection).await?;
            } else {
                result?;
            }
            let count: i64 =
                sqlx::query_scalar("SELECT count(*) FROM network_segments WHERE name = $1")
                    .bind(case.name)
                    .fetch_one(&mut *connection)
                    .await?;
            assert_eq!(count, case.expected_rows, "{}", case.name);
        }
        let attached: (Option<VpcId>, Option<VpcId>) = sqlx::query_as(
            "SELECT ns.vpc_id, np.overlap_vpc_id
             FROM network_segments ns JOIN network_prefixes np ON np.segment_id = ns.id
             WHERE ns.name = 'attached global parent compatibility'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(attached, (Some(vpc_id), None));
        let cloned: (
            i32,
            ConfigVersion,
            Option<i16>,
            Option<bool>,
            String,
            Option<VpcId>,
            i32,
        ) = sqlx::query_as(
            "SELECT ns.mtu, ns.version, ns.vlan_id, ns.can_stretch, ns.allocation_strategy,
                        np.overlap_vpc_id, np.num_reserved
                 FROM network_segments ns JOIN network_prefixes np ON np.segment_id = ns.id
                 WHERE ns.name = 'created segment' AND np.gateway = np.svi_ip",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(
            cloned,
            (
                8765,
                version,
                None,
                Some(false),
                "dynamic".to_owned(),
                None,
                2
            )
        );
        Ok(())
    }
}

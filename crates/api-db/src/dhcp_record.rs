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

use carbide_network::ip::IpAddressFamily;
use carbide_uuid::DbTable;
use carbide_uuid::network::NetworkSegmentId;
use chrono::{DateTime, Utc};
use mac_address::MacAddress;
use model::dhcp_record::DhcpRecord;

use crate::DatabaseError;
use crate::db_read::DbReader;

/// Look up the DHCP record for a MAC on a segment, for one address family.
///
/// Returns `Ok(None)` when the `machine_dhcp_records` view has no row for the
/// triple -- e.g. no address of that family is allocated to the interface, or
/// the allocated address has no containing prefix on the segment.
pub async fn find_by_mac_address(
    conn: impl DbReader<'_>,
    mac_address: &MacAddress,
    segment_id: &NetworkSegmentId,
    address_family: IpAddressFamily,
) -> Result<Option<DhcpRecord>, DatabaseError> {
    let query = format!(
        "SELECT {} FROM machine_dhcp_records
        WHERE mac_address = $1::macaddr AND segment_id = $2::uuid AND family(address) = $3",
        DhcpRecord::db_table_columns(),
    );
    sqlx::query_as(sqlx::AssertSqlSafe(query.as_str()))
        .bind(mac_address)
        .bind(segment_id)
        .bind(address_family.pg_family())
        .fetch_optional(conn)
        .await
        .map_err(|e| DatabaseError::query(&query, e))
}

/// Return the global DHCP record invalidation timestamp.
pub async fn last_invalidation_time(
    conn: impl DbReader<'_>,
) -> Result<DateTime<Utc>, DatabaseError> {
    let query = "SELECT last_deletion FROM machine_interfaces_deletion WHERE id = 1";
    sqlx::query_scalar(query)
        .fetch_one(conn)
        .await
        .map_err(|e| DatabaseError::query(query, e))
}

#[cfg(test)]
mod tests {
    use std::net::IpAddr;

    #[crate::sqlx_test]
    async fn dhcp_view_migration_returns_only_infrastructure_ipv6_host_prefixes(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        crate::migrations::migrate_before(&pool, 20261121032116).await;
        sqlx::raw_sql(
            "INSERT INTO domains (name) VALUES ('dhcp.example');
             INSERT INTO network_segments
                 (name, version, network_segment_type, subdomain_id, allocation_strategy)
                 SELECT name, 'V1-T0', 'underlay',
                        (SELECT id FROM domains WHERE name = 'dhcp.example'), 'reserved'
                 FROM (VALUES ('leases'), ('other-link'), ('static-assignments')) AS s(name);
             INSERT INTO network_prefixes (segment_id, prefix)
                 SELECT ns.id, p.prefix::cidr FROM network_segments ns
                 JOIN (VALUES ('leases', '192.0.2.41/32'), ('leases', '2001:db8::41/128'),
                              ('other-link', '2001:db8::42/128'), ('static-assignments', '100::/128'))
                     AS p(name, prefix) ON ns.name = p.name;
             INSERT INTO machine_interfaces
                 (segment_id, domain_id, mac_address, primary_interface, hostname)
                 SELECT ns.id, ns.subdomain_id, i.mac::macaddr, true, i.hostname
                 FROM network_segments ns
                 JOIN (VALUES ('leases', '02:00:00:00:95:01', 'dual-stack'),
                              ('leases', '02:00:00:00:95:02', 'off-link'),
                              ('static-assignments', '02:00:00:00:95:03', 'anchor'))
                     AS i(name, mac, hostname) ON ns.name = i.name;
             INSERT INTO machine_interface_addresses (interface_id, address, allocation_type)
                 SELECT mi.id, a.ip::inet, 'static' FROM machine_interfaces mi
                 JOIN (VALUES ('dual-stack', '192.0.2.41'), ('dual-stack', '2001:db8::41'),
                              ('off-link', '2001:db8::42'), ('anchor', '100::'))
                     AS a(hostname, ip) ON mi.hostname = a.hostname;",
        )
        .execute(&pool)
        .await?;

        let records = || {
            sqlx::query_as::<_, (IpAddr, ipnetwork::IpNetwork)>(
                "SELECT address, prefix FROM machine_dhcp_records",
            )
            .fetch_all(&pool)
        };
        assert!(records().await?.is_empty());
        sqlx::raw_sql(include_str!(
            "../migrations/20261121032116_dhcp_records_include_ipv6_host_prefixes.sql"
        ))
        .execute(&pool)
        .await?;
        assert_eq!(
            records().await?,
            [("2001:db8::41".parse()?, "2001:db8::41/128".parse()?)]
        );
        Ok(())
    }
}

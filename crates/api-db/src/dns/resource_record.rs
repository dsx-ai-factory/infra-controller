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
use std::net::IpAddr;

use carbide_uuid::domain::DomainId;
use dns_record::SoaRecord;
use ipnetwork::IpNetwork;
use sqlx::postgres::PgRow;
use sqlx::{Error, FromRow, Row};

use crate::DatabaseError;
use crate::db_read::DbReader;

#[derive(Debug, Clone)]
pub struct DbResourceRecord {
    pub q_type: String,
    pub ttl: i32,
    pub q_name: String,
    pub record: String,
    pub domain_id: DomainId,
}

impl From<DbResourceRecord> for model::dns::ResourceRecord {
    fn from(r: DbResourceRecord) -> Self {
        Self {
            q_type: r.q_type,
            q_name: r.q_name,
            ttl: r.ttl as u32,
            content: r.record,
            domain_id: Some(r.domain_id.to_string()),
        }
    }
}

pub struct DbSoaRecord(pub SoaRecord);

impl<'r> FromRow<'r, PgRow> for DbSoaRecord {
    fn from_row(row: &'r PgRow) -> Result<Self, Error> {
        let soa: sqlx::types::Json<SoaRecord> = row.try_get("soa")?;
        Ok(DbSoaRecord(soa.0))
    }
}

impl<'r> FromRow<'r, PgRow> for DbResourceRecord {
    fn from_row(row: &'r PgRow) -> Result<Self, Error> {
        // Stored as IP address in the database
        let record: String = row
            .try_get("resource_record")
            .map(|i: IpAddr| i.to_string())?;
        let q_name: String = row.try_get("q_name")?;
        let q_type: String = row.try_get("q_type")?;
        let ttl: i32 = row.try_get("ttl")?;
        let domain_id = row.try_get("domain_id")?;

        Ok(DbResourceRecord {
            q_name,
            record,
            q_type,
            ttl,
            domain_id,
        })
    }
}

pub async fn get_soa_record(
    txn: impl DbReader<'_>,
    query_name: &str,
) -> Result<Option<DbSoaRecord>, DatabaseError> {
    let (query, domain_name) =
        if let Some(reverse_zone) = crate::dns::normalize_reverse_zone_name(query_name) {
            (
                "SELECT soa FROM domains
             WHERE lower(rtrim(name, '.')) = $1
               AND deleted IS NULL
               AND (
                   lower(rtrim(name, '.')) LIKE '%.in-addr.arpa'
                   OR lower(rtrim(name, '.')) LIKE '%.ip6.arpa'
               )",
                reverse_zone,
            )
        } else {
            (
                "SELECT soa FROM domains WHERE name = $1 AND deleted IS NULL",
                crate::dns::normalize_domain(query_name),
            )
        };
    sqlx::query_as::<_, DbSoaRecord>(query)
        .bind(domain_name)
        .fetch_optional(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))
}

pub async fn find_record(
    txn: impl DbReader<'_>,
    query_name: &str,
) -> Result<Vec<DbResourceRecord>, DatabaseError> {
    // TODO: Configurable defaults for TTL
    let query = r#"
    SELECT
     q_name,
     resource_record,
     domain_id,
     COALESCE(ttl, 300) as ttl,
     COALESCE(q_type, CASE WHEN family(resource_record) = 6 THEN 'AAAA' ELSE 'A' END) as q_type
     from dns_records WHERE q_name=$1"#;

    tracing::info!(query_name, "Looking up DNS record",);
    let result = sqlx::query_as::<_, DbResourceRecord>(query)
        .bind(query_name)
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(result)
}

#[derive(Debug, Clone)]
pub struct DbPtrRecord {
    pub ttl: i32,
    /// The FQDN the PTR record answers with (e.g. `host-1.dwrt1.com.`).
    pub ptr_content: String,
    pub domain_id: DomainId,
}

impl<'r> FromRow<'r, PgRow> for DbPtrRecord {
    fn from_row(row: &'r PgRow) -> Result<Self, Error> {
        Ok(DbPtrRecord {
            ptr_content: row.try_get("ptr_content")?,
            ttl: row.try_get("ttl")?,
            domain_id: row.try_get("domain_id")?,
        })
    }
}

/// Find the PTR answers for an address: the FQDN(s) it resolves back to. Two
/// sources, each mirroring its forward counterpart so a forward A/AAAA record and
/// its PTR round-trip:
/// - a machine interface that holds the address -- the `dns_records_shortname_combined`
///   primary/BMC arm, with `COALESCE(meta.ttl, 300)` to match the forward TTL;
/// - an overlay instance allocated the address -- read straight from the
///   `dns_records_instance` forward view by IP, so forward and reverse share one
///   definition; that view already carries the stored hostname and excludes
///   `host_inband` (the host's own address, answered by the machine source).
///
/// Both look up by `address`, so the query rides the address indexes rather than
/// scanning. Ownership is counted before checking whether an owner has enough DNS
/// metadata to publish a name. More than one matching row means the address is
/// ambiguous without a VPC-scoped DNS request, so the query returns no PTR answer
/// instead of choosing the only owner that happens to have a publishable name.
/// Any still-stored overlay address whose segment row remains present counts as
/// an owner, including during soft-delete cleanup, so stale state can only
/// suppress a PTR answer.
/// Owner and candidate collection stop at two because the only states that matter
/// are zero, one, and ambiguous.
pub async fn find_ptr_record(
    txn: impl DbReader<'_>,
    address: IpAddr,
) -> Result<Vec<DbPtrRecord>, DatabaseError> {
    let query = r#"
    WITH address_owners AS (
        SELECT 1
        FROM (
            SELECT mia.id
            FROM machine_interface_addresses mia
            WHERE mia.address = $1::inet
            UNION ALL
            SELECT ia.id
            FROM instance_addresses ia
            JOIN network_segments ns ON ns.id = ia.segment_id
            WHERE ia.address = $1::inet
              AND ns.network_segment_type <> 'host_inband'
        ) all_owners
        LIMIT 2
    ),
    ptr_candidates AS (
        SELECT ptr_content, ttl, domain_id
        FROM (
            SELECT
                concat(mi.hostname, '.', d.name, '.') AS ptr_content,
                COALESCE(meta.ttl, 300) AS ttl,
                d.id AS domain_id
            FROM machine_interface_addresses mia
            JOIN machine_interfaces mi ON mi.id = mia.interface_id
            JOIN domains d ON d.id = mi.domain_id
            LEFT JOIN dns_record_metadata meta ON meta.id = mi.id
            WHERE mia.address = $1::inet
              AND (mi.primary_interface = TRUE OR mi.interface_type = 'Bmc')
              AND d.deleted IS NULL
            UNION ALL
            SELECT
                instance_records.q_name AS ptr_content,
                COALESCE(instance_records.ttl, 300) AS ttl,
                instance_records.domain_id
            FROM dns_records_instance instance_records
            JOIN domains d ON d.id = instance_records.domain_id
            WHERE instance_records.resource_record = $1::inet
              AND d.deleted IS NULL
        ) all_candidates
        LIMIT 2
    )
    SELECT ptr_content, ttl, domain_id
    FROM ptr_candidates
    WHERE (SELECT count(*) FROM address_owners) = 1
      AND (SELECT count(*) FROM ptr_candidates) = 1"#;

    sqlx::query_as::<_, DbPtrRecord>(query)
        .bind(address.to_string())
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))
}

/// Is there any published record under `name`?
///
/// `name` is absolute and lowercase with its trailing dot, such as
/// `rack1.example.com.`. Only names strictly below it count; a record at
/// `name` itself does not.
///
/// This decides NODATA versus NXDOMAIN for a name that has no records of its
/// own. If `gpu1.rack1.example.com.` exists then `rack1.example.com.` exists
/// too, even with nothing published at it (RFC 8020 §2), and a query for it
/// must not be answered NXDOMAIN.
// TODO: the suffix predicate cannot use an index and `dns_records` is a view,
// so this scans the view on every in-zone miss. The only forward names this
// product publishes under are `adm.<zone>` and `bmc.<zone>`; replace the scan
// with `EXISTS` probes on those two source tables keyed by `domain_id`.
pub async fn any_record_below(txn: impl DbReader<'_>, name: &str) -> Result<bool, DatabaseError> {
    let query = r#"
    SELECT EXISTS (
        SELECT 1 FROM dns_records
        WHERE right(lower(q_name), length($1) + 1) = '.' || $1
    )"#;
    sqlx::query_scalar::<_, bool>(query)
        .bind(name)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))
}

/// True when any address inside `prefix` has a PTR that would be published.
///
/// The two arms are the `ptr_candidates` arms of [`find_ptr_record`] with
/// `<<=` in place of `=`: a primary or BMC machine interface with a live
/// domain, or an instance address the `dns_records_instance` view publishes.
/// An address whose only owner cannot publish a name does not count, so a
/// reverse name exists exactly when some PTR exists at or below it. Used to
/// tell an empty non-terminal in a reverse zone (NODATA) from a name with
/// nothing under it (NXDOMAIN).
///
/// An address with two publishable owners does count, even though
/// [`find_ptr_record`] withholds its answer. The name then classifies as
/// NODATA: the site has PTR data for it and declines to choose. NXDOMAIN would
/// deny the whole subtree, and under RFC 8020 a resolver may stop asking about
/// names below it for the negative TTL.
pub async fn any_ptr_published_within(
    txn: impl DbReader<'_>,
    prefix: IpNetwork,
) -> Result<bool, DatabaseError> {
    let query = r#"
    SELECT EXISTS (
        SELECT 1
        FROM machine_interface_addresses mia
        JOIN machine_interfaces mi ON mi.id = mia.interface_id
        JOIN domains d ON d.id = mi.domain_id
        WHERE mia.address <<= $1::inet
          AND (mi.primary_interface = TRUE OR mi.interface_type = 'Bmc')
          AND d.deleted IS NULL
    ) OR EXISTS (
        SELECT 1
        FROM dns_records_instance instance_records
        JOIN domains d ON d.id = instance_records.domain_id
        WHERE instance_records.resource_record <<= $1::inet
          AND d.deleted IS NULL
    )"#;
    sqlx::query_scalar::<_, bool>(query)
        .bind(prefix.to_string())
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))
}

pub async fn get_all_records_all_domains(
    txn: impl DbReader<'_>,
) -> Result<Vec<DbResourceRecord>, DatabaseError> {
    let query = r#"
        SELECT dr.q_name, dr.resource_record, dr.domain_id,
               COALESCE(dr.ttl, 300) as ttl,
               COALESCE(dr.q_type, CASE WHEN family(dr.resource_record) = 6 THEN 'AAAA' ELSE 'A' END) as q_type
        FROM dns_records dr
        JOIN domains d ON d.id = dr.domain_id
        WHERE d.deleted IS NULL
        ORDER BY dr.q_name
    "#;

    sqlx::query_as::<_, DbResourceRecord>(query)
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))
}

pub async fn get_all_records(
    txn: impl DbReader<'_>,
    query_name: &str,
) -> Result<Vec<DbResourceRecord>, DatabaseError> {
    let domain_name = crate::dns::normalize_domain(query_name);
    let query = r#"
        SELECT dr.q_name, dr.resource_record, dr.domain_id,
               COALESCE(dr.ttl, 300) as ttl,
               COALESCE(dr.q_type, CASE WHEN family(dr.resource_record) = 6 THEN 'AAAA' ELSE 'A' END) as q_type
        FROM dns_records dr
        JOIN domains d ON d.id = dr.domain_id
        WHERE d.name = $1 AND d.deleted IS NULL
    "#;

    sqlx::query_as::<_, DbResourceRecord>(query)
        .bind(domain_name)
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))
}

#[cfg(test)]
mod tests {
    use carbide_uuid::instance::InstanceId;
    use carbide_uuid::network::NetworkSegmentId;
    use carbide_uuid::vpc::VpcId;
    use model::dns::NewDomain;

    use super::find_record;
    use crate::dns::domain;

    /// Seeds a machine, its instance, a forward zone, and a network segment of
    /// `segment_type`. `fixture_name` keeps their database-unique names separate
    /// when one test creates multiple address owners. The returned IDs let the
    /// caller attach addresses and look them up.
    async fn seed_instance_segment(
        conn: &mut sqlx::PgConnection,
        fixture_name: &str,
        zone: &str,
        segment_type: &str,
    ) -> (InstanceId, NetworkSegmentId, VpcId) {
        let zone_domain = domain::persist(NewDomain::new(zone.to_string()), conn)
            .await
            .unwrap();
        let vpc_id: VpcId =
            sqlx::query_scalar("INSERT INTO vpcs (name, version) VALUES ($1, $2) RETURNING id")
                .bind(format!("vpc-{fixture_name}"))
                .bind("1")
                .fetch_one(&mut *conn)
                .await
                .unwrap();
        sqlx::query("INSERT INTO machines (id, dpf) VALUES ($1, '{}'::jsonb)")
            .bind(format!("test-machine-{fixture_name}"))
            .execute(&mut *conn)
            .await
            .unwrap();
        let instance_id: InstanceId =
            sqlx::query_scalar("INSERT INTO instances (machine_id) VALUES ($1) RETURNING id")
                .bind(format!("test-machine-{fixture_name}"))
                .fetch_one(&mut *conn)
                .await
                .unwrap();
        let segment_id: NetworkSegmentId = sqlx::query_scalar(
            "INSERT INTO network_segments (name, version, network_segment_type, subdomain_id, vpc_id)
             VALUES ($1, $2, $3::network_segment_type_t, $4, $5) RETURNING id",
        )
        .bind(format!("seg-{fixture_name}"))
        .bind("1")
        .bind(segment_type)
        .bind(zone_domain.id)
        .bind(vpc_id)
        .fetch_one(&mut *conn)
        .await
        .unwrap();
        (instance_id, segment_id, vpc_id)
    }

    async fn add_address(
        conn: &mut sqlx::PgConnection,
        instance_id: InstanceId,
        segment_id: NetworkSegmentId,
        vpc_id: VpcId,
        address: &str,
        prefix: &str,
    ) {
        // The allocate path stores the IP-derived hostname; mirror that here so the
        // view has a name to publish.
        let hostname =
            crate::host_naming::address_to_hostname(&address.parse::<std::net::IpAddr>().unwrap())
                .unwrap();
        sqlx::query(
            "INSERT INTO instance_addresses (instance_id, address, segment_id, prefix, vpc_id, hostname)
             VALUES ($1::uuid, $2::inet, $3::uuid, $4::cidr, $5::uuid, $6)",
        )
        .bind(instance_id)
        .bind(address)
        .bind(segment_id)
        .bind(prefix)
        .bind(vpc_id)
        .bind(hostname)
        .execute(conn)
        .await
        .unwrap();
    }

    #[crate::sqlx_test]
    async fn overlay_instance_addresses_are_served_forward(pool: sqlx::PgPool) {
        struct Case {
            address: &'static str,
            prefix: &'static str,
            q_name: &'static str,
            q_type: &'static str,
        }
        // One row per address family: the served name is the address in dashed,
        // IP-derived form under the segment's forward zone.
        let cases = [
            Case {
                address: "10.1.2.3",
                prefix: "10.1.2.0/24",
                q_name: "10-1-2-3.tenant.example.com.",
                q_type: "A",
            },
            Case {
                address: "2001:db8:abcd::2",
                prefix: "2001:db8:abcd::/64",
                q_name: "2001-0db8-abcd-0000-0000-0000-0000-0002.tenant.example.com.",
                q_type: "AAAA",
            },
        ];

        let mut txn = pool.begin().await.unwrap();
        let (instance_id, segment_id, vpc_id) =
            seed_instance_segment(txn.as_mut(), "forward", "tenant.example.com", "tenant").await;
        for case in &cases {
            add_address(
                txn.as_mut(),
                instance_id,
                segment_id,
                vpc_id,
                case.address,
                case.prefix,
            )
            .await;
        }

        for case in &cases {
            let records = find_record(txn.as_mut(), case.q_name).await.unwrap();
            assert_eq!(
                records.len(),
                1,
                "one {} record for {}",
                case.q_type,
                case.address
            );
            assert_eq!(records[0].q_type, case.q_type);
            assert_eq!(
                records[0].record.parse::<std::net::IpAddr>().unwrap(),
                case.address.parse::<std::net::IpAddr>().unwrap()
            );
        }
    }

    #[crate::sqlx_test]
    async fn host_inband_instance_addresses_are_not_served_here(pool: sqlx::PgPool) {
        // A host_inband instance address *is* the host's own interface address,
        // already published by the shortname view -- the instance arm must skip it
        // so it is not served twice.
        let mut txn = pool.begin().await.unwrap();
        let (instance_id, segment_id, vpc_id) = seed_instance_segment(
            txn.as_mut(),
            "host-forward",
            "host.example.com",
            "host_inband",
        )
        .await;
        add_address(
            txn.as_mut(),
            instance_id,
            segment_id,
            vpc_id,
            "10.9.9.9",
            "10.9.9.0/24",
        )
        .await;

        let records = find_record(txn.as_mut(), "10-9-9-9.host.example.com.")
            .await
            .unwrap();
        assert!(
            records.is_empty(),
            "host_inband addresses are not published by the instance arm"
        );
    }

    #[crate::sqlx_test]
    async fn overlay_instance_addresses_resolve_reverse_ptr(pool: sqlx::PgPool) {
        struct Case {
            address: &'static str,
            prefix: &'static str,
            ptr: &'static str,
        }
        // One row per address family: the PTR answers with the instance's forward
        // FQDN -- the reverse of #2408's A/AAAA record, so the two round-trip.
        let cases = [
            Case {
                address: "10.1.2.3",
                prefix: "10.1.2.0/24",
                ptr: "10-1-2-3.tenant.example.com.",
            },
            Case {
                address: "2001:db8:abcd::2",
                prefix: "2001:db8:abcd::/64",
                ptr: "2001-0db8-abcd-0000-0000-0000-0000-0002.tenant.example.com.",
            },
        ];

        let mut txn = pool.begin().await.unwrap();
        let (instance_id, segment_id, vpc_id) =
            seed_instance_segment(txn.as_mut(), "reverse", "tenant.example.com", "tenant").await;
        for case in &cases {
            add_address(
                txn.as_mut(),
                instance_id,
                segment_id,
                vpc_id,
                case.address,
                case.prefix,
            )
            .await;
        }

        for case in &cases {
            let ptrs = super::find_ptr_record(
                txn.as_mut(),
                case.address.parse::<std::net::IpAddr>().unwrap(),
            )
            .await
            .unwrap();
            assert_eq!(ptrs.len(), 1, "one PTR for {}", case.address);
            assert_eq!(ptrs[0].ptr_content, case.ptr);
            assert_eq!(ptrs[0].ttl, 300, "instance PTR uses the default TTL");
        }
    }

    #[crate::sqlx_test]
    async fn duplicate_overlay_addresses_have_no_reverse_ptr(pool: sqlx::PgPool) {
        // An address shared by two VPCs has two valid names. Until the request
        // includes VPC identity, PTR lookup must return neither one.
        let mut txn = pool.begin().await.unwrap();
        for (fixture_name, zone) in [
            ("duplicate-a", "tenant-a.example.com"),
            ("duplicate-b", "tenant-b.example.com"),
        ] {
            let (instance_id, segment_id, vpc_id) =
                seed_instance_segment(txn.as_mut(), fixture_name, zone, "tenant").await;
            add_address(
                txn.as_mut(),
                instance_id,
                segment_id,
                vpc_id,
                "10.1.2.3",
                "10.1.2.0/24",
            )
            .await;
        }

        let ptrs = super::find_ptr_record(txn.as_mut(), "10.1.2.3".parse().unwrap())
            .await
            .unwrap();
        assert!(
            ptrs.is_empty(),
            "an address shared by separate VPCs has no unscoped PTR answer"
        );
    }

    #[crate::sqlx_test]
    async fn a_stored_address_on_a_deleted_segment_still_suppresses_reverse_ptr(
        pool: sqlx::PgPool,
    ) {
        // Cleanup may retain an address after its segment is soft-deleted. It
        // still counts as an owner so stale state can only suppress another
        // tenant's PTR, never expose it.
        let mut txn = pool.begin().await.unwrap();
        let (named_instance_id, named_segment_id, named_vpc_id) =
            seed_instance_segment(txn.as_mut(), "named-owner", "named.example.com", "tenant").await;
        add_address(
            txn.as_mut(),
            named_instance_id,
            named_segment_id,
            named_vpc_id,
            "10.1.2.3",
            "10.1.2.0/24",
        )
        .await;
        sqlx::query("UPDATE network_segments SET deleted = NOW() WHERE id = $1")
            .bind(named_segment_id)
            .execute(txn.as_mut())
            .await
            .unwrap();

        let (unnamed_instance_id, unnamed_segment_id, unnamed_vpc_id) = seed_instance_segment(
            txn.as_mut(),
            "unnamed-owner",
            "unnamed.example.com",
            "tenant",
        )
        .await;
        sqlx::query(
            "INSERT INTO instance_addresses (instance_id, address, segment_id, prefix, vpc_id)
             VALUES ($1::uuid, $2::inet, $3::uuid, $4::cidr, $5::uuid)",
        )
        .bind(unnamed_instance_id)
        .bind("10.1.2.3")
        .bind(unnamed_segment_id)
        .bind("10.1.2.0/24")
        .bind(unnamed_vpc_id)
        .execute(txn.as_mut())
        .await
        .unwrap();

        let ptrs = super::find_ptr_record(txn.as_mut(), "10.1.2.3".parse().unwrap())
            .await
            .unwrap();
        assert!(
            ptrs.is_empty(),
            "a retained address from a deleted segment remains fail-closed"
        );
    }

    #[crate::sqlx_test]
    async fn duplicate_non_publishing_machine_address_has_no_reverse_ptr(pool: sqlx::PgPool) {
        // Ownership is counted before DNS eligibility. A Data interface that
        // would not publish its own PTR still makes the overlay owner's answer
        // ambiguous.
        let mut txn = pool.begin().await.unwrap();
        let (instance_id, segment_id, vpc_id) = seed_instance_segment(
            txn.as_mut(),
            "machine-duplicate",
            "tenant.example.com",
            "tenant",
        )
        .await;
        add_address(
            txn.as_mut(),
            instance_id,
            segment_id,
            vpc_id,
            "10.1.2.3",
            "10.1.2.0/24",
        )
        .await;

        let interface_id: uuid::Uuid = sqlx::query_scalar(
            "INSERT INTO machine_interfaces (
                 segment_id, mac_address, primary_interface, hostname, interface_type
             )
             VALUES ($1, '02:00:00:00:38:89', false, 'non-publishing', 'Data')
             RETURNING id",
        )
        .bind(segment_id)
        .fetch_one(txn.as_mut())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO machine_interface_addresses (interface_id, address)
             VALUES ($1, $2::inet)",
        )
        .bind(interface_id)
        .bind("10.1.2.3")
        .execute(txn.as_mut())
        .await
        .unwrap();

        let ptrs = super::find_ptr_record(txn.as_mut(), "10.1.2.3".parse().unwrap())
            .await
            .unwrap();
        assert!(
            ptrs.is_empty(),
            "a non-publishing machine owner still makes an unscoped PTR answer ambiguous"
        );
    }

    #[crate::sqlx_test]
    async fn reverse_existence_follows_publishable_ptrs_not_owners(pool: sqlx::PgPool) {
        // The prefix probe and find_ptr_record must agree on what counts: an
        // owner that cannot publish a name must not make a reverse name exist.
        let mut txn = pool.begin().await.unwrap();
        let (instance_id, segment_id, vpc_id) =
            seed_instance_segment(txn.as_mut(), "existence", "tenant.example.com", "tenant").await;
        let prefix: ipnetwork::IpNetwork = "10.1.2.0/24".parse().unwrap();

        assert!(
            !super::any_ptr_published_within(txn.as_mut(), prefix)
                .await
                .unwrap(),
            "no addresses at all"
        );

        // A Data interface owns an address but publishes no PTR.
        let interface_id: uuid::Uuid = sqlx::query_scalar(
            "INSERT INTO machine_interfaces (
                 segment_id, mac_address, primary_interface, hostname, interface_type
             )
             VALUES ($1, '02:00:00:00:38:91', false, 'data-only', 'Data')
             RETURNING id",
        )
        .bind(segment_id)
        .fetch_one(txn.as_mut())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO machine_interface_addresses (interface_id, address)
             VALUES ($1, '10.1.2.7'::inet)",
        )
        .bind(interface_id)
        .execute(txn.as_mut())
        .await
        .unwrap();
        // An instance address with no hostname owns an address but publishes nothing.
        sqlx::query(
            "INSERT INTO instance_addresses (instance_id, address, segment_id, prefix, vpc_id)
             VALUES ($1::uuid, '10.1.2.8'::inet, $2::uuid, '10.1.2.0/24'::cidr, $3::uuid)",
        )
        .bind(instance_id)
        .bind(segment_id)
        .bind(vpc_id)
        .execute(txn.as_mut())
        .await
        .unwrap();
        assert!(
            !super::any_ptr_published_within(txn.as_mut(), prefix)
                .await
                .unwrap(),
            "owners without a publishable name do not make the range exist"
        );

        add_address(
            txn.as_mut(),
            instance_id,
            segment_id,
            vpc_id,
            "10.1.2.3",
            "10.1.2.0/24",
        )
        .await;
        assert!(
            super::any_ptr_published_within(txn.as_mut(), prefix)
                .await
                .unwrap(),
            "a published instance PTR makes the range exist"
        );
        assert!(
            !super::any_ptr_published_within(txn.as_mut(), "10.1.3.0/24".parse().unwrap())
                .await
                .unwrap(),
            "a neighbouring range is unaffected"
        );
    }

    #[crate::sqlx_test]
    async fn host_inband_instance_addresses_have_no_instance_ptr(pool: sqlx::PgPool) {
        // A host_inband address is the host's own; its PTR comes from the machine
        // source, not the instance arm. With no machine interface here there is no
        // answer -- proving the instance arm excludes host_inband.
        let mut txn = pool.begin().await.unwrap();
        let (instance_id, segment_id, vpc_id) = seed_instance_segment(
            txn.as_mut(),
            "host-reverse",
            "host.example.com",
            "host_inband",
        )
        .await;
        add_address(
            txn.as_mut(),
            instance_id,
            segment_id,
            vpc_id,
            "10.9.9.9",
            "10.9.9.0/24",
        )
        .await;

        let ptrs = super::find_ptr_record(
            txn.as_mut(),
            "10.9.9.9".parse::<std::net::IpAddr>().unwrap(),
        )
        .await
        .unwrap();
        assert!(
            ptrs.is_empty(),
            "host_inband addresses are not answered by the instance arm"
        );
    }

    #[crate::sqlx_test]
    async fn soa_lookup_uses_the_recreated_live_reverse_zone(pool: sqlx::PgPool) {
        // A deleted reverse-zone spelling may be recreated as a new live row.
        // SOA lookup must select that live replacement instead of the old row.
        let mut txn = pool.begin().await.unwrap();
        let mut old = NewDomain::new("0.10.in-addr.arpa");
        old.soa.as_mut().unwrap().0.primary_ns = "old.example.com".to_string();
        let old = domain::persist(old, txn.as_mut()).await.unwrap();
        domain::delete(old, txn.as_mut()).await.unwrap();

        let mut replacement = NewDomain::new("0.10.in-addr.arpa");
        replacement.soa.as_mut().unwrap().0.primary_ns = "new.example.com".to_string();
        domain::persist(replacement, txn.as_mut()).await.unwrap();

        let soa = super::get_soa_record(txn.as_mut(), "0.10.in-addr.arpa.")
            .await
            .unwrap()
            .expect("the live replacement has an SOA record");
        assert_eq!(soa.0.primary_ns, "new.example.com");
    }

    #[crate::sqlx_test]
    async fn a_deleted_forward_domain_has_no_soa_record(pool: sqlx::PgPool) {
        // Soft deletion removes a forward zone from service even though its
        // SOA data remains stored for lifecycle history.
        let mut txn = pool.begin().await.unwrap();
        let domain = domain::persist(NewDomain::new("deleted.example.com"), txn.as_mut())
            .await
            .unwrap();
        domain::delete(domain, txn.as_mut()).await.unwrap();

        let soa = super::get_soa_record(txn.as_mut(), "deleted.example.com")
            .await
            .unwrap();
        assert!(soa.is_none(), "a deleted forward zone cannot serve SOA");
    }

    #[crate::sqlx_test]
    async fn a_deleted_instance_domain_has_no_reverse_ptr(pool: sqlx::PgPool) {
        // An overlay address remains stored while its domain is deleted, but
        // DNS must not publish a PTR through that inactive zone.
        let mut txn = pool.begin().await.unwrap();
        let (instance_id, segment_id, vpc_id) = seed_instance_segment(
            txn.as_mut(),
            "deleted-instance-domain",
            "deleted-instance.example.com",
            "tenant",
        )
        .await;
        add_address(
            txn.as_mut(),
            instance_id,
            segment_id,
            vpc_id,
            "10.1.2.3",
            "10.1.2.0/24",
        )
        .await;
        let domains = domain::find_by_name(txn.as_mut(), "deleted-instance.example.com")
            .await
            .unwrap();
        let [instance_domain] = domains.as_slice() else {
            panic!("test fixture should have exactly one instance domain");
        };
        domain::delete(instance_domain.clone(), txn.as_mut())
            .await
            .unwrap();

        let ptrs = super::find_ptr_record(txn.as_mut(), "10.1.2.3".parse().unwrap())
            .await
            .unwrap();
        assert!(ptrs.is_empty(), "a deleted domain cannot publish PTR");
    }

    #[crate::sqlx_test]
    async fn a_deleted_machine_domain_has_no_reverse_ptr(pool: sqlx::PgPool) {
        // A machine interface keeps its address while its domain is deleted,
        // but DNS must not publish a PTR through that inactive zone.
        let mut txn = pool.begin().await.unwrap();
        let (_, segment_id, _) = seed_instance_segment(
            txn.as_mut(),
            "deleted-machine-domain",
            "deleted-machine.example.com",
            "tenant",
        )
        .await;
        let domains = domain::find_by_name(txn.as_mut(), "deleted-machine.example.com")
            .await
            .unwrap();
        let [machine_domain] = domains.as_slice() else {
            panic!("test fixture should have exactly one machine domain");
        };
        let machine_domain = machine_domain.clone();
        let interface_id: uuid::Uuid = sqlx::query_scalar(
            "INSERT INTO machine_interfaces (
                 segment_id, mac_address, primary_interface, hostname,
                 interface_type, domain_id
             )
             VALUES ($1, '02:00:00:00:38:90', true, 'deleted-machine', 'Data', $2)
             RETURNING id",
        )
        .bind(segment_id)
        .bind(machine_domain.id)
        .fetch_one(txn.as_mut())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO machine_interface_addresses (interface_id, address)
             VALUES ($1, $2::inet)",
        )
        .bind(interface_id)
        .bind("10.1.2.3")
        .execute(txn.as_mut())
        .await
        .unwrap();
        domain::delete(machine_domain, txn.as_mut()).await.unwrap();

        let ptrs = super::find_ptr_record(txn.as_mut(), "10.1.2.3".parse().unwrap())
            .await
            .unwrap();
        assert!(ptrs.is_empty(), "a deleted domain cannot publish PTR");
    }
}

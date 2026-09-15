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

pub mod domain;
pub mod domain_metadata;
pub mod resource_record;

use carbide_uuid::network::NetworkSegmentId;
use ipnetwork::IpNetwork;
use model::dns::NewDomain;
use sqlx::PgTransaction;

use crate::DatabaseResult;

pub fn normalize_domain(name: &str) -> String {
    let normalized_domain = name.trim_end_matches('.').to_ascii_lowercase();
    tracing::debug!(input = %name, normalized = %normalized_domain, "normalized domain name");
    normalized_domain
}

/// Returns the normalized identity for a reverse-DNS zone name. A trailing dot
/// is presentation-only; forward domain names return `None`.
pub fn normalize_reverse_zone_name(name: &str) -> Option<String> {
    let name = normalize_domain(name);
    if name.ends_with(".in-addr.arpa") || name.ends_with(".ip6.arpa") {
        Some(name)
    } else {
        None
    }
}

/// Reverse-name parsing lives in the model crate; re-exported so database
/// callers keep one import path for the DNS helpers.
pub use model::dns::{arpa_qname_to_ip, arpa_qname_to_prefix};

/// Build the reverse-DNS zone name for a network prefix: the network octets
/// (IPv4) or nibbles (IPv6) the prefix covers, in reverse, under `in-addr.arpa`
/// / `ip6.arpa`. The forward inverse of [`arpa_qname_to_ip`].
///
/// Returns `None` for a prefix that is not octet-aligned (IPv4: /8, /16, /24,
/// /32) or nibble-aligned (IPv6: a multiple of 4) -- RFC 2317 classless
/// delegation is out of scope.
pub fn cidr_to_reverse_zone(prefix: IpNetwork) -> Option<String> {
    match prefix {
        IpNetwork::V4(net) => {
            let bits = net.prefix();
            if bits == 0 || bits % 8 != 0 {
                return None;
            }
            let octets = net.network().octets();
            let labels = (bits / 8) as usize;
            let mut parts: Vec<String> = octets[..labels].iter().rev().map(u8::to_string).collect();
            parts.push("in-addr.arpa".to_string());
            Some(parts.join("."))
        }
        IpNetwork::V6(net) => {
            let bits = net.prefix();
            if bits == 0 || bits % 4 != 0 {
                return None;
            }
            let octets = net.network().octets();
            let nibbles = (bits / 4) as usize;
            let mut parts: Vec<String> = (0..nibbles)
                .map(|i| {
                    let byte = octets[i / 2];
                    let nibble = if i % 2 == 0 { byte >> 4 } else { byte & 0x0f };
                    format!("{nibble:x}")
                })
                .collect();
            parts.reverse();
            parts.push("ip6.arpa".to_string());
            Some(parts.join("."))
        }
    }
}

/// Ensure the reverse-DNS zone for a network prefix exists, deriving its name
/// from the prefix and creating the domain only if it is not already present.
/// A network's reverse zone is a consequence of the network existing, so this is
/// called wherever a network segment is created; non-aligned prefixes are skipped
/// (see [`cidr_to_reverse_zone`]).
///
/// The transaction-scoped zone lock makes the find-then-create atomic. When
/// VPC-scoped prefix reuse is enabled, equal prefixes will share one
/// reverse-zone row until the DNS request path has a trusted VPC discriminator.
pub async fn ensure_reverse_zones(
    prefixes: &[IpNetwork],
    txn: &mut PgTransaction<'_>,
) -> DatabaseResult<()> {
    let reverse_zones = reverse_zones_for_prefixes(prefixes);
    let zone_names = reverse_zones
        .iter()
        .map(|(zone, _)| zone.clone())
        .collect::<Vec<_>>();
    lock_normalized_reverse_zone_names(txn, &zone_names).await?;

    for (zone, prefix) in reverse_zones {
        ensure_reverse_zone_locked(prefix, zone, txn).await?;
    }
    Ok(())
}

/// Remove the reverse-DNS zone derived from a network prefix -- the inverse of
/// [`ensure_reverse_zones`]. A network's reverse zone exists only because the
/// network does, so it is dropped wherever a network segment is deleted;
/// non-aligned prefixes never had a zone and are skipped (see
/// [`cidr_to_reverse_zone`]).
///
/// The zone is soft-deleted, matching the segment's own deletion, only after no
/// other live segment uses the same prefix. The zone lock serializes that check
/// with both creates and other deletions.
pub async fn remove_reverse_zones(
    prefixes: &[IpNetwork],
    removed_segment_id: NetworkSegmentId,
    txn: &mut PgTransaction<'_>,
) -> DatabaseResult<()> {
    let reverse_zones = reverse_zones_for_prefixes(prefixes);
    let zone_names = reverse_zones
        .iter()
        .map(|(zone, _)| zone.clone())
        .collect::<Vec<_>>();
    lock_normalized_reverse_zone_names(txn, &zone_names).await?;

    for (zone, prefix) in reverse_zones {
        remove_reverse_zone_locked(prefix, zone, removed_segment_id, txn).await?;
    }
    Ok(())
}

fn reverse_zones_for_prefixes(prefixes: &[IpNetwork]) -> Vec<(String, IpNetwork)> {
    let mut reverse_zones = prefixes
        .iter()
        .filter_map(|prefix| match cidr_to_reverse_zone(*prefix) {
            Some(zone) => Some((zone, *prefix)),
            None => {
                tracing::debug!(%prefix, "no reverse zone: prefix is not octet/nibble aligned");
                None
            }
        })
        .collect::<Vec<_>>();
    reverse_zones.sort_unstable_by(|left, right| left.0.cmp(&right.0));
    reverse_zones.dedup_by(|left, right| left.0 == right.0);
    reverse_zones
}

async fn ensure_reverse_zone_locked(
    prefix: IpNetwork,
    zone: String,
    txn: &mut PgTransaction<'_>,
) -> DatabaseResult<()> {
    if !reverse_zone_has_live_prefix(txn, prefix).await? {
        return Ok(());
    }

    let domains = domain::find_reverse_zone_by_normalized_name(txn.as_mut(), &zone).await?;
    match domains.as_slice() {
        [] => {
            tracing::info!(zone = %zone, %prefix, "creating reverse-DNS zone for network prefix");
            domain::persist(NewDomain::new(zone), txn.as_mut()).await?;
        }
        [_] => {}
        _ => {
            return Err(crate::DatabaseError::Internal {
                message: format!("multiple live domains represent reverse zone {zone}"),
            });
        }
    }
    Ok(())
}

async fn remove_reverse_zone_locked(
    prefix: IpNetwork,
    zone: String,
    removed_segment_id: NetworkSegmentId,
    txn: &mut PgTransaction<'_>,
) -> DatabaseResult<()> {
    if reverse_zone_has_other_live_prefix(txn, prefix, removed_segment_id).await? {
        return Ok(());
    }

    let domains = domain::find_reverse_zone_by_normalized_name(txn.as_mut(), &zone).await?;
    match domains.as_slice() {
        [] => {}
        [domain] => {
            tracing::info!(zone = %zone, %prefix, "removing reverse-DNS zone for deleted network prefix");
            domain::delete(domain.clone(), txn.as_mut()).await?;
        }
        _ => {
            return Err(crate::DatabaseError::Internal {
                message: format!("multiple live domains represent reverse zone {zone}"),
            });
        }
    }
    Ok(())
}

/// Serializes participating reverse-zone writes by normalized name. These are
/// transaction-scoped PostgreSQL advisory locks, not row or table locks. The
/// database's live reverse-name uniqueness constraint remains the backstop for
/// writers outside these paths. Every lock is acquired in sorted name order
/// so a transaction touching both address families cannot deadlock with the
/// same names in another order.
pub async fn lock_reverse_zone_names(
    txn: &mut PgTransaction<'_>,
    names: &[String],
) -> DatabaseResult<()> {
    let names = names
        .iter()
        .filter_map(|name| normalize_reverse_zone_name(name))
        .collect::<Vec<_>>();
    lock_normalized_reverse_zone_names(txn, &names).await
}

async fn lock_normalized_reverse_zone_names(
    txn: &mut PgTransaction<'_>,
    names: &[String],
) -> DatabaseResult<()> {
    let mut names = names.to_vec();
    names.sort_unstable();
    names.dedup();
    let query = "SELECT pg_advisory_xact_lock(hashtextextended('dns:reverse-zone:' || $1, 0))";
    for name in &names {
        sqlx::query(query)
            .bind(name)
            .execute(txn.as_mut())
            .await
            .map_err(|error| crate::DatabaseError::query(query, error))?;
    }
    Ok(())
}

async fn reverse_zone_has_live_prefix(
    txn: &mut PgTransaction<'_>,
    prefix: IpNetwork,
) -> DatabaseResult<bool> {
    let query = r#"
        SELECT EXISTS (
            SELECT 1
            FROM network_prefixes np
            JOIN network_segments ns ON ns.id = np.segment_id
            WHERE np.prefix = $1::cidr
              AND ns.deleted IS NULL
        )
    "#;
    sqlx::query_scalar(query)
        .bind(prefix)
        .fetch_one(txn.as_mut())
        .await
        .map_err(|error| crate::DatabaseError::query(query, error))
}

async fn reverse_zone_has_other_live_prefix(
    txn: &mut PgTransaction<'_>,
    prefix: IpNetwork,
    removed_segment_id: NetworkSegmentId,
) -> DatabaseResult<bool> {
    let query = r#"
        SELECT EXISTS (
            SELECT 1
            FROM network_prefixes np
            JOIN network_segments ns ON ns.id = np.segment_id
            WHERE np.prefix = $1::cidr
              AND ns.id != $2
              AND ns.deleted IS NULL
        )
    "#;
    sqlx::query_scalar(query)
        .bind(prefix)
        .bind(removed_segment_id)
        .fetch_one(txn.as_mut())
        .await
        .map_err(|e| crate::DatabaseError::query(query, e))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use carbide_uuid::network::NetworkSegmentId;
    use tokio::sync::Barrier;

    /// Inserts the minimum live segment and prefix rows without touching DNS.
    /// The returned segment ID lets deletion tests retire the exact owner.
    async fn insert_network_prefix(
        txn: &mut sqlx::PgTransaction<'_>,
        name: &str,
        prefix: ipnetwork::IpNetwork,
    ) -> NetworkSegmentId {
        let segment_id = sqlx::query_scalar(
            "INSERT INTO network_segments (name, version)
             VALUES ($1, 'test')
             RETURNING id",
        )
        .bind(name)
        .fetch_one(txn.as_mut())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO network_prefixes (segment_id, prefix, num_reserved)
             VALUES ($1, $2::cidr, 0)",
        )
        .bind(segment_id)
        .bind(prefix)
        .execute(txn.as_mut())
        .await
        .unwrap();
        segment_id
    }

    #[test]
    fn cidr_to_reverse_zone_derives_aligned_prefixes() {
        use carbide_test_support::value_scenarios;

        value_scenarios!(
            run = |cidr: &str| super::cidr_to_reverse_zone(cidr.parse().unwrap());
            "ipv4 octet-aligned" {
                "10.0.0.0/8" => Some("10.in-addr.arpa".to_string()),
                "192.168.0.0/16" => Some("168.192.in-addr.arpa".to_string()),
                "192.0.2.0/24" => Some("2.0.192.in-addr.arpa".to_string()),
                "192.0.2.1/32" => Some("1.2.0.192.in-addr.arpa".to_string()),
            }
            "ipv6 nibble-aligned" {
                "fd00::/16" => Some("0.0.d.f.ip6.arpa".to_string()),
                "2001:db8::/32" => Some("8.b.d.0.1.0.0.2.ip6.arpa".to_string()),
            }
            "rejects prefixes that are not octet- or nibble-aligned" {
                "192.168.0.0/25" => None,
                "fd00::/17" => None,
                "0.0.0.0/0" => None,
            }
        );
    }

    #[test]
    fn reverse_zone_batches_are_sorted_and_deduplicated() {
        // Different callers may supply the same v4/v6 prefixes in any order.
        // Every batch must converge on one stable advisory-lock sequence.
        let ipv4 = "10.0.0.0/16".parse().unwrap();
        let ipv6 = "fd00::/16".parse().unwrap();
        let reverse_zones = super::reverse_zones_for_prefixes(&[ipv4, ipv6, ipv4]);

        assert_eq!(reverse_zones.len(), 2);
        assert!(
            reverse_zones
                .windows(2)
                .all(|zones| zones[0].0 < zones[1].0),
            "all callers acquire unique reverse-zone locks in sorted name order"
        );
    }

    #[crate::sqlx_test]
    async fn ensure_reverse_zone_creates_idempotently(pool: sqlx::PgPool) {
        let mut txn = pool.begin().await.unwrap();
        let prefix: ipnetwork::IpNetwork = "10.0.0.0/16".parse().unwrap();
        insert_network_prefix(&mut txn, "idempotent", prefix).await;

        // First call creates the zone; the second is a no-op.
        super::ensure_reverse_zones(&[prefix], &mut txn)
            .await
            .unwrap();
        super::ensure_reverse_zones(&[prefix], &mut txn)
            .await
            .unwrap();
        let zones = super::domain::find_by_name(txn.as_mut(), "0.10.in-addr.arpa")
            .await
            .unwrap();
        assert_eq!(zones.len(), 1, "reverse zone created exactly once");

        // A non-aligned prefix derives no zone, so nothing is created.
        let unaligned: ipnetwork::IpNetwork = "10.1.0.0/25".parse().unwrap();
        super::ensure_reverse_zones(&[unaligned], &mut txn)
            .await
            .unwrap();
        assert!(super::cidr_to_reverse_zone(unaligned).is_none());
    }

    #[crate::sqlx_test]
    async fn an_existing_reverse_zone_is_reused(pool: sqlx::PgPool) {
        // A dotted domain created through the Domain API already represents
        // this zone, so network creation must reuse it instead of inserting a
        // second spelling.
        let prefix = "10.0.0.0/16".parse().unwrap();
        let mut txn = pool.begin().await.unwrap();
        insert_network_prefix(&mut txn, "existing-zone", prefix).await;
        let manual = super::domain::persist(
            model::dns::NewDomain::new("0.10.in-addr.arpa."),
            txn.as_mut(),
        )
        .await
        .unwrap();

        super::ensure_reverse_zones(&[prefix], &mut txn)
            .await
            .unwrap();

        let domains =
            super::domain::find_reverse_zone_by_normalized_name(txn.as_mut(), "0.10.in-addr.arpa")
                .await
                .unwrap();
        let [domain] = domains.as_slice() else {
            panic!("the existing reverse zone should be reused");
        };
        assert_eq!(domain.id, manual.id);
        assert_eq!(domain.name, "0.10.in-addr.arpa.");
    }

    #[crate::sqlx_test]
    async fn remove_reverse_zone_deletes_the_zone(pool: sqlx::PgPool) {
        let mut txn = pool.begin().await.unwrap();
        let prefix: ipnetwork::IpNetwork = "10.0.0.0/16".parse().unwrap();
        let removed_segment_id = insert_network_prefix(&mut txn, "removed-zone", prefix).await;

        // A network's zone is dropped when the network is deleted.
        super::ensure_reverse_zones(&[prefix], &mut txn)
            .await
            .unwrap();
        sqlx::query("UPDATE network_segments SET deleted = NOW() WHERE id = $1")
            .bind(removed_segment_id)
            .execute(txn.as_mut())
            .await
            .unwrap();
        super::remove_reverse_zones(&[prefix], removed_segment_id, &mut txn)
            .await
            .unwrap();
        let zones = super::domain::find_by_name(txn.as_mut(), "0.10.in-addr.arpa")
            .await
            .unwrap();
        assert!(zones.is_empty(), "reverse zone removed with its network");

        // Removing again finds no live zone, so deleting a segment twice is a no-op.
        super::remove_reverse_zones(&[prefix], removed_segment_id, &mut txn)
            .await
            .unwrap();
        let after_second = super::domain::find_by_name(txn.as_mut(), "0.10.in-addr.arpa")
            .await
            .unwrap();
        assert!(after_second.is_empty(), "repeated removal stays a no-op");

        // Removing a non-aligned prefix (which never had a zone) leaves other zones intact.
        let control: ipnetwork::IpNetwork = "10.2.0.0/16".parse().unwrap();
        insert_network_prefix(&mut txn, "control-zone", control).await;
        super::ensure_reverse_zones(&[control], &mut txn)
            .await
            .unwrap();
        let unaligned: ipnetwork::IpNetwork = "10.1.0.0/25".parse().unwrap();
        super::remove_reverse_zones(&[unaligned], removed_segment_id, &mut txn)
            .await
            .unwrap();
        let control_zones = super::domain::find_by_name(txn.as_mut(), "2.10.in-addr.arpa")
            .await
            .unwrap();
        assert_eq!(
            control_zones.len(),
            1,
            "removing an unaligned prefix touches no other zone"
        );
    }

    #[crate::sqlx_test]
    async fn a_stale_prefix_snapshot_does_not_recreate_a_removed_zone(pool: sqlx::PgPool) {
        // Startup can collect prefixes before a concurrent segment deletion.
        // The liveness check under the zone lock must catch that stale read.
        let prefix = "10.0.0.0/16".parse().unwrap();
        let mut setup = pool.begin().await.unwrap();
        let removed_segment_id = insert_network_prefix(&mut setup, "stale-snapshot", prefix).await;
        super::ensure_reverse_zones(&[prefix], &mut setup)
            .await
            .unwrap();
        setup.commit().await.unwrap();

        let mut delete = pool.begin().await.unwrap();
        sqlx::query("UPDATE network_segments SET deleted = NOW() WHERE id = $1")
            .bind(removed_segment_id)
            .execute(delete.as_mut())
            .await
            .unwrap();
        super::remove_reverse_zones(&[prefix], removed_segment_id, &mut delete)
            .await
            .unwrap();
        delete.commit().await.unwrap();

        // Startup may have read this prefix before the delete acquired its
        // zone lock. Revalidation under the same lock must reject that stale
        // snapshot after the delete commits.
        let mut startup = pool.begin().await.unwrap();
        super::ensure_reverse_zones(&[prefix], &mut startup)
            .await
            .unwrap();
        startup.commit().await.unwrap();

        let zones = super::domain::find_by_name(&pool, "0.10.in-addr.arpa")
            .await
            .unwrap();
        assert!(zones.is_empty(), "a deleted prefix cannot regain its zone");
    }

    #[crate::sqlx_test]
    async fn concurrent_opposite_order_reverse_zone_creates_do_not_deadlock(pool: sqlx::PgPool) {
        // Two dual-stack creators begin together with opposite input order.
        // Sorted lock acquisition must let both finish and leave one zone per
        // address family.
        // Each task waits at the same barrier before opening its transaction so
        // the two lock batches compete rather than running sequentially.
        async fn create_zones(
            pool: sqlx::PgPool,
            barrier: Arc<Barrier>,
            prefixes: Vec<ipnetwork::IpNetwork>,
        ) {
            barrier.wait().await;
            let mut txn = pool.begin().await.unwrap();
            super::ensure_reverse_zones(&prefixes, &mut txn)
                .await
                .unwrap();
            txn.commit().await.unwrap();
        }

        let ipv4 = "10.0.0.0/16".parse().unwrap();
        let ipv6 = "fd00::/16".parse().unwrap();
        let mut setup = pool.begin().await.unwrap();
        insert_network_prefix(&mut setup, "ordered-ipv4", ipv4).await;
        insert_network_prefix(&mut setup, "ordered-ipv6", ipv6).await;
        setup.commit().await.unwrap();
        let barrier = Arc::new(Barrier::new(2));
        tokio::time::timeout(Duration::from_secs(10), async {
            tokio::join!(
                create_zones(pool.clone(), barrier.clone(), vec![ipv4, ipv6]),
                create_zones(pool.clone(), barrier, vec![ipv6, ipv4])
            )
        })
        .await
        .expect("sorted reverse-zone lock ordering must not deadlock");

        for zone in ["0.10.in-addr.arpa", "0.0.d.f.ip6.arpa"] {
            let zones = super::domain::find_by_name(&pool, zone).await.unwrap();
            assert_eq!(zones.len(), 1, "concurrent creates reuse {zone}");
        }
    }

    #[crate::sqlx_test]
    async fn concurrent_manual_and_network_writers_share_one_zone(pool: sqlx::PgPool) {
        // A Domain API writer and network lifecycle writer use the same zone
        // lock, so their find-then-create operations leave one live identity.
        let prefix = "10.0.0.0/16".parse().unwrap();
        let zone = "0.10.in-addr.arpa".to_string();
        let mut setup = pool.begin().await.unwrap();
        insert_network_prefix(&mut setup, "manual-race", prefix).await;
        setup.commit().await.unwrap();
        let barrier = Arc::new(Barrier::new(2));

        let manual_writer = async {
            barrier.wait().await;
            let mut txn = pool.begin().await.unwrap();
            super::lock_reverse_zone_names(&mut txn, std::slice::from_ref(&zone))
                .await
                .unwrap();
            if super::domain::find_reverse_zone_by_normalized_name(txn.as_mut(), &zone)
                .await
                .unwrap()
                .is_empty()
            {
                super::domain::persist(model::dns::NewDomain::new(zone.clone()), txn.as_mut())
                    .await
                    .unwrap();
            }
            txn.commit().await.unwrap();
        };
        let network_writer = async {
            barrier.wait().await;
            let mut txn = pool.begin().await.unwrap();
            super::ensure_reverse_zones(&[prefix], &mut txn)
                .await
                .unwrap();
            txn.commit().await.unwrap();
        };
        tokio::time::timeout(Duration::from_secs(10), async {
            tokio::join!(manual_writer, network_writer)
        })
        .await
        .expect("manual and network reverse-zone writers must finish");

        let domains = super::domain::find_reverse_zone_by_normalized_name(&pool, &zone)
            .await
            .unwrap();
        assert_eq!(domains.len(), 1);
    }

    #[crate::sqlx_test]
    async fn shared_reverse_zone_is_removed_after_its_last_prefix(pool: sqlx::PgPool) {
        // Equal prefixes share one zone: deleting the first retains it, while
        // deleting the final live prefix removes it.
        let mut txn = pool.begin().await.unwrap();
        sqlx::query(
            "ALTER TABLE network_prefixes DROP CONSTRAINT IF EXISTS network_prefixes_prefix_excl",
        )
        .execute(txn.as_mut())
        .await
        .unwrap();

        let prefix = "10.0.0.0/16".parse().unwrap();
        let first_segment_id = insert_network_prefix(&mut txn, "first", prefix).await;
        let second_segment_id = insert_network_prefix(&mut txn, "second", prefix).await;
        super::ensure_reverse_zones(&[prefix], &mut txn)
            .await
            .unwrap();

        sqlx::query("UPDATE network_segments SET deleted = NOW() WHERE id = $1")
            .bind(first_segment_id)
            .execute(txn.as_mut())
            .await
            .unwrap();
        super::remove_reverse_zones(&[prefix], first_segment_id, &mut txn)
            .await
            .unwrap();
        let zones = super::domain::find_by_name(txn.as_mut(), "0.10.in-addr.arpa")
            .await
            .unwrap();
        assert_eq!(zones.len(), 1, "the second prefix still needs the zone");

        sqlx::query("UPDATE network_segments SET deleted = NOW() WHERE id = $1")
            .bind(second_segment_id)
            .execute(txn.as_mut())
            .await
            .unwrap();
        super::remove_reverse_zones(&[prefix], second_segment_id, &mut txn)
            .await
            .unwrap();
        let zones = super::domain::find_by_name(txn.as_mut(), "0.10.in-addr.arpa")
            .await
            .unwrap();
        assert!(zones.is_empty(), "the last prefix removes the reverse zone");
    }

    #[crate::sqlx_test]
    async fn concurrent_create_and_delete_preserve_the_shared_zone(pool: sqlx::PgPool) {
        // A replacement prefix created while the old segment drains still
        // needs the shared zone after both transactions commit.
        let mut setup = pool.begin().await.unwrap();
        sqlx::query(
            "ALTER TABLE network_prefixes DROP CONSTRAINT IF EXISTS network_prefixes_prefix_excl",
        )
        .execute(setup.as_mut())
        .await
        .unwrap();
        let prefix = "10.0.0.0/16".parse().unwrap();
        let removed_segment_id = insert_network_prefix(&mut setup, "removed", prefix).await;
        super::ensure_reverse_zones(&[prefix], &mut setup)
            .await
            .unwrap();
        setup.commit().await.unwrap();

        let barrier = Arc::new(Barrier::new(2));
        let delete = async {
            barrier.wait().await;
            let mut txn = pool.begin().await.unwrap();
            sqlx::query("UPDATE network_segments SET deleted = NOW() WHERE id = $1")
                .bind(removed_segment_id)
                .execute(txn.as_mut())
                .await
                .unwrap();
            barrier.wait().await;
            super::remove_reverse_zones(&[prefix], removed_segment_id, &mut txn)
                .await
                .unwrap();
            txn.commit().await.unwrap();
        };
        let create = async {
            barrier.wait().await;
            let mut txn = pool.begin().await.unwrap();
            insert_network_prefix(&mut txn, "replacement", prefix).await;
            barrier.wait().await;
            super::ensure_reverse_zones(&[prefix], &mut txn)
                .await
                .unwrap();
            txn.commit().await.unwrap();
        };
        tokio::time::timeout(Duration::from_secs(10), async {
            tokio::join!(delete, create)
        })
        .await
        .expect("concurrent create and delete must finish");

        let zones = super::domain::find_by_name(&pool, "0.10.in-addr.arpa")
            .await
            .unwrap();
        assert_eq!(zones.len(), 1, "the replacement prefix retains the zone");
    }

    #[crate::sqlx_test]
    async fn concurrent_deletes_remove_the_last_shared_zone(pool: sqlx::PgPool) {
        // Both owners become deleted before either lifecycle call can decide
        // whether another live prefix remains. Serialization must still remove
        // the final zone exactly once.
        let mut setup = pool.begin().await.unwrap();
        sqlx::query(
            "ALTER TABLE network_prefixes DROP CONSTRAINT IF EXISTS network_prefixes_prefix_excl",
        )
        .execute(setup.as_mut())
        .await
        .unwrap();
        let prefix = "10.0.0.0/16".parse().unwrap();
        let first_segment_id = insert_network_prefix(&mut setup, "delete-first", prefix).await;
        let second_segment_id = insert_network_prefix(&mut setup, "delete-second", prefix).await;
        super::ensure_reverse_zones(&[prefix], &mut setup)
            .await
            .unwrap();
        setup.commit().await.unwrap();

        // Start both deletions together, then make their zone removals contend
        // only after both segment rows are marked deleted.
        async fn delete_prefix(
            pool: sqlx::PgPool,
            barrier: Arc<Barrier>,
            prefix: ipnetwork::IpNetwork,
            network_segment_id: NetworkSegmentId,
        ) {
            barrier.wait().await;
            let mut txn = pool.begin().await.unwrap();
            sqlx::query("UPDATE network_segments SET deleted = NOW() WHERE id = $1")
                .bind(network_segment_id)
                .execute(txn.as_mut())
                .await
                .unwrap();
            barrier.wait().await;
            super::remove_reverse_zones(&[prefix], network_segment_id, &mut txn)
                .await
                .unwrap();
            txn.commit().await.unwrap();
        }

        let barrier = Arc::new(Barrier::new(2));
        tokio::time::timeout(Duration::from_secs(10), async {
            tokio::join!(
                delete_prefix(pool.clone(), barrier.clone(), prefix, first_segment_id),
                delete_prefix(pool.clone(), barrier, prefix, second_segment_id)
            )
        })
        .await
        .expect("concurrent deletes must finish");

        let zones = super::domain::find_by_name(&pool, "0.10.in-addr.arpa")
            .await
            .unwrap();
        assert!(zones.is_empty(), "the last shared prefix removes the zone");
    }

    #[crate::sqlx_test]
    async fn rolled_back_reverse_zone_create_can_be_retried(pool: sqlx::PgPool) {
        // PostgreSQL releases both the insert and transaction-scoped zone lock
        // on rollback, so a later transaction can create the zone normally.
        let prefix = "10.0.0.0/16".parse().unwrap();
        let mut setup = pool.begin().await.unwrap();
        insert_network_prefix(&mut setup, "rollback", prefix).await;
        setup.commit().await.unwrap();

        let mut rolled_back = pool.begin().await.unwrap();
        super::ensure_reverse_zones(&[prefix], &mut rolled_back)
            .await
            .unwrap();
        rolled_back.rollback().await.unwrap();

        let mut committed = pool.begin().await.unwrap();
        super::ensure_reverse_zones(&[prefix], &mut committed)
            .await
            .unwrap();
        committed.commit().await.unwrap();

        let zones = super::domain::find_by_name(&pool, "0.10.in-addr.arpa")
            .await
            .unwrap();
        assert_eq!(zones.len(), 1, "the retry creates one live reverse zone");
    }

    #[test]
    fn test_normalize_domain_name() {
        use carbide_test_support::value_scenarios;

        value_scenarios!(
            run = |name: &str| super::normalize_domain(name);
            "strips the trailing dot and folds case to ASCII lowercase" {
                "example.com." => "example.com".to_string(),
                "EXAMPLE.COM." => "example.com".to_string(),
                "Example.Com" => "example.com".to_string(),
            }
        );
    }

    #[test]
    fn normalizes_only_reverse_zone_names() {
        // Only child zones below the ARPA roots use normalized identity.
        // Forward names and the roots themselves keep their existing behavior.
        use carbide_test_support::value_scenarios;

        value_scenarios!(
            run = |name: &str| super::normalize_reverse_zone_name(name);
            "reverse zones" {
                "0.10.IN-ADDR.ARPA." => Some("0.10.in-addr.arpa".to_string()),
                "0.0.d.f.ip6.arpa" => Some("0.0.d.f.ip6.arpa".to_string()),
            }
            "forward and arpa-root names" {
                "tenant.example.com" => None,
                "in-addr.arpa" => None,
                "ip6.arpa." => None,
            }
        );
    }

    #[crate::sqlx_test]
    async fn find_ptr_record_resolves_address_to_hostname(pool: sqlx::PgPool) {
        sqlx::query(
            "INSERT INTO domains (id, name)
             VALUES ('10000000-0000-0000-0000-000000000001', 'dwrt1.com')",
        )
        .execute(&pool)
        .await
        .unwrap();

        sqlx::query(
            "INSERT INTO network_segments (id, name, version)
             VALUES ('20000000-0000-0000-0000-000000000001', 'tenant-segment', 'test')",
        )
        .execute(&pool)
        .await
        .unwrap();

        sqlx::query(
            "INSERT INTO machines (id, dpf)
             VALUES ('host-1', '{\"enabled\": true, \"used_for_ingestion\": false}'::jsonb)",
        )
        .execute(&pool)
        .await
        .unwrap();

        // host-1 has three interfaces on the same domain: the primary, a BMC, and a
        // plain (non-primary, non-BMC) data interface.
        sqlx::query(
            "INSERT INTO machine_interfaces (
                id, machine_id, segment_id, mac_address, domain_id,
                primary_interface, hostname, association_type
             )
             VALUES (
                '30000000-0000-0000-0000-000000000001', 'host-1',
                '20000000-0000-0000-0000-000000000001', '02:00:00:00:00:01',
                '10000000-0000-0000-0000-000000000001', true, 'host-1', 'Machine'
             )",
        )
        .execute(&pool)
        .await
        .unwrap();

        sqlx::query(
            "INSERT INTO machine_interfaces (
                id, machine_id, segment_id, mac_address, domain_id,
                primary_interface, hostname, association_type, interface_type
             )
             VALUES (
                '30000000-0000-0000-0000-000000000002', 'host-1',
                '20000000-0000-0000-0000-000000000001', '02:00:00:00:00:02',
                '10000000-0000-0000-0000-000000000001', false, 'host-1-bmc', 'Machine', 'Bmc'
             )",
        )
        .execute(&pool)
        .await
        .unwrap();

        sqlx::query(
            "INSERT INTO machine_interfaces (
                id, machine_id, segment_id, mac_address, domain_id,
                primary_interface, hostname, association_type
             )
             VALUES (
                '30000000-0000-0000-0000-000000000003', 'host-1',
                '20000000-0000-0000-0000-000000000001', '02:00:00:00:00:03',
                '10000000-0000-0000-0000-000000000001', false, 'host-1-data', 'Machine'
             )",
        )
        .execute(&pool)
        .await
        .unwrap();

        for (interface_id, address) in [
            ("30000000-0000-0000-0000-000000000001", "192.168.0.1"),
            ("30000000-0000-0000-0000-000000000002", "192.168.0.2"),
            ("30000000-0000-0000-0000-000000000003", "192.168.0.3"),
        ] {
            sqlx::query(
                "INSERT INTO machine_interface_addresses (interface_id, address)
                 VALUES ($1::uuid, $2::inet)",
            )
            .bind(interface_id)
            .bind(address)
            .execute(&pool)
            .await
            .unwrap();
        }

        // Primary and BMC interfaces answer PTR (matching the forward shortname view);
        // the plain data interface and an address no interface holds do not.
        let cases = [
            ("192.168.0.1", Some("host-1.dwrt1.com.")),
            ("192.168.0.2", Some("host-1-bmc.dwrt1.com.")),
            ("192.168.0.3", None),
            ("10.9.9.9", None),
        ];
        for (address, expected) in cases {
            let records = super::resource_record::find_ptr_record(&pool, address.parse().unwrap())
                .await
                .unwrap();
            match expected {
                Some(fqdn) => {
                    assert_eq!(records.len(), 1, "address {address}");
                    assert_eq!(records[0].ptr_content, fqdn, "address {address}");
                }
                None => assert!(
                    records.is_empty(),
                    "address {address} should resolve to nothing"
                ),
            }
        }
    }
}

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
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::str::FromStr;

use carbide_network::ip::IpAddressFamily;
use carbide_test_support::query_counter::count_queries;
use mac_address::MacAddress;
use model::address_selection_strategy::AddressSelectionStrategy;
use model::allocation_type::AllocationType;
use model::dns::{Domain, NewDomain};
use model::network_prefix::NewNetworkPrefix;
use model::network_segment::{
    AllocationStrategy, NetworkSegmentControllerState, NetworkSegmentType, NewNetworkSegment,
};

use crate as db;
use crate::test_support::network_segment::admin_segment;
use crate::test_support::postgres::wait_for_blocked_query;

async fn init_dwrt1_domain(txn: &mut sqlx::PgTransaction<'_>) -> db::DatabaseResult<Domain> {
    db::dns::domain::persist(NewDomain::new("dwrt1.com"), txn.as_mut()).await
}

/// Test that machine_interface::create allocates the correct IPv4 address
/// from the admin segment (192.0.2.0/24 with num_reserved=3, gateway=.1).
#[crate::sqlx_test]
async fn test_machine_interface_create_with_ipv4_prefix(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = pool.begin().await?;

    let network_segment = db::network_segment::persist(
        admin_segment("ADMIN_TEST", "192.0.2.0/24", "192.0.2.1", 3),
        &mut txn,
        NetworkSegmentControllerState::Ready,
    )
    .await?;
    let network_prefix = network_segment
        .prefixes
        .first()
        .expect("network_segment should have had at least one prefix");

    // The next IP should be .3, since num_reserved = 3.
    let expected_ip = match network_prefix.prefix.ip() {
        IpAddr::V4(ip) => {
            let [o1, o2, o3, _] = ip.octets();
            Ipv4Addr::new(
                o1,
                o2,
                o3,
                network_prefix
                    .num_reserved
                    .try_into()
                    .expect("too many reserved IPs in admin segment"),
            )
        }
        _ => panic!("admin segment should have an IPv4 prefix"),
    };

    let interface = db::machine_interface::create(
        &mut txn,
        std::slice::from_ref(&network_segment),
        MacAddress::from_str("ff:ff:ff:ff:ff:ff").as_ref().unwrap(),
        true,
        AddressSelectionStrategy::NextAvailableIp,
        None,
    )
    .await
    .unwrap();

    assert_eq!(
        interface.addresses.len(),
        1,
        "interface should have 1 address allocated"
    );
    assert_eq!(
        interface.addresses[0], expected_ip,
        "interface address should be the first available IP after reserved"
    );

    // Allocate a second interface and verify it gets a different address
    let interface2 = db::machine_interface::create(
        &mut txn,
        std::slice::from_ref(&network_segment),
        &MacAddress::from_str("ff:ff:ff:ff:ff:fe").unwrap(),
        false,
        AddressSelectionStrategy::NextAvailableIp,
        None,
    )
    .await
    .unwrap();

    assert_ne!(
        interface.addresses[0], interface2.addresses[0],
        "two allocations should produce different addresses"
    );

    Ok(())
}

#[crate::sqlx_test]
async fn test_allocation_skips_a_long_range_of_parked_reservations(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = pool.begin().await?;
    let segment = db::network_segment::persist(
        admin_segment("OCCUPIED-RANGE", "192.0.0.0/20", "192.0.0.1", 2),
        &mut txn,
        NetworkSegmentControllerState::Ready,
    )
    .await?;

    // The first free host sits between a run of 1,100 parked reservations and
    // another reservation, so selecting it requires finding an interior gap.
    let reservations = sqlx::query(
        "INSERT INTO machine_interface_addresses (address, allocation_type, reserved_by_mac)
         SELECT '192.0.0.0'::inet + n, 'static', ('0200' || lpad(to_hex(n), 8, '0'))::macaddr
         FROM (SELECT generate_series(2, 1101) AS n UNION ALL SELECT 1103) AS reserved",
    )
    .execute(txn.as_mut())
    .await?;
    assert_eq!(reservations.rows_affected(), 1_101);
    txn.commit().await?;

    let mut txn = pool.begin().await?;
    let interface = db::machine_interface::create(
        &mut txn,
        std::slice::from_ref(&segment),
        &"aa:bb:cc:dd:49:08".parse()?,
        true,
        AddressSelectionStrategy::NextAvailableIp,
        None,
    )
    .await?;
    let interface_id = interface.id;
    txn.commit().await?;

    let mut conn = pool.acquire().await?;
    let persisted = db::machine_interface::find_one(conn.as_mut(), interface_id).await?;
    assert_eq!(persisted.addresses, vec!["192.0.4.78".parse::<IpAddr>()?]);
    Ok(())
}

#[crate::sqlx_test]
async fn test_ipv4_allocation_advances_past_a_locked_candidate_batch(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    struct Case {
        scenario: &'static str,
        third_octet: u8,
        last_locked_octet: u8,
        expected_octet: u8,
    }
    for case in [
        Case {
            scenario: "try later candidates in a partially locked batch",
            third_octet: 22,
            last_locked_octet: 3,
            expected_octet: 4,
        },
        Case {
            scenario: "advance by whole batches and leave earlier ownership behind",
            third_octet: 23,
            last_locked_octet: 194,
            expected_octet: 195,
        },
    ] {
        let mut txn = pool.begin().await?;
        let segment = db::network_segment::persist(
            admin_segment(
                case.scenario,
                &format!("192.0.{}.0/24", case.third_octet),
                &format!("192.0.{}.1", case.third_octet),
                2,
            ),
            &mut txn,
            NetworkSegmentControllerState::Ready,
        )
        .await?;
        let parked = db::machine_interface_address::ReservedAddress {
            address: Ipv4Addr::new(192, 0, case.third_octet, 2).into(),
            reserved_by_mac: format!("aa:bb:cc:dd:49:{:02x}", case.third_octet).parse()?,
            allocation_type: AllocationType::Static,
        };
        db::machine_interface_address::insert_reserved(
            txn.as_mut(),
            parked.reserved_by_mac,
            parked.address,
            parked.allocation_type,
        )
        .await?;
        txn.commit().await?;

        // The parked .2 remains an owner when later batches move the SQL
        // lower bound. Advisory claims have no address rows to filter out.
        let mut blocker = pool.begin().await?;
        super::lock_network_segments_shared(&mut blocker, &[segment.id]).await?;
        for last_octet in 3..=case.last_locked_octet {
            assert!(
                super::try_lock_ip_candidate(
                    &mut blocker,
                    &segment,
                    Ipv4Addr::new(192, 0, case.third_octet, last_octet).into(),
                )
                .await?
            );
        }

        let mut txn = pool.begin().await?;
        let mac = "aa:bb:cc:dd:49:01".parse()?;
        let interface = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            db::machine_interface::create(
                &mut txn,
                std::slice::from_ref(&segment),
                &mac,
                true,
                AddressSelectionStrategy::NextAvailableIp,
                None,
            ),
        )
        .await??;
        let interface_id = interface.id;
        txn.commit().await?;

        let mut conn = pool.acquire().await?;
        let persisted = db::machine_interface::find_one(conn.as_mut(), interface_id).await?;
        assert_eq!(
            persisted.addresses,
            vec![IpAddr::V4(Ipv4Addr::new(
                192,
                0,
                case.third_octet,
                case.expected_octet,
            ))],
            "{}",
            case.scenario
        );
        assert_eq!(
            db::machine_interface_address::find_reserved(
                conn.as_mut(),
                Some(parked.reserved_by_mac),
                Some(parked.address),
            )
            .await?,
            vec![parked],
            "{}",
            case.scenario
        );
        blocker.rollback().await?;
    }
    Ok(())
}

#[crate::sqlx_test]
async fn test_locked_candidates_before_an_owned_tail_remain_retryable(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = pool.begin().await?;
    let segment = db::network_segment::persist(
        admin_segment("LOCKED-BEFORE-OWNED-TAIL", "192.0.30.0/26", "192.0.30.1", 2),
        &mut txn,
        NetworkSegmentControllerState::Ready,
    )
    .await?;
    sqlx::query(
        "INSERT INTO machine_interface_addresses (address, allocation_type, reserved_by_mac)
         SELECT '192.0.30.0'::inet + n, 'static', ('0200' || lpad(to_hex(n), 8, '0'))::macaddr
         FROM generate_series(34, 62) AS n",
    )
    .execute(txn.as_mut())
    .await?;
    txn.commit().await?;

    let mut blocker = pool.begin().await?;
    super::lock_network_segments_shared(&mut blocker, &[segment.id]).await?;
    for last_octet in 2..=33 {
        assert!(
            super::try_lock_ip_candidate(
                &mut blocker,
                &segment,
                Ipv4Addr::new(192, 0, 30, last_octet).into(),
            )
            .await?
        );
    }

    // The next query is still within the prefix but finds only owned addresses.
    // The earlier candidates are locked, so this is temporary contention.
    let mut txn = pool.begin().await?;
    let mac = "aa:bb:cc:dd:49:30".parse()?;
    let error = db::machine_interface::create(
        &mut txn,
        std::slice::from_ref(&segment),
        &mac,
        true,
        AddressSelectionStrategy::NextAvailableIp,
        None,
    )
    .await
    .unwrap_err();
    assert!(matches!(error, db::DatabaseError::TryAgain), "{error:?}");
    txn.commit().await?;
    blocker.rollback().await?;

    let mut txn = pool.begin().await?;
    let interface = db::machine_interface::create(
        &mut txn,
        std::slice::from_ref(&segment),
        &mac,
        true,
        AddressSelectionStrategy::NextAvailableIp,
        None,
    )
    .await?;
    txn.commit().await?;
    let mut conn = pool.acquire().await?;
    let persisted = db::machine_interface::find_one(conn.as_mut(), interface.id).await?;
    assert_eq!(persisted.addresses, vec!["192.0.30.2".parse::<IpAddr>()?]);
    Ok(())
}

#[crate::sqlx_test]
async fn test_dynamic_allocation_retries_late_conflict_and_rolls_back_partial_addresses(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = pool.begin().await?;
    let mut definition =
        admin_segment("DUAL-STACK-LATE-CONFLICT", "192.0.23.0/24", "192.0.23.1", 2);
    definition.prefixes.push(NewNetworkPrefix {
        prefix: "2001:db8:4902::/64".parse()?,
        gateway: None,
        dhcpv6_link_address: None,
        num_reserved: 2,
    });
    let mut segment =
        db::network_segment::persist(definition, &mut txn, NetworkSegmentControllerState::Ready)
            .await?;
    // Insert IPv4 first so the IPv6 conflict must roll back an assigned address.
    segment
        .prefixes
        .sort_by_key(|prefix| prefix.prefix.is_ipv6());
    txn.commit().await?;

    let reserved_mac = "aa:bb:cc:dd:49:04".parse()?;
    let reserved_address = "2001:db8:4902::2".parse()?;
    let mut owner_txn = pool.begin().await?;
    db::machine_interface_address::insert_reserved(
        &mut owner_txn,
        reserved_mac,
        reserved_address,
        AllocationType::Static,
    )
    .await?;
    let owner_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *owner_txn)
        .await?;

    // The uncommitted reservation is invisible during candidate selection.
    // Commit it only after the allocator reaches the unique ownership check.
    let mac = "aa:bb:cc:dd:49:05".parse()?;
    let allocate = async {
        let mut txn = pool.begin().await?;
        let interface = db::machine_interface::create(
            &mut txn,
            std::slice::from_ref(&segment),
            &mac,
            true,
            AddressSelectionStrategy::NextAvailableIp,
            None,
        )
        .await?;
        txn.commit().await?;
        Ok::<_, Box<dyn std::error::Error>>(interface.id)
    };
    let commit_owner = async {
        wait_for_blocked_query(
            &pool,
            owner_pid,
            "INSERT INTO machine_interface_addresses (interface_id",
        )
        .await;
        owner_txn.commit().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    };
    let (interface_id, ()) = tokio::try_join!(allocate, commit_owner)?;

    let mut conn = pool.acquire().await?;
    let interfaces = db::machine_interface::find_by_mac_address(conn.as_mut(), mac).await?;
    assert_eq!(interfaces.len(), 1);
    assert_eq!(interfaces[0].id, interface_id);
    let mut addresses = db::machine_interface_address::find_for_interface(&mut conn, interface_id)
        .await?
        .into_iter()
        .map(|address| (address.address, address.allocation_type))
        .collect::<Vec<_>>();
    addresses.sort_by_key(|(address, _)| *address);
    // Reusing the first IPv4 candidate proves the failed attempt rolled back
    // its partial IPv4 address row.
    assert_eq!(
        addresses,
        vec![
            ("192.0.23.2".parse::<IpAddr>()?, AllocationType::Dhcp),
            ("2001:db8:4902::3".parse::<IpAddr>()?, AllocationType::Dhcp),
        ]
    );
    assert_eq!(
        db::machine_interface_address::find_reserved(&mut conn, Some(reserved_mac), None).await?,
        vec![db::machine_interface_address::ReservedAddress {
            address: reserved_address,
            reserved_by_mac: reserved_mac,
            allocation_type: AllocationType::Static,
        }]
    );
    Ok(())
}

#[crate::sqlx_test]
async fn test_machine_interface_rejects_prefix_strategy_even_at_host_width(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = pool.begin().await?;
    let segment = db::network_segment::persist(
        admin_segment("PREFIX-STRATEGY", "192.0.24.0/24", "192.0.24.1", 2),
        &mut txn,
        NetworkSegmentControllerState::Ready,
    )
    .await?;
    let mac = "aa:bb:cc:dd:49:06".parse()?;
    let error = db::machine_interface::create(
        &mut txn,
        std::slice::from_ref(&segment),
        &mac,
        true,
        AddressSelectionStrategy::NextAvailablePrefix(32),
        None,
    )
    .await
    .unwrap_err();
    assert!(matches!(error, db::DatabaseError::InvalidArgument(_)));
    txn.commit().await?;

    assert!(
        db::machine_interface::find_by_mac_address(&pool, mac)
            .await?
            .is_empty()
    );
    let address_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM machine_interface_addresses
         WHERE address BETWEEN $1::inet AND $2::inet",
    )
    .bind(segment.prefixes[0].prefix.network())
    .bind(segment.prefixes[0].prefix.broadcast())
    .fetch_one(&pool)
    .await?;
    assert_eq!(address_count, 0);
    Ok(())
}

/// Verify that machine_interface::create falls through to a later candidate
/// admin segment when the first candidate is exhausted.
#[crate::sqlx_test]
async fn test_machine_interface_create_falls_through_admin_segments(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = pool.begin().await?;

    // Create two tiny admin segments with one allocatable address each.
    let first_segment = db::network_segment::persist(
        admin_segment("ADMIN_TINY_1", "192.0.20.0/30", "192.0.20.1", 2),
        &mut txn,
        NetworkSegmentControllerState::Ready,
    )
    .await?;
    let second_segment = db::network_segment::persist(
        admin_segment("ADMIN_TINY_2", "192.0.21.0/30", "192.0.21.1", 2),
        &mut txn,
        NetworkSegmentControllerState::Ready,
    )
    .await?;
    let candidate_segments = [first_segment.clone(), second_segment.clone()];

    // Allocate the only usable address from the first candidate segment.
    let first_interface = db::machine_interface::create(
        &mut txn,
        &candidate_segments,
        &MacAddress::from_str("aa:bb:cc:dd:ee:10").unwrap(),
        true,
        AddressSelectionStrategy::NextAvailableIp,
        None,
    )
    .await?;
    assert_eq!(first_interface.segment_id, first_segment.id);
    assert_eq!(
        first_interface.addresses,
        vec![IpAddr::V4("192.0.20.2".parse()?)]
    );

    // Allocate again with the same candidates and verify it falls through.
    let second_interface = db::machine_interface::create(
        &mut txn,
        &candidate_segments,
        &MacAddress::from_str("aa:bb:cc:dd:ee:11").unwrap(),
        false,
        AddressSelectionStrategy::NextAvailableIp,
        None,
    )
    .await?;

    let second_interface_id = second_interface.id;
    txn.commit().await?;

    // Re-read the second interface to verify the persisted segment and address.
    let mut txn = pool.begin().await?;
    let persisted_interface =
        db::machine_interface::find_one(txn.as_mut(), second_interface_id).await?;
    assert_eq!(persisted_interface.segment_id, second_segment.id);
    assert_eq!(
        persisted_interface.addresses,
        vec![IpAddr::V4("192.0.21.2".parse()?)]
    );

    Ok(())
}

#[crate::sqlx_test]
async fn test_segment_fallback_preserves_candidate_contention(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = pool.begin().await?;
    let first_segment = db::network_segment::persist(
        admin_segment("LOCKED-FIRST", "192.0.26.0/30", "192.0.26.1", 2),
        &mut txn,
        NetworkSegmentControllerState::Ready,
    )
    .await?;
    let second_segment = db::network_segment::persist(
        admin_segment("FALLBACK-SECOND", "192.0.27.0/30", "192.0.27.1", 2),
        &mut txn,
        NetworkSegmentControllerState::Ready,
    )
    .await?;
    txn.commit().await?;
    let candidate_segments = [first_segment.clone(), second_segment.clone()];
    let first_address = "192.0.26.2".parse()?;
    let mut blocker = pool.begin().await?;
    super::lock_network_segments_shared(&mut blocker, &[first_segment.id]).await?;
    assert!(super::try_lock_ip_candidate(&mut blocker, &first_segment, first_address).await?);

    // Contention must not replace a later error unrelated to address availability.
    let mut txn = pool.begin().await?;
    let duplicate_mac: MacAddress = "aa:bb:cc:dd:49:15".parse()?;
    sqlx::query(
        "INSERT INTO machine_interfaces
         (segment_id, mac_address, primary_interface, hostname)
         VALUES ($1, $2, false, 'duplicate-mac')",
    )
    .bind(second_segment.id)
    .bind(duplicate_mac)
    .execute(txn.as_mut())
    .await?;
    let error = db::machine_interface::create(
        &mut txn,
        &candidate_segments,
        &duplicate_mac,
        false,
        AddressSelectionStrategy::NextAvailableIp,
        None,
    )
    .await
    .unwrap_err();
    assert!(
        matches!(error, db::DatabaseError::NetworkSegmentDuplicateMacAddress(mac) if mac == duplicate_mac),
        "{error:?}"
    );
    txn.commit().await?;

    // A locked first segment still permits allocation from a later segment.
    let mut txn = pool.begin().await?;
    let interface = db::machine_interface::create(
        &mut txn,
        &candidate_segments,
        &"aa:bb:cc:dd:49:10".parse()?,
        true,
        AddressSelectionStrategy::NextAvailableIp,
        None,
    )
    .await?;
    txn.commit().await?;
    let mut conn = pool.acquire().await?;
    let persisted = db::machine_interface::find_one(conn.as_mut(), interface.id).await?;
    assert_eq!(persisted.segment_id, second_segment.id);
    assert_eq!(persisted.addresses, vec!["192.0.27.2".parse::<IpAddr>()?]);
    drop(conn);

    // The fallback is now full, but the first segment is only temporarily busy.
    let mut txn = pool.begin().await?;
    let (result, query_count) = count_queries(db::machine_interface::create(
        &mut txn,
        &candidate_segments,
        &"aa:bb:cc:dd:49:11".parse()?,
        false,
        AddressSelectionStrategy::NextAvailableIp,
        None,
    ))
    .await;
    let error = result.unwrap_err();
    let db::DatabaseError::Internal { message } = error else {
        panic!("expected contention and exhaustion details, got {error:?}");
    };
    assert_eq!(
        message,
        format!(
            "host address allocation can be retried after earlier lock contention; final segment `FALLBACK-SECOND` ({}) is exhausted: No IP addresses left in prefix 192.0.27.0/30",
            second_segment.id
        )
    );
    // Four statements for contention, then three for the full fallback.
    // Advancing past the /30's only candidate needs no further gap query.
    assert_eq!(query_count, 7);
    txn.commit().await?;
    blocker.rollback().await?;

    // Once the first address has an owner, both segments are exhausted.
    let mut txn = pool.begin().await?;
    db::machine_interface_address::insert_reserved(
        &mut txn,
        "aa:bb:cc:dd:49:12".parse()?,
        first_address,
        AllocationType::Static,
    )
    .await?;
    let error = db::machine_interface::create(
        &mut txn,
        &candidate_segments,
        &"aa:bb:cc:dd:49:11".parse()?,
        false,
        AddressSelectionStrategy::NextAvailableIp,
        None,
    )
    .await
    .unwrap_err();
    assert!(
        matches!(error, db::DatabaseError::ResourceExhausted(_)),
        "{error:?}"
    );
    txn.commit().await?;
    Ok(())
}

#[crate::sqlx_test]
async fn test_hostname_retry_error_is_not_replaced_by_earlier_contention(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = pool.begin().await?;
    let domain = init_dwrt1_domain(&mut txn).await?;
    let first_segment = db::network_segment::persist(
        admin_segment("LOCKED-FIRST", "192.0.28.0/30", "192.0.28.1", 2),
        &mut txn,
        NetworkSegmentControllerState::Ready,
    )
    .await?;
    let mut definition = admin_segment("FQDN-CONFLICT", "192.0.29.0/30", "192.0.29.1", 2);
    definition.subdomain_id = Some(domain.id);
    let second_segment =
        db::network_segment::persist(definition, &mut txn, NetworkSegmentControllerState::Ready)
            .await?;

    // The address is free, but another interface retains its IP-derived name.
    // Creation reaches the actual FQDN uniqueness constraint.
    sqlx::query(
        "INSERT INTO machine_interfaces
         (segment_id, mac_address, primary_interface, hostname, domain_id)
         VALUES ($1, 'aa:bb:cc:dd:49:13'::macaddr, false, '192-0-29-2', $2)",
    )
    .bind(second_segment.id)
    .bind(domain.id)
    .execute(txn.as_mut())
    .await?;
    txn.commit().await?;

    let mut blocker = pool.begin().await?;
    super::lock_network_segments_shared(&mut blocker, &[first_segment.id]).await?;
    assert!(
        super::try_lock_ip_candidate(&mut blocker, &first_segment, "192.0.28.2".parse()?).await?
    );
    let mut txn = pool.begin().await?;
    let (result, query_count) = count_queries(db::machine_interface::create(
        &mut txn,
        &[first_segment.clone(), second_segment.clone()],
        &"aa:bb:cc:dd:49:14".parse()?,
        false,
        AddressSelectionStrategy::NextAvailableIp,
        None,
    ))
    .await;
    let error = result.unwrap_err();
    let db::DatabaseError::Internal { message } = error else {
        panic!("expected the hostname retry error, got {error:?}");
    };
    assert_eq!(
        message,
        format!(
            "ownership or hostname retries exhausted while allocating machine interface addresses from segments `LOCKED-FIRST` ({}), `FQDN-CONFLICT` ({})",
            first_segment.id, second_segment.id
        )
    );
    // Four statements for contention, then five per hostname attempt. SQLx
    // may also resolve the interface_type enum once on this connection.
    let expected_queries = 4 + super::HOST_ALLOCATION_MAX_ATTEMPTS * 5;
    assert!(
        (expected_queries..=expected_queries + 1).contains(&query_count),
        "expected {expected_queries} queries plus at most one type lookup, got {query_count}"
    );
    txn.commit().await?;
    blocker.rollback().await?;
    Ok(())
}

/// Verify that machine_interface::create allocates from an IPv6-only segment.
#[crate::sqlx_test]
async fn test_machine_interface_create_with_ipv6_prefix(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = pool.begin().await?;
    let domain = init_dwrt1_domain(&mut txn).await?;

    // Create an underlay segment with only an IPv6 prefix
    let new_ns = NewNetworkSegment {
        name: "IPV6-UNDERLAY-TEST".to_string(),
        subdomain_id: Some(domain.id),
        vpc_id: None,
        mtu: 1500,
        prefixes: vec![NewNetworkPrefix {
            prefix: "2001:db8:abcd::0/112".parse().unwrap(),
            gateway: None,
            dhcpv6_link_address: None,
            num_reserved: 2,
        }],
        vlan_id: None,
        vni: None,
        segment_type: NetworkSegmentType::Underlay,
        id: uuid::Uuid::new_v4().into(),
        can_stretch: None,
        allocation_strategy: AllocationStrategy::Dynamic,
        infer_slaac_eui64_addresses: false,
    };
    let network_segment =
        db::network_segment::persist(new_ns, &mut txn, NetworkSegmentControllerState::Ready)
            .await?;

    let interface = db::machine_interface::create(
        &mut txn,
        std::slice::from_ref(&network_segment),
        &MacAddress::from_str("aa:bb:cc:dd:ee:01").unwrap(),
        true,
        AddressSelectionStrategy::NextAvailableIp,
        None,
    )
    .await?;

    assert_eq!(
        interface.addresses.len(),
        1,
        "interface should have 1 address allocated"
    );
    let addr = interface.addresses[0];
    assert!(
        addr.is_ipv6(),
        "allocated address should be IPv6, got {addr}"
    );

    // Allocate a second interface to verify sequential allocation works
    let interface2 = db::machine_interface::create(
        &mut txn,
        std::slice::from_ref(&network_segment),
        &MacAddress::from_str("aa:bb:cc:dd:ee:02").unwrap(),
        false,
        AddressSelectionStrategy::NextAvailableIp,
        None,
    )
    .await?;

    let addr2 = interface2.addresses[0];
    assert!(
        addr2.is_ipv6(),
        "second address should be IPv6, got {addr2}"
    );
    assert_ne!(
        addr, addr2,
        "two allocations should produce different addresses"
    );

    Ok(())
}

/// An address can still belong to an interface on `static-assignments` when a
/// managed prefix is added around it later. Both initial DHCPv6 allocation and
/// family recovery must skip that address even though its owning interface
/// remains on a different segment.
#[crate::sqlx_test]
async fn test_ipv6_allocation_skips_address_owned_outside_managed_segment(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = pool.begin().await?;

    let static_assignments = db::network_segment::persist(
        NewNetworkSegment {
            name: db::network_segment::STATIC_ASSIGNMENTS_SEGMENT_NAME.to_string(),
            subdomain_id: None,
            vpc_id: None,
            mtu: 1500,
            prefixes: vec![NewNetworkPrefix {
                prefix: "169.254.254.254/32".parse()?,
                gateway: None,
                dhcpv6_link_address: None,
                num_reserved: 1,
            }],
            vlan_id: None,
            vni: None,
            segment_type: NetworkSegmentType::Underlay,
            id: uuid::Uuid::new_v4().into(),
            can_stretch: Some(false),
            allocation_strategy: AllocationStrategy::Reserved,
            infer_slaac_eui64_addresses: false,
        },
        &mut txn,
        NetworkSegmentControllerState::Ready,
    )
    .await?;
    let occupied_address: IpAddr = "2001:db8:4801::2".parse()?;
    db::machine_interface::preallocate_machine_interface(
        &mut txn,
        MacAddress::from_str("aa:bb:cc:dd:48:01")?,
        occupied_address,
        None,
    )
    .await?;

    let owner = db::machine_interface_address::find_by_address(txn.as_mut(), occupied_address)
        .await?
        .expect("static IPv6 owner should exist");
    assert_eq!(owner.segment_id, static_assignments.id);
    let owner_interface_id = owner.id;

    // Add the managed prefix after the static reservation. Its first DHCP
    // candidate is already owned by the interface on `static-assignments`.
    let managed_segment = db::network_segment::persist(
        NewNetworkSegment {
            name: "IPV6-GLOBAL-OWNER".to_string(),
            subdomain_id: None,
            vpc_id: None,
            mtu: 1500,
            prefixes: vec![NewNetworkPrefix {
                prefix: "2001:db8:4801::/112".parse()?,
                gateway: None,
                dhcpv6_link_address: None,
                num_reserved: 2,
            }],
            vlan_id: None,
            vni: None,
            segment_type: NetworkSegmentType::Underlay,
            id: uuid::Uuid::new_v4().into(),
            can_stretch: Some(false),
            allocation_strategy: AllocationStrategy::Dynamic,
            infer_slaac_eui64_addresses: false,
        },
        &mut txn,
        NetworkSegmentControllerState::Ready,
    )
    .await?;
    let next_available_address: IpAddr = "2001:db8:4801::3".parse()?;

    let interface = db::machine_interface::create(
        &mut txn,
        std::slice::from_ref(&managed_segment),
        &MacAddress::from_str("aa:bb:cc:dd:48:02")?,
        true,
        AddressSelectionStrategy::NextAvailableIp,
        None,
    )
    .await?;
    assert_eq!(interface.addresses, vec![next_available_address]);

    db::machine_interface_address::delete(&mut txn, &interface.id).await?;
    let recovered = db::machine_interface::allocate_address_for_family(
        &mut txn,
        interface.id,
        &managed_segment,
        IpAddressFamily::Ipv6,
        AllocationType::Dhcp,
    )
    .await?;
    assert_eq!(recovered, vec![next_available_address]);

    let owner = db::machine_interface_address::find_by_address(txn.as_mut(), occupied_address)
        .await?
        .expect("static IPv6 owner should remain unchanged");
    assert_eq!(owner.id, owner_interface_id);

    Ok(())
}

#[crate::sqlx_test]
async fn test_ipv6_allocation_skips_parked_reservation(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = pool.begin().await?;
    let mut definition = admin_segment("IPV6-PARKED-RESERVATION", "::/0", "::1", 2);
    // The schema rejects gateways on IPv6 prefixes.
    definition.prefixes[0].gateway = None;
    let segment =
        db::network_segment::persist(definition, &mut txn, NetworkSegmentControllerState::Ready)
            .await?;
    let reservations = [
        db::machine_interface_address::ReservedAddress {
            address: "ffff:ffff:ffff:ffff:ffff:ffff:ffff:fffe".parse()?,
            reserved_by_mac: "aa:bb:cc:dd:49:02".parse()?,
            allocation_type: AllocationType::Static,
        },
        db::machine_interface_address::ReservedAddress {
            address: "::2".parse()?,
            reserved_by_mac: "aa:bb:cc:dd:49:03".parse()?,
            allocation_type: AllocationType::Static,
        },
        db::machine_interface_address::ReservedAddress {
            address: "::3".parse()?,
            reserved_by_mac: "aa:bb:cc:dd:49:04".parse()?,
            allocation_type: AllocationType::Static,
        },
    ];
    // Parked reservations have no interface row, but still own their addresses.
    // Insert out of address order so a heap scan cannot supply the selector's order.
    // The huge gap starts at ::3, after the first occupied row, so selection must
    // evaluate its gap predicate without overflowing a signed 64-bit subtraction.
    for reservation in &reservations {
        db::machine_interface_address::insert_reserved(
            &mut txn,
            reservation.reserved_by_mac,
            reservation.address,
            reservation.allocation_type,
        )
        .await?;
    }
    txn.commit().await?;

    let mut txn = pool.begin().await?;
    sqlx::raw_sql(
        "SET LOCAL enable_indexscan = off;
         SET LOCAL enable_indexonlyscan = off;
         SET LOCAL enable_bitmapscan = off;",
    )
    .execute(&mut *txn)
    .await?;
    let mac = "aa:bb:cc:dd:49:07".parse()?;
    let interface = db::machine_interface::create(
        &mut txn,
        std::slice::from_ref(&segment),
        &mac,
        true,
        AddressSelectionStrategy::NextAvailableIp,
        None,
    )
    .await?;
    let interface_id = interface.id;
    txn.commit().await?;

    let mut conn = pool.acquire().await?;
    let persisted = db::machine_interface::find_one(conn.as_mut(), interface_id).await?;
    assert_eq!(persisted.addresses, vec!["::4".parse::<IpAddr>()?]);
    assert_eq!(
        db::machine_interface_address::find_reserved(&mut conn, None, None).await?,
        reservations
    );
    Ok(())
}

/// Verify that a dual-stack segment (IPv4 + IPv6 prefixes) allocates one
/// address from each family, and that the hostname is derived from the IPv4
/// address (more human-readable).
#[crate::sqlx_test]
async fn test_machine_interface_create_dual_stack(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = pool.begin().await?;
    let domain = init_dwrt1_domain(&mut txn).await?;

    let new_ns = NewNetworkSegment {
        name: "DUAL-STACK-TEST".to_string(),
        subdomain_id: Some(domain.id),
        vpc_id: None,
        mtu: 1500,
        prefixes: vec![
            NewNetworkPrefix {
                prefix: "10.99.1.0/24".parse().unwrap(),
                gateway: Some("10.99.1.1".parse().unwrap()),
                dhcpv6_link_address: None,
                num_reserved: 1,
            },
            NewNetworkPrefix {
                prefix: "2001:db8:99::0/112".parse().unwrap(),
                gateway: None,
                dhcpv6_link_address: None,
                num_reserved: 1,
            },
        ],
        vlan_id: None,
        vni: None,
        segment_type: NetworkSegmentType::Underlay,
        id: uuid::Uuid::new_v4().into(),
        can_stretch: None,
        allocation_strategy: AllocationStrategy::Dynamic,
        infer_slaac_eui64_addresses: false,
    };
    let network_segment =
        db::network_segment::persist(new_ns, &mut txn, NetworkSegmentControllerState::Ready)
            .await?;

    let interface = db::machine_interface::create(
        &mut txn,
        std::slice::from_ref(&network_segment),
        &MacAddress::from_str("aa:bb:cc:00:00:01").unwrap(),
        true,
        AddressSelectionStrategy::NextAvailableIp,
        None,
    )
    .await?;

    // Dual-stack: should have one IPv4 and one IPv6 address
    assert_eq!(
        interface.addresses.len(),
        2,
        "dual-stack interface should have 2 addresses, got {:?}",
        interface.addresses
    );

    let has_v4 = interface.addresses.iter().any(|a| a.is_ipv4());
    let has_v6 = interface.addresses.iter().any(|a| a.is_ipv6());
    assert!(has_v4, "should have an IPv4 address");
    assert!(has_v6, "should have an IPv6 address");

    // Hostname should be derived from the IPv4 address (preferred for readability)
    assert!(
        !interface.hostname.contains(':'),
        "hostname should be derived from IPv4 (no colons), got: {}",
        interface.hostname
    );
    assert!(
        interface.hostname.contains('-'),
        "hostname should have dashes from IPv4 dot replacement, got: {}",
        interface.hostname
    );

    // Allocate a second interface and verify both families get new addresses
    let interface2 = db::machine_interface::create(
        &mut txn,
        std::slice::from_ref(&network_segment),
        &MacAddress::from_str("aa:bb:cc:00:00:02").unwrap(),
        false,
        AddressSelectionStrategy::NextAvailableIp,
        None,
    )
    .await?;

    assert_eq!(
        interface2.addresses.len(),
        2,
        "second dual-stack interface should also have 2 addresses"
    );

    // No addresses should overlap between the two interfaces
    for addr in &interface.addresses {
        assert!(
            !interface2.addresses.contains(addr),
            "address {addr} was allocated to both interfaces"
        );
    }

    Ok(())
}

/// Initial allocation and family recovery find the same first free IPv6 host,
/// including when the prefix covers the entire IPv6 address space.
#[crate::sqlx_test]
async fn test_machine_interface_ipv6_allocation_across_prefix_widths(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    for (index, (name, prefix)) in [
        ("IPV6-PREFIX-0", "::/0"),
        ("IPV6-PREFIX-64", "2001:db8:6400::/64"),
        ("IPV6-PREFIX-65", "2001:db8:6500::/65"),
        ("IPV6-PREFIX-80", "2001:db8:8000::/80"),
        ("IPV6-PREFIX-96", "2001:db8:9600::/96"),
        ("IPV6-PREFIX-97", "2001:db8:9700::/97"),
        ("IPV6-PREFIX-112", "2001:db8:1120::/112"),
    ]
    .into_iter()
    .enumerate()
    {
        // Isolate each prefix so /0 does not overlap the other fixtures.
        let mut txn = pool.begin().await?;
        let network_segment = db::network_segment::persist(
            NewNetworkSegment {
                name: name.to_string(),
                subdomain_id: None,
                vpc_id: None,
                mtu: 1500,
                prefixes: vec![NewNetworkPrefix {
                    prefix: prefix.parse().unwrap(),
                    gateway: None,
                    dhcpv6_link_address: None,
                    num_reserved: 2,
                }],
                vlan_id: None,
                vni: None,
                segment_type: NetworkSegmentType::Underlay,
                id: uuid::Uuid::new_v4().into(),
                can_stretch: None,
                allocation_strategy: AllocationStrategy::Dynamic,
                infer_slaac_eui64_addresses: false,
            },
            &mut txn,
            NetworkSegmentControllerState::Ready,
        )
        .await?;

        let IpAddr::V6(network_address) = network_segment.prefixes[0].prefix.network() else {
            panic!("fixture must contain an IPv6 prefix");
        };
        let expected = IpAddr::V6(Ipv6Addr::from(u128::from(network_address) + 2));
        let interface = db::machine_interface::create(
            &mut txn,
            std::slice::from_ref(&network_segment),
            &MacAddress::from_str(&format!("aa:bb:cc:dd:40:{index:02x}")).unwrap(),
            true,
            AddressSelectionStrategy::NextAvailableIp,
            None,
        )
        .await?;
        assert_eq!(interface.addresses, vec![expected], "{name}");

        db::machine_interface_address::delete_by_interface_family(
            &mut txn,
            interface.id,
            IpAddressFamily::Ipv6,
            AllocationType::Dhcp,
        )
        .await?;
        let allocated = db::machine_interface::allocate_address_for_family(
            &mut txn,
            interface.id,
            &network_segment,
            IpAddressFamily::Ipv6,
            AllocationType::Dhcp,
        )
        .await?;
        assert_eq!(allocated, vec![expected], "{name}");
        txn.rollback().await?;
    }

    Ok(())
}

/// The maximum IPv6 host address can be allocated once without arithmetic overflow.
#[crate::sqlx_test]
async fn test_machine_interface_ipv6_single_address_allocation(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = pool.begin().await?;
    let address: IpAddr = "ffff:ffff:ffff:ffff:ffff:ffff:ffff:ffff".parse()?;
    let mut definition = admin_segment(
        "IPV6-SINGLETON",
        "ffff:ffff:ffff:ffff:ffff:ffff:ffff:ffff/128",
        "ffff:ffff:ffff:ffff:ffff:ffff:ffff:ffff",
        0,
    );
    // IPv6 prefixes cannot have a gateway, including this single-address prefix.
    definition.prefixes[0].gateway = None;
    let segment =
        db::network_segment::persist(definition, &mut txn, NetworkSegmentControllerState::Ready)
            .await?;
    let interface = db::machine_interface::create(
        &mut txn,
        std::slice::from_ref(&segment),
        &MacAddress::from_str("aa:bb:cc:dd:42:00")?,
        true,
        AddressSelectionStrategy::NextAvailableIp,
        None,
    )
    .await?;
    let interface_id = interface.id;
    txn.commit().await?;

    let mut txn = pool.begin().await?;
    let persisted = db::machine_interface::find_one(txn.as_mut(), interface_id).await?;
    assert_eq!(persisted.addresses, vec![address]);
    let error = db::machine_interface::create(
        &mut txn,
        std::slice::from_ref(&segment),
        &MacAddress::from_str("aa:bb:cc:dd:42:01")?,
        false,
        AddressSelectionStrategy::NextAvailableIp,
        None,
    )
    .await
    .unwrap_err();
    assert!(matches!(error, db::DatabaseError::ResourceExhausted(_)));
    Ok(())
}

/// Verifies that IPv6 exhaustion on one candidate segment falls through to the
/// next candidate segment instead of aborting allocation.
#[crate::sqlx_test]
async fn test_machine_interface_ipv6_exhausted_segment_falls_through(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = pool.begin().await?;

    // Create one exhausted IPv6 segment followed by one segment with capacity.
    let exhausted_segment = db::network_segment::persist(
        NewNetworkSegment {
            name: "IPV6-EXHAUSTED-FIRST".to_string(),
            subdomain_id: None,
            vpc_id: None,
            mtu: 1500,
            prefixes: vec![NewNetworkPrefix {
                prefix: "2001:db8:7100::/127".parse().unwrap(),
                gateway: None,
                dhcpv6_link_address: None,
                num_reserved: 2,
            }],
            vlan_id: None,
            vni: None,
            segment_type: NetworkSegmentType::Underlay,
            id: uuid::Uuid::new_v4().into(),
            can_stretch: None,
            allocation_strategy: AllocationStrategy::Dynamic,
            infer_slaac_eui64_addresses: false,
        },
        &mut txn,
        NetworkSegmentControllerState::Ready,
    )
    .await?;
    let available_segment = db::network_segment::persist(
        NewNetworkSegment {
            name: "IPV6-AVAILABLE-SECOND".to_string(),
            subdomain_id: None,
            vpc_id: None,
            mtu: 1500,
            prefixes: vec![NewNetworkPrefix {
                prefix: "2001:db8:7200::/126".parse().unwrap(),
                gateway: None,
                dhcpv6_link_address: None,
                num_reserved: 2,
            }],
            vlan_id: None,
            vni: None,
            segment_type: NetworkSegmentType::Underlay,
            id: uuid::Uuid::new_v4().into(),
            can_stretch: None,
            allocation_strategy: AllocationStrategy::Dynamic,
            infer_slaac_eui64_addresses: false,
        },
        &mut txn,
        NetworkSegmentControllerState::Ready,
    )
    .await?;

    // Allocate across both candidates; the exhausted first segment should not abort the request.
    let interface = db::machine_interface::create(
        &mut txn,
        &[exhausted_segment, available_segment.clone()],
        &MacAddress::from_str("aa:bb:cc:dd:41:00").unwrap(),
        true,
        AddressSelectionStrategy::NextAvailableIp,
        None,
    )
    .await?;
    assert_eq!(interface.segment_id, available_segment.id);
    assert_eq!(interface.addresses.len(), 1);
    assert!(
        available_segment.prefixes[0]
            .prefix
            .contains(interface.addresses[0])
    );
    let interface_id = interface.id;
    txn.commit().await?;

    // Re-read the interface to verify the fallback segment persisted.
    let mut txn = pool.begin().await?;
    let persisted = db::machine_interface::find_one(txn.as_mut(), interface_id).await?;
    assert_eq!(persisted.segment_id, available_segment.id);
    assert!(persisted.addresses.iter().any(|address| address.is_ipv6()));

    Ok(())
}

#[crate::sqlx_test]
async fn test_family_recovery_retries_late_conflict_without_rolling_back_sibling(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = pool.begin().await?;
    let mut definition =
        admin_segment("FAMILY-RECOVERY-CONFLICT", "192.0.25.0/24", "192.0.25.1", 2);
    definition.prefixes.push(NewNetworkPrefix {
        prefix: "2001:db8:4909::/64".parse()?,
        gateway: None,
        dhcpv6_link_address: None,
        num_reserved: 2,
    });
    let segment =
        db::network_segment::persist(definition, &mut txn, NetworkSegmentControllerState::Ready)
            .await?;
    let interface = db::machine_interface::create(
        &mut txn,
        std::slice::from_ref(&segment),
        &"aa:bb:cc:dd:49:09".parse()?,
        true,
        AddressSelectionStrategy::NextAvailableIp,
        None,
    )
    .await?;
    assert!(
        db::machine_interface_address::delete_by_interface_family(
            &mut txn,
            interface.id,
            IpAddressFamily::Ipv6,
            AllocationType::Dhcp,
        )
        .await?
    );
    txn.commit().await?;

    let reserved_mac = "aa:bb:cc:dd:49:0a".parse()?;
    let reserved_address = "2001:db8:4909::2".parse()?;
    let mut owner_txn = pool.begin().await?;
    db::machine_interface_address::insert_reserved(
        &mut owner_txn,
        reserved_mac,
        reserved_address,
        AllocationType::Static,
    )
    .await?;
    let owner_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *owner_txn)
        .await?;

    let static_v4 = "192.0.25.42".parse()?;
    let recovered_v6 = "2001:db8:4909::3".parse()?;
    let recover = async {
        let mut txn = pool.begin().await?;
        super::lock_network_segment_exclusive(&mut txn, &segment).await?;
        // This sibling update belongs to the caller's transaction, so retrying
        // IPv6 allocation must preserve both its address and Static type.
        db::machine_interface_address::assign_static(&mut txn, interface.id, static_v4).await?;
        let addresses = db::machine_interface::allocate_address_for_family(
            &mut txn,
            interface.id,
            &segment,
            IpAddressFamily::Ipv6,
            AllocationType::Dhcp,
        )
        .await?;
        assert_eq!(addresses, vec![recovered_v6]);
        txn.commit().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    };
    let commit_owner = async {
        // Selection cannot see the parked reservation until it commits.
        // Wait for the ownership insert to block before making it visible.
        wait_for_blocked_query(
            &pool,
            owner_pid,
            "INSERT INTO machine_interface_addresses (interface_id",
        )
        .await;
        owner_txn.commit().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    };
    tokio::try_join!(recover, commit_owner)?;

    let mut conn = pool.acquire().await?;
    let mut addresses = db::machine_interface_address::find_for_interface(&mut conn, interface.id)
        .await?
        .into_iter()
        .map(|address| (address.address, address.allocation_type))
        .collect::<Vec<_>>();
    addresses.sort_by_key(|(address, _)| *address);
    assert_eq!(
        addresses,
        vec![
            (static_v4, AllocationType::Static),
            (recovered_v6, AllocationType::Dhcp),
        ]
    );
    assert_eq!(
        db::machine_interface_address::find_reserved(&mut conn, Some(reserved_mac), None).await?,
        vec![db::machine_interface_address::ReservedAddress {
            address: reserved_address,
            reserved_by_mac: reserved_mac,
            allocation_type: AllocationType::Static,
        }]
    );
    Ok(())
}

/// Recovering one family preserves the sibling's exact address and allocation
/// type, while the recovered address uses the requested DHCP or Static type.
#[crate::sqlx_test]
async fn test_allocate_address_for_family_dual_stack_round_trip(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = pool.begin().await?;

    // Create a dual-stack admin segment and allocate both families initially.
    let network_segment = db::network_segment::persist(
        NewNetworkSegment {
            name: "DUAL-STACK-REALLOCATE".to_string(),
            subdomain_id: None,
            vpc_id: None,
            mtu: 1500,
            prefixes: vec![
                NewNetworkPrefix {
                    prefix: "10.88.1.0/24".parse().unwrap(),
                    gateway: Some("10.88.1.1".parse().unwrap()),
                    dhcpv6_link_address: None,
                    num_reserved: 1,
                },
                NewNetworkPrefix {
                    prefix: "2001:db8:8801::/64".parse().unwrap(),
                    gateway: None,
                    dhcpv6_link_address: None,
                    num_reserved: 2,
                },
            ],
            vlan_id: None,
            vni: None,
            segment_type: NetworkSegmentType::Underlay,
            id: uuid::Uuid::new_v4().into(),
            can_stretch: None,
            allocation_strategy: AllocationStrategy::Dynamic,
            infer_slaac_eui64_addresses: false,
        },
        &mut txn,
        NetworkSegmentControllerState::Ready,
    )
    .await?;
    let interface = db::machine_interface::create(
        &mut txn,
        std::slice::from_ref(&network_segment),
        &MacAddress::from_str("aa:bb:cc:dd:40:ff").unwrap(),
        true,
        AddressSelectionStrategy::NextAvailableIp,
        None,
    )
    .await?;
    assert_eq!(interface.addresses.len(), 2);

    // Move both addresses away from the first free host so accidental sibling
    // reallocation would change the stored address, even if its type survived.
    let static_v4: IpAddr = "10.88.1.42".parse()?;
    let static_v6: IpAddr = "2001:db8:8801::42".parse()?;
    for address in [static_v4, static_v6] {
        db::machine_interface_address::assign_static(&mut txn, interface.id, address).await?;
    }
    txn.commit().await?;

    struct Case {
        family: IpAddressFamily,
        allocation_type: AllocationType,
        recovered_address: IpAddr,
        expected: [(IpAddr, AllocationType); 2],
    }
    let first_v4 = "10.88.1.2".parse()?;
    let first_v6 = "2001:db8:8801::2".parse()?;
    // These phases share the interface: IPv6 recovery starts with the IPv4
    // address committed by the preceding phase.
    for case in [
        Case {
            family: IpAddressFamily::Ipv4,
            allocation_type: AllocationType::Dhcp,
            recovered_address: first_v4,
            expected: [
                (first_v4, AllocationType::Dhcp),
                (static_v6, AllocationType::Static),
            ],
        },
        Case {
            family: IpAddressFamily::Ipv6,
            allocation_type: AllocationType::Static,
            recovered_address: first_v6,
            expected: [
                (first_v4, AllocationType::Dhcp),
                (first_v6, AllocationType::Static),
            ],
        },
    ] {
        let mut txn = pool.begin().await?;
        assert!(
            db::machine_interface_address::delete_by_interface_family(
                &mut txn,
                interface.id,
                case.family,
                AllocationType::Static,
            )
            .await?
        );
        let allocated = db::machine_interface::allocate_address_for_family(
            &mut txn,
            interface.id,
            &network_segment,
            case.family,
            case.allocation_type,
        )
        .await?;
        assert_eq!(allocated, vec![case.recovered_address], "{:?}", case.family);
        txn.commit().await?;

        let mut conn = pool.acquire().await?;
        let mut persisted =
            db::machine_interface_address::find_for_interface(&mut conn, interface.id)
                .await?
                .into_iter()
                .map(|address| (address.address, address.allocation_type))
                .collect::<Vec<_>>();
        persisted.sort_by_key(|(address, _)| *address);
        assert_eq!(persisted, case.expected, "{:?}", case.family);
        let snapshot = db::machine_interface::find_one(conn.as_mut(), interface.id).await?;
        assert_eq!(snapshot.hostname, "10-88-1-2", "{:?}", case.family);
    }

    // Recover both families from an addressless interface in one caller
    // transaction, including the lock upgrade for IPv6 after IPv4 recovery.
    let mut txn = pool.begin().await?;
    let families = [
        (IpAddressFamily::Ipv4, AllocationType::Dhcp, first_v4),
        (IpAddressFamily::Ipv6, AllocationType::Static, first_v6),
    ];
    for (family, allocation_type, _) in families {
        assert!(
            db::machine_interface_address::delete_by_interface_family(
                &mut txn,
                interface.id,
                family,
                allocation_type,
            )
            .await?
        );
    }
    for (family, allocation_type, expected_address) in families {
        let allocated = db::machine_interface::allocate_address_for_family(
            &mut txn,
            interface.id,
            &network_segment,
            family,
            allocation_type,
        )
        .await?;
        assert_eq!(allocated, vec![expected_address], "{family:?}");
    }
    txn.commit().await?;

    let mut conn = pool.acquire().await?;
    let mut persisted = db::machine_interface_address::find_for_interface(&mut conn, interface.id)
        .await?
        .into_iter()
        .map(|address| (address.address, address.allocation_type))
        .collect::<Vec<_>>();
    persisted.sort_by_key(|(address, _)| *address);
    assert_eq!(
        persisted,
        vec![
            (first_v4, AllocationType::Dhcp),
            (first_v6, AllocationType::Static),
        ]
    );

    Ok(())
}

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
use std::collections::HashSet;
use std::net::IpAddr;
use std::time::{Duration, Instant};

use futures::future::try_join_all;
use mac_address::MacAddress;
use model::address_selection_strategy::AddressSelectionStrategy;
use model::network_segment::NetworkSegmentControllerState;
use sqlx::Acquire;
use sqlx::postgres::PgPoolOptions;
use tokio::sync::Barrier;

use crate::test_support::network_segment::admin_segment;

// Keep occupancy fixed as the prefix grows. Each allocation is rolled back
// so every sample must find the same first gap. Timings include SQLx result
// transfer. Full-create and legacy candidate-only timings have different scopes;
// they are observations, not speedup ratios or CI thresholds.
#[crate::sqlx_test]
#[ignore = "allocation benchmark; run explicitly with --ignored --nocapture"]
async fn benchmark_host_allocation(pool: sqlx::PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let cases = [
        // Prefix, gateway, occupied count, first occupied offset.
        ("10.0.0.0/18", "10.0.0.1", 5_000_i64, 2_i64),
        ("10.0.0.0/16", "10.0.0.1", 5_000, 2),
        ("10.0.0.0/12", "10.0.0.1", 5_000, 2),
        ("10.0.0.0/18", "10.0.0.1", 16_340, 2),
        ("fd00::/112", "fd00::1", 5_000, 2),
        ("fd00::/64", "fd00::1", 5_000, 2),
        ("::/0", "::1", 5_000, 2),
        // An early gap exposes the cost of transferring a dense occupied tail.
        ("10.0.0.0/16", "10.0.0.1", 32_779, 256),
    ];
    let mac: MacAddress = "02:ff:00:00:00:01".parse()?;

    for (cidr, gateway, occupied, first_occupied) in cases {
        let mut fixture = pool.begin().await?;
        let mut config = admin_segment("ALLOCATION_BENCHMARK", cidr, gateway, 2);
        if config.prefixes[0].prefix.is_ipv6() {
            config.prefixes[0].gateway = None;
        }
        let segment = crate::network_segment::persist(
            config,
            &mut fixture,
            NetworkSegmentControllerState::Ready,
        )
        .await?;
        let prefix = &segment.prefixes[0];
        let base = prefix.prefix.network();

        // Parked reservations populate the same address index without needing
        // thousands of interface rows. The remaining addresses are IPv4 and
        // outside every target prefix, including the IPv6 /0. The 32,779 total
        // matches https://github.com/dsx-ai-factory/infra-controller/issues/5050.
        sqlx::query(
            "INSERT INTO machine_interface_addresses (address, allocation_type, reserved_by_mac)
             SELECT $1::inet + n, 'static', ('0200' || lpad(to_hex(n), 8, '0'))::macaddr
             FROM generate_series($2::bigint, $2 + $3 - 1) AS n",
        )
        .bind(base)
        .bind(first_occupied)
        .bind(occupied)
        .execute(fixture.as_mut())
        .await?;
        sqlx::query(
            "INSERT INTO machine_interface_addresses (address, allocation_type, reserved_by_mac)
             SELECT '192.168.0.0'::inet + n, 'static', ('0201' || lpad(to_hex(n), 8, '0'))::macaddr
             FROM generate_series(1::bigint, $1) AS n",
        )
        .bind(32_779 - occupied)
        .execute(fixture.as_mut())
        .await?;
        sqlx::query("ANALYZE machine_interface_addresses")
            .execute(fixture.as_mut())
            .await?;
        let first_free = if first_occupied == 2 { occupied + 2 } else { 2 };
        let expected: IpAddr = sqlx::query_scalar("SELECT $1::inet + $2::bigint")
            .bind(base)
            .bind(first_free)
            .fetch_one(fixture.as_mut())
            .await?;

        let mut create_times = Vec::new();
        let mut legacy_times = Vec::new();
        for sample in 0..6 {
            if prefix.prefix.is_ipv4() {
                // The old candidate query is retained only as a benchmark
                // baseline. Its time excludes interface creation and locking.
                let reserved = if prefix.gateway.is_none() {
                    prefix.num_reserved.max(2)
                } else {
                    prefix.num_reserved.max(1)
                };
                let started = Instant::now();
                let candidates: Vec<IpAddr> = sqlx::query_scalar(
                    r#"
SELECT ($1::inet + ip_series.n)::inet AS ip
FROM generate_series($3, (1 << $2) - 2) AS ip_series(n)
LEFT JOIN machine_interface_addresses AS mia
  ON mia.address = ($1::inet + ip_series.n)::inet
WHERE mia.address IS NULL
  AND ($4::inet IS NULL OR ($1::inet + ip_series.n)::inet <> $4::inet)
  AND ($5::inet IS NULL OR ($1::inet + ip_series.n)::inet <> $5::inet)
ORDER BY ip
LIMIT $6;
    "#,
                )
                .bind(prefix.prefix.ip())
                .bind(32_i32 - i32::from(prefix.prefix.prefix()))
                .bind(reserved)
                .bind(prefix.gateway)
                .bind(prefix.svi_ip)
                .bind(32_i64)
                .fetch_all(fixture.as_mut())
                .await?;
                let elapsed = started.elapsed().as_secs_f64() * 1_000.0;
                assert_eq!(candidates.first(), Some(&expected));
                if sample != 0 {
                    legacy_times.push(elapsed);
                }
            }

            let mut attempt = fixture.begin().await?;
            let started = Instant::now();
            let interface = super::create(
                &mut attempt,
                std::slice::from_ref(&segment),
                &mac,
                true,
                AddressSelectionStrategy::NextAvailableIp,
                None,
            )
            .await?;
            let elapsed = started.elapsed().as_secs_f64() * 1_000.0;
            assert_eq!(interface.addresses, vec![expected]);
            if sample != 0 {
                create_times.push(elapsed);
            }
            attempt.rollback().await?;
        }
        create_times.sort_by(f64::total_cmp);
        legacy_times.sort_by(f64::total_cmp);
        let legacy_median = match legacy_times.get(2) {
            Some(elapsed) => format!("{elapsed:.3}"),
            None => "n/a".to_string(),
        };
        println!(
            "prefix={cidr} occupied={occupied} first_occupied={first_occupied} full_create_median_ms={:.3} legacy_candidate_only_median_ms={legacy_median}",
            create_times[2],
        );
        fixture.rollback().await?;
    }
    Ok(())
}

// This local diagnostic opens 48 allocation transactions before releasing them.
// The wave covers barrier release, creation, and commit; per-request times start
// after the barrier. Neither includes fixture or connection setup.
// Open transactions do not imply 48 simultaneously executing SQL statements.
#[crate::sqlx_test]
#[ignore = "allocation contention benchmark; run explicitly with --ignored --nocapture"]
async fn benchmark_concurrent_host_allocation(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    const ALLOCATIONS: usize = 48;
    let mut fixture = pool.begin().await?;
    let segment = crate::network_segment::persist(
        admin_segment(
            "CONCURRENT_ALLOCATION_BENCHMARK",
            "10.0.0.0/16",
            "10.0.0.1",
            2,
        ),
        &mut fixture,
        NetworkSegmentControllerState::Ready,
    )
    .await?;
    sqlx::query(
        "INSERT INTO machine_interface_addresses (address, allocation_type, reserved_by_mac)
         SELECT '10.0.0.0'::inet + n, 'static', ('0200' || lpad(to_hex(n), 8, '0'))::macaddr
         FROM generate_series(2::bigint, 5001) AS n",
    )
    .execute(fixture.as_mut())
    .await?;
    sqlx::query(
        "INSERT INTO machine_interface_addresses (address, allocation_type, reserved_by_mac)
         SELECT '192.168.0.0'::inet + n, 'static', ('0201' || lpad(to_hex(n), 8, '0'))::macaddr
         FROM generate_series(1::bigint, 27779) AS n",
    )
    .execute(fixture.as_mut())
    .await?;
    sqlx::query("ANALYZE machine_interface_addresses")
        .execute(fixture.as_mut())
        .await?;
    let reservations =
        crate::machine_interface_address::find_reserved(&mut fixture, None, None).await?;
    assert_eq!(reservations.len(), 32_779);
    fixture.commit().await?;

    // The SQLx test pool has fewer connections than the 32-candidate batch.
    // Copy its options without printing connection details or credentials.
    let allocation_pool = PgPoolOptions::new()
        .max_connections(ALLOCATIONS as u32)
        .connect_with(pool.connect_options().as_ref().clone())
        .await?;
    let mut transactions = Vec::with_capacity(ALLOCATIONS);
    for _ in 0..ALLOCATIONS {
        transactions.push(allocation_pool.begin().await?);
    }
    let barrier = Barrier::new(ALLOCATIONS + 1);
    let allocations = try_join_all(
        transactions
            .into_iter()
            .enumerate()
            .map(|(index, mut txn)| {
                let barrier = &barrier;
                let segment = &segment;
                async move {
                    let mac = MacAddress::new([0x02, 0xff, 0, 0, 0, index as u8]);
                    barrier.wait().await;
                    let started = Instant::now();
                    let interface = super::create(
                        &mut txn,
                        std::slice::from_ref(segment),
                        &mac,
                        true,
                        AddressSelectionStrategy::NextAvailableIp,
                        None,
                    )
                    .await?;
                    txn.commit().await?;
                    Ok::<_, Box<dyn std::error::Error>>((interface, started.elapsed()))
                }
            }),
    );
    let started = Instant::now();
    // This is a liveness guard for a manual run, not a performance threshold.
    let completed = tokio::time::timeout(Duration::from_secs(120), async {
        let (results, _) = tokio::join!(allocations, barrier.wait());
        results
    })
    .await;
    let wave_elapsed = started.elapsed();
    allocation_pool.close().await;
    let results = completed??;
    assert_eq!(results.len(), ALLOCATIONS);

    let mut conn = pool.acquire().await?;
    let persisted = super::find_all(&mut conn).await?;
    assert_eq!(persisted.len(), ALLOCATIONS);
    assert_eq!(
        persisted
            .iter()
            .map(|interface| interface.id)
            .collect::<HashSet<_>>(),
        results
            .iter()
            .map(|(interface, _)| interface.id)
            .collect::<HashSet<_>>()
    );
    let addresses = persisted
        .iter()
        .map(|interface| {
            assert_eq!(interface.addresses.len(), 1);
            interface.addresses[0]
        })
        .collect::<HashSet<_>>();
    assert_eq!(addresses.len(), ALLOCATIONS);
    assert_eq!(
        crate::machine_interface_address::find_reserved(&mut conn, None, None).await?,
        reservations
    );

    println!(
        "local_concurrency_benchmark debug_assertions={} transactions={ALLOCATIONS} occupied=5000 total_reserved=32779 wave_create_commit_ms={:.3}",
        cfg!(debug_assertions),
        wave_elapsed.as_secs_f64() * 1_000.0,
    );
    for (index, (_, elapsed)) in results.iter().enumerate() {
        println!(
            "allocation={index} create_commit_ms={:.3}",
            elapsed.as_secs_f64() * 1_000.0,
        );
    }
    Ok(())
}

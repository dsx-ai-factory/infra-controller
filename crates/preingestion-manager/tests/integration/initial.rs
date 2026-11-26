// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use std::net::IpAddr;
use std::sync::Arc;

use carbide_preingestion_manager::PreingestionManager;
use carbide_redfish::libredfish::test_support::RedfishSim;
use carbide_test_harness::prelude::*;
use carbide_test_harness::test_support::default_config;
use carbide_uuid::machine::MachineInterfaceId;
use db::TransactionVending as _;
use mac_address::MacAddress;
use model::allocation_type::AllocationType;
use model::site_explorer::{InitialBmcResetPhase, PreingestionState};
use sqlx::PgConnection;

use crate::common;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn manager(pool: &PgPool, env: &TestHarness) -> PreingestionManager {
    PreingestionManager::new(
        pool.clone(),
        default_config::get().preingestion_manager(),
        Arc::new(RedfishSim::default()),
        env.test_meter.meter(),
        None,
        None,
        None,
        env.api().work_lock_manager_handle(),
        vec![],
        env.api().instrumentation().clone(),
    )
}

async fn insert_interface(txn: &mut PgConnection, name: &str, addresses: &[IpAddr]) -> TestResult {
    let mac: MacAddress = "02:00:00:00:00:70".parse()?;
    let interface_id: MachineInterfaceId = sqlx::query_scalar(
        "WITH segment AS (
            INSERT INTO network_segments (name, version)
            VALUES ($1, 'V1-T0') RETURNING id
         ) INSERT INTO machine_interfaces (segment_id, mac_address, primary_interface, hostname)
           SELECT id, $2, false, $1 FROM segment RETURNING id",
    )
    .bind(name)
    .bind(mac)
    .fetch_one(&mut *txn)
    .await?;
    for address in addresses {
        db::machine_interface_address::insert(txn, interface_id, *address, AllocationType::Dhcp)
            .await?;
    }
    Ok(())
}

async fn insert_initial_endpoint(txn: &mut PgConnection, address: IpAddr) -> TestResult {
    common::insert_endpoint_version(txn, &address.to_string(), "1", "1.13.2", false).await?;
    db::explored_endpoints::set_preingestion_for_addresses(
        &[address],
        PreingestionState::Initial,
        txn,
    )
    .await?;
    Ok(())
}

async fn state(pool: &PgPool, address: IpAddr) -> TestResult<PreingestionState> {
    Ok(db::explored_endpoints::find_by_ips(pool, vec![address])
        .await?
        .pop()
        .expect("explored endpoint")
        .preingestion_state)
}

#[sqlx_test]
async fn ipv6_waits_for_allocated_ipv4_to_finish_preingestion(pool: PgPool) -> TestResult {
    let env = TestHarness::builder(pool.clone()).build().await;
    let ipv4: IpAddr = "192.0.2.70".parse()?;
    let ipv6: IpAddr = "2001:db8::70".parse()?;
    let mut txn = pool.txn_begin().await?;
    insert_interface(txn.as_mut(), "dual-stack-initial", &[ipv6, ipv4]).await?;
    insert_initial_endpoint(txn.as_mut(), ipv6).await?;
    txn.commit().await?;

    // The allocated IPv4 address is primary even before its first report arrives.
    manager(&pool, &env).run_single_iteration().await?;
    assert_eq!(state(&pool, ipv6).await?, PreingestionState::Initial);
    assert!(
        db::explored_endpoints::find_by_ips(&pool, vec![ipv4])
            .await?
            .is_empty()
    );

    let mut txn = pool.txn_begin().await?;
    insert_initial_endpoint(txn.as_mut(), ipv4).await?;
    db::explored_endpoints::set_waiting_for_explorer_refresh(ipv4, txn.as_mut()).await?;
    txn.commit().await?;
    for (scenario, primary_state) in [
        ("primary not started", PreingestionState::Initial),
        (
            "primary in progress",
            PreingestionState::InitialBMCReset {
                phase: InitialBmcResetPhase::Start { attempts: 3 },
            },
        ),
        (
            "primary failed",
            PreingestionState::Failed {
                reason: "repair the primary endpoint".to_string(),
            },
        ),
    ] {
        let mut txn = pool.txn_begin().await?;
        db::explored_endpoints::set_preingestion_for_addresses(
            &[ipv4],
            primary_state.clone(),
            txn.as_mut(),
        )
        .await?;
        txn.commit().await?;
        manager(&pool, &env).run_single_iteration().await?;
        assert_eq!(state(&pool, ipv4).await?, primary_state, "{scenario}");
        assert_eq!(
            state(&pool, ipv6).await?,
            PreingestionState::Initial,
            "{scenario}"
        );
    }

    // Retrying the failed primary admits only IPv4 to the reset phase.
    let mut txn = pool.txn_begin().await?;
    db::explored_endpoints::set_preingestion_for_addresses(
        &[ipv4],
        PreingestionState::Initial,
        txn.as_mut(),
    )
    .await?;
    sqlx::query(
        "UPDATE explored_endpoints SET waiting_for_explorer_refresh = false WHERE address = $1",
    )
    .bind(ipv4)
    .execute(txn.as_mut())
    .await?;
    txn.commit().await?;
    manager(&pool, &env).run_single_iteration().await?;
    assert_eq!(
        state(&pool, ipv4).await?,
        PreingestionState::InitialBMCReset {
            phase: InitialBmcResetPhase::Start { attempts: 0 },
        }
    );
    assert_eq!(state(&pool, ipv6).await?, PreingestionState::Initial);

    let mut txn = pool.txn_begin().await?;
    db::explored_endpoints::set_preingestion_complete(ipv4, txn.as_mut()).await?;
    txn.commit().await?;
    manager(&pool, &env).run_single_iteration().await?;
    for address in [ipv4, ipv6] {
        assert_eq!(state(&pool, address).await?, PreingestionState::Complete);
    }
    Ok(())
}

#[sqlx_test]
async fn initial_selection_is_local_to_each_interface(pool: PgPool) -> TestResult {
    let env = TestHarness::builder(pool.clone()).build().await;
    let ipv6_only: IpAddr = "2001:db8::70".parse()?;
    let other_segment_ipv4: IpAddr = "192.0.2.70".parse()?;
    let no_interface: IpAddr = "192.0.2.71".parse()?;
    let mut txn = pool.txn_begin().await?;
    // The same MAC on another segment must not make this IPv6-only interface secondary.
    insert_interface(txn.as_mut(), "ipv6-only", &[ipv6_only]).await?;
    insert_interface(txn.as_mut(), "other-segment", &[other_segment_ipv4]).await?;
    let cases = [
        ("IPv6-only interface", ipv6_only),
        ("IPv4 on another interface", other_segment_ipv4),
        ("endpoint without an interface", no_interface),
    ];
    for (_, address) in cases {
        insert_initial_endpoint(txn.as_mut(), address).await?;
    }
    txn.commit().await?;

    manager(&pool, &env).run_single_iteration().await?;

    for (scenario, address) in cases {
        assert_eq!(
            state(&pool, address).await?,
            PreingestionState::InitialBMCReset {
                phase: InitialBmcResetPhase::Start { attempts: 0 },
            },
            "{scenario}"
        );
    }
    Ok(())
}

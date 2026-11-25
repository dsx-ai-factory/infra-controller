// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use std::sync::Arc;
use std::time::Duration;

use carbide_redfish::libredfish::test_support::RedfishSim;
use carbide_test_harness::prelude::{TestHarness, sqlx_test, sqlx_testing};
use carbide_uuid::machine::MachineInterfaceId;
use db::db_read::PgPoolReader;
use db::test_support::postgres::{wait_for_blocked_queries, wait_for_blocked_query};
use model::allocation_type::AllocationType;
use model::site_explorer::EndpointExplorationReport;
use sqlx::PgPool;

use super::*;
use crate::{PreingestionManager, PreingestionManagerConfig};

#[sqlx_test]
async fn concurrent_alias_claims_recheck_persisted_owner_and_completion(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = TestHarness::builder(pool.clone()).build().await;
    let manager = PreingestionManager::new(
        pool.clone(),
        PreingestionManagerConfig {
            run_interval: Duration::from_secs(1),
            concurrency_limit: 2,
            hgx_bmc_gpu_reboot_delay: Duration::ZERO,
            max_concurrent_bfb_copies: 1,
            autoupdate: false,
            no_reset_retries: false,
            firmware_download_cache_directory: Default::default(),
            firmware: Default::default(),
        },
        Arc::new(RedfishSim::default()),
        env.test_meter.meter(),
        None,
        None,
        None,
        env.api().work_lock_manager_handle(),
        vec![],
        env.api().instrumentation().clone(),
    );
    let static_info = &manager.static_info;
    let bmc_mac: MacAddress = "02:00:00:00:00:40".parse()?;
    let ipv4: IpAddr = "192.0.2.40".parse()?;
    let ipv6: IpAddr = "2001:db8::40".parse()?;
    let check = PreingestionState::NvosPasswordRotation {
        rotation_state: NvosPasswordRotationState::Check,
    };
    let rotating = PreingestionState::NvosPasswordRotation {
        rotation_state: NvosPasswordRotationState::Rotate {
            target_version: 0,
            job_id: None,
        },
    };
    let mut txn = pool.txn_begin().await?;
    let interface_id: MachineInterfaceId = sqlx::query_scalar(
        "WITH segment AS (
            INSERT INTO network_segments (name, version)
            VALUES ('nvos-alias-claim', 'V1-T0') RETURNING id
         ) INSERT INTO machine_interfaces (segment_id, mac_address, primary_interface, hostname)
           SELECT id, $1, false, 'nvos-alias-claim' FROM segment RETURNING id",
    )
    .bind(bmc_mac)
    .fetch_one(txn.as_mut())
    .await?;
    for address in [ipv4, ipv6] {
        db::machine_interface_address::insert(
            &mut txn,
            interface_id,
            address,
            AllocationType::Dhcp,
        )
        .await?;
        db::explored_endpoints::insert(
            address,
            &EndpointExplorationReport::default(),
            false,
            &mut txn,
        )
        .await?;
    }
    db::explored_endpoints::set_preingestion_for_addresses(&[ipv4, ipv6], check.clone(), &mut txn)
        .await?;
    let initial_target = db::credential_rotation::set_initial_target_version(
        &mut txn,
        CredentialRotationType::Nvos,
        Default::default(),
    )
    .await?;
    assert!(matches!(initial_target, db::ConditionalWrite::Applied(_)));
    txn.commit().await?;

    // Independent claims serialize without the manager iteration lock.
    let mut blocker = pool.txn_begin().await?;
    let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(blocker.as_mut())
        .await?;
    // EXCLUSIVE blocks both FOR UPDATE statements but permits ordinary reads.
    sqlx::query("LOCK TABLE machine_interfaces IN EXCLUSIVE MODE")
        .execute(&mut *blocker)
        .await?;

    let mut observer = PgPoolReader::from(pool.clone());
    let claims = async {
        tokio::try_join!(
            static_info.claim_nvos_password_rotation(ipv4, bmc_mac),
            static_info.claim_nvos_password_rotation(ipv6, bmc_mac),
        )?;
        Ok::<_, Box<dyn std::error::Error>>(())
    };
    tokio::pin!(claims);
    tokio::select! {
        result = &mut claims => panic!("claims must wait for the interface lock: {result:?}"),
        _ = wait_for_blocked_queries(&mut observer, blocker_pid, "machine_interfaces", 2) => {},
    }
    blocker.commit().await?;
    claims.await?;

    let endpoints = db::explored_endpoints::find_by_ips(&pool, vec![ipv4, ipv6]).await?;
    assert_eq!(endpoints.len(), 2);
    let owner_address = endpoints
        .iter()
        .find(|endpoint| endpoint.preingestion_state == rotating)
        .expect("one alias must own rotation")
        .address;
    let waiting_address = if owner_address == ipv4 { ipv6 } else { ipv4 };
    for endpoint in endpoints {
        let expected = if endpoint.address == owner_address {
            &rotating
        } else {
            &check
        };
        assert_eq!(&endpoint.preingestion_state, expected);
    }

    // Completion proceeds while another transaction holds the interface lock. Until
    // completion commits, the claimant still sees the persisted Rotate owner.
    let mut blocker = pool.txn_begin().await?;
    db::machine_interface::find_by_mac_address_for_update(&mut blocker, bmc_mac).await?;
    let mut endpoint_gate = pool.txn_begin().await?;
    let endpoint_gate_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(endpoint_gate.as_mut())
        .await?;
    assert!(db::explored_endpoints::lock_by_address(&mut endpoint_gate, owner_address).await?);
    let completion = static_info.record_nvos_password_rotation_succeeded(owner_address, bmc_mac, 0);
    tokio::pin!(completion);
    tokio::select! {
        result = &mut completion => panic!("completion must wait for the endpoint lock: {result:?}"),
        _ = wait_for_blocked_query(&mut observer, endpoint_gate_pid, "explored_endpoints") => {},
    }
    blocker.commit().await?;
    static_info
        .claim_nvos_password_rotation(waiting_address, bmc_mac)
        .await?;

    let endpoints = db::explored_endpoints::find_by_ips(&pool, vec![ipv4, ipv6]).await?;
    assert_eq!(endpoints.len(), 2);
    for endpoint in endpoints {
        let expected = if endpoint.address == owner_address {
            &rotating
        } else {
            &check
        };
        assert_eq!(&endpoint.preingestion_state, expected);
    }
    let operation = db::credential_rotation::device_rotation_operation_state(
        &pool,
        CredentialRotationType::Nvos,
        bmc_mac,
    )
    .await?;
    assert!(operation.is_none());

    // After the atomic commit, the claimant observes the confirmed version and
    // advances to firmware checks without creating another rotation owner.
    endpoint_gate.commit().await?;
    completion.await?;
    static_info
        .claim_nvos_password_rotation(waiting_address, bmc_mac)
        .await?;

    let endpoints = db::explored_endpoints::find_by_ips(&pool, vec![ipv4, ipv6]).await?;
    assert_eq!(endpoints.len(), 2);
    for endpoint in endpoints {
        assert_eq!(
            endpoint.preingestion_state,
            PreingestionState::CheckFirmware
        );
    }
    let operation = db::credential_rotation::device_rotation_operation_state(
        &pool,
        CredentialRotationType::Nvos,
        bmc_mac,
    )
    .await?;
    assert_eq!(
        operation.and_then(|operation| operation.current_version),
        Some(0)
    );
    Ok(())
}

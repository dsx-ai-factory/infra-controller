// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use std::sync::Arc;
use std::time::Duration;

use carbide_redfish::libredfish::test_support::RedfishSim;
use carbide_test_harness::prelude::{TestHarness, sqlx_test, sqlx_testing};
use carbide_uuid::machine::MachineInterfaceId;
use db::db_read::PgPoolReader;
use db::test_support::postgres::wait_for_blocked_queries;
use model::allocation_type::AllocationType;
use model::site_explorer::EndpointExplorationReport;
use sqlx::PgPool;

use super::*;
use crate::{PreingestionManager, PreingestionManagerConfig};

#[sqlx_test]
async fn concurrent_claims_recheck_persisted_endpoint_state(
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
    let address: IpAddr = "192.0.2.40".parse()?;
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
            VALUES ('nvos-endpoint-claim', 'V1-T0') RETURNING id
         ) INSERT INTO machine_interfaces (segment_id, mac_address, primary_interface, hostname)
           SELECT id, $1, false, 'nvos-endpoint-claim' FROM segment RETURNING id",
    )
    .bind(bmc_mac)
    .fetch_one(txn.as_mut())
    .await?;
    db::machine_interface_address::insert(&mut txn, interface_id, address, AllocationType::Dhcp)
        .await?;
    db::explored_endpoints::insert(
        address,
        &EndpointExplorationReport::default(),
        false,
        &mut txn,
    )
    .await?;
    db::explored_endpoints::set_preingestion_for_addresses(&[address], check, &mut txn).await?;
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
            static_info.claim_nvos_password_rotation(address, bmc_mac),
            static_info.claim_nvos_password_rotation(address, bmc_mac),
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

    let endpoints = db::explored_endpoints::find_by_ips(&pool, vec![address]).await?;
    assert_eq!(endpoints.len(), 1);
    assert_eq!(endpoints[0].preingestion_state, rotating);

    // A stale Check must preserve the submitted NVOS job.
    static_info
        .set_nvos_rotation_state(address, 0, Some("password-job".to_string()))
        .await?;
    static_info
        .claim_nvos_password_rotation(address, bmc_mac)
        .await?;
    let endpoints = db::explored_endpoints::find_by_ips(&pool, vec![address]).await?;
    assert_eq!(endpoints.len(), 1);
    assert_eq!(
        endpoints[0].preingestion_state,
        PreingestionState::NvosPasswordRotation {
            rotation_state: NvosPasswordRotationState::Rotate {
                target_version: 0,
                job_id: Some("password-job".to_string()),
            },
        }
    );

    static_info
        .record_nvos_password_rotation_succeeded(address, bmc_mac, 0)
        .await?;
    let endpoints = db::explored_endpoints::find_by_ips(&pool, vec![address]).await?;
    assert_eq!(endpoints.len(), 1);
    assert_eq!(
        endpoints[0].preingestion_state,
        PreingestionState::SwitchCertificateRotation {
            rotation_state: SwitchCertificateRotationState::DisableMtls { job_id: None },
        }
    );
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

    // The confirmed password version must not restart TLS from a stale Check.
    let tls_state = PreingestionState::SwitchCertificateRotation {
        rotation_state: SwitchCertificateRotationState::ConfigureCertificate {
            job_id: Some("certificate-job".to_string()),
        },
    };
    let mut txn = pool.txn_begin().await?;
    db::explored_endpoints::set_preingestion_for_addresses(&[address], tls_state.clone(), &mut txn)
        .await?;
    txn.commit().await?;
    static_info
        .claim_nvos_password_rotation(address, bmc_mac)
        .await?;
    let endpoints = db::explored_endpoints::find_by_ips(&pool, vec![address]).await?;
    assert_eq!(endpoints.len(), 1);
    assert_eq!(endpoints[0].preingestion_state, tls_state);
    Ok(())
}

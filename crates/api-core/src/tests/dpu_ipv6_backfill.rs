// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Preserve the native IPv6 backfill contract through the selected-backend caller.

use std::collections::{HashMap, HashSet};
use std::str::FromStr;
use std::sync::Arc;

use carbide_instrument::Instrumentation;
use carbide_resource_pool::{ResourcePoolBackendError, ResourcePoolRegistry};
use carbide_site_explorer::dpu_loopback::update_dpu_loopback_ips_v6;
use carbide_utils::test_support::test_meter::TestMeter;
use carbide_uuid::machine::{MachineId, MachineIdSource, MachineType};
use carbide_uuid::network::NetworkSegmentId;
use db::machine;
use model::machine::ManagedHostState;
use model::resource_pool::common::{
    CommonPools, FNN_ASN, LOOPBACK_IP, LOOPBACK_IP_V6, VLANID, VNI, VPC_VNI,
};
use model::resource_pool::{Range, ResourcePoolDef, ResourcePoolType};
use tokio_util::sync::CancellationToken;

fn machine_id(seed: u8, ty: MachineType) -> MachineId {
    MachineId::new(MachineIdSource::ProductBoardChassisSerial, [seed; 32], ty)
}

fn integer_pool() -> ResourcePoolDef {
    ResourcePoolDef {
        ranges: vec![Range {
            start: "1".to_string(),
            end: "16".to_string(),
            auto_assign: true,
        }],
        prefix: None,
        pool_type: ResourcePoolType::Integer,
        delegate_prefix_len: None,
    }
}

fn address_pool(pool_type: ResourcePoolType, prefix: &str) -> ResourcePoolDef {
    ResourcePoolDef {
        ranges: Vec::new(),
        prefix: Some(prefix.to_string()),
        pool_type,
        delegate_prefix_len: None,
    }
}

/// Seeds the small Integrated pools used by the retained backfill scenarios.
/// Omitting the IPv6 prefix models an unconfigured optional pool.
async fn common_pools(
    pool: &sqlx::PgPool,
    loopback_ip_v6_prefix: Option<&str>,
) -> Result<Arc<CommonPools>, Box<dyn std::error::Error>> {
    let mut definitions = HashMap::from([
        (
            LOOPBACK_IP.to_string(),
            address_pool(ResourcePoolType::Ipv4, "192.0.2.0/29"),
        ),
        (VLANID.to_string(), integer_pool()),
        (VNI.to_string(), integer_pool()),
        (VPC_VNI.to_string(), integer_pool()),
        (FNN_ASN.to_string(), integer_pool()),
    ]);
    if let Some(prefix) = loopback_ip_v6_prefix {
        definitions.insert(
            LOOPBACK_IP_V6.to_string(),
            address_pool(ResourcePoolType::Ipv6, prefix),
        );
    }

    let mut txn = pool.begin().await?;
    db::resource_pool::define_all_from(txn.as_mut(), &definitions).await?;
    txn.commit().await?;

    Ok(db::resource_pool::create_common_pools(pool.clone(), HashSet::new()).await?)
}

/// Updating one DPU advances its siblings' versions. Backfill must reload each
/// target and leave every address and version unchanged on a second pass.
#[crate::sqlx_test]
async fn ipv6_loopback_backfill_reloads_each_dpu_and_is_idempotent(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let test_meter = TestMeter::default();
    let instrumentation = Instrumentation::new(test_meter.meter());
    let common_pools = common_pools(&pool, Some("2001:db8:2::/125")).await?;
    let host_machine_id =
        MachineId::from_str("fm100htes3rn1npvbtm5qd57dkilaag7ljugl1llmm7rfuq1ov50i0rpl30")?;
    let dpu_machine_ids = [
        MachineId::from_str("fm100dskla0ihp0pn4tv7v1js2k2mo37sl0jjr8141okqg8pjpdpfihaa80")?,
        MachineId::from_str("fm100ds27v4uuq7sgs4gsjummskt0b3tedugtpevjrbfh6su081n9jufcq0")?,
    ];

    // These rows model DPUs that predate `loopback_ip_v6`, so create them
    // without common pools and then connect both to the same host group.
    let mut txn = pool.begin().await?;
    machine::create(
        txn.as_mut(),
        None,
        &host_machine_id,
        ManagedHostState::Ready,
        None,
        2,
    )
    .await?;
    for dpu_machine_id in &dpu_machine_ids {
        machine::create(
            txn.as_mut(),
            None,
            dpu_machine_id,
            ManagedHostState::Ready,
            None,
            2,
        )
        .await?;
    }

    let network_segment_id: NetworkSegmentId = sqlx::query_scalar(
        "INSERT INTO network_segments (name, version)
         VALUES ('ipv6-loopback-backfill', 'V1-T0') RETURNING id",
    )
    .fetch_one(txn.as_mut())
    .await?;
    for (index, dpu_machine_id) in dpu_machine_ids.iter().enumerate() {
        sqlx::query(
            "INSERT INTO machine_interfaces
                (machine_id, attached_dpu_machine_id, association_type,
                 segment_id, mac_address, primary_interface, hostname)
             VALUES ($1, $2, 'Machine', $3, $4::macaddr, $5, $6)",
        )
        .bind(host_machine_id)
        .bind(dpu_machine_id)
        .bind(network_segment_id)
        .bind(format!("02:00:00:00:10:{index:02x}"))
        .bind(index == 0)
        .bind(format!("backfill-dpu-{index}"))
        .execute(txn.as_mut())
        .await?;
    }
    txn.commit().await?;

    update_dpu_loopback_ips_v6(
        &pool,
        &ResourcePoolRegistry::default(),
        &common_pools,
        &CancellationToken::new(),
        &instrumentation,
    )
    .await?;

    let mut txn = pool.begin().await?;
    let host_config = machine::get_network_config(txn.as_mut(), &host_machine_id).await?;
    let first_dpu_config = machine::get_network_config(txn.as_mut(), &dpu_machine_ids[0]).await?;
    let second_dpu_config = machine::get_network_config(txn.as_mut(), &dpu_machine_ids[1]).await?;
    let allocated = [
        first_dpu_config
            .value
            .loopback_ip_v6
            .expect("first DPU should be backfilled"),
        second_dpu_config
            .value
            .loopback_ip_v6
            .expect("second DPU should be backfilled"),
    ];
    assert_ne!(allocated[0], allocated[1]);
    assert_eq!(host_config.value.loopback_ip_v6, None);
    assert_eq!(host_config.version, first_dpu_config.version);
    assert_eq!(first_dpu_config.version, second_dpu_config.version);
    let version_after_backfill = host_config.version;
    txn.commit().await?;

    let stats_after_backfill = db::resource_pool::stats(&pool, LOOPBACK_IP_V6).await?;
    assert_eq!(stats_after_backfill.used, 2);

    // A second startup pass must preserve both reservations and avoid a
    // version bump: `None` is the only row state the backfill owns.
    update_dpu_loopback_ips_v6(
        &pool,
        &ResourcePoolRegistry::default(),
        &common_pools,
        &CancellationToken::new(),
        &instrumentation,
    )
    .await?;
    let stats_after_rerun = db::resource_pool::stats(&pool, LOOPBACK_IP_V6).await?;
    assert_eq!(stats_after_rerun, stats_after_backfill);

    let mut txn = pool.begin().await?;
    let host_config = machine::get_network_config(txn.as_mut(), &host_machine_id).await?;
    let first_dpu_config = machine::get_network_config(txn.as_mut(), &dpu_machine_ids[0]).await?;
    let second_dpu_config = machine::get_network_config(txn.as_mut(), &dpu_machine_ids[1]).await?;
    assert_eq!(host_config.version, version_after_backfill);
    assert_eq!(
        [
            first_dpu_config.value.loopback_ip_v6.unwrap(),
            second_dpu_config.value.loopback_ip_v6.unwrap(),
        ],
        allocated
    );
    txn.rollback().await?;
    Ok(())
}

/// A DPU already being deleted must not consume the remaining IPv6 capacity.
#[crate::sqlx_test]
async fn ipv6_loopback_backfill_skips_force_deleting_dpus(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let test_meter = TestMeter::default();
    let instrumentation = Instrumentation::new(test_meter.meter());
    let common_pools = common_pools(&pool, Some("2001:db8:5::1/128")).await?;
    let dpu_ids = [
        machine_id(100, MachineType::Dpu),
        machine_id(101, MachineType::Dpu),
    ];
    let mut txn = pool.begin().await?;
    for id in dpu_ids {
        machine::create(txn.as_mut(), None, &id, ManagedHostState::Ready, None, 2).await?;
    }
    let ordered_dpus: Vec<MachineId> =
        sqlx::query_scalar("SELECT id FROM machines WHERE id = ANY($1) ORDER BY id")
            .bind(dpu_ids.as_slice())
            .fetch_all(txn.as_mut())
            .await?;
    let deleting_id = ordered_dpus[0];
    let eligible_id = ordered_dpus[1];
    sqlx::query("UPDATE machines SET controller_state = $2 WHERE id = $1")
        .bind(deleting_id)
        .bind(sqlx::types::Json(&ManagedHostState::ForceDeletion))
        .execute(txn.as_mut())
        .await?;
    txn.commit().await?;
    let deleting_before = machine::get_network_config(&pool, &deleting_id).await?;

    // Deletion is already committed. A separate owner keeps that row busy;
    // neither contention nor later pool exhaustion should override its skip.
    let mut gate = pool.begin().await?;
    sqlx::query("SELECT id FROM machines WHERE id = $1 FOR NO KEY UPDATE")
        .bind(deleting_id)
        .fetch_one(gate.as_mut())
        .await?;
    let passes = async {
        update_dpu_loopback_ips_v6(
            &pool,
            &ResourcePoolRegistry::default(),
            &common_pools,
            &CancellationToken::new(),
            &instrumentation,
        )
        .await?;
        let eligible_first = machine::get_network_config(&pool, &eligible_id).await?;
        let first_stats = db::resource_pool::stats(&pool, LOOPBACK_IP_V6).await?;

        // The independent DPU consumed the only address. The remaining
        // candidate is still force-deleting and still locked by the gate.
        update_dpu_loopback_ips_v6(
            &pool,
            &ResourcePoolRegistry::default(),
            &common_pools,
            &CancellationToken::new(),
            &instrumentation,
        )
        .await?;
        let deleting_after = machine::get_network_config(&pool, &deleting_id).await?;
        let eligible_second = machine::get_network_config(&pool, &eligible_id).await?;
        let second_stats = db::resource_pool::stats(&pool, LOOPBACK_IP_V6).await?;
        let deleting_allocations: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM resource_pool_allocations
             WHERE pool_name = $1 AND owner_id = $2",
        )
        .bind(LOOPBACK_IP_V6)
        .bind(deleting_id.to_string())
        .fetch_one(&pool)
        .await?;
        Ok::<_, Box<dyn std::error::Error>>((
            eligible_first,
            first_stats,
            deleting_after,
            eligible_second,
            second_stats,
            deleting_allocations,
        ))
    };
    tokio::pin!(passes);
    let result = tokio::time::timeout(std::time::Duration::from_secs(10), &mut passes).await;
    let released = gate.rollback().await;
    let result = match result {
        Ok(result) => result,
        Err(error) => {
            // Join the real future after releasing its gate before failing.
            let late_result =
                tokio::time::timeout(std::time::Duration::from_secs(30), &mut passes).await;
            return Err(format!(
                "backfill exceeded its gated deadline: {error}; \
                 release={released:?}; late_result={late_result:?}"
            )
            .into());
        }
    };
    released?;
    let (
        eligible_first,
        first_stats,
        deleting_after,
        eligible_second,
        second_stats,
        deleting_allocations,
    ) = result?;
    assert_eq!(
        eligible_first.value.loopback_ip_v6,
        Some("2001:db8:5::1".parse()?)
    );
    assert_eq!(first_stats.used, 1);
    assert_eq!(first_stats.free, 0);
    assert_eq!(deleting_after.value, deleting_before.value);
    assert_eq!(deleting_after.version, deleting_before.version);
    assert_eq!(deleting_after.value.loopback_ip_v6, None);
    assert_eq!(deleting_allocations, 0);
    assert_eq!(eligible_second.value, eligible_first.value);
    assert_eq!(eligible_second.version, eligible_first.version);
    assert_eq!(second_stats, first_stats);
    Ok(())
}

/// Deletion after the initial scan must roll back the unused reservation,
/// including when backfill waited behind another allocator.
#[crate::sqlx_test]
async fn ipv6_loopback_backfill_releases_reservation_after_late_force_deletion(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let test_meter = TestMeter::default();
    let instrumentation = Instrumentation::new(test_meter.meter());
    use db::test_support::postgres::wait_for_blocked_query;
    use futures::future::maybe_done;

    let common_pools = common_pools(&pool, Some("2001:db8:8::1/128")).await?;
    let dpu_ids = [
        machine_id(102, MachineType::Dpu),
        machine_id(103, MachineType::Dpu),
    ];
    let mut setup = pool.begin().await?;
    for id in dpu_ids {
        machine::create(setup.as_mut(), None, &id, ManagedHostState::Ready, None, 2).await?;
    }
    let ordered_dpus: Vec<MachineId> =
        sqlx::query_scalar("SELECT id FROM machines WHERE id = ANY($1) ORDER BY id")
            .bind(dpu_ids.as_slice())
            .fetch_all(setup.as_mut())
            .await?;
    setup.commit().await?;
    let deleting_id = ordered_dpus[0];
    let later_id = ordered_dpus[1];
    let deleting_before = machine::find_one(&pool, &deleting_id, Default::default())
        .await?
        .expect("the first Ready DPU exists");
    let later_before = machine::get_network_config(&pool, &later_id).await?;
    let stats_before = db::resource_pool::stats(&pool, LOOPBACK_IP_V6).await?;
    assert_eq!(stats_before.used, 0);
    assert_eq!(stats_before.free, 1);

    // Only a fresh allocation writes this table. Observing that statement
    // behind the gate proves the initial eligibility read has passed;
    // neither the gate nor the backfill has locked the DPU row yet.
    let mut gate = pool.begin().await?;
    sqlx::query("LOCK TABLE resource_pool_allocations IN SHARE MODE")
        .execute(gate.as_mut())
        .await?;
    let gate_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(gate.as_mut())
        .await?;
    let backfill_pool = pool.clone();
    let backfill_common_pools = Arc::clone(&common_pools);
    let task = tokio::spawn(async move {
        update_dpu_loopback_ips_v6(
            &backfill_pool,
            &ResourcePoolRegistry::default(),
            &backfill_common_pools,
            &CancellationToken::new(),
            &instrumentation,
        )
        .await
    });
    let abort_backfill = task.abort_handle();
    let backfill = maybe_done(task);
    tokio::pin!(backfill);
    let at_gate = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        tokio::select! {
            () = &mut backfill => {
                return Err("backfill finished before the fresh allocation gate".into());
            }
            _ = wait_for_blocked_query(&pool, gate_pid, "WITH allocate AS") => {}
        }
        let mut deletion = pool.begin().await?;
        let advanced = machine::advance(
            &deleting_before,
            deletion.as_mut(),
            &ManagedHostState::ForceDeletion,
            None,
        )
        .await?;
        deletion.commit().await?;
        Ok::<_, Box<dyn std::error::Error>>(advanced)
    })
    .await;
    let released = gate.rollback().await;
    let joined_after_release =
        tokio::time::timeout(std::time::Duration::from_secs(30), &mut backfill)
            .await
            .is_ok();
    if !joined_after_release {
        abort_backfill.abort();
        tokio::time::timeout(std::time::Duration::from_secs(30), &mut backfill)
            .await
            .expect("cancelled backfill must join after releasing its allocation gate");
    }
    let result = backfill
        .as_mut()
        .take_output()
        .expect("backfill task joined");
    released?;
    assert!(
        at_gate.expect("ForceDeletion must commit while allocation is gated")?,
        "the state writer must advance the first DPU"
    );
    assert!(
        joined_after_release,
        "backfill required cancellation after gate release"
    );
    result??;

    let deleting_after = machine::find_one(&pool, &deleting_id, Default::default())
        .await?
        .expect("ForceDeletion must leave the DPU available for its controller");
    assert_eq!(deleting_after.state.value, ManagedHostState::ForceDeletion);
    assert_eq!(
        deleting_after.network_config.value,
        deleting_before.network_config.value
    );
    assert_eq!(
        deleting_after.network_config.version,
        deleting_before.network_config.version
    );
    let deleting_allocations: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM resource_pool_allocations
         WHERE pool_name = $1 AND owner_type = 'machine' AND owner_id = $2",
    )
    .bind(LOOPBACK_IP_V6)
    .bind(deleting_id.to_string())
    .fetch_one(&pool)
    .await?;
    assert_eq!(deleting_allocations, 0);

    // The next independent DPU can use the only address only if the
    // rejected reservation was rolled back before the scan continued.
    let later_after = machine::get_network_config(&pool, &later_id).await?;
    let mut expected_later = later_before.value;
    expected_later.loopback_ip_v6 = Some("2001:db8:8::1".parse()?);
    assert_eq!(later_after.value, expected_later);
    assert_eq!(
        later_after.version.version_nr(),
        later_before.version.version_nr() + 1
    );
    let stats_after = db::resource_pool::stats(&pool, LOOPBACK_IP_V6).await?;
    assert_eq!(stats_after.used, 1);
    assert_eq!(stats_after.free, 0);
    Ok(())
}

/// A same-ID replacement with the same configuration version must reject the
/// old captured lifetime, independently of provider cancellation. A later pass
/// can assign that replacement normally.
#[crate::sqlx_test]
async fn ipv6_loopback_backfill_preserves_same_version_replacement(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let test_meter = TestMeter::default();
    let instrumentation = Instrumentation::new(test_meter.meter());
    let common_pools = common_pools(&pool, Some("2001:db8:6::1/128")).await?;
    let dpu_machine_id =
        MachineId::from_str("fm100dskla0ihp0pn4tv7v1js2k2mo37sl0jjr8141okqg8pjpdpfihaa80")?;
    let mut txn = pool.begin().await?;
    machine::create(
        &mut txn,
        None,
        &dpu_machine_id,
        ManagedHostState::Ready,
        None,
        2,
    )
    .await?;
    let reserved = machine::allocate_loopback_ip_v6(
        &common_pools,
        &mut txn,
        &dpu_machine_id.to_string(),
        None,
    )
    .await?
    .expect("configured IPv6 value");
    txn.commit().await?;
    let original = machine::MachineCreationIdentity::capture(&pool, &dpu_machine_id).await?;
    let original_config = machine::get_network_config(&pool, &dpu_machine_id).await?;

    // Preparation captures the old lifetime before waiting on its owned value.
    let mut gate = pool.begin().await?;
    let gate_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *gate)
        .await?;
    sqlx::query("SELECT value FROM resource_pool WHERE name = $1 AND value = $2 FOR UPDATE")
        .bind(LOOPBACK_IP_V6)
        .bind(reserved.to_string())
        .fetch_one(&mut *gate)
        .await?;
    let registry = ResourcePoolRegistry::default();
    let cancel = CancellationToken::new();
    let mut backfill = Box::pin(update_dpu_loopback_ips_v6(
        &pool,
        &registry,
        &common_pools,
        &cancel,
        &instrumentation,
    ));
    tokio::select! {
        result = &mut backfill => panic!("backfill passed the ownership gate: {result:?}"),
        _ = db::test_support::postgres::wait_for_blocked_query(&pool, gate_pid, "resource_pool") => {}
    }
    let mut replacement_txn = pool.begin().await?;
    machine::force_cleanup(&mut replacement_txn, &dpu_machine_id).await?;
    machine::create(
        &mut replacement_txn,
        None,
        &dpu_machine_id,
        ManagedHostState::Ready,
        None,
        2,
    )
    .await?;
    // ConfigVersion also carries a timestamp. Match the complete old version
    // so only the independently captured machine lifetime can reject this row.
    sqlx::query("UPDATE machines SET network_config_version = $1 WHERE id = $2")
        .bind(original_config.version)
        .bind(dpu_machine_id)
        .execute(&mut *replacement_txn)
        .await?;
    replacement_txn.commit().await?;
    let replacement = machine::get_network_config(&pool, &dpu_machine_id).await?;
    assert_eq!(replacement.version, original_config.version);
    assert_ne!(
        machine::MachineCreationIdentity::capture(&pool, &dpu_machine_id)
            .await?
            .created(),
        original.created()
    );
    // Free the old lifetime's value under the held lock. A stale attempt can
    // allocate it, but its configuration write must reject and roll back.
    assert_eq!(
        db::resource_pool::release(
            &common_pools.ethernet.pool_loopback_ip_v6,
            &mut gate,
            reserved,
            model::resource_pool::OwnerType::Machine,
            &dpu_machine_id.to_string(),
        )
        .await?,
        db::ConditionalWrite::Applied(())
    );
    gate.commit().await?;
    let error = tokio::time::timeout(std::time::Duration::from_secs(10), &mut backfill)
        .await?
        .expect_err("the old preparation must not write the replacement");
    assert!(matches!(
        error,
        carbide_resource_pool::ResourcePoolBackendError::Database(
            db::DatabaseError::ConcurrentModificationError(..)
        )
    ));
    drop(backfill);
    let after = machine::get_network_config(&pool, &dpu_machine_id).await?;
    assert_eq!(after.value, replacement.value);
    assert_eq!(after.version, replacement.version);
    let stats = db::resource_pool::stats(&pool, LOOPBACK_IP_V6).await?;
    assert_eq!(stats.used, 0);
    assert_eq!(stats.free, 1);
    update_dpu_loopback_ips_v6(&pool, &registry, &common_pools, &cancel, &instrumentation).await?;
    assert_eq!(
        machine::get_network_config(&pool, &dpu_machine_id)
            .await?
            .loopback_ip_v6,
        Some(reserved)
    );
    Ok(())
}

/// A busy target or host must release its tentative allocation, allow later
/// DPUs to progress, and remain repairable after the lock is released.
#[crate::sqlx_test]
async fn ipv6_loopback_backfill_rolls_back_busy_groups_and_retries_a_later_pass(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let test_meter = TestMeter::default();
    let instrumentation = Instrumentation::new(test_meter.meter());
    const DROP_ATTEMPT_COUNTER: &str =
        "DROP TRIGGER count_busy_backfill_allocation ON resource_pool_allocations;
         DROP FUNCTION count_busy_backfill_allocation();
         DROP SEQUENCE busy_backfill_attempts";

    let common_pools = common_pools(&pool, Some("2001:db8:7::/126")).await?;
    for (scenario, seed, target_busy) in [("busy group", 0, false), ("busy target", 2, true)] {
        let pool_before = db::resource_pool::stats(&pool, LOOPBACK_IP_V6).await?;
        let host_id = machine_id(seed, MachineType::Host);
        let dpu_ids = [
            machine_id(seed, MachineType::Dpu),
            machine_id(seed + 1, MachineType::Dpu),
        ];
        let mut setup = pool.begin().await?;
        for id in [host_id, dpu_ids[0], dpu_ids[1]] {
            machine::create(setup.as_mut(), None, &id, ManagedHostState::Ready, None, 2).await?;
        }
        let ordered_dpus: Vec<MachineId> =
            sqlx::query_scalar("SELECT id FROM machines WHERE id = ANY($1) ORDER BY id")
                .bind(dpu_ids.as_slice())
                .fetch_all(setup.as_mut())
                .await?;
        assert_eq!(ordered_dpus.len(), 2);
        let busy_id = ordered_dpus[0];
        let later_id = ordered_dpus[1];
        let segment_id: NetworkSegmentId = sqlx::query_scalar(
            "INSERT INTO network_segments (name, version)
             VALUES ($1, 'V1-T0') RETURNING id",
        )
        .bind(format!("ipv6-loopback-busy-{seed}"))
        .fetch_one(setup.as_mut())
        .await?;
        sqlx::query(
            "INSERT INTO machine_interfaces
                (machine_id, attached_dpu_machine_id, association_type,
                 segment_id, mac_address, primary_interface, hostname)
             VALUES ($1, $2, 'Machine', $3, $4::macaddr, true, $5)",
        )
        .bind(host_id)
        .bind(busy_id)
        .bind(segment_id)
        .bind(format!("02:00:00:00:11:{seed:02x}"))
        .bind(format!("busy-backfill-{seed}"))
        .execute(setup.as_mut())
        .await?;
        setup.commit().await?;
        let original_host = machine::get_network_config(&pool, &host_id).await?;
        let original_dpu = machine::get_network_config(&pool, &busy_id).await?;

        // Each fresh attempt reserves before NOWAIT. Sequence increments
        // survive rollback, unlike the reservation rows being counted.
        let counter = format!(
            r#"
            CREATE SEQUENCE busy_backfill_attempts;
            CREATE FUNCTION count_busy_backfill_allocation()
            RETURNS trigger LANGUAGE plpgsql AS $$
            BEGIN
                PERFORM nextval('busy_backfill_attempts');
                RETURN NEW;
            END;
            $$;
            CREATE TRIGGER count_busy_backfill_allocation
            AFTER INSERT ON resource_pool_allocations
            FOR EACH ROW
            WHEN (NEW.pool_name = '{LOOPBACK_IP_V6}'
                  AND NEW.owner_type = 'machine' AND NEW.owner_id = '{busy_id}')
            EXECUTE FUNCTION count_busy_backfill_allocation();
            "#
        );
        // Interpolate only the internal pool name and canonical fixture ID.
        sqlx::raw_sql(sqlx::AssertSqlSafe(counter))
            .execute(&pool)
            .await?;

        // Hold either actual contention boundary through the retry budget.
        // Rejection must release the reservation and any completed target write.
        let mut group_lock = pool.begin().await?;
        sqlx::query("SELECT id FROM machines WHERE id = $1 FOR NO KEY UPDATE")
            .bind(if target_busy { busy_id } else { host_id })
            .fetch_one(group_lock.as_mut())
            .await?;
        let registry = ResourcePoolRegistry::default();
        let cancel = CancellationToken::new();
        let backfill =
            update_dpu_loopback_ips_v6(&pool, &registry, &common_pools, &cancel, &instrumentation);
        tokio::pin!(backfill);
        let reconciled =
            tokio::time::timeout(std::time::Duration::from_secs(10), &mut backfill).await;
        let at_gate = async {
            let host = machine::get_network_config(&pool, &host_id).await?;
            let dpu = machine::get_network_config(&pool, &busy_id).await?;
            let later = machine::get_network_config(&pool, &later_id).await?;
            let stats = db::resource_pool::stats(&pool, LOOPBACK_IP_V6).await?;
            let busy_allocations: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM resource_pool_allocations
                 WHERE pool_name = $1 AND owner_id = $2",
            )
            .bind(LOOPBACK_IP_V6)
            .bind(busy_id.to_string())
            .fetch_one(&pool)
            .await?;
            let attempts: i64 = sqlx::query_scalar(
                "SELECT COALESCE(pg_sequence_last_value('busy_backfill_attempts'), 0)",
            )
            .fetch_one(&pool)
            .await?;
            Ok::<_, Box<dyn std::error::Error>>((
                host,
                dpu,
                later,
                stats,
                busy_allocations,
                attempts,
            ))
        }
        .await;
        let released = group_lock.rollback().await;
        released?;
        let reconciled = match reconciled {
            Ok(result) => result,
            Err(error) => {
                let late_result =
                    tokio::time::timeout(std::time::Duration::from_secs(30), &mut backfill).await?;
                tracing::info!(
                    scenario,
                    ?late_result,
                    "Joined backfill after releasing its gate"
                );
                sqlx::raw_sql(DROP_ATTEMPT_COUNTER).execute(&pool).await?;
                return Err(error.into());
            }
        };
        sqlx::raw_sql(DROP_ATTEMPT_COUNTER).execute(&pool).await?;
        let error =
            reconciled.expect_err("persistent contention must remain visible after the pass");
        match error {
            ResourcePoolBackendError::Database(db::DatabaseError::Sqlx(error)) => assert_eq!(
                error
                    .source
                    .as_database_error()
                    .and_then(|error| error.code())
                    .as_deref(),
                Some("55P03"),
                "{scenario}: retain the original NOWAIT error"
            ),
            error => panic!("{scenario}: expected a NOWAIT error, got {error:?}"),
        }

        let (
            host_after_contention,
            dpu_after_contention,
            later_config,
            stats,
            busy_allocations,
            attempts,
        ) = at_gate?;
        assert_eq!(
            attempts, 3,
            "{scenario}: each NOWAIT retry makes a fresh reservation"
        );
        assert_eq!(host_after_contention.value, original_host.value);
        assert_eq!(host_after_contention.version, original_host.version);
        assert_eq!(dpu_after_contention.value, original_dpu.value);
        assert_eq!(dpu_after_contention.version, original_dpu.version);
        let later_address = later_config
            .value
            .loopback_ip_v6
            .expect("an independent later DPU must still commit");
        assert_eq!(stats.used, pool_before.used + 1, "{scenario}");
        assert_eq!(stats.free, pool_before.free - 1, "{scenario}");
        assert_eq!(
            busy_allocations, 0,
            "failed attempts must release the reservation"
        );

        // Change the config before the next pass to check that recovery
        // preserves it.
        let mut change = pool.begin().await?;
        let (mut current_config, current_version) =
            machine::get_network_config(change.as_mut(), &busy_id)
                .await?
                .take();
        current_config.use_admin_network = Some(false);
        assert_eq!(
            machine::try_update_network_config(
                change.as_mut(),
                &busy_id,
                current_version,
                &current_config,
            )
            .await?,
            db::ConditionalWrite::Applied(())
        );
        change.commit().await?;
        let changed_config = machine::get_network_config(&pool, &busy_id).await?;
        update_dpu_loopback_ips_v6(
            &pool,
            &ResourcePoolRegistry::default(),
            &common_pools,
            &CancellationToken::new(),
            &instrumentation,
        )
        .await?;

        let host_after_retry = machine::get_network_config(&pool, &host_id).await?;
        let dpu_after_retry = machine::get_network_config(&pool, &busy_id).await?;
        let assigned = dpu_after_retry
            .value
            .loopback_ip_v6
            .expect("the next pass must backfill the released group");
        current_config.loopback_ip_v6 = Some(assigned);
        assert_eq!(dpu_after_retry.value, current_config);
        assert_eq!(host_after_retry.value, original_host.value);
        assert_eq!(host_after_retry.version, dpu_after_retry.version);
        assert_eq!(
            dpu_after_retry.version.version_nr(),
            changed_config.version.version_nr() + 1
        );
        assert_ne!(assigned, later_address);
        let later_after_retry = machine::get_network_config(&pool, &later_id).await?;
        assert_eq!(later_after_retry.value, later_config.value);
        assert_eq!(later_after_retry.version, later_config.version);
        let stats = db::resource_pool::stats(&pool, LOOPBACK_IP_V6).await?;
        assert_eq!(stats.used, pool_before.used + 2, "{scenario}");
        assert_eq!(stats.free, pool_before.free - 2, "{scenario}");
    }
    Ok(())
}

/// Exhaustion on a later DPU must preserve the rows committed earlier in the
/// pass so another request can continue after pool growth.
#[crate::sqlx_test]
async fn ipv6_loopback_backfill_preserves_completed_rows_when_pool_exhausts(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let test_meter = TestMeter::default();
    let instrumentation = Instrumentation::new(test_meter.meter());
    let common_pools = common_pools(&pool, Some("2001:db8:3::1/128")).await?;
    let dpu_machine_ids = [
        MachineId::from_str("fm100dskla0ihp0pn4tv7v1js2k2mo37sl0jjr8141okqg8pjpdpfihaa80")?,
        MachineId::from_str("fm100ds27v4uuq7sgs4gsjummskt0b3tedugtpevjrbfh6su081n9jufcq0")?,
    ];

    // Passing no common pools recreates rows from before the IPv6 pool was
    // configured without consuming its only address during insertion.
    let mut txn = pool.begin().await?;
    for dpu_machine_id in &dpu_machine_ids {
        machine::create(
            txn.as_mut(),
            None,
            dpu_machine_id,
            ManagedHostState::Ready,
            None,
            2,
        )
        .await?;
    }
    txn.commit().await?;

    let error = update_dpu_loopback_ips_v6(
        &pool,
        &ResourcePoolRegistry::default(),
        &common_pools,
        &CancellationToken::new(),
        &instrumentation,
    )
    .await
    .expect_err("one address cannot backfill two DPUs");
    assert!(matches!(
        error,
        ResourcePoolBackendError::Database(db::DatabaseError::ResourceExhausted(_))
    ));

    let mut txn = pool.begin().await?;
    let configs_after_first_pass = [
        machine::get_network_config(txn.as_mut(), &dpu_machine_ids[0])
            .await?
            .value,
        machine::get_network_config(txn.as_mut(), &dpu_machine_ids[1])
            .await?
            .value,
    ];
    assert_eq!(
        configs_after_first_pass
            .iter()
            .filter(|config| config.loopback_ip_v6.is_some())
            .count(),
        1
    );
    txn.commit().await?;
    let stats_after_first_pass = db::resource_pool::stats(&pool, LOOPBACK_IP_V6).await?;
    assert_eq!(stats_after_first_pass.used, 1);
    assert_eq!(stats_after_first_pass.free, 0);

    let error = update_dpu_loopback_ips_v6(
        &pool,
        &ResourcePoolRegistry::default(),
        &common_pools,
        &CancellationToken::new(),
        &instrumentation,
    )
    .await
    .expect_err("the remaining DPU must continue to report exhaustion");
    assert!(matches!(
        error,
        ResourcePoolBackendError::Database(db::DatabaseError::ResourceExhausted(_))
    ));

    let mut txn = pool.begin().await?;
    let configs_after_second_pass = [
        machine::get_network_config(txn.as_mut(), &dpu_machine_ids[0])
            .await?
            .value,
        machine::get_network_config(txn.as_mut(), &dpu_machine_ids[1])
            .await?
            .value,
    ];
    txn.commit().await?;
    assert_eq!(configs_after_second_pass, configs_after_first_pass);
    assert_eq!(
        db::resource_pool::stats(&pool, LOOPBACK_IP_V6).await?,
        stats_after_first_pass
    );
    // Both failed passes must record into the supplied provider, so a hidden
    // global metrics destination cannot mask lost IPv6 failure counts.
    let failures = test_meter.parsed_metrics("carbide_resource_pool_lifecycle_failures_total");
    assert_eq!(failures.len(), 1);
    assert!(failures[0].0.contains(r#"value_type="ipv6""#));
    assert_eq!(failures[0].1, "2");
    Ok(())
}

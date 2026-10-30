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

use std::borrow::Borrow;
use std::collections::HashSet;
use std::net::IpAddr;
use std::str::FromStr;
use std::time::Duration;

use carbide_instrument::testing::MetricsCapture;
use carbide_network::ip::IpAddressFamily;
use carbide_uuid::machine::{MachineId, MachineInterfaceId};
use common::api_fixtures::network_segment::{
    FIXTURE_ADMIN_NETWORK_SEGMENT_GATEWAY, FIXTURE_HOST_INBAND_NETWORK_SEGMENT_GATEWAY,
    create_host_inband_network_segment,
};
use common::api_fixtures::{
    FIXTURE_DHCP_RELAY_ADDRESS, create_managed_host_multi_dpu, create_managed_host_with_config,
    create_test_env,
};
use db::dhcp_entry::DhcpEntry;
use db::{self};
use itertools::Itertools;
use mac_address::MacAddress;
use model::address_selection_strategy::AddressSelectionStrategy;
use model::allocation_type::AllocationType;
use model::machine::MachineInterfaceSnapshot;
use model::machine::machine_id::from_hardware_info;
use model::machine_interface::InterfaceType;
use model::machine_interface_address::MachineInterfaceAssociation;
use model::network_segment::NetworkSegmentType;
use model::test_support::ManagedHostConfig;
use rpc::forge::InterfaceSearchQuery;
use rpc::forge::forge_server::Forge;
use tokio::sync::broadcast;
use tonic::Code;

use crate::DatabaseError;
use crate::test_support::fixture_config::{FixtureDefault as _, ManagedHostConfigExt as _};
use crate::tests::common;
use crate::tests::common::api_fixtures::dpu::{create_dpu_machine, dpu_discover_machine};
use crate::tests::common::postgres::wait_for_blocked_query;

/// Adds an older BMC row so tests can prove topology selection wins over row
/// age.
async fn insert_older_bmc_interface(
    txn: &mut sqlx::PgConnection,
    machine_id: MachineId,
    current_interface: &MachineInterfaceSnapshot,
    mac_address: MacAddress,
    ip_address: IpAddr,
) -> Result<MachineInterfaceId, Box<dyn std::error::Error>> {
    let interface_id = sqlx::query_scalar(
        "INSERT INTO machine_interfaces
         (machine_id, association_type, segment_id, mac_address,
          primary_interface, hostname, interface_type, created)
         VALUES ($1, 'Machine', $2, $3, false, 'stale-bmc', 'Bmc',
                 $4 - INTERVAL '1 day')
         RETURNING id",
    )
    .bind(machine_id)
    .bind(current_interface.segment_id)
    .bind(mac_address)
    .bind(current_interface.created)
    .fetch_one(&mut *txn)
    .await?;
    db::machine_interface_address::insert(txn, interface_id, ip_address, AllocationType::Dhcp)
        .await?;
    Ok(interface_id)
}

#[crate::sqlx_test]
async fn only_one_primary_interface_per_machine(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let host_config = env.managed_host_config();
    let dpu = host_config.get_and_assert_single_dpu();
    let other_host_config = env.managed_host_config();
    let other_dpu = other_host_config.get_and_assert_single_dpu();

    let mut txn = env.pool.begin().await?;

    let network_segment = db::network_segment::admin(&mut txn)
        .await?
        .into_iter()
        .next()
        .unwrap();

    let new_interface = db::machine_interface::create(
        &mut txn,
        std::slice::from_ref(&network_segment),
        &dpu.oob_mac_address,
        true,
        AddressSelectionStrategy::NextAvailableIp,
        None,
    )
    .await?;

    let machine_id = from_hardware_info(&host_config.borrow().into()).unwrap();
    let new_machine = db::machine::get_or_create(&mut txn, None, &machine_id, &new_interface)
        .await
        .expect("Unable to create machine");

    txn.commit().await.unwrap();

    let mut txn = env.pool.begin().await?;

    let should_failed_machine_interface = db::machine_interface::create(
        &mut txn,
        std::slice::from_ref(&network_segment),
        &other_dpu.oob_mac_address,
        true,
        AddressSelectionStrategy::NextAvailableIp,
        None,
    )
    .await?;

    let output = db::machine_interface::associate_interface_with_machine(
        &should_failed_machine_interface.id,
        MachineInterfaceAssociation::Machine(new_machine.id),
        &mut txn,
    )
    .await;

    txn.commit().await.unwrap();

    assert!(matches!(output, Err(DatabaseError::OnePrimaryInterface)));

    Ok(())
}

#[crate::sqlx_test]
async fn many_non_primary_interfaces_per_machine(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let mut txn = env.pool.begin().await?;
    let network_segment = db::network_segment::admin(&mut txn)
        .await?
        .into_iter()
        .next()
        .unwrap();

    db::machine_interface::create(
        &mut txn,
        std::slice::from_ref(&network_segment),
        MacAddress::from_str("ff:ff:ff:ff:ff:ff").as_ref().unwrap(),
        true,
        AddressSelectionStrategy::NextAvailableIp,
        None,
    )
    .await
    .expect("Unable to create machine interface");

    txn.commit().await.unwrap();
    let mut txn = env.pool.begin().await?;

    let should_be_ok_interface = db::machine_interface::create(
        &mut txn,
        std::slice::from_ref(&network_segment),
        MacAddress::from_str("ff:ff:ff:ff:ff:ef").as_ref().unwrap(),
        false,
        AddressSelectionStrategy::NextAvailableIp,
        None,
    )
    .await;

    txn.commit().await.unwrap();

    assert!(should_be_ok_interface.is_ok());

    Ok(())
}

#[crate::sqlx_test]
async fn reconcile_admin_addresses_moves_dhcp_to_new_primary_and_is_idempotent(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let mh = create_managed_host_multi_dpu(&env, 2).await;
    let mut txn = env.pool.begin().await?;

    // Start with a reconciled multi-DPU host and flip the primary flag in
    // the DB to simulate the persistence side of set-primary-dpu.
    let mut interface_map = db::machine_interface::find_by_machine_ids(&mut txn, &[mh.id]).await?;
    let interfaces = interface_map.remove(&mh.id).unwrap();
    let primary = interfaces
        .iter()
        .find(|interface| {
            interface.network_segment_type == Some(NetworkSegmentType::Admin)
                && interface.attached_dpu_machine_id.is_some()
                && interface.primary_interface
        })
        .cloned()
        .unwrap();
    let secondary = interfaces
        .iter()
        .find(|interface| {
            interface.network_segment_type == Some(NetworkSegmentType::Admin)
                && interface.attached_dpu_machine_id.is_some()
                && !interface.primary_interface
        })
        .cloned()
        .unwrap();
    let active_address = primary.addresses[0];
    // Change policy after allocation so reconciliation must reuse the donor,
    // not satisfy the new primary by allocating another DHCP address.
    let reserved =
        sqlx::query("UPDATE network_segments SET allocation_strategy = 'reserved' WHERE id = $1")
            .bind(primary.segment_id)
            .execute(&mut *txn)
            .await?;
    assert_eq!(reserved.rows_affected(), 1);
    db::machine_interface::set_primary_interface(&primary.id, false, &mut txn).await?;
    db::machine_interface::set_primary_interface(&secondary.id, true, &mut txn).await?;

    // Reserved policy forbids fresh DHCP allocation, but moving an existing
    // address to the new primary must still preserve the host's admin IP.
    let active_config_changed =
        db::machine_interface::reconcile_admin_addresses_for_host(&mut txn, &mh.id)
            .await
            .unwrap();
    assert!(active_config_changed);

    // Verify the new primary owns the preserved DHCP address and the old
    // primary is addressless and DNS-silent.
    let mut interface_map = db::machine_interface::find_by_machine_ids(&mut txn, &[mh.id]).await?;
    let interfaces = interface_map.remove(&mh.id).unwrap();
    let new_primary = interfaces
        .iter()
        .find(|interface| interface.id == secondary.id)
        .unwrap();
    let old_primary = interfaces
        .iter()
        .find(|interface| interface.id == primary.id)
        .unwrap();
    assert_eq!(new_primary.addresses, vec![active_address]);
    assert!(old_primary.addresses.is_empty());
    assert!(old_primary.domain_id.is_none());
    assert!(old_primary.hostname.starts_with("noip-"));

    let address_rows =
        db::machine_interface_address::find_for_interface(&mut txn, new_primary.id).await?;
    assert!(
        address_rows
            .iter()
            .any(|address| address.address == active_address
                && address.allocation_type == AllocationType::Dhcp)
    );

    // A second reconcile should be a no-op.
    let active_config_changed =
        db::machine_interface::reconcile_admin_addresses_for_host(&mut txn, &mh.id)
            .await
            .unwrap();
    assert!(!active_config_changed);

    txn.commit().await?;

    let mut txn = env.pool.begin().await?;
    let new_primary = db::machine_interface::find_one(&mut *txn, secondary.id).await?;
    let old_primary = db::machine_interface::find_one(&mut *txn, primary.id).await?;
    assert_eq!(new_primary.addresses, vec![active_address]);
    assert!(old_primary.addresses.is_empty());
    txn.commit().await?;

    Ok(())
}

/// An addressless primary cannot allocate from a Reserved segment. Once an
/// operator restores a static address, reconciliation must leave it untouched.
#[crate::sqlx_test]
async fn reconcile_admin_addresses_requires_an_existing_address_on_reserved_segment(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let mh = create_managed_host_multi_dpu(&env, 1).await;
    let mut txn = env.pool.begin().await?;
    let mut interface_map = db::machine_interface::find_by_machine_ids(&mut txn, &[mh.id]).await?;
    let interfaces = interface_map.remove(&mh.id).unwrap();
    let admin_interfaces: Vec<_> = interfaces
        .iter()
        .filter(|interface| interface.network_segment_type == Some(NetworkSegmentType::Admin))
        .collect();
    let [primary] = admin_interfaces.as_slice() else {
        panic!("single-DPU host must have exactly one admin interface")
    };
    assert!(primary.primary_interface);
    assert_eq!(primary.attached_dpu_machine_id, Some(mh.dpu().id));
    let [active_address] = primary.addresses.as_slice() else {
        panic!("primary admin interface must have exactly one IPv4 address")
    };
    assert!(active_address.is_ipv4());

    // Remove the only DHCP address after reserving the segment: no donor can
    // satisfy repair, so this reaches the fresh-allocation rejection.
    let reserved =
        sqlx::query("UPDATE network_segments SET allocation_strategy = 'reserved' WHERE id = $1")
            .bind(primary.segment_id)
            .execute(&mut *txn)
            .await?;
    assert_eq!(reserved.rows_affected(), 1);
    assert!(
        db::machine_interface_address::delete_by_interface_family(
            &mut txn,
            primary.id,
            IpAddressFamily::Ipv4,
            AllocationType::Dhcp,
        )
        .await?
    );
    txn.commit().await?;

    let mut txn = env.pool.begin().await?;
    let error = db::machine_interface::reconcile_admin_addresses_for_host(&mut txn, &mh.id)
        .await
        .expect_err("Reserved segment must reject a fresh DHCP allocation");
    assert!(matches!(
        error,
        DatabaseError::Internal { message } if message.contains("static DHCP leases only")
    ));
    txn.rollback().await?;

    let mut txn = env.pool.begin().await?;
    let persisted = db::machine_interface::find_one(&mut *txn, primary.id).await?;
    assert!(persisted.addresses.is_empty());
    assert!(
        db::machine_interface_address::find_for_interface(&mut txn, primary.id)
            .await?
            .is_empty()
    );

    // An operator-provided Static address restores service without reopening
    // dynamic allocation; the next reconciliation must preserve its type.
    db::machine_interface_address::assign_static(&mut txn, primary.id, *active_address).await?;
    txn.commit().await?;

    let mut txn = env.pool.begin().await?;
    assert!(!db::machine_interface::reconcile_admin_addresses_for_host(&mut txn, &mh.id).await?);
    txn.commit().await?;

    let mut txn = env.pool.begin().await?;
    let persisted = db::machine_interface::find_one(&mut *txn, primary.id).await?;
    assert_eq!(persisted.addresses, vec![*active_address]);
    let addresses = db::machine_interface_address::find_for_interface(&mut txn, primary.id).await?;
    let [address] = addresses.as_slice() else {
        panic!("reconciliation must preserve exactly one static address")
    };
    assert_eq!(address.address, *active_address);
    assert_eq!(address.allocation_type, AllocationType::Static);
    txn.commit().await?;

    Ok(())
}

/// A change made to a reconciled two-DPU host before the reconcile under test.
#[derive(Clone, Copy, Debug)]
enum HostChange {
    None,
    /// The dormant link becomes the primary, so the address must move to it.
    MovePrimaryFlag,
    /// The dormant link gains a DHCP address row that must be deleted.
    DhcpRowOnDormantLink,
    /// The addressless dormant link gains a domain that must be cleared.
    DomainOnDormantLink,
    /// The primary loses its domain and must get its segment's subdomain back.
    PrimaryDomainCleared,
    /// No interface of the host is primary any more.
    ClearPrimaryFlag,
    /// The primary loses its address rows, hostname, and domain and must
    /// allocate again.
    PrimaryAddressesDeleted,
}

/// What `reconcile_admin_addresses_for_host` does for a host state, judged
/// from outside: whether it takes the admin segment locks, and what the
/// locked pass then reports.
#[derive(Clone, Copy, Debug)]
enum ReconcileExpectation {
    /// The unlocked pass answers: no segment lock, no change, `Ok(false)`.
    NoRepair,
    /// The locked pass writes and reports whether the active config changed.
    Repairs { active_config_changed: bool },
    /// The locked pass fails.
    Fails,
}

/// The unlocked pass must send every host the locked pass would write to or
/// fail on into the locked pass, and keep every other host off the admin
/// segment locks. An exclusive gate on the segments makes the difference
/// observable: the locked pass waits for it, the unlocked pass does not.
#[crate::sqlx_test]
async fn reconcile_admin_addresses_locks_segments_only_for_a_repair(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    struct Case {
        change: HostChange,
        expect: ReconcileExpectation,
    }
    let cases = [
        Case {
            change: HostChange::None,
            expect: ReconcileExpectation::NoRepair,
        },
        Case {
            change: HostChange::MovePrimaryFlag,
            expect: ReconcileExpectation::Repairs {
                active_config_changed: true,
            },
        },
        Case {
            change: HostChange::DhcpRowOnDormantLink,
            expect: ReconcileExpectation::Repairs {
                active_config_changed: false,
            },
        },
        Case {
            change: HostChange::DomainOnDormantLink,
            expect: ReconcileExpectation::Repairs {
                active_config_changed: false,
            },
        },
        Case {
            change: HostChange::PrimaryDomainCleared,
            expect: ReconcileExpectation::Repairs {
                active_config_changed: true,
            },
        },
        Case {
            change: HostChange::ClearPrimaryFlag,
            expect: ReconcileExpectation::Fails,
        },
    ];

    let env = create_test_env(pool).await;
    let admin_subdomain = admin_segment_subdomain(&env).await?;
    assert!(
        admin_subdomain.is_some(),
        "the domain checks need an admin segment with a subdomain"
    );
    for Case { change, expect } in cases {
        let mh = create_managed_host_multi_dpu(&env, 2).await;
        apply_host_change(&env, &mh, change).await?;
        let before = host_interface_state(&env, &mh).await?;

        let mut gate = env.pool.begin().await?;
        db::machine_interface::lock_all_admin_segments(&mut gate).await?;
        let gate_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *gate)
            .await?;

        let mut txn = env.pool.begin().await?;
        let result = {
            let reconcile =
                db::machine_interface::reconcile_admin_addresses_for_host(&mut txn, &mh.id);
            tokio::pin!(reconcile);
            match expect {
                ReconcileExpectation::NoRepair => {
                    let result = tokio::time::timeout(Duration::from_secs(10), &mut reconcile)
                        .await
                        .unwrap_or_else(|_| {
                            panic!("{change:?}: a host with nothing to repair waited on the admin segment locks")
                        });
                    gate.rollback().await?;
                    result
                }
                ReconcileExpectation::Repairs { .. } | ReconcileExpectation::Fails => {
                    tokio::select! {
                        result = &mut reconcile => {
                            panic!("{change:?}: a pending repair bypassed the admin segment locks: {result:?}")
                        }
                        _ = wait_for_blocked_query(&env.pool, gate_pid, "pg_advisory_xact_lock") => {}
                    }
                    gate.rollback().await?;
                    reconcile.await
                }
            }
        };

        match expect {
            ReconcileExpectation::NoRepair => {
                assert!(!result?, "{change:?}");
                txn.commit().await?;
                assert_eq!(host_interface_state(&env, &mh).await?, before, "{change:?}");
            }
            ReconcileExpectation::Repairs {
                active_config_changed,
            } => {
                assert_eq!(result?, active_config_changed, "{change:?}");
                txn.commit().await?;
                let after = host_interface_state(&env, &mh).await?;
                assert_ne!(after, before, "{change:?}");
                let (primary, dormant): (Vec<_>, Vec<_>) = after
                    .into_iter()
                    .partition(|interface| interface.primary_interface);
                assert_eq!(primary.len(), 1, "{change:?}");
                assert!(!primary[0].addresses.is_empty(), "{change:?}");
                assert_eq!(primary[0].domain_id, admin_subdomain, "{change:?}");
                assert!(
                    dormant
                        .iter()
                        .all(|interface| interface.addresses.is_empty()
                            && interface.domain_id.is_none()),
                    "{change:?}"
                );
            }
            ReconcileExpectation::Fails => {
                assert!(result.is_err(), "{change:?}");
                txn.rollback().await?;
            }
        }
    }

    Ok(())
}

/// A DPU rediscovery whose post-commit reconcile fails still answers with its
/// machine id: the failure is counted and the host's rows are left as they
/// were.
#[crate::sqlx_test]
async fn dpu_rediscovery_counts_a_failed_admin_address_reconcile(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let host_config = ManagedHostConfig::default().with_dpu_count(2);
    let mh = create_managed_host_with_config(&env, host_config.clone()).await;
    apply_host_change(&env, &mh, HostChange::ClearPrimaryFlag).await?;
    let before = host_interface_state(&env, &mh).await?;
    let dpu = &host_config.dpus[0];
    let dpu_interface = db::machine_interface::find_by_mac_address(&env.pool, dpu.oob_mac_address)
        .await?
        .into_iter()
        .find(|interface| {
            interface
                .machine_id
                .is_some_and(|machine_id| machine_id.machine_type().is_dpu())
        })
        .expect("the DPU OOB interface should belong to the DPU machine");
    let dpu_machine_id = dpu_interface
        .machine_id
        .expect("the OOB interface was selected by its DPU machine id");

    let metrics = MetricsCapture::start();
    assert_eq!(
        dpu_discover_machine(&env, dpu, dpu_interface.id).await,
        dpu_machine_id
    );
    assert_eq!(
        metrics.counter_delta(
            "carbide_api_host_admin_address_reconcile_failures_total",
            &[]
        ),
        1.0
    );
    assert_eq!(host_interface_state(&env, &mh).await?, before);

    Ok(())
}

/// A transient failure of the after-commit reconcile leaves the host's rows
/// and network config version as they were, returns the error for the
/// caller to count, and flags the host's BMC endpoint for exploration; the
/// next call makes the repair. The failure is an allocation that finds every
/// address of the admin segment locked by another session.
#[crate::sqlx_test]
async fn after_commit_reconcile_flags_a_transient_failure_and_repairs_on_the_next_call(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let admin_subdomain = admin_segment_subdomain(&env).await?;
    assert!(
        admin_subdomain.is_some(),
        "the domain check needs an admin segment with a subdomain"
    );
    let mh = create_managed_host_multi_dpu(&env, 2).await;
    apply_host_change(&env, &mh, HostChange::PrimaryAddressesDeleted).await?;
    let before = host_interface_state(&env, &mh).await?;
    let version_before = host_network_config_version(&env, &mh).await?;
    assert!(!host_exploration_requested(&env, &mh).await?);

    let mut gate = env.pool.begin().await?;
    hold_every_admin_address_lock(&env, &mut gate).await?;
    let result = tokio::time::timeout(
        Duration::from_secs(30),
        db::machine_interface::reconcile_admin_addresses_after_commit(&env.pool, &mh.id),
    )
    .await?;
    assert!(matches!(result, Err(DatabaseError::TryAgain)), "{result:?}");
    assert!(
        host_exploration_requested(&env, &mh).await?,
        "a transient failure flags the host BMC for exploration"
    );
    assert_eq!(host_interface_state(&env, &mh).await?, before);
    assert_eq!(
        host_network_config_version(&env, &mh).await?,
        version_before
    );
    gate.rollback().await?;

    let active_config_changed = tokio::time::timeout(
        Duration::from_secs(30),
        db::machine_interface::reconcile_admin_addresses_after_commit(&env.pool, &mh.id),
    )
    .await??;
    assert!(active_config_changed);
    let (primary, dormant): (Vec<_>, Vec<_>) = host_interface_state(&env, &mh)
        .await?
        .into_iter()
        .partition(|interface| interface.primary_interface);
    assert_eq!(primary.len(), 1);
    assert_eq!(primary[0].addresses.len(), 1, "{primary:?}");
    assert!(FIXTURE_ADMIN_NETWORK_SEGMENT_GATEWAY.contains(primary[0].addresses[0]));
    assert_ne!(primary[0].hostname, "needs-reconciliation");
    assert_eq!(primary[0].domain_id, admin_subdomain);
    assert!(
        dormant
            .iter()
            .all(|interface| interface.addresses.is_empty() && interface.domain_id.is_none()),
        "{dormant:?}"
    );
    assert_eq!(
        host_network_config_version(&env, &mh).await?,
        version_before + 1
    );
    // The request stays for Site Explorer to consume with its next report.
    assert!(host_exploration_requested(&env, &mh).await?);

    Ok(())
}

/// A transient failure that clears between attempts is recovered within one
/// call of the after-commit reconcile: the first attempt's allocation finds
/// every address of the admin segment locked by another session, the second
/// allocates once those locks are gone, and the host's BMC endpoint is never
/// flagged for exploration. A gate on the admin segment parks each attempt
/// before it allocates, so the call is seen making exactly two.
#[crate::sqlx_test]
async fn after_commit_reconcile_retries_once_a_transient_failure_clears(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let mh = create_managed_host_multi_dpu(&env, 2).await;
    apply_host_change(&env, &mh, HostChange::PrimaryAddressesDeleted).await?;
    let version_before = host_network_config_version(&env, &mh).await?;

    let mut address_gate = env.pool.begin().await?;
    hold_every_admin_address_lock(&env, &mut address_gate).await?;
    let mut segment_gate = env.pool.begin().await?;
    let segment_gate_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *segment_gate)
        .await?;
    db::machine_interface::lock_all_admin_segments(&mut segment_gate).await?;

    // The first attempt waits for the admin segment.
    let reconcile =
        db::machine_interface::reconcile_admin_addresses_after_commit(&env.pool, &mh.id);
    tokio::pin!(reconcile);
    tokio::select! {
        result = &mut reconcile => {
            panic!("the reconcile finished while the admin segment was held: {result:?}")
        }
        _ = wait_for_blocked_query(&env.pool, segment_gate_pid, "pg_advisory_xact_lock_shared") => {}
    }

    // Released, the first attempt takes the segment, finds every address
    // locked, and fails. The segment taken again is granted once that attempt
    // has rolled back, so the second attempt waits for it in turn.
    segment_gate.rollback().await?;
    let mut segment_gate = env.pool.begin().await?;
    let segment_gate_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *segment_gate)
        .await?;
    tokio::select! {
        result = &mut reconcile => {
            panic!("the reconcile finished while every admin address was locked: {result:?}")
        }
        locked = db::machine_interface::lock_all_admin_segments(&mut segment_gate) => locked?,
    }
    tokio::select! {
        result = &mut reconcile => {
            panic!("the reconcile finished without a second attempt: {result:?}")
        }
        _ = wait_for_blocked_query(&env.pool, segment_gate_pid, "pg_advisory_xact_lock_shared") => {}
    }
    assert!(
        !host_exploration_requested(&env, &mh).await?,
        "a failure that is retried does not flag the host BMC for exploration"
    );

    // The failure clears before the second attempt allocates.
    address_gate.rollback().await?;
    segment_gate.rollback().await?;
    let active_config_changed = tokio::time::timeout(Duration::from_secs(30), reconcile).await??;
    assert!(active_config_changed);
    let primary = host_interface_state(&env, &mh)
        .await?
        .into_iter()
        .find(|interface| interface.primary_interface)
        .expect("the host has a primary admin link");
    assert_eq!(primary.addresses.len(), 1, "{primary:?}");
    assert!(FIXTURE_ADMIN_NETWORK_SEGMENT_GATEWAY.contains(primary.addresses[0]));
    assert_eq!(
        host_network_config_version(&env, &mh).await?,
        version_before + 1
    );
    assert!(!host_exploration_requested(&env, &mh).await?);

    Ok(())
}

/// When every swap of the network config bump finds the version already
/// moved, the after-commit reconcile gives up with
/// `NetworkConfigVersionChanged`: the repair is rolled back with the bump and
/// the host's BMC endpoint is flagged for exploration. The swaps are counted,
/// so the call is seen making three attempts of three swaps each.
#[crate::sqlx_test]
async fn after_commit_reconcile_rolls_back_when_the_config_bump_loses_every_swap(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let mh = create_managed_host_multi_dpu(&env, 2).await;
    apply_host_change(&env, &mh, HostChange::PrimaryDomainCleared).await?;
    let before = host_interface_state(&env, &mh).await?;
    let version_before = host_network_config_version(&env, &mh).await?;
    assert!(!host_exploration_requested(&env, &mh).await?);

    move_version_under_every_swap(&env.pool).await?;
    let result = tokio::time::timeout(
        Duration::from_secs(30),
        db::machine_interface::reconcile_admin_addresses_after_commit(&env.pool, &mh.id),
    )
    .await?;
    let Err(DatabaseError::NetworkConfigVersionChanged {
        machine_id,
        attempts,
    }) = result
    else {
        panic!("{result:?}")
    };
    assert_eq!(machine_id, MachineId::from(mh.id));
    assert_eq!(attempts, 3, "every swap of the last bump lost");
    assert_eq!(
        network_config_swaps(&env.pool).await?,
        9,
        "three reconcile attempts of three swaps each"
    );
    assert_eq!(
        host_interface_state(&env, &mh).await?,
        before,
        "the repair is rolled back with the bump"
    );
    assert_eq!(
        host_network_config_version(&env, &mh).await?,
        version_before
    );
    assert!(host_exploration_requested(&env, &mh).await?);

    Ok(())
}

/// Reconciles `mh`, a two-DPU host, then applies `change` to its DPU-backed
/// admin links.
async fn apply_host_change(
    env: &common::api_fixtures::TestEnv,
    mh: &common::api_fixtures::TestManagedHost,
    change: HostChange,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = env.pool.begin().await?;
    db::machine_interface::reconcile_admin_addresses_for_host(&mut txn, &mh.id).await?;
    txn.commit().await?;
    let mut txn = env.pool.begin().await?;
    let mut interface_map = db::machine_interface::find_by_machine_ids(&mut txn, &[mh.id]).await?;
    let interfaces = interface_map.remove(&mh.id).unwrap();
    let dpu_admin_interface = |primary: bool| {
        interfaces
            .iter()
            .find(|interface| {
                interface.network_segment_type == Some(NetworkSegmentType::Admin)
                    && interface.attached_dpu_machine_id.is_some()
                    && interface.primary_interface == primary
            })
            .cloned()
            .unwrap()
    };
    let primary = dpu_admin_interface(true);
    let dormant = dpu_admin_interface(false);
    match change {
        HostChange::None => {}
        HostChange::MovePrimaryFlag => {
            db::machine_interface::set_primary_interface(&primary.id, false, &mut txn).await?;
            db::machine_interface::set_primary_interface(&dormant.id, true, &mut txn).await?;
        }
        HostChange::DhcpRowOnDormantLink => {
            db::machine_interface_address::insert(
                &mut txn,
                dormant.id,
                "198.51.100.77".parse()?,
                AllocationType::Dhcp,
            )
            .await?;
        }
        HostChange::DomainOnDormantLink => {
            sqlx::query("UPDATE machine_interfaces SET domain_id = $1 WHERE id = $2")
                .bind(env.domain)
                .bind(dormant.id)
                .execute(&mut *txn)
                .await?;
        }
        HostChange::PrimaryDomainCleared => {
            sqlx::query("UPDATE machine_interfaces SET domain_id = NULL WHERE id = $1")
                .bind(primary.id)
                .execute(&mut *txn)
                .await?;
        }
        HostChange::ClearPrimaryFlag => {
            db::machine_interface::set_primary_interface(&primary.id, false, &mut txn).await?;
        }
        HostChange::PrimaryAddressesDeleted => {
            sqlx::query("DELETE FROM machine_interface_addresses WHERE interface_id = $1")
                .bind(primary.id)
                .execute(&mut *txn)
                .await?;
            sqlx::query(
                "UPDATE machine_interfaces SET hostname = 'needs-reconciliation', domain_id = NULL
                 WHERE id = $1",
            )
            .bind(primary.id)
            .execute(&mut *txn)
            .await?;
        }
    }
    txn.commit().await?;
    Ok(())
}

/// The subdomain the fixture admin segment publishes its primaries under.
async fn admin_segment_subdomain(
    env: &common::api_fixtures::TestEnv,
) -> Result<Option<carbide_uuid::domain::DomainId>, Box<dyn std::error::Error>> {
    let mut txn = env.pool.begin().await?;
    let admin_segment = db::network_segment::admin(&mut txn)
        .await?
        .into_iter()
        .next()
        .expect("the test env should have an admin segment");
    txn.commit().await?;
    Ok(admin_segment.config.subdomain_id)
}

/// The DPU-backed admin links of `mh`, as the fields the reconcile may write.
#[derive(Debug, PartialEq, Eq)]
struct AdminLinkState {
    primary_interface: bool,
    hostname: String,
    domain_id: Option<carbide_uuid::domain::DomainId>,
    addresses: Vec<std::net::IpAddr>,
}

async fn host_interface_state(
    env: &common::api_fixtures::TestEnv,
    mh: &common::api_fixtures::TestManagedHost,
) -> Result<Vec<AdminLinkState>, Box<dyn std::error::Error>> {
    let mut txn = env.pool.begin().await?;
    let mut interface_map = db::machine_interface::find_by_machine_ids(&mut txn, &[mh.id]).await?;
    let mut interfaces = interface_map.remove(&mh.id).unwrap_or_default();
    interfaces.sort_by_key(|interface| interface.id);
    Ok(interfaces
        .into_iter()
        .filter(|interface| {
            interface.network_segment_type == Some(NetworkSegmentType::Admin)
                && interface.attached_dpu_machine_id.is_some()
        })
        .map(|interface| AdminLinkState {
            primary_interface: interface.primary_interface,
            hostname: interface.hostname,
            domain_id: interface.domain_id,
            addresses: interface.addresses,
        })
        .collect())
}

/// The host's network config version as stored.
async fn host_network_config_version(
    env: &common::api_fixtures::TestEnv,
    mh: &common::api_fixtures::TestManagedHost,
) -> Result<u64, Box<dyn std::error::Error>> {
    Ok(db::machine::get_network_config(&env.pool, &mh.id)
        .await?
        .version
        .version_nr())
}

/// Whether the host's BMC endpoint is flagged for exploration.
async fn host_exploration_requested(
    env: &common::api_fixtures::TestEnv,
    mh: &common::api_fixtures::TestManagedHost,
) -> Result<bool, Box<dyn std::error::Error>> {
    let mut txn = env.pool.begin().await?;
    let bmc_ip = mh
        .host()
        .bmc_ip(&mut txn)
        .await
        .expect("the fixture host should have a BMC address");
    let endpoint = db::explored_endpoints::find_all_by_ip(bmc_ip, &mut txn)
        .await?
        .into_iter()
        .next()
        .expect("the fixture host BMC should have an explored endpoint");
    txn.commit().await?;
    Ok(endpoint.exploration_requested)
}

/// Holds the allocator's per-address lock on every address of the fixture
/// admin segment in `gate`, so an allocation on it finds every candidate
/// taken and returns `TryAgain`. The segment lock itself stays free.
async fn hold_every_admin_address_lock(
    env: &common::api_fixtures::TestEnv,
    gate: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = env.pool.begin().await?;
    let segment = db::network_segment::admin(&mut txn)
        .await?
        .into_iter()
        .next()
        .expect("the test env should have an admin segment");
    txn.commit().await?;
    let keys = segment
        .prefixes
        .iter()
        .flat_map(|prefix| prefix.prefix.iter())
        .map(|address| format!("{}:{}", segment.id, address))
        .collect::<Vec<_>>();
    let all_taken: bool = sqlx::query_scalar(
        "SELECT bool_and(pg_try_advisory_xact_lock(hashtextextended(key, 0)))
         FROM unnest($1::text[]) AS key",
    )
    .bind(&keys)
    .fetch_one(&mut **gate)
    .await?;
    assert!(
        all_taken,
        "no other session should hold an admin address lock"
    );
    Ok(())
}

/// Makes every swap of a machine's network config version lose the way it
/// loses to a concurrent writer: the version the swap would publish is on the
/// row before the swap runs, so the swap matches nothing. Only the reconcile
/// under test swaps versions while this is installed. Each swap is counted
/// on `network_config_swaps`, a sequence, which the rollback of the attempt
/// does not undo.
async fn move_version_under_every_swap(pool: &sqlx::PgPool) -> Result<(), sqlx::Error> {
    sqlx::raw_sql(
        "CREATE SEQUENCE network_config_swaps;
         CREATE FUNCTION move_version_under_the_swap() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
             IF pg_trigger_depth() > 1 THEN
                 RETURN NEW;
             END IF;
             PERFORM nextval('network_config_swaps');
             UPDATE machines SET network_config_version = NEW.network_config_version
             WHERE id = OLD.id;
             RETURN NULL;
         END $$;
         CREATE TRIGGER move_version_under_the_swap
             BEFORE UPDATE OF network_config_version ON machines
             FOR EACH ROW EXECUTE FUNCTION move_version_under_the_swap();",
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// The swaps `move_version_under_every_swap` has counted.
async fn network_config_swaps(pool: &sqlx::PgPool) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("SELECT COALESCE(pg_sequence_last_value('network_config_swaps'), 0)")
        .fetch_one(pool)
        .await
}

#[crate::sqlx_test]
async fn reconcile_admin_addresses_allows_non_dpu_primary_admin_interface(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let mh = create_managed_host_multi_dpu(&env, 2).await;
    let mut txn = env.pool.begin().await?;

    let admin_segment = db::network_segment::admin(&mut txn)
        .await?
        .into_iter()
        .next()
        .unwrap();

    let mut interface_map = db::machine_interface::find_by_machine_ids(&mut txn, &[mh.id]).await?;
    let interfaces = interface_map.remove(&mh.id).unwrap();
    for interface in interfaces
        .iter()
        .filter(|interface| interface.primary_interface)
    {
        db::machine_interface::set_primary_interface(&interface.id, false, &mut txn).await?;
    }

    let active_interface = db::machine_interface::create(
        &mut txn,
        std::slice::from_ref(&admin_segment),
        &MacAddress::from_str("9a:9b:9c:9d:9e:a1")?,
        true,
        AddressSelectionStrategy::NextAvailableIp,
        None,
    )
    .await?;
    db::machine_interface::associate_interface_with_machine(
        &active_interface.id,
        MachineInterfaceAssociation::Machine(mh.id.into()),
        &mut txn,
    )
    .await?;

    let active_config_changed =
        db::machine_interface::reconcile_admin_addresses_for_host(&mut txn, &mh.id).await?;
    assert!(!active_config_changed);

    let mut interface_map = db::machine_interface::find_by_machine_ids(&mut txn, &[mh.id]).await?;
    let interfaces = interface_map.remove(&mh.id).unwrap();
    let active_interface = interfaces
        .iter()
        .find(|interface| interface.id == active_interface.id)
        .unwrap();
    assert!(active_interface.primary_interface);
    assert!(active_interface.attached_dpu_machine_id.is_none());
    assert!(!active_interface.addresses.is_empty());

    let dpu_admin_interfaces = interfaces
        .iter()
        .filter(|interface| {
            interface.network_segment_type == Some(NetworkSegmentType::Admin)
                && interface.attached_dpu_machine_id.is_some()
        })
        .collect::<Vec<_>>();
    assert_eq!(dpu_admin_interfaces.len(), 2);
    for interface in dpu_admin_interfaces {
        assert!(!interface.primary_interface);
        assert!(interface.addresses.is_empty());
        assert!(interface.domain_id.is_none());
        assert!(interface.hostname.starts_with("noip-"));
    }

    txn.commit().await?;

    Ok(())
}

#[crate::sqlx_test]
async fn reconcile_admin_addresses_errors_without_any_primary_admin_interface(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let mh = create_managed_host_multi_dpu(&env, 2).await;
    let mut txn = env.pool.begin().await?;

    let mut interface_map = db::machine_interface::find_by_machine_ids(&mut txn, &[mh.id]).await?;
    let interfaces = interface_map.remove(&mh.id).unwrap();
    for interface in interfaces
        .iter()
        .filter(|interface| interface.primary_interface)
    {
        db::machine_interface::set_primary_interface(&interface.id, false, &mut txn).await?;
    }

    let result = db::machine_interface::reconcile_admin_addresses_for_host(&mut txn, &mh.id).await;
    let error = result.expect_err("reconcile should reject hosts with no primary admin interface");
    assert!(error.to_string().contains("no primary admin interface"));

    txn.rollback().await?;

    Ok(())
}

/// A managed-DPU host that boots from an integrated NIC has a HostInband primary and
/// no primary *admin* interface -- its DPU admin links are present but dormant.
/// Reconcile must treat that as valid (clean up the dormant links), not as the
/// "no primary admin interface" error.
#[crate::sqlx_test]
async fn reconcile_admin_addresses_allows_host_inband_primary_with_dormant_dpu_links(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let mh = create_managed_host_multi_dpu(&env, 2).await;
    create_host_inband_network_segment(&env.api, None).await;

    let mut txn = env.pool.begin().await?;

    let host_inband_gateway = FIXTURE_HOST_INBAND_NETWORK_SEGMENT_GATEWAY.ip();
    let host_inband_segment = db::network_segment::for_segment_type_all(
        &mut txn,
        std::slice::from_ref(&host_inband_gateway),
        NetworkSegmentType::HostInband,
    )
    .await?
    .into_iter()
    .next()
    .expect("the host-inband fixture segment should exist");

    // Demote the DPU admin links: the integrated NIC is taking over as primary.
    let mut interface_map = db::machine_interface::find_by_machine_ids(&mut txn, &[mh.id]).await?;
    let interfaces = interface_map.remove(&mh.id).unwrap();
    for interface in interfaces
        .iter()
        .filter(|interface| interface.primary_interface)
    {
        db::machine_interface::set_primary_interface(&interface.id, false, &mut txn).await?;
    }

    // Own a HostInband integrated NIC as the host's primary.
    let host_nic = db::machine_interface::create(
        &mut txn,
        std::slice::from_ref(&host_inband_segment),
        &MacAddress::from_str("9a:9b:9c:9d:9e:b2")?,
        true,
        AddressSelectionStrategy::NextAvailableIp,
        None,
    )
    .await?;
    db::machine_interface::associate_interface_with_machine(
        &host_nic.id,
        MachineInterfaceAssociation::Machine(mh.id.into()),
        &mut txn,
    )
    .await?;

    // No primary admin interface, but a HostInband primary exists -- the DPU admin
    // links are legitimately dormant, not a broken host. Reconcile must not error.
    db::machine_interface::reconcile_admin_addresses_for_host(&mut txn, &mh.id).await?;

    let mut interface_map = db::machine_interface::find_by_machine_ids(&mut txn, &[mh.id]).await?;
    let interfaces = interface_map.remove(&mh.id).unwrap();

    let host_nic = interfaces
        .iter()
        .find(|interface| interface.id == host_nic.id)
        .unwrap();
    assert!(host_nic.primary_interface);
    assert!(host_nic.attached_dpu_machine_id.is_none());

    let dpu_admin_interfaces = interfaces
        .iter()
        .filter(|interface| {
            interface.network_segment_type == Some(NetworkSegmentType::Admin)
                && interface.attached_dpu_machine_id.is_some()
        })
        .collect::<Vec<_>>();
    assert_eq!(dpu_admin_interfaces.len(), 2);
    for interface in dpu_admin_interfaces {
        assert!(!interface.primary_interface);
        assert!(interface.addresses.is_empty());
    }

    txn.commit().await?;

    Ok(())
}

#[crate::sqlx_test]
async fn return_existing_machine_interface_on_rediscover(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    // TODO: This tests only DHCP without Machines. For Interfaces with a Machine,
    // there are tests in `machine_dhcp.rs`
    // This should also be migrated to use actual API calls
    let env = create_test_env(pool).await;
    let mut txn = env.pool.begin().await?;

    let test_mac = "ff:ff:ff:ff:ff:ff".parse().unwrap();

    let new_machine = db::machine_interface::validate_existing_mac_and_create(
        &mut txn,
        test_mac,
        std::slice::from_ref(&FIXTURE_DHCP_RELAY_ADDRESS.parse().unwrap()),
        None,
        None,
    )
    .await?;

    let existing_machine = db::machine_interface::validate_existing_mac_and_create(
        &mut txn,
        test_mac,
        std::slice::from_ref(&FIXTURE_DHCP_RELAY_ADDRESS.parse().unwrap()),
        None,
        None,
    )
    .await?;

    assert_eq!(new_machine.id, existing_machine.id);

    Ok(())
}

#[crate::sqlx_test]
async fn find_all_interfaces_test_cases(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;

    let mut txn = env.pool.begin().await?;

    let network_segment = db::network_segment::admin(&mut txn)
        .await?
        .into_iter()
        .next()
        .unwrap();

    let mut interfaces: Vec<MachineInterfaceSnapshot> = Vec::new();
    for i in 0..2 {
        let mut txn = env.pool.begin().await?;
        let interface = db::machine_interface::create(
            &mut txn,
            std::slice::from_ref(&network_segment),
            MacAddress::from_str(format!("ff:ff:ff:ff:ff:0{i}").as_str())
                .as_ref()
                .unwrap(),
            true,
            AddressSelectionStrategy::NextAvailableIp,
            None,
        )
        .await?;
        db::dhcp_entry::persist(
            DhcpEntry {
                machine_interface_id: interface.id,
                vendor_string: format!("NVIDIA {i} 1"),
            },
            &mut txn,
        )
        .await?;
        db::dhcp_entry::persist(
            DhcpEntry {
                machine_interface_id: interface.id,
                vendor_string: format!("NVIDIA {i} 2"),
            },
            &mut txn,
        )
        .await?;
        interfaces.push(interface);
        txn.commit().await.unwrap();
    }

    let response = env
        .api
        .find_interfaces(tonic::Request::new(InterfaceSearchQuery {
            id: None,
            ip: None,
        }))
        .await
        .unwrap()
        .into_inner();
    // Assert members
    for (idx, interface) in interfaces.into_iter().enumerate().take(2) {
        assert_eq!(response.interfaces[idx].hostname, interface.hostname);
        assert_eq!(
            response.interfaces[idx].mac_address,
            interface.mac_address.to_string()
        );
        // The newer vendor wins
        assert_eq!(
            response.interfaces[idx].vendor.clone().unwrap().to_string(),
            format!("NVIDIA {idx} 2")
        );
        assert_eq!(
            response.interfaces[idx]
                .domain_id
                .as_ref()
                .unwrap()
                .to_string(),
            interface.domain_id.unwrap().to_string()
        );
    }
    Ok(())
}

#[crate::sqlx_test]
async fn find_interfaces_test_cases(pool: sqlx::PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let host_config = env.managed_host_config();
    let dpu = host_config.get_and_assert_single_dpu();

    let mut txn = env.pool.begin().await?;

    let network_segment = db::network_segment::admin(&mut txn)
        .await?
        .into_iter()
        .next()
        .unwrap();

    let new_interface = db::machine_interface::create(
        &mut txn,
        std::slice::from_ref(&network_segment),
        &dpu.oob_mac_address,
        true,
        AddressSelectionStrategy::NextAvailableIp,
        None,
    )
    .await?;

    db::dhcp_entry::persist(
        DhcpEntry {
            machine_interface_id: new_interface.id,
            vendor_string: "NVIDIA".to_string(),
        },
        &mut txn,
    )
    .await?;
    db::dhcp_entry::persist(
        DhcpEntry {
            machine_interface_id: new_interface.id,
            vendor_string: "NVIDIA New".to_string(),
        },
        &mut txn,
    )
    .await?;
    txn.commit().await?;

    let response = env
        .api
        .find_interfaces(tonic::Request::new(InterfaceSearchQuery {
            id: Some(new_interface.id),
            ip: None,
        }))
        .await
        .unwrap()
        .into_inner();
    // Assert members
    // For new_interface
    assert_eq!(response.interfaces[0].hostname, new_interface.hostname);
    assert_eq!(
        response.interfaces[0].mac_address,
        new_interface.mac_address.to_string()
    );
    assert_eq!(
        response.interfaces[0].vendor.clone().unwrap(),
        "NVIDIA New".to_string()
    );
    assert_eq!(
        response.interfaces[0]
            .domain_id
            .as_ref()
            .unwrap()
            .to_string(),
        new_interface.domain_id.unwrap().to_string()
    );

    Ok(())
}

#[crate::sqlx_test]
async fn create_parallel_mi(pool: sqlx::PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let mut txn = env.pool.begin().await?;
    let network = db::network_segment::admin(&mut txn)
        .await?
        .into_iter()
        .next()
        .unwrap();
    txn.commit().await.unwrap();

    let (tx, _rx1) = broadcast::channel(10);
    let max_interfaces = 250;
    let mut handles = vec![];
    for i in 0..max_interfaces {
        let n = network.clone();
        let mac = format!("ff:ff:ff:ff:{:02}:{:02}", i / 100, i % 100);
        let db_pool = env.pool.clone();
        let mut rx = tx.subscribe();
        let h = tokio::spawn(async move {
            // Let's start all threads together.
            _ = rx.recv().await.unwrap();
            let mut txn = db_pool.begin().await.unwrap();
            db::machine_interface::create(
                &mut txn,
                std::slice::from_ref(&n),
                &MacAddress::from_str(&mac).unwrap(),
                true,
                AddressSelectionStrategy::NextAvailableIp,
                None,
            )
            .await
            .unwrap();

            // This call must pass. inner_txn is an illusion. Lock is still alive.
            _ = db::machine_interface::find_all(&mut txn).await.unwrap();
            txn.commit().await.unwrap();
        });
        handles.push(h);
    }

    tx.send(10).unwrap();

    for h in handles {
        _ = h.await;
    }
    let mut txn = env.pool.begin().await?;
    let interfaces = db::machine_interface::find_all(&mut txn).await.unwrap();

    assert_eq!(interfaces.len(), max_interfaces);
    let ips = interfaces
        .iter()
        .map(|x| x.addresses[0].to_string())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect_vec();
    assert_eq!(interfaces.len(), ips.len());

    Ok(())
}

#[crate::sqlx_test]
async fn test_find_for_update_by_ip(pool: sqlx::PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let mut txn = env.pool.begin().await?;

    let network_segment = db::network_segment::admin(&mut txn)
        .await?
        .into_iter()
        .next()
        .unwrap();
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

    let iface =
        db::machine_interface::find_for_update_by_ip(&mut txn, interface.addresses[0]).await?;
    assert_eq!(iface.id, interface.id);

    Ok(())
}

#[crate::sqlx_test]
async fn test_delete_interface(pool: sqlx::PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;

    let dhcp_response = env
        .api
        .discover_dhcp(tonic::Request::new(rpc::forge::DhcpDiscovery {
            mac_address: "FF:FF:FF:FF:FF:AA".to_string(),
            relay_address: "192.0.2.1".to_string(),
            link_address: None,
            vendor_string: None,
            circuit_id: None,
            remote_id: None,
            desired_address: None,
            address_family: None,
            message_kind: None,
            duid: None,
        }))
        .await
        .unwrap()
        .into_inner();

    let last_invalidation_time = dhcp_response
        .last_invalidation_time
        .expect("Last invalidation time should be set");

    // Find the Machine Interface ID for our new record
    let interface = env
        .api
        .find_interfaces(tonic::Request::new(rpc::forge::InterfaceSearchQuery {
            id: None,
            ip: Some(dhcp_response.address.clone()),
        }))
        .await
        .unwrap()
        .into_inner()
        .interfaces
        .remove(0);
    let interface_id = interface.id.unwrap();

    env.api
        .delete_interface(tonic::Request::new(rpc::forge::InterfaceDeleteQuery {
            id: Some(interface_id),
            mac_address: None,
        }))
        .await
        .unwrap();

    let mut txn = env.pool.begin().await?;
    let found = db::machine_interface::find_one(txn.as_mut(), interface_id).await;
    assert!(
        matches!(found, Err(DatabaseError::FindOneReturnedNoResultsError(_))),
        "the interface row should be gone once delete_interface succeeds"
    );

    txn.commit().await?;

    // The next discover_dhcp should return an updated timestamp
    let dhcp_response = env
        .api
        .discover_dhcp(tonic::Request::new(rpc::forge::DhcpDiscovery {
            mac_address: "FF:FF:FF:FF:FF:AA".to_string(),
            relay_address: "192.0.2.1".to_string(),
            link_address: None,
            vendor_string: None,
            circuit_id: None,
            remote_id: None,
            desired_address: None,
            address_family: None,
            message_kind: None,
            duid: None,
        }))
        .await
        .unwrap()
        .into_inner();
    let new_invalidation_time = dhcp_response
        .last_invalidation_time
        .expect("Last invalidation time should be set");
    assert!(new_invalidation_time > last_invalidation_time);

    Ok(())
}

#[crate::sqlx_test]
async fn test_delete_interface_with_machine(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool.clone()).await;
    let host_config = env.managed_host_config();
    let dpu_machine_id = create_dpu_machine(&env, &host_config).await;

    let mut txn = pool.begin().await?;
    let interface = db::machine_interface::find_by_machine_ids(&mut txn, &[dpu_machine_id])
        .await
        .unwrap();

    let interface = &interface.get(&dpu_machine_id).unwrap()[0];
    txn.commit().await.unwrap();

    let response = env
        .api
        .delete_interface(tonic::Request::new(rpc::forge::InterfaceDeleteQuery {
            id: Some(interface.id),
            mac_address: None,
        }))
        .await;

    match response {
        Ok(_) => panic!("machine deletion is not failed."),
        Err(x) => {
            let c = x.code();
            match c {
                Code::InvalidArgument => {
                    let msg = String::from(x.message());
                    if !msg.contains("already a machine") {
                        panic!("machine interface deletion failed with wrong message {msg}");
                    }
                    return Ok(());
                }
                _ => {
                    panic!("machine interface deletion failed with wrong code {c}");
                }
            }
        }
    }
}

#[crate::sqlx_test]
async fn test_delete_bmc_interface_with_machine(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool.clone()).await;
    let host_config = env.managed_host_config();
    let _rpc_machine_id = create_dpu_machine(&env, &host_config).await;

    let mut txn = pool.begin().await?;
    let interfaces = db::machine_interface::find_all(&mut txn).await.unwrap();
    txn.commit().await.unwrap();

    let interfaces = interfaces
        .iter()
        .filter(|x| x.attached_dpu_machine_id.is_none())
        .collect::<Vec<&MachineInterfaceSnapshot>>();
    if interfaces.len() != 2 {
        // We have only four interfaces, 2 for managed host and 2 for bmc (host and dpu).
        panic!("Wrong interface count {}.", interfaces.len());
    }

    let bmc_interface = interfaces[0];

    let response = env
        .api
        .delete_interface(tonic::Request::new(rpc::forge::InterfaceDeleteQuery {
            id: Some(bmc_interface.id),
            mac_address: None,
        }))
        .await;

    match response {
        Ok(_) => panic!("machine deletion is not failed."),
        Err(x) => {
            let c = x.code();
            match c {
                Code::InvalidArgument => {
                    let msg = String::from(x.message());
                    if !msg.contains("this looks like a BMC interface and attached") {
                        panic!("machine interface deletion failed with wrong message {msg}");
                    }
                    return Ok(());
                }
                _ => {
                    panic!("machine interface deletion failed with wrong code {c}");
                }
            }
        }
    }
}

#[crate::sqlx_test]
async fn machine_bmc_info_uses_topology_selected_bmc_and_interfaces_exclude_it(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    // Topology identifies the active BMC even when an older attached interface
    // remains. Each reader should use that selection without exposing BMCs in
    // the machine's general interface list.
    let env = create_test_env(pool.clone()).await;
    let host_config = env.managed_host_config();
    let host_bmc_mac = host_config.bmc_mac_address;
    let dpu_bmc_mac = host_config.get_and_assert_single_dpu().bmc_mac_address;
    let managed_host = create_managed_host_with_config(&env, host_config).await;

    let mut txn = pool.begin().await?;
    let host_machine = managed_host.host().db_machine(&mut txn).await;
    let dpu_machine = managed_host.dpu().db_machine(&mut txn).await;
    let interfaces = db::machine_interface::find_all(&mut txn).await?;

    let host_bmc_interface = interfaces
        .iter()
        .find(|interface| {
            interface.machine_id == Some(host_machine.id.into())
                && interface.interface_type == InterfaceType::Bmc
        })
        .expect("host BMC interface must exist");
    let host_bmc_interface_id = host_bmc_interface.id;
    let host_bmc_interface_mac = host_bmc_interface.mac_address;
    let host_bmc_interface_ip = host_bmc_interface
        .addresses
        .first()
        .expect("host BMC interface must have an address")
        .to_owned();
    assert_eq!(host_bmc_interface_mac, host_bmc_mac);

    let dpu_bmc_interface = interfaces
        .iter()
        .find(|interface| {
            interface.machine_id == Some(dpu_machine.id.into())
                && interface.interface_type == InterfaceType::Bmc
        })
        .expect("DPU BMC interface must exist");
    let dpu_bmc_interface_id = dpu_bmc_interface.id;
    let dpu_bmc_interface_mac = dpu_bmc_interface.mac_address;
    let dpu_bmc_interface_ip = dpu_bmc_interface
        .addresses
        .first()
        .expect("DPU BMC interface must have an address")
        .to_owned();
    assert_eq!(dpu_bmc_interface_mac, dpu_bmc_mac);

    let host_stale_bmc_ip: IpAddr = "192.0.0.101".parse()?;
    let host_stale_bmc_interface_id = insert_older_bmc_interface(
        txn.as_mut(),
        host_machine.id.into(),
        host_bmc_interface,
        "02:00:00:00:01:01".parse()?,
        host_stale_bmc_ip,
    )
    .await?;
    let dpu_stale_bmc_interface_id = insert_older_bmc_interface(
        txn.as_mut(),
        dpu_machine.id.into(),
        dpu_bmc_interface,
        "02:00:00:00:01:02".parse()?,
        "192.0.0.102".parse()?,
    )
    .await?;
    assert!(host_stale_bmc_ip < host_bmc_interface_ip);

    let host_machine = managed_host.host().db_machine(&mut txn).await;
    let dpu_machine = managed_host.dpu().db_machine(&mut txn).await;

    assert_eq!(
        host_machine.status.bmc_info.machine_interface_id,
        Some(host_bmc_interface_id)
    );
    assert_eq!(
        host_machine.status.bmc_info.mac,
        Some(host_bmc_interface_mac)
    );
    assert_eq!(host_machine.status.bmc_info.ip, Some(host_bmc_interface_ip));
    assert!(
        host_machine
            .status
            .interfaces
            .iter()
            .all(|interface| interface.interface_type != InterfaceType::Bmc
                && interface.id != host_bmc_interface_id)
    );

    assert_eq!(
        dpu_machine.status.bmc_info.machine_interface_id,
        Some(dpu_bmc_interface_id)
    );
    assert_eq!(dpu_machine.status.bmc_info.mac, Some(dpu_bmc_interface_mac));
    assert_eq!(dpu_machine.status.bmc_info.ip, Some(dpu_bmc_interface_ip));
    assert!(
        dpu_machine
            .status
            .interfaces
            .iter()
            .all(|interface| interface.interface_type != InterfaceType::Bmc
                && interface.id != dpu_bmc_interface_id)
    );

    let managed_snapshot = managed_host.snapshot(&mut txn).await;
    assert_eq!(
        managed_snapshot
            .host_snapshot
            .status
            .bmc_info
            .machine_interface_id,
        Some(host_bmc_interface_id)
    );
    assert_eq!(
        managed_snapshot.dpu_snapshots[0]
            .status
            .bmc_info
            .machine_interface_id,
        Some(dpu_bmc_interface_id)
    );

    let host_machine_id: MachineId = host_machine.id.into();
    let bmc_ips_by_machine = db::machine_topology::find_machine_bmc_pairs_by_machine_id(
        txn.as_mut(),
        vec![host_machine_id],
    )
    .await?;
    assert_eq!(
        bmc_ips_by_machine,
        vec![(host_machine_id, Some(host_bmc_interface_ip.to_string()))]
    );

    txn.commit().await?;

    let host_rpc_machine = managed_host.host().rpc_machine().await;
    let dpu_rpc_machine = managed_host.dpu().rpc_machine().await;
    let rpc_bmc_type = rpc::forge::InterfaceType::Bmc as i32;
    let host_bmc_interface_mac = host_bmc_interface_mac.to_string();
    let dpu_bmc_interface_mac = dpu_bmc_interface_mac.to_string();
    let host_bmc_interface_ip = host_bmc_interface_ip.to_string();
    let dpu_bmc_interface_ip = dpu_bmc_interface_ip.to_string();

    let host_bmc_info = host_rpc_machine
        .bmc_info
        .as_ref()
        .expect("host RPC BMC info must exist");
    assert_eq!(
        host_bmc_info.machine_interface_id,
        Some(host_bmc_interface_id)
    );
    assert_eq!(
        host_bmc_info.mac.as_deref(),
        Some(host_bmc_interface_mac.as_str())
    );
    assert_eq!(
        host_bmc_info.ip.as_deref(),
        Some(host_bmc_interface_ip.as_str())
    );
    assert!(
        host_rpc_machine
            .status
            .as_ref()
            .unwrap()
            .interfaces
            .iter()
            .all(|interface| interface.interface_type != Some(rpc_bmc_type)
                && interface.id != Some(host_bmc_interface_id))
    );

    let dpu_bmc_info = dpu_rpc_machine
        .bmc_info
        .as_ref()
        .expect("DPU RPC BMC info must exist");
    assert_eq!(
        dpu_bmc_info.machine_interface_id,
        Some(dpu_bmc_interface_id)
    );
    assert_eq!(
        dpu_bmc_info.mac.as_deref(),
        Some(dpu_bmc_interface_mac.as_str())
    );
    assert_eq!(
        dpu_bmc_info.ip.as_deref(),
        Some(dpu_bmc_interface_ip.as_str())
    );
    assert!(
        dpu_rpc_machine
            .status
            .as_ref()
            .unwrap()
            .interfaces
            .iter()
            .all(|interface| interface.interface_type != Some(rpc_bmc_type)
                && interface.id != Some(dpu_bmc_interface_id))
    );

    // If the selected interfaces stop belonging to these machines, snapshots
    // and direct lookup must not silently fall back to the older BMCs.
    let mut txn = pool.begin().await?;
    sqlx::query(
        "UPDATE machine_interfaces
         SET machine_id = NULL, association_type = 'None'
         WHERE id = ANY($1)",
    )
    .bind(vec![host_bmc_interface_id, dpu_bmc_interface_id])
    .execute(txn.as_mut())
    .await?;

    let host_machine = managed_host.host().db_machine(&mut txn).await;
    assert_eq!(host_machine.status.bmc_info.ip, None);
    assert_eq!(host_machine.status.bmc_info.mac, None);

    let managed_snapshot = managed_host.snapshot(&mut txn).await;
    assert_eq!(managed_snapshot.host_snapshot.status.bmc_info.ip, None);
    assert_eq!(managed_snapshot.dpu_snapshots[0].status.bmc_info.ip, None);

    let bmc_pairs = db::machine_topology::find_machine_bmc_pairs_by_machine_id(
        txn.as_mut(),
        vec![host_machine_id],
    )
    .await?;
    assert!(bmc_pairs.is_empty());

    // A topology without an interface ID retains the attached-BMC fallback.
    sqlx::query(
        "UPDATE machine_topologies
         SET topology = jsonb_set(
             topology,
             '{bmc_info,machine_interface_id}',
             'null'::jsonb
         )
         WHERE machine_id = ANY($1)",
    )
    .bind(vec![host_machine_id, dpu_machine.id.into()])
    .execute(txn.as_mut())
    .await?;

    let managed_snapshot = managed_host.snapshot(&mut txn).await;
    assert_eq!(
        managed_snapshot
            .host_snapshot
            .status
            .bmc_info
            .machine_interface_id,
        Some(host_stale_bmc_interface_id)
    );
    assert_eq!(
        managed_snapshot.dpu_snapshots[0]
            .status
            .bmc_info
            .machine_interface_id,
        Some(dpu_stale_bmc_interface_id)
    );
    let bmc_pairs = db::machine_topology::find_machine_bmc_pairs_by_machine_id(
        txn.as_mut(),
        vec![host_machine_id],
    )
    .await?;
    assert_eq!(
        bmc_pairs,
        vec![(host_machine_id, Some(host_stale_bmc_ip.to_string()))]
    );

    txn.rollback().await?;

    Ok(())
}

#[crate::sqlx_test]
async fn test_hostname_equals_ip(pool: sqlx::PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let mut txn = env.pool.begin().await?;

    let network_segment = db::network_segment::admin(&mut txn)
        .await?
        .into_iter()
        .next()
        .unwrap();
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
        interface.hostname,
        interface
            .addresses
            .iter()
            .find(|x| x.is_ipv4())
            .unwrap()
            .to_string()
            .replace('.', "-")
    );
    Ok(())
}

#[crate::sqlx_test]
async fn test_max_one_interface_association(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    use carbide_uuid::power_shelf::PowerShelfId;
    use carbide_uuid::switch::SwitchId;
    use model::power_shelf::{NewPowerShelf, PowerShelfConfig};
    use model::switch::{NewSwitch, SwitchConfig};

    let env = create_test_env(pool).await;
    let mut txn = env.pool.begin().await?;

    let network_segment = db::network_segment::admin(&mut txn)
        .await?
        .into_iter()
        .next()
        .unwrap();
    let interface = db::machine_interface::create(
        &mut txn,
        std::slice::from_ref(&network_segment),
        MacAddress::from_str("ff:ff:ff:ff:ff:ff").as_ref().unwrap(),
        true,
        AddressSelectionStrategy::NextAvailableIp,
        None,
    )
    .await?;

    // Create a switch and associate the interface with it
    let switch_id = SwitchId::from(uuid::Uuid::new_v4());
    let new_switch = NewSwitch {
        id: switch_id,
        config: SwitchConfig {
            name: "Test Switch".to_string(),
            enable_nmxc: false,
            fabric_manager_config: None,
        },
        bmc_mac_address: None,
        metadata: None,
        rack_id: None,
        slot_number: None,
        tray_index: None,
    };
    db::switch::create(&mut txn, &new_switch).await?;

    db::machine_interface::associate_interface_with_machine(
        &interface.id,
        MachineInterfaceAssociation::Switch(switch_id),
        &mut txn,
    )
    .await?;

    // Now try to associate the same interface with a power shelf - should fail
    let power_shelf_id = PowerShelfId::from(uuid::Uuid::new_v4());
    let new_power_shelf = NewPowerShelf {
        id: power_shelf_id,
        config: PowerShelfConfig {
            name: "Test Power Shelf".to_string(),
            capacity: None,
            voltage: None,
        },
        bmc_mac_address: None,
        metadata: None,
        rack_id: None,
    };
    db::power_shelf::create(&mut txn, &new_power_shelf).await?;

    let output = db::machine_interface::associate_interface_with_machine(
        &interface.id,
        MachineInterfaceAssociation::PowerShelf(power_shelf_id),
        &mut txn,
    )
    .await;

    txn.commit().await.unwrap();

    assert!(matches!(
        output,
        Err(DatabaseError::MaxOneInterfaceAssociation)
    ));

    Ok(())
}

#[crate::sqlx_test]
async fn test_power_shelf_association(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    use carbide_uuid::power_shelf::PowerShelfId;
    use model::power_shelf::{NewPowerShelf, PowerShelfConfig};

    let env = create_test_env(pool).await;
    let mut txn = env.pool.begin().await?;

    let network_segment = db::network_segment::admin(&mut txn)
        .await?
        .into_iter()
        .next()
        .unwrap();
    let interface = db::machine_interface::create(
        &mut txn,
        std::slice::from_ref(&network_segment),
        MacAddress::from_str("ff:ff:ff:ff:ff:ff").as_ref().unwrap(),
        true,
        AddressSelectionStrategy::NextAvailableIp,
        None,
    )
    .await?;

    // Create a power shelf
    let power_shelf_id = PowerShelfId::from(uuid::Uuid::new_v4());
    let new_power_shelf = NewPowerShelf {
        id: power_shelf_id,
        config: PowerShelfConfig {
            name: "Test Power Shelf".to_string(),
            capacity: Some(10000),
            voltage: Some(480),
        },
        bmc_mac_address: None,
        metadata: None,
        rack_id: None,
    };
    db::power_shelf::create(&mut txn, &new_power_shelf).await?;

    // Associate the interface with the power shelf
    let result = db::machine_interface::associate_interface_with_machine(
        &interface.id,
        MachineInterfaceAssociation::PowerShelf(power_shelf_id),
        &mut txn,
    )
    .await;

    txn.commit().await.unwrap();

    assert!(result.is_ok());
    assert_eq!(result.unwrap(), interface.id);

    Ok(())
}

#[crate::sqlx_test]
async fn test_switch_association(pool: sqlx::PgPool) -> Result<(), Box<dyn std::error::Error>> {
    use carbide_uuid::switch::SwitchId;
    use model::switch::{NewSwitch, SwitchConfig};

    let env = create_test_env(pool).await;
    let mut txn = env.pool.begin().await?;

    let network_segment = db::network_segment::admin(&mut txn)
        .await?
        .into_iter()
        .next()
        .unwrap();
    let interface = db::machine_interface::create(
        &mut txn,
        std::slice::from_ref(&network_segment),
        MacAddress::from_str("ff:ff:ff:ff:ff:ff").as_ref().unwrap(),
        true,
        AddressSelectionStrategy::NextAvailableIp,
        None,
    )
    .await?;

    // Create a switch
    let switch_id = SwitchId::from(uuid::Uuid::new_v4());
    let new_switch = NewSwitch {
        id: switch_id,
        config: SwitchConfig {
            name: "Test Switch".to_string(),
            enable_nmxc: false,
            fabric_manager_config: None,
        },
        bmc_mac_address: None,
        metadata: None,
        rack_id: None,
        slot_number: Some(2),
        tray_index: Some(1),
    };
    db::switch::create(&mut txn, &new_switch).await?;

    // Associate the interface with the switch
    let result = db::machine_interface::associate_interface_with_machine(
        &interface.id,
        MachineInterfaceAssociation::Switch(switch_id),
        &mut txn,
    )
    .await;

    txn.commit().await.unwrap();

    assert!(result.is_ok());
    assert_eq!(result.unwrap(), interface.id);

    Ok(())
}

#[crate::sqlx_test]
async fn test_static_create_returns_address_already_in_use(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let relay: std::net::IpAddr = FIXTURE_DHCP_RELAY_ADDRESS.parse().unwrap();
    let existing_mac = MacAddress::from_str("aa:bb:cc:dd:ee:10").unwrap();
    let new_mac = MacAddress::from_str("aa:bb:cc:dd:ee:11").unwrap();

    let mut txn = env.pool.begin().await?;
    let existing_interface = db::machine_interface::validate_existing_mac_and_create(
        &mut txn,
        existing_mac,
        std::slice::from_ref(&relay),
        None,
        None,
    )
    .await?;
    let target_ip = existing_interface.addresses[0];
    txn.commit().await?;

    let mut txn = env.pool.begin().await?;
    let segment = db::network_segment::for_relay(&mut txn, relay)
        .await?
        .expect("relay segment exists");
    let result = db::machine_interface::create(
        &mut txn,
        std::slice::from_ref(&segment),
        &new_mac,
        true,
        AddressSelectionStrategy::StaticAddress(target_ip),
        None,
    )
    .await;
    assert!(
        matches!(result, Err(DatabaseError::AddressAlreadyInUse(_))),
        "expected AddressAlreadyInUse, got: {result:?}"
    );

    // Existing interface is untouched.
    let mut txn = env.pool.begin().await?;
    let found = db::machine_interface::find_by_ip(&mut *txn, target_ip).await?;
    let found = found.expect("existing interface should still own the IP");
    assert_eq!(found.id, existing_interface.id);
    assert_eq!(found.mac_address, existing_mac);

    Ok(())
}

#[crate::sqlx_test]
async fn test_static_create_is_noop_when_same_mac_already_owns_address(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    // If create_static_path is called with a MAC that already owns
    // the target IP, it should return the existing snapshot rather
    // than erroring; re-applying the same intent should be a noop.
    let env = create_test_env(pool).await;
    let static_ip: std::net::IpAddr = "192.0.2.231".parse().unwrap();
    let mac = MacAddress::from_str("aa:bb:cc:dd:ee:12").unwrap();

    let mut txn = env.pool.begin().await?;
    let segment = db::network_segment::admin(&mut txn)
        .await?
        .into_iter()
        .next()
        .unwrap();
    let first = db::machine_interface::create(
        &mut txn,
        std::slice::from_ref(&segment),
        &mac,
        true,
        AddressSelectionStrategy::StaticAddress(static_ip),
        None,
    )
    .await?;
    txn.commit().await?;

    // And now re-run the same create; should succeed and
    // return the same interface.
    let mut txn = env.pool.begin().await?;
    let again = db::machine_interface::create(
        &mut txn,
        std::slice::from_ref(&segment),
        &mac,
        true,
        AddressSelectionStrategy::StaticAddress(static_ip),
        None,
    )
    .await?;
    txn.commit().await?;
    assert_eq!(
        again.id, first.id,
        "re-create with same MAC should be a noop"
    );
    assert_eq!(again.mac_address, mac);

    Ok(())
}

// Deleting by MAC address removes the interface, for the case where an operator has a
// leftover interface record and only knows the BMC MAC (not the interface id).
#[crate::sqlx_test]
async fn test_delete_interface_by_mac_address(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let mac = "FF:FF:FF:FF:FF:BB";

    let dhcp_response = env
        .api
        .discover_dhcp(tonic::Request::new(rpc::forge::DhcpDiscovery {
            mac_address: mac.to_string(),
            relay_address: FIXTURE_DHCP_RELAY_ADDRESS.to_string(),
            link_address: None,
            vendor_string: None,
            circuit_id: None,
            remote_id: None,
            desired_address: None,
            address_family: None,
            message_kind: None,
            duid: None,
        }))
        .await
        .unwrap()
        .into_inner();
    let interface_id = dhcp_response
        .machine_interface_id
        .expect("discover_dhcp must return an interface id");

    env.api
        .delete_interface(tonic::Request::new(rpc::forge::InterfaceDeleteQuery {
            id: None,
            mac_address: Some(mac.to_string()),
        }))
        .await
        .unwrap();

    let mut txn = env.pool.begin().await?;
    assert!(
        db::machine_interface::find_by_mac_address(txn.as_mut(), MacAddress::from_str(mac)?)
            .await?
            .is_empty(),
        "the interface should be gone once delete by MAC succeeds"
    );
    let found = db::machine_interface::find_one(txn.as_mut(), interface_id).await;
    assert!(matches!(
        found,
        Err(DatabaseError::FindOneReturnedNoResultsError(_))
    ));
    txn.commit().await?;

    Ok(())
}

// An unknown MAC is reported as not found rather than silently succeeding.
#[crate::sqlx_test]
async fn test_delete_interface_by_unknown_mac_is_not_found(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;

    let status = env
        .api
        .delete_interface(tonic::Request::new(rpc::forge::InterfaceDeleteQuery {
            id: None,
            mac_address: Some("FF:FF:FF:FF:FF:CC".to_string()),
        }))
        .await
        .expect_err("an unknown MAC should not succeed");
    assert_eq!(status.code(), Code::NotFound);

    Ok(())
}

// A leftover boot override must not block interface deletion: `machine_boot_override`
// has a foreign key to `machine_interfaces` with no ON DELETE CASCADE, so the delete
// fails with a FK violation unless the override is cleared first.
#[crate::sqlx_test]
async fn test_delete_interface_clears_boot_override(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let mac = "FF:FF:FF:FF:FF:DD";

    let dhcp_response = env
        .api
        .discover_dhcp(tonic::Request::new(rpc::forge::DhcpDiscovery {
            mac_address: mac.to_string(),
            relay_address: FIXTURE_DHCP_RELAY_ADDRESS.to_string(),
            link_address: None,
            vendor_string: None,
            circuit_id: None,
            remote_id: None,
            desired_address: None,
            address_family: None,
            message_kind: None,
            duid: None,
        }))
        .await
        .unwrap()
        .into_inner();
    let interface_id = dhcp_response
        .machine_interface_id
        .expect("discover_dhcp must return an interface id");

    let mut txn = env.pool.begin().await?;
    db::machine_boot_override::create(
        txn.as_mut(),
        interface_id,
        Some("custom-pxe-script".to_string()),
        None,
    )
    .await?;
    txn.commit().await?;

    env.api
        .delete_interface(tonic::Request::new(rpc::forge::InterfaceDeleteQuery {
            id: Some(interface_id),
            mac_address: None,
        }))
        .await
        .expect("a leftover boot override must not block deletion");

    let mut txn = env.pool.begin().await?;
    assert!(
        db::machine_boot_override::find_optional(txn.as_mut(), interface_id)
            .await?
            .is_none(),
        "the boot override should be gone with its interface"
    );
    txn.commit().await?;

    Ok(())
}

// A MAC is unique only per network segment, so the same MAC can legitimately exist on
// several segments -- the exact shape of #3046, where a host returns to the site on a
// different network prefix. Deleting by MAC must remove all of them.
#[crate::sqlx_test]
async fn test_delete_interface_by_mac_removes_every_segment(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let mac = MacAddress::from_str("FF:FF:FF:FF:FE:01")?;

    create_host_inband_network_segment(&env.api, None).await;

    let mut txn = env.pool.begin().await?;
    let admin_segment = db::network_segment::admin(&mut txn)
        .await?
        .into_iter()
        .next()
        .unwrap();
    let host_inband_gateway = FIXTURE_HOST_INBAND_NETWORK_SEGMENT_GATEWAY.ip();
    let inband_segment = db::network_segment::for_segment_type_all(
        &mut txn,
        std::slice::from_ref(&host_inband_gateway),
        NetworkSegmentType::HostInband,
    )
    .await?
    .into_iter()
    .next()
    .expect("the host-inband fixture segment should exist");
    for segment in [&admin_segment, &inband_segment] {
        db::machine_interface::create(
            &mut txn,
            std::slice::from_ref(segment),
            &mac,
            false,
            AddressSelectionStrategy::NextAvailableIp,
            None,
        )
        .await?;
    }
    txn.commit().await?;

    let mut txn = env.pool.begin().await?;
    assert_eq!(
        db::machine_interface::find_by_mac_address(txn.as_mut(), mac)
            .await?
            .len(),
        2,
        "the same MAC should exist on both segments"
    );
    txn.commit().await?;

    env.api
        .delete_interface(tonic::Request::new(rpc::forge::InterfaceDeleteQuery {
            id: None,
            mac_address: Some(mac.to_string()),
        }))
        .await
        .unwrap();

    let mut txn = env.pool.begin().await?;
    assert!(
        db::machine_interface::find_by_mac_address(txn.as_mut(), mac)
            .await?
            .is_empty(),
        "every interface carrying the MAC should be deleted"
    );
    txn.commit().await?;

    Ok(())
}

// When a MAC matches one free interface and one still owned by a switch, the whole
// request is refused and the free interface survives -- deletion is all-or-nothing.
#[crate::sqlx_test]
async fn test_delete_interface_by_mac_refuses_when_any_match_is_owned(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    use carbide_uuid::switch::SwitchId;
    use model::switch::{NewSwitch, SwitchConfig};

    let env = create_test_env(pool).await;
    let mac = MacAddress::from_str("FF:FF:FF:FF:FE:02")?;

    create_host_inband_network_segment(&env.api, None).await;

    let mut txn = env.pool.begin().await?;
    let admin_segment = db::network_segment::admin(&mut txn)
        .await?
        .into_iter()
        .next()
        .unwrap();
    let host_inband_gateway = FIXTURE_HOST_INBAND_NETWORK_SEGMENT_GATEWAY.ip();
    let inband_segment = db::network_segment::for_segment_type_all(
        &mut txn,
        std::slice::from_ref(&host_inband_gateway),
        NetworkSegmentType::HostInband,
    )
    .await?
    .into_iter()
    .next()
    .expect("the host-inband fixture segment should exist");

    let free_interface = db::machine_interface::create(
        &mut txn,
        std::slice::from_ref(&admin_segment),
        &mac,
        false,
        AddressSelectionStrategy::NextAvailableIp,
        None,
    )
    .await?;
    let owned_interface = db::machine_interface::create(
        &mut txn,
        std::slice::from_ref(&inband_segment),
        &mac,
        false,
        AddressSelectionStrategy::NextAvailableIp,
        None,
    )
    .await?;

    let switch_id = SwitchId::from(uuid::Uuid::new_v4());
    db::switch::create(
        &mut txn,
        &NewSwitch {
            id: switch_id,
            config: SwitchConfig {
                name: "Test Switch".to_string(),
                enable_nmxc: false,
                fabric_manager_config: None,
            },
            bmc_mac_address: None,
            metadata: None,
            rack_id: None,
            slot_number: Some(2),
            tray_index: Some(1),
        },
    )
    .await?;
    db::machine_interface::associate_interface_with_machine(
        &owned_interface.id,
        MachineInterfaceAssociation::Switch(switch_id),
        &mut txn,
    )
    .await?;
    txn.commit().await?;

    let status = env
        .api
        .delete_interface(tonic::Request::new(rpc::forge::InterfaceDeleteQuery {
            id: None,
            mac_address: Some(mac.to_string()),
        }))
        .await
        .expect_err("a switch-owned match must refuse the whole request");
    assert_eq!(status.code(), Code::InvalidArgument);

    let mut txn = env.pool.begin().await?;
    assert_eq!(
        db::machine_interface::find_by_mac_address(txn.as_mut(), mac)
            .await?
            .len(),
        2,
        "nothing should be deleted when any match is refused"
    );
    assert!(
        db::machine_interface::find_one(txn.as_mut(), free_interface.id)
            .await
            .is_ok(),
        "the eligible interface must survive an all-or-nothing refusal"
    );
    txn.commit().await?;

    Ok(())
}

// An unknown interface id reports not-found, matching the unknown-MAC arm. Previously
// this fell through as an internal error.
#[crate::sqlx_test]
async fn test_delete_interface_by_unknown_id_is_not_found(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;

    let status = env
        .api
        .delete_interface(tonic::Request::new(rpc::forge::InterfaceDeleteQuery {
            id: Some(carbide_uuid::machine::MachineInterfaceId::from(
                uuid::Uuid::new_v4(),
            )),
            mac_address: None,
        }))
        .await
        .expect_err("an unknown interface id should not succeed");
    assert_eq!(status.code(), Code::NotFound);

    Ok(())
}

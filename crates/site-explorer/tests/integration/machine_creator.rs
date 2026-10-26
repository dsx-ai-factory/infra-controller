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
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use carbide_instrument::testing::MetricsCapture;
use carbide_site_explorer::config::SiteExplorerConfig;
use carbide_site_explorer::test_support::TestSiteExplorer;
use carbide_test_harness::network::segment::TestNetworkSegment;
use carbide_test_harness::prelude::*;
use carbide_test_harness::test_support::fixture_config::{
    DpuConfigExt as _, FixtureDefault as _, ManagedHostConfigExt as _,
};
use carbide_uuid::machine::{HostMachineId, MachineId, MachineInterfaceId, StableHostMachineId};
use carbide_uuid::rack::{RackId, RackProfileId};
use component_manager::TestMachineInfoProvider;
use db::ObjectFilter;
use db::test_support::postgres::wait_for_blocked_query;
use mac_address::MacAddress;
use model::expected_machine::{ExpectedMachineData, HostDpuPolicy};
use model::machine::MachineInterfaceSnapshot;
use model::machine::machine_search_config::MachineSearchConfig;
use model::machine_boot_interface::{MachineBootInterface, MachineBootInterfaceTarget};
use model::machine_interface_address::MachineInterfaceAssociation;
use model::network_segment::NetworkSegmentType;
use model::rack::RackConfig;
use model::site_explorer::ExploredManagedHost;
use model::test_support::{DpuConfig, ManagedHostConfig};
use rpc::forge::forge_server::Forge;
use rpc::{DiscoveryData, DiscoveryInfo, MachineDiscoveryInfo};
use tonic::Request;

use super::machine_fixtures::{
    Env, ExploredHostFixture, expected_machine, explored_host_fixture, machine_creator,
    machine_creator_config, machine_creator_with_dpf, machine_creator_with_info_provider,
};

async fn assert_no_machines_created(pool: &PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let machines = db::machine::find(
        pool,
        ObjectFilter::<MachineId>::All,
        MachineSearchConfig {
            include_predicted_host: true,
            ..Default::default()
        },
    )
    .await?;

    assert_eq!(
        machines.len(),
        0,
        "expected no machine rows after machine-information preflight failure, got {machines:#?}"
    );
    Ok(())
}

#[sqlx_test]
async fn test_machine_creator_machine_info_errors_for_rack_without_profile(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = Env::new(pool).await;
    let provider = Arc::new(TestMachineInfoProvider::default());
    let creator = machine_creator_with_info_provider(&env, provider.clone());
    let rack_id = RackId::new(uuid::Uuid::new_v4().to_string());

    let mut txn = env.pool.begin().await?;
    db::rack::create(txn.as_mut(), &rack_id, None, &RackConfig::default(), None).await?;
    txn.commit().await?;

    let managed_host =
        ManagedHostConfig::default().with_expected_machine_data(ExpectedMachineData {
            rack_id: Some(rack_id.clone()),
            ..Default::default()
        });
    let mut fixture = explored_host_fixture(&env, &managed_host).await;

    let result = creator
        .create_managed_host(
            &fixture.host,
            &mut fixture.host_report,
            Some(&expected_machine(&managed_host)),
            &env.pool,
        )
        .await;

    let Err(error) = result else {
        return Err(std::io::Error::other("expected missing rack profile error").into());
    };

    assert!(error.to_string().contains("has no rack_profile_id"));
    assert_eq!(provider.call_count(), 0);
    assert_no_machines_created(&env.pool).await?;

    Ok(())
}

#[sqlx_test]
async fn test_machine_creator_machine_info_errors_for_unknown_profile(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = Env::new(pool).await;
    let provider = Arc::new(TestMachineInfoProvider::default());
    let creator = machine_creator_with_info_provider(&env, provider.clone());
    let rack_id = RackId::new(uuid::Uuid::new_v4().to_string());

    let mut txn = env.pool.begin().await?;
    db::rack::create(
        txn.as_mut(),
        &rack_id,
        Some(&RackProfileId::new("Unknown")),
        &RackConfig::default(),
        None,
    )
    .await?;
    txn.commit().await?;

    let managed_host =
        ManagedHostConfig::default().with_expected_machine_data(ExpectedMachineData {
            rack_id: Some(rack_id.clone()),
            ..Default::default()
        });
    let mut fixture = explored_host_fixture(&env, &managed_host).await;

    let result = creator
        .create_managed_host(
            &fixture.host,
            &mut fixture.host_report,
            Some(&expected_machine(&managed_host)),
            &env.pool,
        )
        .await;

    let Err(error) = result else {
        return Err(std::io::Error::other("expected unknown rack profile error").into());
    };

    assert!(error.to_string().contains("is not configured"));
    assert_eq!(provider.call_count(), 0);
    assert_no_machines_created(&env.pool).await?;

    Ok(())
}

async fn dhcp_discover_dpu_oob_iface(
    api: &Api,
    segment: TestNetworkSegment,
    mac_address: MacAddress,
) {
    api.discover_dhcp(
        rpc::forge::DhcpDiscovery::builder(mac_address, segment.relay_address)
            .vendor_string("bluefield")
            .tonic_request(),
    )
    .await
    .expect("DPU OOB DHCP discovery should succeed");
}

#[sqlx_test]
async fn test_zero_dpu_rediscovery_after_stable_registration(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let test_harness = TestHarness::builder(pool.clone()).build().await;
    let domain = test_harness.test_domain().await;
    let network_controller = test_harness.network_controller();
    let underlay_segment = network_controller.create_underlay_segment(&domain).await;
    let host_inband_segment = network_controller.create_host_inband_segment(&domain).await;
    let env = Env {
        pool,
        underlay_segment,
        test_harness,
    };
    let creator = machine_creator(&env, machine_creator_config());
    let mock_host = ManagedHostConfig {
        dpus: vec![],
        expected_machine_data: Some(ExpectedMachineData {
            dpu_policy: HostDpuPolicy::Ignore,
            ..Default::default()
        }),
        ..ManagedHostConfig::default()
    };
    let inband_mac = mock_host.dhcp_mac_address();
    let expected = expected_machine(&mock_host);
    let mut fixture = explored_host_fixture(&env, &mock_host).await;
    assert!(
        creator
            .create_managed_host(
                &fixture.host,
                &mut fixture.host_report,
                Some(&expected),
                &env.pool,
            )
            .await?
    );
    let dhcp_response = env
        .api()
        .discover_dhcp(
            rpc::forge::DhcpDiscovery::builder(inband_mac, host_inband_segment.relay_address)
                .vendor_string("Bluefield")
                .tonic_request(),
        )
        .await?
        .into_inner();
    let predicted_id = dhcp_response.machine_id.expect("host prediction promoted");
    let stable_id = StableHostMachineId::try_from(model::machine::machine_id::from_hardware_info(
        &model::hardware_info::HardwareInfo::from(&mock_host),
    )?)?;
    let mut txn = env.pool.begin().await?;
    db::machine::try_sync_stable_id_with_current_machine_id_for_host(
        txn.as_mut(),
        Some(predicted_id.try_into()?),
        &stable_id,
    )
    .await?;
    assert_eq!(
        db::machine_topology::find_machine_id_by_bmc_mac::<HostMachineId>(
            txn.as_mut(),
            mock_host.bmc_mac_address,
        )
        .await?,
        Some(stable_id.into()),
    );
    // Model an ingested host without boot metadata so rediscovery must persist reconciliation.
    sqlx::query("DELETE FROM machine_boot_interfaces WHERE machine_id = $1")
        .bind(stable_id)
        .execute(txn.as_mut())
        .await?;
    txn.commit().await?;

    assert!(
        !creator
            .create_managed_host(
                &fixture.host,
                &mut fixture.host_report,
                Some(&expected),
                &env.pool,
            )
            .await?,
        "rediscovery must reuse the stable host",
    );
    let desired = db::machine_desired_boot_interface::get(&env.pool, &stable_id)
        .await?
        .expect("rediscovery should initialize the stable host's boot target");
    assert_eq!(
        desired.value,
        MachineBootInterfaceTarget::Pair(MachineBootInterface {
            mac_address: inband_mac,
            interface_id: "NIC.Embedded.1-1-1".to_string(),
        }),
    );
    let machines = db::machine::find(
        &env.pool,
        ObjectFilter::<MachineId>::All,
        MachineSearchConfig {
            include_predicted_host: true,
            ..Default::default()
        },
    )
    .await?;
    assert_eq!(machines.len(), 1);
    assert_eq!(machines[0].id, stable_id.into());
    Ok(())
}

#[sqlx_test]
async fn test_mi_attach_dpu_if_mi_exists_during_machine_creation(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = Env::new(pool).await;
    let creator = machine_creator(&env, machine_creator_config());

    let mock_host = ManagedHostConfig::default();
    let mock_dpu = mock_host.dpus.first().unwrap();

    // Create MI now.
    dhcp_discover_dpu_oob_iface(env.api(), env.underlay_segment, mock_dpu.oob_mac_address).await;

    let mut fixture = explored_host_fixture(&env, &mock_host).await;

    // Machine interface should not have any machine id associated with it right now.
    let mut txn = env.pool.begin().await?;
    let mi =
        db::machine_interface::find_by_mac_address(txn.as_mut(), mock_dpu.oob_mac_address).await?;
    assert!(mi[0].attached_dpu_machine_id.is_none());
    assert!(mi[0].machine_id.is_none());
    txn.rollback().await?;

    assert!(
        creator
            .create_managed_host(
                &fixture.host,
                &mut fixture.host_report,
                Some(&expected_machine(&mock_host)),
                &env.pool,
            )
            .await?
    );

    // At this point, create_managed_host must have updated the associated machine id in
    // machine_interfaces table.
    let mut txn = env.pool.begin().await?;
    let mi =
        db::machine_interface::find_by_mac_address(txn.as_mut(), mock_dpu.oob_mac_address).await?;
    assert!(mi[0].attached_dpu_machine_id.is_some());
    assert!(mi[0].machine_id.is_some());
    txn.rollback().await?;

    Ok(())
}

#[sqlx_test]
async fn test_dpu_interface_predictions_apply_when_dhcp_follows_machine_creation(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = Env::new(pool).await;
    let creator = machine_creator(&env, machine_creator_config());
    let mock_host = ManagedHostConfig::default();
    let mock_dpu = mock_host.dpus.first().unwrap();
    let mut fixture = explored_host_fixture(&env, &mock_host).await;
    let dpu_machine_id = fixture.dpu_machine_ids[&0];

    // The DPU machine exists before its first DHCP request, so there is no real interface yet.
    let mut txn = env.pool.begin().await?;
    let mi = db::machine_interface::find_by_machine_ids(&mut txn, &[dpu_machine_id]).await?;
    assert!(mi.is_empty());
    txn.rollback().await?;

    assert!(
        creator
            .create_managed_host(
                &fixture.host,
                &mut fixture.host_report,
                Some(&expected_machine(&mock_host)),
                &env.pool,
            )
            .await?
    );

    // Site Explorer cannot associate an interface that does not exist yet, but it must leave a
    // trusted MAC claim for DHCP to promote without waiting for another exploration sweep.
    let mut txn = env.pool.begin().await?;
    let machine = db::machine::find_one(
        txn.as_mut(),
        &dpu_machine_id,
        MachineSearchConfig {
            include_dpus: true,
            ..MachineSearchConfig::default()
        },
    )
    .await?;
    assert!(machine.is_some());

    let mi = db::machine_interface::find_by_machine_ids(&mut txn, &[dpu_machine_id]).await?;
    assert!(mi.is_empty());
    let prediction =
        db::predicted_machine_interface::find_by_mac_address(&mut txn, mock_dpu.oob_mac_address)
            .await?
            .expect("DPU OOB prediction should exist");
    assert_eq!(prediction.machine_id, dpu_machine_id.into());
    assert_eq!(
        prediction.expected_network_segment_type,
        model::network_segment::NetworkSegmentType::Underlay
    );
    assert!(prediction.primary_interface);
    txn.rollback().await?;

    // DHCP must consume the trusted prediction and create an already-owned interface.
    dhcp_discover_dpu_oob_iface(env.api(), env.underlay_segment, mock_dpu.oob_mac_address).await;

    let mut txn = env.pool.begin().await?;
    let interfaces =
        db::machine_interface::find_by_mac_address(txn.as_mut(), mock_dpu.oob_mac_address).await?;
    let [interface] = interfaces.as_slice() else {
        panic!("expected one promoted DPU OOB interface, got {interfaces:#?}");
    };
    assert_eq!(interface.machine_id, Some(dpu_machine_id.into()));
    assert_eq!(interface.attached_dpu_machine_id, Some(dpu_machine_id));
    assert!(interface.primary_interface);
    assert!(
        db::predicted_machine_interface::find_by_mac_address(&mut txn, mock_dpu.oob_mac_address,)
            .await?
            .is_none()
    );

    let topologies = db::machine_topology::find_by_machine_ids(&mut txn, &[dpu_machine_id]).await?;
    let topology = &topologies[&dpu_machine_id][0];
    let discovery_info = DiscoveryInfo::try_from(topology.topology().discovery_data.info.clone())?;
    let interface_id = interface.id;
    txn.commit().await?;

    let response = env
        .api()
        .discover_machine(Request::new(MachineDiscoveryInfo {
            machine_interface_id: Some(interface_id),
            discovery_data: Some(DiscoveryData::Info(discovery_info)),
            create_machine: true,
            ..Default::default()
        }))
        .await?
        .into_inner();
    assert_eq!(response.machine_id, Some(dpu_machine_id.into()));

    Ok(())
}

#[sqlx_test]
async fn test_dpu_interface_predictions_apply_when_dhcp_follows_multi_dpu_machine_creation(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    const NUM_DPUS: usize = 2;

    let env = Env::new(pool).await;
    let creator = machine_creator(&env, machine_creator_config());
    let mock_host = ManagedHostConfig::default().with_dpu_count(NUM_DPUS);
    let mut fixture = explored_host_fixture(&env, &mock_host).await;

    assert!(
        creator
            .create_managed_host(
                &fixture.host,
                &mut fixture.host_report,
                Some(&expected_machine(&mock_host)),
                &env.pool,
            )
            .await?
    );

    for dpu in &mock_host.dpus {
        dhcp_discover_dpu_oob_iface(env.api(), env.underlay_segment, dpu.oob_mac_address).await;
    }

    let mut txn = env.pool.begin().await?;
    for (dpu_index, dpu) in mock_host.dpus.iter().enumerate() {
        let dpu_index = dpu_index.try_into().expect("DPU index should fit into u8");
        let interfaces =
            db::machine_interface::find_by_mac_address(&mut *txn, dpu.oob_mac_address).await?;
        assert_eq!(interfaces.len(), 1);
        assert_eq!(
            interfaces[0].machine_id,
            Some(fixture.dpu_machine_ids[&dpu_index].into())
        );
        assert_eq!(
            interfaces[0].attached_dpu_machine_id,
            Some(fixture.dpu_machine_ids[&dpu_index])
        );
    }
    txn.commit().await?;

    Ok(())
}

/// Creating a multi-DPU host takes the admin segment locks in the allocators'
/// mode, before it writes a row: another shared holder lets the creation
/// through, an exclusive holder makes it wait. The admin address reconcile
/// that follows the commit leaves one addressed primary and addressless
/// dormant links either way.
#[sqlx_test]
async fn test_machine_creator_takes_admin_segment_locks_in_allocation_mode(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    const NUM_DPUS: usize = 2;

    let env = Env::new(pool).await;
    let creator = machine_creator(&env, machine_creator_config());

    // A shared holder (another IPv4 allocator) does not delay creation.
    let shared_host = ManagedHostConfig::default().with_dpu_count(NUM_DPUS);
    let mut fixture = explored_host_fixture(&env, &shared_host).await;
    let mut gate = env.pool.begin().await?;
    let admin_segment_ids =
        db::network_segment::list_segment_ids(gate.as_mut(), Some(NetworkSegmentType::Admin))
            .await?;
    db::machine_interface::lock_network_segments_shared(gate.as_mut(), &admin_segment_ids).await?;
    let created = tokio::time::timeout(
        Duration::from_secs(30),
        creator.create_managed_host(
            &fixture.host,
            &mut fixture.host_report,
            Some(&expected_machine(&shared_host)),
            &env.pool,
        ),
    )
    .await
    .expect("creation must not wait on a shared admin segment holder")?;
    assert!(created);
    gate.rollback().await?;
    assert_host_links_reconciled(&env.pool, &shared_host, NUM_DPUS).await?;

    // An exclusive holder makes creation wait for the segments, then it completes.
    let gated_host = ManagedHostConfig::default().with_dpu_count(NUM_DPUS);
    let mut fixture = explored_host_fixture(&env, &gated_host).await;
    let mut gate = env.pool.begin().await?;
    db::machine_interface::lock_all_admin_segments(gate.as_mut()).await?;
    let gate_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(gate.as_mut())
        .await?;
    let expected = expected_machine(&gated_host);
    let creation = creator.create_managed_host(
        &fixture.host,
        &mut fixture.host_report,
        Some(&expected),
        &env.pool,
    );
    tokio::pin!(creation);
    let creation_pid = tokio::select! {
        result = &mut creation => {
            panic!("creation finished while the admin segments were held exclusively: {result:?}")
        }
        blocked = wait_for_blocked_query(&env.pool, gate_pid, "pg_advisory_xact_lock_shared(") => blocked,
    };
    // The request is shared and precedes the first row write: Postgres
    // assigns a transaction id on the first write, and no host link exists.
    let (wrote_nothing, pending_mode): (bool, String) = sqlx::query_as(
        "SELECT activity.backend_xid IS NULL, lock.mode \
         FROM pg_stat_activity activity \
         JOIN pg_locks lock ON lock.pid = activity.pid \
         WHERE activity.pid = $1 AND lock.locktype = 'advisory' AND NOT lock.granted",
    )
    .bind(creation_pid)
    .fetch_one(&env.pool)
    .await?;
    assert!(
        wrote_nothing,
        "the lock request must precede the first row write"
    );
    assert_eq!(pending_mode, "ShareLock");
    assert!(host_links(&env.pool, &gated_host).await?.is_empty());
    gate.rollback().await?;
    assert!(creation.await?);
    assert_host_links_reconciled(&env.pool, &gated_host, NUM_DPUS).await?;

    Ok(())
}

/// A zero-DPU host allocates no admin address, so its creation takes no admin
/// segment lock and completes while an exclusive holder has them.
#[sqlx_test]
async fn test_zero_dpu_host_creation_does_not_wait_on_admin_segment_locks(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = Env::new(pool).await;
    let creator = machine_creator(&env, machine_creator_config());
    let host = ManagedHostConfig {
        dpus: vec![],
        ..ManagedHostConfig::default()
    }
    .with_expected_machine_data(ExpectedMachineData {
        dpu_policy: HostDpuPolicy::Ignore,
        ..Default::default()
    });
    let mut fixture = explored_host_fixture(&env, &host).await;

    let mut gate = env.pool.begin().await?;
    db::machine_interface::lock_all_admin_segments(gate.as_mut()).await?;
    let created = tokio::time::timeout(
        Duration::from_secs(30),
        creator.create_managed_host(
            &fixture.host,
            &mut fixture.host_report,
            Some(&expected_machine(&host)),
            &env.pool,
        ),
    )
    .await
    .expect("zero-DPU creation must not wait on the admin segment locks")?;
    assert!(created);
    gate.rollback().await?;

    Ok(())
}

/// Running creation again for an ingested host takes the steady-state path,
/// which locks no admin segment: it completes while an exclusive holder has
/// them and reports no new machine.
#[sqlx_test]
async fn test_steady_state_host_repair_does_not_wait_on_admin_segment_locks(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    const NUM_DPUS: usize = 2;

    let env = Env::new(pool).await;
    let creator = machine_creator(&env, machine_creator_config());
    let host = ManagedHostConfig::default().with_dpu_count(NUM_DPUS);
    let mut fixture = explored_host_fixture(&env, &host).await;
    assert!(
        creator
            .create_managed_host(
                &fixture.host,
                &mut fixture.host_report,
                Some(&expected_machine(&host)),
                &env.pool,
            )
            .await?
    );
    // Nothing is pending on the host, so the reconcile after the steady-state
    // commit has no reason to wait on the gate either.
    assert_host_links_reconciled(&env.pool, &host, NUM_DPUS).await?;

    let mut gate = env.pool.begin().await?;
    db::machine_interface::lock_all_admin_segments(gate.as_mut()).await?;
    let created = tokio::time::timeout(
        Duration::from_secs(30),
        creator.create_managed_host(
            &fixture.host,
            &mut fixture.host_report,
            Some(&expected_machine(&host)),
            &env.pool,
        ),
    )
    .await
    .expect("the steady-state repair must not wait on the admin segment locks")?;
    assert!(!created);
    gate.rollback().await?;

    Ok(())
}

/// The steady-state path keeps its own commit when the admin address
/// reconcile after it fails: the host stays and the failure is counted. A
/// host without a primary admin interface fails for good, so its BMC
/// endpoint is not flagged for exploration.
#[sqlx_test]
async fn test_steady_state_host_repair_counts_a_failed_admin_address_reconcile(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    const NUM_DPUS: usize = 2;

    let env = Env::new(pool).await;
    let creator = machine_creator(&env, machine_creator_config());
    let host = ManagedHostConfig::default().with_dpu_count(NUM_DPUS);
    let mut fixture = explored_host_fixture(&env, &host).await;
    assert!(
        creator
            .create_managed_host(
                &fixture.host,
                &mut fixture.host_report,
                Some(&expected_machine(&host)),
                &env.pool,
            )
            .await?
    );
    let links = host_links(&env.pool, &host).await?;
    let host_machine_id = links[0]
        .machine_id
        .expect("a host link belongs to the host");
    let primary = links
        .iter()
        .find(|link| link.primary_interface)
        .expect("one host link is the primary");

    // The host as ingestion leaves it for the explorer: its BMC interface
    // belongs to the machine and its endpoint row exists. Then no interface
    // of the host is primary any more, which the locked pass fails on.
    let host_bmc_ip = fixture.host.host_bmc_ip;
    let bmc_interface = db::machine_interface::find_by_mac_address(&env.pool, host.bmc_mac_address)
        .await?
        .remove(0);
    let mut txn = env.pool.begin().await?;
    db::machine_interface::associate_bmc_interface(
        &bmc_interface.id,
        MachineInterfaceAssociation::Machine(host_machine_id),
        txn.as_mut(),
    )
    .await?;
    db::explored_endpoints::insert(host_bmc_ip, &fixture.host_report, false, txn.as_mut()).await?;
    db::machine_interface::set_primary_interface(&primary.id, false, txn.as_mut()).await?;
    txn.commit().await?;

    let metrics = MetricsCapture::start();
    let created = creator
        .create_managed_host(
            &fixture.host,
            &mut fixture.host_report,
            Some(&expected_machine(&host)),
            &env.pool,
        )
        .await?;
    assert!(!created);
    assert_eq!(
        metrics.counter_delta(
            "carbide_site_explorer_host_admin_address_reconcile_failures_total",
            &[]
        ),
        1.0
    );
    assert!(
        !exploration_requested(&env.pool, host_bmc_ip).await?,
        "a reconcile failure that a retry cannot fix must not flag the host BMC for exploration"
    );

    Ok(())
}

/// Whether the explored endpoint at `address` is flagged for exploration.
async fn exploration_requested(
    pool: &PgPool,
    address: IpAddr,
) -> Result<bool, Box<dyn std::error::Error>> {
    Ok(sqlx::query_scalar(
        "SELECT exploration_requested FROM explored_endpoints WHERE address = $1",
    )
    .bind(address)
    .fetch_one(pool)
    .await?)
}

/// The host-side links of every DPU of `host`.
async fn host_links(
    pool: &PgPool,
    host: &ManagedHostConfig,
) -> Result<Vec<MachineInterfaceSnapshot>, Box<dyn std::error::Error>> {
    let mut host_links = Vec::new();
    for dpu in &host.dpus {
        host_links
            .extend(db::machine_interface::find_by_mac_address(pool, dpu.host_mac_address).await?);
    }
    Ok(host_links)
}

/// Every DPU of `host` has a host-side link, exactly one of them is the
/// addressed primary, and the others hold no address.
async fn assert_host_links_reconciled(
    pool: &PgPool,
    host: &ManagedHostConfig,
    dpu_count: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    let host_links = host_links(pool, host).await?;
    assert_eq!(host_links.len(), dpu_count);
    assert!(host_links.iter().all(|link| link.machine_id.is_some()));
    let (primary, dormant): (Vec<_>, Vec<_>) =
        host_links.iter().partition(|link| link.primary_interface);
    assert_eq!(primary.len(), 1);
    assert!(!primary[0].addresses.is_empty());
    assert_eq!(dormant.len(), dpu_count - 1);
    assert!(dormant.iter().all(|link| link.addresses.is_empty()));
    Ok(())
}

/// `create_machines` must skip a host whose BMCs all belong to machines unless
/// the host was explored this run or a BMC lost its machine. The DPU OOB
/// interface is the witness: only the per-host steady-state transaction
/// re-links it.
#[sqlx_test]
async fn test_site_explorer_skips_ingested_hosts_not_explored_this_run(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = Env::new(pool).await;
    let hosts = [ManagedHostConfig::default(), ManagedHostConfig::default()].map(|host| {
        let serial_number = host.serial.clone();
        host.with_expected_machine_data(ExpectedMachineData {
            serial_number,
            ..Default::default()
        })
    });
    let [host_a, host_b] = &hosts;
    let oob_mac_a = host_a.get_and_assert_single_dpu().oob_mac_address;
    let oob_mac_b = host_b.get_and_assert_single_dpu().oob_mac_address;

    let mut fixtures = Vec::new();
    for host in &hosts {
        // The OOB interface exists before creation, so creation links it.
        let dpu = host.get_and_assert_single_dpu();
        dhcp_discover_dpu_oob_iface(env.api(), env.underlay_segment, dpu.oob_mac_address).await;
        fixtures.push(explored_host_fixture(&env, host).await);
    }

    let mut endpoints = Vec::new();
    let mut txn = env.pool.begin().await?;
    for (host, fixture) in hosts.iter().zip(&fixtures) {
        db::expected_machine::create(txn.as_mut(), expected_machine(host)).await?;
        endpoints.push((fixture.host.host_bmc_ip, fixture.host_report.clone()));
        for dpu in &fixture.host.dpus {
            endpoints.push((dpu.bmc_ip, (*dpu.report).clone()));
        }
    }
    for (ip, report) in &endpoints {
        db::explored_endpoints::insert(*ip, report, false, txn.as_mut()).await?;
        db::explored_endpoints::set_preingestion_complete(*ip, txn.as_mut()).await?;
    }
    txn.commit().await?;

    // Nothing is explored unless requested, so `explored_this_run` holds
    // exactly the requested hosts.
    let explorer = super::env::test_site_explorer(
        &env.test_harness,
        SiteExplorerConfig {
            explorations_per_run: 0,
            create_machines: Arc::new(true.into()),
            ..Default::default()
        },
    );
    explorer.insert_endpoints(endpoints);

    // Neither host has a machine yet, so both enter the transaction.
    explorer.run_single_iteration().await?;
    assert!(interface_machine_id(&env.pool, oob_mac_a).await?.is_some());
    assert!(interface_machine_id(&env.pool, oob_mac_b).await?.is_some());

    unlink_machine_interface(&env.pool, oob_mac_a).await?;
    unlink_machine_interface(&env.pool, oob_mac_b).await?;
    let host_a_bmc_ip = fixtures[0].host.host_bmc_ip;
    let mut txn = env.pool.begin().await?;
    db::explored_endpoints::request_exploration_for_addresses(&[host_a_bmc_ip], txn.as_mut())
        .await?;
    txn.commit().await?;

    explorer.run_single_iteration().await?;
    assert_eq!(
        explorer
            .endpoint_explorer()
            .explore_endpoint_calls
            .lock()
            .unwrap()
            .iter()
            .map(|call| call.ip_address)
            .collect::<Vec<_>>(),
        vec![host_a_bmc_ip],
        "only the requested host BMC must be explored"
    );
    assert!(
        interface_machine_id(&env.pool, oob_mac_a).await?.is_some(),
        "a host explored this run must re-enter the steady-state transaction"
    );
    assert!(
        interface_machine_id(&env.pool, oob_mac_b).await?.is_none(),
        "a host with every BMC linked to a machine and not explored this run must be skipped"
    );

    // A BMC without a machine readmits the host without an exploration.
    unlink_machine_interface(
        &env.pool,
        host_b.get_and_assert_single_dpu().bmc_mac_address,
    )
    .await?;
    explorer.run_single_iteration().await?;
    assert!(
        interface_machine_id(&env.pool, oob_mac_b).await?.is_some(),
        "a host whose DPU BMC lost its machine must enter the transaction unexplored"
    );

    Ok(())
}

/// With `create_machines` off, an ingested host explored this run still has
/// its admin addresses reconciled, so the exploration a failed reconcile
/// requested on the host's BMC endpoint makes the repair it left: the admin
/// address moves from the dormant link back to the primary and the host's
/// network config version advances. An ingested host not explored this run
/// is left as it is.
#[sqlx_test]
async fn test_explored_host_admin_addresses_are_reconciled_without_create_machines(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    const NUM_DPUS: usize = 2;

    let env = Env::new(pool).await;
    let hosts = [ManagedHostConfig::default(), ManagedHostConfig::default()]
        .map(|host| host.with_dpu_count(NUM_DPUS));
    let (explorer, fixtures) = ingest_hosts_then_disable_creation(&env, &hosts).await?;

    // Leave both hosts as a failed reconcile does, with the admin address on
    // the dormant link, and flag only the first host's BMC as it does.
    let mut pending = Vec::new();
    for host in &hosts {
        pending.push(leave_admin_address_on_dormant_link(&env.pool, host).await?);
    }
    let Ok([requested, unexplored]) = <[_; 2]>::try_from(pending) else {
        panic!("two hosts were left needing a repair")
    };
    let requested_bmc_ip = fixtures[0].host.host_bmc_ip;
    let mut txn = env.pool.begin().await?;
    db::explored_endpoints::request_exploration_for_addresses(&[requested_bmc_ip], txn.as_mut())
        .await?;
    txn.commit().await?;

    let metrics = MetricsCapture::start();
    explorer.run_single_iteration().await?;
    assert_eq!(
        metrics.counter_delta(
            "carbide_site_explorer_host_admin_address_reconcile_failures_total",
            &[]
        ),
        0.0
    );
    assert!(
        !exploration_requested(&env.pool, requested_bmc_ip).await?,
        "the exploration consumes the request"
    );

    let dormant = db::machine_interface::find_one(&env.pool, requested.dormant_link_id).await?;
    assert!(dormant.addresses.is_empty(), "{dormant:?}");
    assert_host_links_reconciled(&env.pool, &hosts[0], NUM_DPUS).await?;
    assert_eq!(
        network_config_version(&env.pool, &requested.host_machine_id).await?,
        requested.version_before + 1,
        "the repaired admin config is republished"
    );

    let dormant = db::machine_interface::find_one(&env.pool, unexplored.dormant_link_id).await?;
    assert_eq!(dormant.addresses, vec![unexplored.admin_ip], "{dormant:?}");
    assert_eq!(
        network_config_version(&env.pool, &unexplored.host_machine_id).await?,
        unexplored.version_before,
        "a host not explored this run is not reconciled"
    );

    Ok(())
}

/// The admin address pass holds the admin-lock admission permit around each
/// reconcile, as every flow that may wait on the admin segment locks does:
/// with every permit held, the pass makes no repair until one is released.
#[sqlx_test]
async fn test_explored_host_admin_address_pass_waits_for_admission(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    const NUM_DPUS: usize = 2;

    let env = Env::new(pool).await;
    let hosts = [ManagedHostConfig::default().with_dpu_count(NUM_DPUS)];
    let (explorer, fixtures) = ingest_hosts_then_disable_creation(&env, &hosts).await?;
    let pending = leave_admin_address_on_dormant_link(&env.pool, &hosts[0]).await?;
    let mut txn = env.pool.begin().await?;
    db::explored_endpoints::request_exploration_for_addresses(
        &[fixtures[0].host.host_bmc_ip],
        txn.as_mut(),
    )
    .await?;
    txn.commit().await?;

    let permits = hold_every_admin_lock_admission_permit().await;
    let iteration = explorer.run_single_iteration();
    tokio::pin!(iteration);
    assert!(
        tokio::time::timeout(Duration::from_millis(250), &mut iteration)
            .await
            .is_err(),
        "the pass must wait for an admission permit"
    );
    let dormant = db::machine_interface::find_one(&env.pool, pending.dormant_link_id).await?;
    assert_eq!(
        dormant.addresses,
        vec![pending.admin_ip],
        "no repair is made without a permit"
    );

    drop(permits);
    tokio::time::timeout(Duration::from_secs(30), iteration).await??;
    let dormant = db::machine_interface::find_one(&env.pool, pending.dormant_link_id).await?;
    assert!(dormant.addresses.is_empty(), "{dormant:?}");
    assert_eq!(
        network_config_version(&env.pool, &pending.host_machine_id).await?,
        pending.version_before + 1
    );

    Ok(())
}

/// Every permit of the admin-lock admission gate: `ADMIN_LOCK_ADMISSION` of
/// them, 16 by default.
async fn hold_every_admin_lock_admission_permit() -> Vec<tokio::sync::OwnedSemaphorePermit> {
    let permits = std::env::var("ADMIN_LOCK_ADMISSION")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(16);
    let mut held = Vec::with_capacity(permits);
    for _ in 0..permits {
        held.push(db::machine_interface::admin_lock_admission().await);
    }
    held
}

/// Ingests `hosts` with `create_machines` on, then turns it off and returns
/// the explorer and the hosts' fixtures. Nothing is explored unless requested,
/// so `explored_this_run` holds exactly the requested endpoints.
async fn ingest_hosts_then_disable_creation(
    env: &Env,
    hosts: &[ManagedHostConfig],
) -> Result<(TestSiteExplorer, Vec<ExploredHostFixture>), Box<dyn std::error::Error>> {
    let mut fixtures = Vec::new();
    for host in hosts {
        fixtures.push(explored_host_fixture(env, host).await);
    }
    let mut endpoints = Vec::new();
    let mut txn = env.pool.begin().await?;
    for (host, fixture) in hosts.iter().zip(&fixtures) {
        let host = host
            .clone()
            .with_expected_machine_data(ExpectedMachineData {
                serial_number: host.serial.clone(),
                ..Default::default()
            });
        db::expected_machine::create(txn.as_mut(), expected_machine(&host)).await?;
        endpoints.push((fixture.host.host_bmc_ip, fixture.host_report.clone()));
        for dpu in &fixture.host.dpus {
            endpoints.push((dpu.bmc_ip, (*dpu.report).clone()));
        }
    }
    for (ip, report) in &endpoints {
        db::explored_endpoints::insert(*ip, report, false, txn.as_mut()).await?;
        db::explored_endpoints::set_preingestion_complete(*ip, txn.as_mut()).await?;
    }
    txn.commit().await?;

    let create_machines = Arc::new(AtomicBool::new(true));
    let explorer = super::env::test_site_explorer(
        &env.test_harness,
        SiteExplorerConfig {
            explorations_per_run: 0,
            create_machines: create_machines.clone(),
            ..Default::default()
        },
    );
    explorer.insert_endpoints(endpoints);
    explorer.run_single_iteration().await?;
    create_machines.store(false, Ordering::Relaxed);
    Ok((explorer, fixtures))
}

/// A reconciled two-DPU host left needing an admin address repair.
struct PendingAdminRepair {
    host_machine_id: MachineId,
    dormant_link_id: MachineInterfaceId,
    /// The host's admin address, now on the dormant link.
    admin_ip: IpAddr,
    version_before: u64,
}

/// Moves the host's admin address from its primary link to its dormant link:
/// the state a host is left in when its admin address was leased through a
/// link that is now dormant.
async fn leave_admin_address_on_dormant_link(
    pool: &PgPool,
    host: &ManagedHostConfig,
) -> Result<PendingAdminRepair, Box<dyn std::error::Error>> {
    let (primary, dormant): (Vec<_>, Vec<_>) = host_links(pool, host)
        .await?
        .into_iter()
        .partition(|link| link.primary_interface);
    let ([primary], [dormant]) = (primary.as_slice(), dormant.as_slice()) else {
        panic!("a two-DPU host has one primary and one dormant link")
    };
    let host_machine_id = primary.machine_id.expect("a host link belongs to the host");
    let &[admin_ip] = primary.addresses.as_slice() else {
        panic!("the primary link holds the host's admin address: {primary:?}")
    };
    assert!(dormant.addresses.is_empty(), "{dormant:?}");
    sqlx::query("UPDATE machine_interface_addresses SET interface_id = $1 WHERE interface_id = $2")
        .bind(dormant.id)
        .bind(primary.id)
        .execute(pool)
        .await?;
    Ok(PendingAdminRepair {
        host_machine_id,
        dormant_link_id: dormant.id,
        admin_ip,
        version_before: network_config_version(pool, &host_machine_id).await?,
    })
}

/// The network config version of `machine_id` as stored.
async fn network_config_version(
    pool: &PgPool,
    machine_id: &MachineId,
) -> Result<u64, Box<dyn std::error::Error>> {
    Ok(db::machine::get_network_config(pool, machine_id)
        .await?
        .version
        .version_nr())
}

async fn interface_machine_id(
    pool: &PgPool,
    mac_address: MacAddress,
) -> Result<Option<MachineId>, Box<dyn std::error::Error>> {
    let interfaces = db::machine_interface::find_by_mac_address(pool, mac_address).await?;
    let [interface] = interfaces.as_slice() else {
        panic!("expected one interface for {mac_address}, got {interfaces:#?}");
    };
    Ok(interface.machine_id)
}

/// Detaches an interface from its machine, as if it had never been linked.
async fn unlink_machine_interface(
    pool: &PgPool,
    mac_address: MacAddress,
) -> Result<(), Box<dyn std::error::Error>> {
    sqlx::query(
        "UPDATE machine_interfaces
         SET machine_id = NULL, attached_dpu_machine_id = NULL, association_type = 'None'
         WHERE mac_address = $1",
    )
    .bind(mac_address)
    .execute(pool)
    .await?;
    Ok(())
}

#[sqlx_test]
async fn test_machine_creator_rejects_partial_dpu_machine_set(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    const NUM_DPUS: usize = 2;

    let env = Env::new(pool).await;
    let creator = machine_creator(&env, machine_creator_config());
    let mock_host = ManagedHostConfig::default().with_dpu_count(NUM_DPUS);
    let mut fixture = explored_host_fixture(&env, &mock_host).await;

    // Ingest an earlier report containing only the first DPU so that both its machine and its
    // host association exist. The complete report below should then be rejected as a partial set.
    let partial_host = ExploredManagedHost {
        host_bmc_ip: fixture.host.host_bmc_ip,
        dpus: vec![fixture.host.dpus[0].clone()],
    };
    let mut partial_host_report = fixture.host_report.clone();
    assert!(
        creator
            .create_managed_host(
                &partial_host,
                &mut partial_host_report,
                Some(&expected_machine(&mock_host)),
                &env.pool,
            )
            .await?
    );

    creator
        .create_managed_host(
            &fixture.host,
            &mut fixture.host_report,
            Some(&expected_machine(&mock_host)),
            &env.pool,
        )
        .await
        .expect_err("a partial DPU machine set should be rejected");

    let mut txn = env.pool.begin().await?;
    assert!(
        db::machine::find_one(
            &mut *txn,
            &fixture.dpu_machine_ids[&1],
            MachineSearchConfig::default(),
        )
        .await?
        .is_none()
    );
    txn.commit().await?;

    Ok(())
}

#[sqlx_test]
async fn test_machine_creator_creates_managed_host_with_dpf_disabled(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = Env::new(pool).await;
    let creator = machine_creator(&env, machine_creator_config());

    let mock_dpu = DpuConfig::with_serial("MT2328XZ185R".to_string());
    let mock_host = ManagedHostConfig {
        expected_machine_data: Some(ExpectedMachineData {
            dpf_enabled: Some(false),
            ..Default::default()
        }),
        ..ManagedHostConfig::default().with_dpus(vec![mock_dpu.clone()])
    };
    let mut fixture = explored_host_fixture(&env, &mock_host).await;

    assert!(
        creator
            .create_managed_host(
                &fixture.host,
                &mut fixture.host_report,
                Some(&expected_machine(&mock_host)),
                &env.pool,
            )
            .await?
    );

    let machines = db::machine::find(
        &env.pool,
        ObjectFilter::<MachineId>::All,
        MachineSearchConfig {
            include_predicted_host: true,
            ..Default::default()
        },
    )
    .await?;

    assert_eq!(machines.len(), 2);
    for machine in machines {
        if machine.is_dpu() {
            // DPU has no expected-machine entry, so it always defaults to `true`.
            assert!(machine.config.dpf.enabled);
        } else {
            // Host has expected-machine entry with `dpf_enabled: Some(false)`.
            assert!(!machine.config.dpf.enabled);
        }
    }

    Ok(())
}

#[sqlx_test]
async fn test_machine_creator_creates_managed_host_with_dpf_enabled(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = Env::new(pool).await;
    let creator = machine_creator_with_dpf(&env, machine_creator_config(), true);

    let mock_dpu = DpuConfig::with_serial("MT2328XZ185R".to_string());
    let mock_host = ManagedHostConfig {
        expected_machine_data: Some(ExpectedMachineData {
            dpf_enabled: Some(true),
            ..Default::default()
        }),
        ..ManagedHostConfig::default().with_dpus(vec![mock_dpu.clone()])
    };
    let mut fixture = explored_host_fixture(&env, &mock_host).await;

    assert!(
        creator
            .create_managed_host(
                &fixture.host,
                &mut fixture.host_report,
                Some(&expected_machine(&mock_host)),
                &env.pool,
            )
            .await?
    );

    let machines = db::machine::find(
        &env.pool,
        ObjectFilter::<MachineId>::All,
        MachineSearchConfig {
            include_predicted_host: true,
            ..Default::default()
        },
    )
    .await?;

    assert_eq!(machines.len(), 2);
    for machine in machines {
        assert!(machine.config.dpf.enabled);
        if !machine.is_dpu() {
            assert!(machine.config.dpf.used_for_ingestion);
        }
    }

    Ok(())
}

/// `create_managed_host` must refuse to create a Managed Host when no
/// `expected_machines` entry is supplied.
#[sqlx_test]
async fn test_machine_creator_rejects_unexpected_host(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = Env::new(pool).await;
    let creator = machine_creator(&env, machine_creator_config());

    let mock_host = ManagedHostConfig::default();
    let mut fixture = explored_host_fixture(&env, &mock_host).await;

    assert!(
        !creator
            .create_managed_host(&fixture.host, &mut fixture.host_report, None, &env.pool,)
            .await?
    );

    let machines = db::machine::find(
        &env.pool,
        ObjectFilter::<MachineId>::All,
        MachineSearchConfig {
            include_predicted_host: true,
            ..Default::default()
        },
    )
    .await?;
    assert_eq!(
        machines.len(),
        0,
        "expected no Machine rows for an unexpected host, got {machines:#?}"
    );

    Ok(())
}

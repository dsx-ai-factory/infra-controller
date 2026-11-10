// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Exercise DPU loopback callers below the production remote-selection gate.

use std::collections::HashSet;
use std::future::{Future, poll_fn};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::task::Poll;
use std::time::Duration;

use carbide_resource_pool::{PoolDefinitionSource, ResourcePoolBackendError};
use carbide_site_explorer::config::SiteExplorerConfig;
use carbide_site_explorer::dpu_loopback::{DPU_IPV4_LOOPBACK_SLOT, PreparedDpuLoopback};
use carbide_site_explorer::errors::SiteExplorerError;
use carbide_site_explorer::{MachineCreator, SiteExplorer};
use carbide_uuid::machine::MachineId;
use db::resource_pool::remote;
use model::expected_machine::ExpectedMachine;
use model::hardware_info::HardwareInfo;
use model::machine::machine_id::host_id_from_dpu_hardware_info;
use model::machine::machine_search_config::MachineSearchConfig;
use model::machine::{CURRENT_STATE_MODEL_VERSION, ManagedHostState};
use model::resource_pool::OwnerType;
use model::resource_pool::allocation::{AllocationSource, AllocationState, ResourcePoolAllocation};
use model::resource_pool::common::LOOPBACK_IP;
use model::resource_pool::remote::AllocationResult;
use model::site_explorer::{EndpointExplorationReport, ExploredDpu, ExploredManagedHost};
use rpc::forge;
use rpc::forge::forge_server::Forge;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use tokio::sync::Barrier;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tonic::Request;

use crate::test_support::fixture_config::ManagedHostConfigExt as _;
use crate::tests::common::api_fixtures;
use crate::tests::common::api_fixtures::dpu::dpu_discover_dhcp;
use crate::tests::common::api_fixtures::site_explorer::{
    MockExploredHost, register_expected_machine,
};
use crate::tests::common::resource_pool::{PausedCall, RemotePoolFixture};

/// Both callers must derive the same hardware ID from their different reports.
struct DpuInputs {
    machine_id: MachineId,
    request: forge::MachineDiscoveryInfo,
    explored: ExploredManagedHost,
    host_report: EndpointExplorationReport,
    expected: ExpectedMachine,
}

/// Each admission case protects a different stop boundary without repeating
/// the full caller-by-stop combination matrix.
#[derive(Clone, Copy)]
enum AdmissionCase {
    DiscoveryDeadline,
    ExplorerCancellation,
}

/// Prepare discovery and exploration inputs without creating a machine. Moving
/// the unused loopback pool first keeps every reservation on the test provider.
async fn fixture(database: PgPool) -> eyre::Result<(RemotePoolFixture, DpuInputs)> {
    fixture_with_dpu_count(database, 1).await
}

/// Build sibling DPU reports for one host so preparation shares the real
/// MachineCreator input and must finish every sibling before writing machines.
async fn fixture_with_dpu_count(
    database: PgPool,
    dpu_count: u8,
) -> eyre::Result<(RemotePoolFixture, DpuInputs)> {
    let mut fixture = RemotePoolFixture::start_ipv4(database).await?;
    fixture
        .initialize(PoolDefinitionSource::SeedConfiguration)
        .await?;
    let config = fixture
        .env
        .managed_host_config()
        .with_dpu_count(usize::from(dpu_count));
    register_expected_machine(&fixture.env, &config, Some(false)).await;
    let mut mock = MockExploredHost::new(&fixture.env, config.clone())
        .discover_dhcp_host_bmc(|result, _| {
            result?;
            Ok(())
        })
        .await?;
    for index in 0..dpu_count {
        mock = mock
            .discover_dhcp_dpu_bmc(index, |result, _| {
                result?;
                Ok(())
            })
            .await?;
    }
    let host_bmc_ip = mock.host_bmc_ip.expect("host BMC lease");
    let dpu_bmc_ips = (0..dpu_count)
        .map(|index| (index, mock.dpu_bmc_ips[&index]))
        .collect::<Vec<_>>();
    drop(mock);
    let mut interface_ids = Vec::new();
    for dpu in &config.dpus {
        interface_ids.push(dpu_discover_dhcp(&fixture.env, &dpu.oob_mac_address.to_string()).await);
    }
    let reports = config.exploration_results(Some(host_bmc_ip), &dpu_bmc_ips)?;
    let dpus = reports
        .dpu_reports
        .into_iter()
        .map(|report| ExploredDpu {
            bmc_ip: report.bmc_ip,
            host_pf_mac_address: Some(config.dpus[usize::from(report.dpu_index)].host_mac_address),
            host_chassis_id: None,
            report: Arc::new(report.report),
        })
        .collect::<Vec<_>>();
    let machine_id = dpus
        .first()
        .expect("at least one DPU")
        .report
        .machine_id
        .expect("DPU report identity");
    let expected =
        db::expected_machine::find_by_bmc_mac_address(&fixture.env.pool, config.bmc_mac_address)
            .await?
            .expect("registered expected machine");
    let inputs = DpuInputs {
        machine_id,
        request: forge::MachineDiscoveryInfo {
            machine_interface_id: Some(*interface_ids.first().expect("DPU OOB lease")),
            discovery_data: Some(rpc::DiscoveryData::Info(rpc::DiscoveryInfo::try_from(
                HardwareInfo::from(config.dpus.first().expect("at least one DPU")),
            )?)),
            create_machine: true,
            ..Default::default()
        },
        explored: ExploredManagedHost { host_bmc_ip, dpus },
        host_report: reports.host_report.expect("host report").1,
        expected,
    };
    Ok((fixture, inputs))
}

/// `creator` uses the API's resolved registry so both callers see the same
/// remote request instead of falling back to Integrated inventory.
fn creator(fixture: &RemotePoolFixture) -> MachineCreator {
    MachineCreator::new(
        fixture.env.pool.clone(),
        (&fixture.env.config.site_explorer).into(),
        fixture.env.api.resource_pool_registry.clone(),
        fixture.env.common_pools.clone(),
        Arc::new(fixture.env.config.rack_profiles.clone()),
        None,
        false,
    )
}

/// `receipt` includes pending and terminal cleanup records. An active-only
/// lookup would hide whether the original request was cancelled or replaced.
async fn receipt(
    fixture: &RemotePoolFixture,
    machine_id: MachineId,
) -> eyre::Result<ResourcePoolAllocation> {
    Ok(remote::latest_for_owner_slot(
        &fixture.env.pool,
        OwnerType::Machine,
        &machine_id.to_string(),
        DPU_IPV4_LOOPBACK_SLOT,
    )
    .await?
    .expect("saved DPU IPv4 request"))
}

/// `wait_for_preparation` observes provider reservation before its reply, while
/// NICo still has a Pending request without a saved assignment.
async fn wait_for_preparation(pause: &PausedCall) -> eyre::Result<()> {
    tokio::time::timeout(Duration::from_secs(10), pause.entered.notified()).await?;
    Ok(())
}

/// `wait_for_cleanup_waiter` observes a specific admin-segment lock queue edge,
/// including a cleanup blocked behind another waiter rather than the holder.
async fn wait_for_cleanup_waiter(
    pool: &PgPool,
    blocker: i32,
    waiter: Option<i32>,
) -> eyre::Result<i32> {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let waiting = sqlx::query_scalar(
                "SELECT pid FROM pg_stat_activity
                 WHERE datname = current_database() AND wait_event_type = 'Lock'
                   AND $1 = ANY(pg_blocking_pids(pid))
                   AND ($2::integer IS NULL OR pid = $2)
                   AND strpos(query, 'pg_advisory_xact_lock') > 0",
            )
            .bind(blocker)
            .bind(waiter)
            .fetch_optional(pool)
            .await?;
            if let Some(pid) = waiting {
                break eyre::Ok(pid);
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?
}

/// `owning_site_explorer` prepares fixture reports for ingestion, after BMC and
/// firmware setup, and preserves the production registry and shutdown wiring.
async fn owning_site_explorer(
    fixture: &RemotePoolFixture,
    inputs: &DpuInputs,
) -> eyre::Result<SiteExplorer> {
    let mut endpoints = vec![(inputs.explored.host_bmc_ip, inputs.host_report.clone())];
    endpoints.extend(
        inputs
            .explored
            .dpus
            .iter()
            .map(|dpu| (dpu.bmc_ip, (*dpu.report).clone())),
    );
    // Enter the owning iteration at ingestion, after the separate firmware
    // and BMC setup lifecycle has completed for these fixture reports.
    let mut txn = fixture.env.pool.begin().await?;
    for (address, report) in &endpoints {
        db::explored_endpoints::insert(*address, report, false, &mut txn).await?;
        db::explored_endpoints::set_preingestion_complete(*address, &mut txn).await?;
    }
    txn.commit().await?;
    fixture.env.endpoint_explorer.insert_endpoints(endpoints);
    Ok(SiteExplorer::new(
        fixture.env.pool.clone(),
        SiteExplorerConfig {
            enabled: Arc::new(true.into()),
            create_machines: Arc::new(true.into()),
            run_interval: Duration::from_secs(3600),
            ..fixture.env.config.site_explorer.clone()
        },
        fixture.env.test_meter.meter(),
        fixture.env.api.endpoint_exploration_service.clone(),
        fixture.env.api.bmc_client.clone(),
        fixture.env.api.resource_pool_registry.clone(),
        fixture.env.common_pools.clone(),
        fixture.env.api.work_lock_manager_handle.clone(),
        fixture.env.config.rack_profiles.clone(),
        None,
        fixture.env.test_credential_manager.clone(),
        false,
    ))
}

/// Integrated preparation defers allocation to the entity transaction, so it
/// must not opt ordinary discovery into a provider reservation's admission limit.
#[crate::sqlx_test]
async fn integrated_dpu_preparation_has_no_provider_reservation(
    database: PgPool,
) -> eyre::Result<()> {
    let env = api_fixtures::create_test_env(database).await;
    let machine_id = "fm100dskla0ihp0pn4tv7v1js2k2mo37sl0jjr8141okqg8pjpdpfihaa80".parse()?;
    let prepared = PreparedDpuLoopback::prepare(
        &env.pool,
        &env.api.resource_pool_registry,
        &env.common_pools,
        machine_id,
        &env.api.cancel_token,
    )
    .await?;
    assert!(!prepared.has_provider_reservation());
    Ok(())
}

/// Discovery must recover a lost reply while a competing Site Explorer call
/// leaves the saved request intact. Repair and deletion then use that ownership.
#[crate::sqlx_test]
async fn remote_dpu_discovery_and_site_repair_reuse_a_lost_reply(
    database: PgPool,
) -> eyre::Result<()> {
    Box::pin(async move {
        let (fixture, mut inputs) = fixture(database).await?;
        let creator = creator(&fixture);
        // Keep a machine present without its loopback, so the finder negative
        // below depends on Pending ownership being excluded, not row absence.
        let mut txn = fixture.env.pool.begin().await?;
        db::machine::create(&mut txn, Some(&fixture.env.common_pools), &inputs.machine_id,
            ManagedHostState::Created, None, CURRENT_STATE_MODEL_VERSION).await?;
        txn.commit().await?;
        fixture.provider.allocate_pause.pause_next.store(true, Ordering::SeqCst);
        fixture.provider.lose_allocate_reply.store(true, Ordering::SeqCst);
        let api = fixture.env.api.clone();
        let mut discovery = api.discover_machine(Request::new(inputs.request.clone()));
        tokio::select! {
            result = &mut discovery => panic!("discovery finished before provider gate: {result:?}"),
            result = wait_for_preparation(&fixture.provider.allocate_pause) => result?,
        }
        let pending = receipt(&fixture, inputs.machine_id).await?;
        assert_eq!(pending.state, AllocationState::Pending);
        assert_eq!(fixture.provider.local.provisioned_count(), 1);
        // Another real caller cannot replace the saved request while its reply
        // is missing. Its next reconciliation reuses the attached ownership.
        let occupied = creator.create_managed_host(
            &inputs.explored, &mut inputs.host_report, Some(&inputs.expected), &fixture.env.pool,
        ).await.expect_err("the pending owner slot belongs to discovery");
        assert!(matches!(occupied, SiteExplorerError::DatabaseError(
            db::DatabaseError::FailedPrecondition(ref message),
        ) if message.contains("unfinished request") && message.contains(DPU_IPV4_LOOPBACK_SLOT)));

        // Preflight has finished. Hold the caller interface so the real provider
        // reply can be saved while the entity transaction cannot attach it.
        let mut interface_gate = fixture.env.pool.begin().await?;
        sqlx::query("SELECT id FROM machine_interfaces WHERE id = $1 FOR UPDATE")
            .bind(inputs.request.machine_interface_id.expect("DPU OOB interface"))
            .fetch_one(&mut *interface_gate).await?;
        fixture.provider.allocate_pause.resume.notify_one();
        let saved = tokio::select! {
            result = &mut discovery => panic!("discovery passed the interface gate: {result:?}"),
            saved = tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    let saved = receipt(&fixture, inputs.machine_id).await?;
                    if matches!(&saved.source, AllocationSource::Remote { result: Some(AllocationResult::Allocated(_)), .. }) {
                        break eyre::Ok(saved);
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }) => saved??,
        };
        assert_eq!(saved.allocation_id, pending.allocation_id);
        assert_eq!(saved.state, AllocationState::Pending);
        assert!(db::machine::get_network_config(&fixture.env.pool, &inputs.machine_id)
            .await?.value.loopback_ip.is_none());
        let not_found = fixture.env.api.find_ip_address(Request::new(forge::FindIpAddressRequest {
            ip: "198.51.100.10".into(),
        })).await.expect_err("saved Pending ownership must not identify an existing DPU");
        assert_eq!(not_found.code(), tonic::Code::NotFound);
        interface_gate.commit().await?;
        let result = tokio::time::timeout(Duration::from_secs(10), &mut discovery).await??;
        assert_eq!(result.into_inner().machine_id, Some(inputs.machine_id));
        let attached = receipt(&fixture, inputs.machine_id).await?;
        assert_eq!(attached.allocation_id, pending.allocation_id);
        assert_eq!(attached.state, AllocationState::Attached);
        let before = db::machine::get_network_config(&fixture.env.pool, &inputs.machine_id).await?;
        let address = before.value.loopback_ip.expect("attached IPv4");
        assert_eq!(address.to_string(), "198.51.100.10");

        // Losing the configuration field must preserve both finder ownership
        // and the reservation that Site Explorer restores on its next pass.
        let mut txn = fixture.env.pool.begin().await?;
        let mut cleared = before.value.clone();
        cleared.loopback_ip = None;
        assert_eq!(db::machine::try_update_network_config(
            &mut txn, &inputs.machine_id, before.version, &cleared,
        ).await?, db::ConditionalWrite::Applied(()));
        txn.commit().await?;
        let cleared_version = db::machine::get_network_config(&fixture.env.pool, &inputs.machine_id).await?.version;
        let reused = PreparedDpuLoopback::prepare(
            &fixture.env.pool, &fixture.env.api.resource_pool_registry,
            &fixture.env.common_pools, inputs.machine_id, &fixture.cancel,
        ).await?;
        assert!(!reused.has_provider_reservation(), "attached ownership needs no provider admission deadline");
        let found = fixture.env.api.find_ip_address(Request::new(forge::FindIpAddressRequest {
            ip: address.to_string(),
        })).await?.into_inner();
        assert!(found.errors.is_empty(), "{:?}", found.errors);
        let owned = found.matches.iter().filter(|entry| entry.ip_type == forge::IpType::LoopbackIp as i32).collect::<Vec<_>>();
        assert_eq!(owned.len(), 1);
        assert_eq!(owned[0].owner_id.as_deref(), Some(inputs.machine_id.to_string().as_str()));
        let explorer = owning_site_explorer(&fixture, &inputs).await?;
        explorer.run_single_iteration().await?;
        let repaired = db::machine::get_network_config(&fixture.env.pool, &inputs.machine_id).await?;
        assert_eq!(repaired.value.loopback_ip, Some(address));
        assert_ne!(repaired.version, cleared_version);
        assert_eq!(receipt(&fixture, inputs.machine_id).await?, attached);
        assert_eq!(fixture.provider.allocations.load(Ordering::SeqCst), 1);

        // Force deletion must release the original provider handle, including
        // after reconciliation reused it without another Allocate call.
        let deleted = fixture.env.api.admin_force_delete_machine(Request::new(forge::AdminForceDeleteMachineRequest {
            host_query: inputs.machine_id.to_string(), ..Default::default()
        })).await?.into_inner();
        assert!(deleted.all_done);
        let released = receipt(&fixture, inputs.machine_id).await?;
        assert_eq!(released.allocation_id, attached.allocation_id);
        assert_eq!(released.source, attached.source);
        assert_eq!(released.state, AllocationState::ReleaseRequested);
        fixture.remote(LOOPBACK_IP).reconcile(released.allocation_id, &fixture.cancel).await?;
        assert_eq!(fixture.provider.local.provisioned_count(), 0);
        assert_eq!(receipt(&fixture, inputs.machine_id).await?.state, AllocationState::Released);
        drop(discovery);
        fixture.stop().await
    }).await
}

/// Each caller must roll back machine creation and attachment on a failed
/// configuration write, then retain cancellation that permits a later retry.
#[crate::sqlx_test]
async fn remote_dpu_configuration_failures_roll_back_both_callers(
    database: PgPool,
) -> eyre::Result<()> {
    Box::pin(async move {
        let (fixture, mut inputs) = fixture(database).await?;
        // Reject the real configuration write after provider preparation, so
        // rollback cannot be mistaken for undoing the external reservation.
        sqlx::query("ALTER TABLE machines ADD CONSTRAINT reject_dpu_loopback CHECK (network_config->>'loopback_ip' IS NULL)")
            .execute(&fixture.env.pool).await?;
        fixture.env.api.discover_machine(Request::new(inputs.request.clone())).await
            .expect_err("injected configuration write rejection");
        assert!(db::machine::find_one(&fixture.env.pool, &inputs.machine_id, MachineSearchConfig::default()).await?.is_none());
        let failed = receipt(&fixture, inputs.machine_id).await?;
        assert_eq!(failed.state, AllocationState::CancelRequested);
        assert_eq!(fixture.provider.local.provisioned_count(), 1);
        fixture.remote(LOOPBACK_IP).reconcile(failed.allocation_id, &fixture.cancel).await?;
        assert_eq!(receipt(&fixture, inputs.machine_id).await?.state, AllocationState::Cancelled);

        // Keep the constraint for a Site Explorer attempt as well. Its own
        // rollback/cancellation path must work before either caller can retry.
        let site_error = creator(&fixture).create_managed_host(
            &inputs.explored, &mut inputs.host_report, Some(&inputs.expected), &fixture.env.pool,
        ).await.expect_err("injected Site Explorer configuration rejection");
        assert!(matches!(site_error, SiteExplorerError::DatabaseError(ref error)
            if error.violates_constraint("reject_dpu_loopback")));
        assert!(db::machine::find_one(&fixture.env.pool, &inputs.machine_id, MachineSearchConfig::default()).await?.is_none());
        let site_failed = receipt(&fixture, inputs.machine_id).await?;
        assert_ne!(site_failed.allocation_id, failed.allocation_id);
        assert_eq!(site_failed.state, AllocationState::CancelRequested);
        assert_eq!(fixture.provider.local.provisioned_count(), 1);
        fixture.remote(LOOPBACK_IP).reconcile(site_failed.allocation_id, &fixture.cancel).await?;
        assert_eq!(receipt(&fixture, inputs.machine_id).await?.state, AllocationState::Cancelled);
        sqlx::query("ALTER TABLE machines DROP CONSTRAINT reject_dpu_loopback").execute(&fixture.env.pool).await?;

        // A Site Explorer creation, rather than discovery's retry, must also
        // attach through the selected backend after cleanup is confirmed.
        assert!(creator(&fixture).create_managed_host(
            &inputs.explored, &mut inputs.host_report, Some(&inputs.expected), &fixture.env.pool,
        ).await?);
        let machine = db::machine::find_one(&fixture.env.pool, &inputs.machine_id, MachineSearchConfig::default())
            .await?.expect("site-created DPU");
        assert!(machine.network_config.loopback_ip.is_some());
        let attached = receipt(&fixture, inputs.machine_id).await?;
        assert_ne!(attached.allocation_id, failed.allocation_id);
        assert_ne!(attached.allocation_id, site_failed.allocation_id);
        assert_eq!(attached.state, AllocationState::Attached);
        assert_eq!(fixture.provider.local.provisioned_count(), 1);
        fixture.remote(LOOPBACK_IP).reconcile(failed.allocation_id, &fixture.cancel).await?;
        fixture.remote(LOOPBACK_IP).reconcile(site_failed.allocation_id, &fixture.cancel).await?;
        assert_eq!(receipt(&fixture, inputs.machine_id).await?, attached);
        assert_eq!(fixture.provider.local.provisioned_count(), 1);
        fixture.stop().await
    }).await
}

/// Force deletion during preparation prevents resurrection, and a terminal
/// cancellation replay leaves a later assignment intact.
#[crate::sqlx_test]
async fn remote_dpu_deletion_during_preparation_preserves_replacement(
    database: PgPool,
) -> eyre::Result<()> {
    Box::pin(async move {
        let (fixture, inputs) = fixture(database).await?;
        // Start with an existing row but no IPv4 loopback, so preparation
        // captures this lifetime before leaving the database for provider I/O.
        let mut txn = fixture.env.pool.begin().await?;
        db::machine::create(&mut txn, Some(&fixture.env.common_pools), &inputs.machine_id,
            ManagedHostState::Created, None, CURRENT_STATE_MODEL_VERSION).await?;
        let original_identity = db::machine::MachineCreationIdentity::capture(txn.as_mut(), &inputs.machine_id).await?;
        txn.commit().await?;
        fixture.provider.allocate_pause.pause_next.store(true, Ordering::SeqCst);
        let api = fixture.env.api.clone();
        let mut discovery = api.discover_machine(Request::new(inputs.request.clone()));
        tokio::select! {
            result = &mut discovery => panic!("discovery finished before provider gate: {result:?}"),
            result = wait_for_preparation(&fixture.provider.allocate_pause) => result?,
        }
        let pending = receipt(&fixture, inputs.machine_id).await?;
        assert!(fixture.env.api.admin_force_delete_machine(Request::new(forge::AdminForceDeleteMachineRequest {
            host_query: inputs.machine_id.to_string(), ..Default::default()
        })).await?.into_inner().all_done);
        assert_eq!(receipt(&fixture, inputs.machine_id).await?.state, AllocationState::CancelRequested);
        fixture.provider.allocate_pause.resume.notify_one();
        tokio::time::timeout(Duration::from_secs(10), &mut discovery).await?
            .expect_err("deleted DPU cannot attach or resurrect");
        assert!(db::machine::find_one(&fixture.env.pool, &inputs.machine_id, MachineSearchConfig::default()).await?.is_none());
        fixture.remote(LOOPBACK_IP).reconcile(pending.allocation_id, &fixture.cancel).await?;
        assert_eq!(receipt(&fixture, inputs.machine_id).await?.state, AllocationState::Cancelled);

        // Reingest after cancellation, then replay the terminal provider record.
        fixture.env.api.discover_machine(Request::new(inputs.request.clone())).await?;
        let replacement = receipt(&fixture, inputs.machine_id).await?;
        assert_ne!(replacement.allocation_id, pending.allocation_id);
        assert_eq!(replacement.state, AllocationState::Attached);
        let replacement_config = db::machine::get_network_config(&fixture.env.pool, &inputs.machine_id).await?;
        assert!(replacement_config.value.loopback_ip.is_some());
        assert_ne!(db::machine::MachineCreationIdentity::capture(&fixture.env.pool, &inputs.machine_id).await?.created(), original_identity.created());
        fixture.remote(LOOPBACK_IP).reconcile(pending.allocation_id, &fixture.cancel).await?;
        assert_eq!(receipt(&fixture, inputs.machine_id).await?, replacement);
        assert_eq!(db::machine::get_network_config(&fixture.env.pool, &inputs.machine_id).await?.take(), replacement_config.take());
        assert_eq!(fixture.provider.local.provisioned_count(), 1);
        drop(discovery);
        fixture.stop().await
    }).await
}

/// Host-link ownership can change while discovery waits for its provider.
/// Reattachment must take segment locks before writing any machine or interface.
#[crate::sqlx_test]
async fn remote_dpu_discovery_rechecks_host_link_after_preparation(
    database: PgPool,
) -> eyre::Result<()> {
    Box::pin(async move {
        let (fixture, inputs) = fixture(database).await?;
        assert!(!fixture.env.config.site_explorer.create_machines.load(Ordering::Relaxed));
        let Some(rpc::DiscoveryData::Info(info)) = &inputs.request.discovery_data else {
            panic!("DPU hardware report");
        };
        let hardware_info = HardwareInfo::try_from(info.clone())?;
        let host_mac = hardware_info.factory_mac_address()?;
        let host_id = host_id_from_dpu_hardware_info(&hardware_info)?;

        // An already-owned host NIC initially permits discovery to skip segment
        // locks. No DPU exists yet, so preparation still calls the provider.
        let host_interface_id = dpu_discover_dhcp(&fixture.env, &host_mac.to_string()).await;
        let mut txn = fixture.env.pool.begin().await?;
        let host_interface = db::machine_interface::find_one(&mut *txn, host_interface_id).await?;
        db::machine::get_or_create(
            &mut txn, Some(&fixture.env.common_pools), &host_id, &host_interface,
        ).await?;
        txn.commit().await?;
        assert!(db::machine_interface::mac_address_has_machine(&fixture.env.pool, host_mac).await?);
        fixture.provider.allocate_pause.pause_next.store(true, Ordering::SeqCst);
        let api = fixture.env.api.clone();
        let mut discovery = api.discover_machine(Request::new(inputs.request.clone()));
        tokio::select! {
            result = &mut discovery => panic!("discovery finished before provider gate: {result:?}"),
            result = wait_for_preparation(&fixture.provider.allocate_pause) => result?,
        }
        let pending = receipt(&fixture, inputs.machine_id).await?;

        // Real force deletion uses admission and clears the host association
        // while discovery has released its permit for provider preparation.
        assert!(fixture.env.api.admin_force_delete_machine(Request::new(forge::AdminForceDeleteMachineRequest {
            host_query: host_id.to_string(), ..Default::default()
        })).await?.into_inner().all_done);
        let detached = db::machine_interface::find_one(&fixture.env.pool, host_interface_id).await?;
        assert!(detached.machine_id.is_none());

        // Hold segment locks after that committed change. Discovery must wait
        // at this gate before writing any machine or interface rows.
        let mut gate = fixture.env.pool.begin().await?;
        db::machine_interface::lock_all_admin_segments(&mut gate).await?;
        let gate_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()").fetch_one(&mut *gate).await?;
        fixture.provider.allocate_pause.resume.notify_one();
        let discovery_pid = tokio::select! {
            result = &mut discovery => panic!("discovery bypassed the segment gate: {result:?}"),
            result = wait_for_cleanup_waiter(&fixture.env.pool, gate_pid, None) => result?,
        };
        let mode: String = sqlx::query_scalar(
            "SELECT mode FROM pg_locks WHERE pid = $1 AND locktype = 'advisory' AND NOT granted",
        ).bind(discovery_pid).fetch_one(&fixture.env.pool).await?;
        assert_eq!(mode, "ExclusiveLock");
        let has_written_nothing: bool = sqlx::query_scalar(
            "SELECT backend_xid IS NULL FROM pg_stat_activity WHERE pid = $1",
        ).bind(discovery_pid).fetch_one(&fixture.env.pool).await?;
        assert!(has_written_nothing, "segment admission must precede row writes");
        assert!(db::machine::find_one(&fixture.env.pool, &inputs.machine_id, MachineSearchConfig::default()).await?.is_none());
        assert_eq!(receipt(&fixture, inputs.machine_id).await?.state, AllocationState::Pending);

        // Once the writer finishes, the same reservation attaches and the host
        // link is restored. Reload both records rather than relying on RPC success.
        gate.commit().await?;
        let result = tokio::time::timeout(Duration::from_secs(10), &mut discovery).await??;
        assert_eq!(result.into_inner().machine_id, Some(inputs.machine_id));
        let attached = receipt(&fixture, inputs.machine_id).await?;
        assert_eq!(attached.allocation_id, pending.allocation_id);
        assert_eq!(attached.state, AllocationState::Attached);
        let host_link = db::machine_interface::find_one(&fixture.env.pool, host_interface_id).await?;
        assert_eq!(host_link.machine_id, Some(host_id));
        assert_eq!(host_link.attached_dpu_machine_id.map(MachineId::from), Some(inputs.machine_id));
        assert_eq!(fixture.provider.allocations.load(Ordering::SeqCst), 1);
        drop(discovery);
        fixture.stop().await
    }).await
}

/// A force-delete RPC that captured the old lifetime must leave a replacement
/// with the same hardware ID and its newer provider assignment unchanged.
#[crate::sqlx_test]
async fn remote_dpu_stale_force_delete_preserves_same_id_replacement(
    database: PgPool,
) -> eyre::Result<()> {
    Box::pin(async move {
        let (fixture, inputs) = fixture(database).await?;
        fixture.env.api.discover_machine(Request::new(inputs.request.clone())).await?;
        let original = receipt(&fixture, inputs.machine_id).await?;
        let original_created = db::machine::MachineCreationIdentity::capture(&fixture.env.pool, &inputs.machine_id).await?.created();
        // Stop cleanup after its RPC has captured and admitted the old lifetime.
        // Replacement and stale cleanup then queue behind the same segment gate.
        let mut gate = fixture.env.pool.begin().await?;
        db::machine_interface::lock_all_admin_segments(&mut gate).await?;
        let gate_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()").fetch_one(&mut *gate).await?;
        let api = fixture.env.api.clone();
        let mut first = Box::pin(api.admin_force_delete_machine(Request::new(forge::AdminForceDeleteMachineRequest {
            host_query: inputs.machine_id.to_string(), ..Default::default()
        })));
        let first_pid = tokio::select! {
            result = &mut first => panic!("first cleanup passed the gate: {result:?}"),
            pid = wait_for_cleanup_waiter(&fixture.env.pool, gate_pid, None) => pid?,
        };

        // Queue a fixture replacement between two real cleanup RPCs. Its gate
        // transaction deliberately holds the segment lock while preparing on a
        // separate connection; replacement uses the shared consumer API rather
        // than another public discovery RPC that would need this same lock.
        let mut replacement_txn = fixture.env.pool.begin().await?;
        let replacement_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *replacement_txn).await?;
        let mut replacement = Box::pin(async {
            db::machine_interface::lock_all_admin_segments(&mut replacement_txn).await?;
            assert!(db::machine::find_one(&fixture.env.pool, &inputs.machine_id, MachineSearchConfig::default()).await?.is_none());
            assert_eq!(receipt(&fixture, inputs.machine_id).await?.state, AllocationState::ReleaseRequested);
            fixture.remote(LOOPBACK_IP).reconcile(original.allocation_id, &fixture.cancel).await?;
            let prepared = PreparedDpuLoopback::prepare(
                &fixture.env.pool, &fixture.env.api.resource_pool_registry,
                &fixture.env.common_pools, inputs.machine_id, &fixture.cancel,
            ).await?;
            assert!(prepared.has_provider_reservation(), "a new provider assignment retains bounded admission");
            prepared.check_identity(&mut replacement_txn).await?;
            db::machine::create(&mut replacement_txn, Some(&fixture.env.common_pools), &inputs.machine_id,
                ManagedHostState::Created, None, CURRENT_STATE_MODEL_VERSION).await?;
            prepared.attach(&mut replacement_txn, true).await?;
            replacement_txn.commit().await?;
            let created = db::machine::MachineCreationIdentity::capture(&fixture.env.pool, &inputs.machine_id).await?.created();
            let config = db::machine::get_network_config(&fixture.env.pool, &inputs.machine_id).await?;
            let receipt = receipt(&fixture, inputs.machine_id).await?;
            let machine: serde_json::Value = sqlx::query_scalar("SELECT to_jsonb(m) FROM machines m WHERE id = $1")
                .bind(inputs.machine_id).fetch_one(&fixture.env.pool).await?;
            eyre::Ok((created, config, receipt, machine))
        });
        tokio::select! {
            result = &mut first => panic!("first cleanup passed the gate: {result:?}"),
            result = &mut replacement => panic!("replacement passed first cleanup: {result:?}"),
            result = wait_for_cleanup_waiter(&fixture.env.pool, first_pid, Some(replacement_pid)) => { result?; },
        }
        let mut stale = Box::pin(api.admin_force_delete_machine(Request::new(forge::AdminForceDeleteMachineRequest {
            host_query: inputs.machine_id.to_string(), ..Default::default()
        })));
        tokio::select! {
            result = &mut first => panic!("first cleanup passed the gate: {result:?}"),
            result = &mut replacement => panic!("replacement passed first cleanup: {result:?}"),
            result = &mut stale => panic!("stale cleanup passed replacement: {result:?}"),
            result = wait_for_cleanup_waiter(&fixture.env.pool, replacement_pid, None) => { result?; },
        }
        gate.commit().await?;
        // Leave the stale future unpolled until the committed replacement and
        // its provider handle have been observed. Its SQL lock may be granted,
        // but the cleanup code cannot continue until we resume the RPC below.
        let (first_result, replacement_snapshot) = tokio::time::timeout(Duration::from_secs(30), async {
            tokio::join!(&mut first, &mut replacement)
        }).await?;
        assert!(first_result?.into_inner().all_done);
        let (created, config, attached, machine) = replacement_snapshot?;
        assert_ne!(created, original_created);
        assert_eq!(attached.state, AllocationState::Attached);
        assert_ne!(attached.allocation_id, original.allocation_id);
        assert!(config.value.loopback_ip.is_some());
        let AllocationSource::Remote { result: Some(AllocationResult::Allocated(assignment)), .. } = &attached.source else {
            panic!("replacement must have a saved provider assignment");
        };
        assert!(fixture.provider.local.is_provisioned(&assignment.allocation_id));
        assert!(tokio::time::timeout(Duration::from_secs(10), &mut stale).await??.into_inner().all_done);
        assert_eq!(db::machine::MachineCreationIdentity::capture(&fixture.env.pool, &inputs.machine_id).await?.created(), created);
        assert_eq!(db::machine::get_network_config(&fixture.env.pool, &inputs.machine_id).await?.take(), config.take());
        assert_eq!(receipt(&fixture, inputs.machine_id).await?, attached);
        let current: serde_json::Value = sqlx::query_scalar("SELECT to_jsonb(m) FROM machines m WHERE id = $1")
            .bind(inputs.machine_id).fetch_one(&fixture.env.pool).await?;
        assert_eq!(current, machine);
        assert_eq!(fixture.provider.local.provisioned_count(), 1);
        assert!(fixture.provider.local.is_provisioned(&assignment.allocation_id));
        assert_eq!(fixture.provider.allocations.load(Ordering::SeqCst), 2);
        assert_eq!(fixture.provider.releases.load(Ordering::SeqCst), 1);
        drop(stale);
        drop(replacement);
        drop(first);
        fixture.stop().await
    }).await
}

/// Sibling preparation drains mixed results before cancelling successful
/// reservations; a retry still prepares all three siblings together.
#[crate::sqlx_test]
async fn remote_dpu_siblings_prepare_together(database: PgPool) -> eyre::Result<()> {
    Box::pin(async move {
        let (fixture, mut inputs) = fixture_with_dpu_count(database, 3).await?;
        // All three requests must start before one is rejected. This exposes
        // cleanup of the other two successes without allowing a partial host.
        fixture.provider.reject_allocate_call.store(2, Ordering::SeqCst);
        let mixed_barrier = Arc::new(Barrier::new(4));
        *fixture.provider.allocate_barrier.lock().expect("Allocate barrier") = Some(mixed_barrier.clone());
        let creator = creator(&fixture);
        {
        let mixed = creator.create_managed_host(
            &inputs.explored, &mut inputs.host_report, Some(&inputs.expected), &fixture.env.pool,
        );
        tokio::pin!(mixed);
        tokio::select! {
            result = &mut mixed => panic!("creation bypassed the sibling barrier: {result:?}"),
            result = tokio::time::timeout(Duration::from_secs(10), mixed_barrier.wait()) => { result?; },
        }
        tokio::time::timeout(Duration::from_secs(10), &mut mixed).await?
            .expect_err("one sibling provider request was rejected");
        assert_eq!(fixture.provider.allocations.load(Ordering::SeqCst), 3);
        assert_eq!(fixture.provider.local.provisioned_count(), 2);
        assert_eq!(sqlx::query_scalar::<_, i64>("SELECT count(*) FROM machines")
            .fetch_one(&fixture.env.pool).await?, 0, "no predicted host may commit either");
        let mut saved_successes = 0;
        for dpu in &inputs.explored.dpus {
            let machine_id = dpu.report.machine_id.expect("DPU report identity");
            assert!(db::machine::find_one(&fixture.env.pool, &machine_id, MachineSearchConfig::default()).await?.is_none());
            let cancelled = receipt(&fixture, machine_id).await?;
            assert_eq!(cancelled.state, AllocationState::CancelRequested);
            if matches!(cancelled.source, AllocationSource::Remote { result: Some(AllocationResult::Allocated(_)), .. }) {
                saved_successes += 1;
            }
            fixture.remote(LOOPBACK_IP).reconcile(cancelled.allocation_id, &fixture.cancel).await?;
            assert_eq!(receipt(&fixture, machine_id).await?.state, AllocationState::Cancelled);
        }
        assert_eq!(saved_successes, 2);
        assert_eq!(fixture.provider.local.provisioned_count(), 0);
        }
        // After every original request is cleaned, the same reports can retry.
        // A fresh barrier also rejects a regression to serial sibling work.
        fixture.provider.reject_allocate_call.store(0, Ordering::SeqCst);
        let barrier = Arc::new(Barrier::new(4));
        *fixture
            .provider
            .allocate_barrier
            .lock()
            .expect("Allocate barrier") = Some(barrier.clone());
        let pool = fixture.env.pool.clone();
        let creation = creator.create_managed_host(
            &inputs.explored,
            &mut inputs.host_report,
            Some(&inputs.expected),
            &pool,
        );
        tokio::pin!(creation);
        tokio::select! {
            result = &mut creation => panic!("creation bypassed the three-DPU barrier: {result:?}"),
            result = tokio::time::timeout(Duration::from_secs(10), barrier.wait()) => { result?; },
        }
        assert_eq!(fixture.provider.allocations.load(Ordering::SeqCst), 6);
        assert!(tokio::time::timeout(Duration::from_secs(10), &mut creation).await??);
        let mut addresses = HashSet::new();
        for dpu in &inputs.explored.dpus {
            let machine_id = dpu.report.machine_id.expect("DPU report identity");
            let config = db::machine::get_network_config(&fixture.env.pool, &machine_id).await?;
            addresses.insert(config.value.loopback_ip.expect("attached sibling loopback"));
            assert_eq!(
                receipt(&fixture, machine_id).await?.state,
                AllocationState::Attached
            );
        }
        assert_eq!(addresses.len(), 3);
        assert_eq!(fixture.provider.local.provisioned_count(), 3);
        fixture.stop().await
    })
    .await
}

/// The initial lookup stops on cancellation or its own deadline while waiting
/// for a connection, before it can reserve a value or save a request.
#[crate::sqlx_test]
async fn remote_dpu_lookup_cancellation_stops_pool_wait(database: PgPool) -> eyre::Result<()> {
    Box::pin(async move {
        let (fixture, inputs) = fixture(database).await?;
        let single = PgPoolOptions::new()
            .max_connections(1)
            .connect_with(fixture.env.pool.connect_options().as_ref().clone())
            .await?;
        for deadline in [false, true] {
            // Exhaust only this pool, so both cases stop before any provider work.
            let held = single.acquire().await?;
            let cancel = CancellationToken::new();
            let mut preparation = Box::pin(PreparedDpuLoopback::prepare(
                &single,
                &fixture.env.api.resource_pool_registry,
                &fixture.env.common_pools,
                inputs.machine_id,
                &cancel,
            ));
            assert!(
                poll_fn(|cx| Poll::Ready(preparation.as_mut().poll(cx)))
                    .await
                    .is_pending()
            );
            if deadline {
                // Arm the real five-second lookup timer before advancing Tokio.
                // Resume wall time before any database or provider I/O below.
                tokio::time::pause();
                tokio::time::advance(Duration::from_secs(6)).await;
                tokio::time::resume();
            } else {
                cancel.cancel();
            }
            let error = tokio::time::timeout(Duration::from_secs(1), preparation)
                .await?
                .err()
                .expect("lookup must stop while its only connection remains held");
            match (deadline, error) {
                (
                    false,
                    ResourcePoolBackendError::Cancelled {
                        operation: "DPU loopback lookup",
                    },
                )
                | (
                    true,
                    ResourcePoolBackendError::DeadlineExceeded {
                        operation: "DPU loopback lookup",
                    },
                ) => {}
                (_, error) => panic!("unexpected lookup failure (deadline={deadline}): {error:?}"),
            }
            assert_eq!(fixture.provider.allocations.load(Ordering::SeqCst), 0);
            assert!(
                remote::latest_for_owner_slot(
                    &fixture.env.pool,
                    OwnerType::Machine,
                    &inputs.machine_id.to_string(),
                    DPU_IPV4_LOOPBACK_SLOT,
                )
                .await?
                .is_none()
            );
            drop(held);
            assert_eq!(
                sqlx::query_scalar::<_, i32>("SELECT 1")
                    .fetch_one(&single)
                    .await?,
                1
            );
        }
        single.close().await;
        fixture.stop().await
    })
    .await
}

/// A completed provider reservation must be cancelled if Core admission times
/// out or the owning Site Explorer stops while waiting for admission.
#[crate::sqlx_test]
#[ignore = "run alone: drains process-wide admin admission"]
async fn remote_dpu_post_preparation_admission_stops_retain_cleanup(
    database: PgPool,
) -> eyre::Result<()> {
    Box::pin(async move {
        let (fixture, inputs) = fixture(database).await?;
        // One fixture and owner are reused only after the preceding request's
        // cancellation completes. Each phase therefore has its own generation.
        let mut previous_request = None;
        for case in [AdmissionCase::DiscoveryDeadline, AdmissionCase::ExplorerCancellation] {
            fixture.provider.allocate_pause.pause_next.store(true, Ordering::SeqCst);
            let cancel = CancellationToken::new();
            let mut tasks = JoinSet::new();
            if matches!(case, AdmissionCase::ExplorerCancellation) {
                owning_site_explorer(&fixture, &inputs).await?
                    .start(&mut tasks, cancel.clone())?;
            }
            let api = fixture.env.api.clone();
            let mut caller = Box::pin(async {
                match case {
                    AdmissionCase::DiscoveryDeadline => {
                        let error = api.discover_machine(Request::new(inputs.request.clone())).await
                            .expect_err("provider-backed discovery admission must expire");
                        assert_eq!(error.code(), tonic::Code::DeadlineExceeded);
                        assert_eq!(error.message(), "DPU admin lock admission timed out");
                        assert_eq!(error.metadata().get("nico-error-code").and_then(|value| value.to_str().ok()), Some("NICO-API-504"));
                    }
                    AdmissionCase::ExplorerCancellation => {
                        while let Some(result) = tasks.join_next().await {
                            result?;
                        }
                    }
                }
                eyre::Ok(())
            });
            tokio::select! {
                result = &mut caller => panic!("caller stopped before provider reservation: {result:?}"),
                result = wait_for_preparation(&fixture.provider.allocate_pause) => result?,
            }
            let pending = receipt(&fixture, inputs.machine_id).await?;
            assert_ne!(Some(pending.allocation_id), previous_request);
            previous_request = Some(pending.allocation_id);
            assert_eq!(pending.state, AllocationState::Pending);
            assert_eq!(fixture.provider.local.provisioned_count(), 1);

            // Core has passed its initial authorization/admission by this gate.
            // Draining the process-wide semaphore is safe only in this ignored
            // test's dedicated process; ordinary parallel tests must not do it.
            let permits = tokio::time::timeout(Duration::from_secs(2), async {
                let mut permits = Vec::new();
                for _ in 0..db::machine_interface::admin_lock_admission_permits() {
                    permits.push(db::machine_interface::admin_lock_admission().await);
                }
                permits
            }).await?;
            fixture.provider.allocate_pause.resume.notify_one();

            // A saved result alone can precede the provider future's return.
            // Its released operation lease proves preparation has finished;
            // the caller can now be stopped specifically at admin admission.
            let prepared = tokio::select! {
                result = &mut caller => panic!("caller bypassed exhausted admission: {result:?}"),
                result = tokio::time::timeout(Duration::from_secs(2), async {
                    loop {
                        let saved = receipt(&fixture, inputs.machine_id).await?;
                        let leased: bool = sqlx::query_scalar(
                            "SELECT EXISTS(SELECT 1 FROM work_locks WHERE work_key = $1)",
                        ).bind(format!("resource-pool:{}", pending.allocation_id))
                            .fetch_one(&fixture.env.pool).await?;
                        if matches!(&saved.source, AllocationSource::Remote {
                            result: Some(AllocationResult::Allocated(_)), ..
                        }) && !leased {
                            break eyre::Ok(saved);
                        }
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                }) => result??,
            };
            assert_eq!(prepared.allocation_id, pending.allocation_id);
            assert_eq!(prepared.state, AllocationState::Pending);
            assert!(poll_fn(|cx| Poll::Ready(caller.as_mut().poll(cx))).await.is_pending());
            match case {
                AdmissionCase::DiscoveryDeadline => {
                    // Only Tokio admission time moves; database cleanup and
                    // provider reconciliation below run after wall time resumes.
                    tokio::time::pause();
                    tokio::time::advance(Duration::from_secs(6)).await;
                    tokio::time::resume();
                }
                AdmissionCase::ExplorerCancellation => cancel.cancel(),
            }
            tokio::time::timeout(Duration::from_secs(5), &mut caller).await??;
            drop(caller);

            let cancelled = receipt(&fixture, inputs.machine_id).await?;
            assert_eq!(cancelled.allocation_id, prepared.allocation_id);
            assert_eq!(cancelled.state, AllocationState::CancelRequested);
            assert_eq!(cancelled.source, prepared.source);
            assert_eq!(sqlx::query_scalar::<_, i64>("SELECT count(*) FROM machines")
                .fetch_one(&fixture.env.pool).await?, 0, "no DPU or predicted host may commit");
            let AllocationSource::Remote { result: Some(AllocationResult::Allocated(assignment)), .. } = &cancelled.source else {
                panic!("admission cleanup must retain the original provider assignment");
            };
            assert!(fixture.provider.local.is_provisioned(&assignment.allocation_id));
            assert_eq!(fixture.provider.local.provisioned_count(), 1);

            // Local cancellation intent must survive until recovery confirms
            // cleanup of this exact assignment, before the next caller retries.
            drop(permits);
            fixture.remote(LOOPBACK_IP).reconcile(cancelled.allocation_id, &fixture.cancel).await?;
            let cleaned = receipt(&fixture, inputs.machine_id).await?;
            assert_eq!(cleaned.allocation_id, cancelled.allocation_id);
            assert_eq!(cleaned.state, AllocationState::Cancelled);
            assert_eq!(fixture.provider.local.provisioned_count(), 0);
        }
        assert_eq!(fixture.provider.allocations.load(Ordering::SeqCst), 2);
        fixture.stop().await
    }).await
}

/// The token supplied to the owning Site Explorer task must reach a stalled
/// provider preparation, so shutdown does not wait for its allocation deadline.
#[crate::sqlx_test]
async fn remote_dpu_site_explorer_shutdown_cancels_preparation(
    database: PgPool,
) -> eyre::Result<()> {
    Box::pin(async move {
        let (fixture, inputs) = fixture(database).await?;
        let site_explorer = owning_site_explorer(&fixture, &inputs).await?;
        fixture
            .provider
            .allocate_pause
            .pause_next
            .store(true, Ordering::SeqCst);
        let cancel = CancellationToken::new();
        let mut tasks = JoinSet::new();
        site_explorer.start(&mut tasks, cancel.clone())?;
        wait_for_preparation(&fixture.provider.allocate_pause).await?;
        assert_eq!(fixture.provider.local.provisioned_count(), 1);
        // Cancel the owning start token while the provider still holds its
        // reply. Completion must save cleanup without creating either machine.
        cancel.cancel();
        tokio::time::timeout(Duration::from_secs(5), async {
            while let Some(result) = tasks.join_next().await {
                result?;
            }
            eyre::Ok(())
        })
        .await??;
        let cancelled = receipt(&fixture, inputs.machine_id).await?;
        assert_eq!(cancelled.state, AllocationState::CancelRequested);
        assert!(
            db::machine::find_one(
                &fixture.env.pool,
                &inputs.machine_id,
                MachineSearchConfig::default()
            )
            .await?
            .is_none()
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM machines")
                .fetch_one(&fixture.env.pool)
                .await?,
            0
        );
        fixture.provider.allocate_pause.resume.notify_one();
        fixture
            .remote(LOOPBACK_IP)
            .reconcile(cancelled.allocation_id, &fixture.cancel)
            .await?;
        assert_eq!(
            receipt(&fixture, inputs.machine_id).await?.state,
            AllocationState::Cancelled
        );
        assert_eq!(fixture.provider.local.provisioned_count(), 0);
        fixture.stop().await
    })
    .await
}

/// A request that cannot create its DPU must fail authorization before it
/// reserves remotely or writes a durable allocation request.
#[crate::sqlx_test]
async fn remote_dpu_rejected_discovery_has_no_provider_effects(
    database: PgPool,
) -> eyre::Result<()> {
    Box::pin(async move {
        let (fixture, mut inputs) = fixture(database).await?;
        let interface_id = inputs
            .request
            .machine_interface_id
            .expect("DPU OOB interface");
        let before: serde_json::Value =
            sqlx::query_scalar("SELECT to_jsonb(i) FROM machine_interfaces i WHERE id = $1")
                .bind(interface_id)
                .fetch_one(&fixture.env.pool)
                .await?;
        // Remove only creation authorization. The real remote backend remains
        // selected, so premature preparation would leave observable effects.
        inputs.request.create_machine = false;
        let error = fixture
            .env
            .api
            .discover_machine(Request::new(inputs.request))
            .await
            .expect_err("an unassociated interface cannot discover without creation authorization");
        assert_eq!(error.code(), tonic::Code::PermissionDenied);
        assert_eq!(fixture.provider.allocations.load(Ordering::SeqCst), 0);
        assert_eq!(fixture.provider.local.provisioned_count(), 0);
        assert!(
            remote::latest_for_owner_slot(
                &fixture.env.pool,
                OwnerType::Machine,
                &inputs.machine_id.to_string(),
                DPU_IPV4_LOOPBACK_SLOT
            )
            .await?
            .is_none()
        );
        assert!(
            db::machine::find_one(
                &fixture.env.pool,
                &inputs.machine_id,
                MachineSearchConfig::default()
            )
            .await?
            .is_none()
        );
        let after: serde_json::Value =
            sqlx::query_scalar("SELECT to_jsonb(i) FROM machine_interfaces i WHERE id = $1")
                .bind(interface_id)
                .fetch_one(&fixture.env.pool)
                .await?;
        assert_eq!(after, before);
        fixture.stop().await
    })
    .await
}

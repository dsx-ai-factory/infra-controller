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

//! Admin boot-interface routing: `machine_setup` and
//! `set_dpu_first_boot_order` persist an exact desired target for a managed
//! host and leave Redfish work to the machine controller. DPU and unowned BMC
//! endpoints keep the direct Redfish behavior used during discovery.
//!
//! These tests keep the routing boundary visible: managed requests change the
//! desired generation without touching Redfish, while direct requests still
//! appear in the Redfish simulator.

use carbide_redfish::libredfish::test_support::RedfishSimAction;
use carbide_uuid::machine::{HostMachineId, StableHostMachineId};
use chrono::{DateTime, Utc};
use ipnetwork::IpNetwork;
use model::machine::{InstanceState, ManagedHostState};
use model::machine_boot_interface::{
    BootInterfaceSelectionSource, MachineBootInterface, MachineBootInterfaceTarget,
};
use model::network_segment::NetworkSegmentType;
use model::test_support::ManagedHostConfig;
use rpc::forge;
use rpc::forge::forge_server::Forge;

use crate::handlers::bmc_endpoint_explorer::summarize_boot_interface_candidates_for_test;
use crate::test_support::fixture_config::{FixtureDefault as _, ManagedHostConfigExt as _};
use crate::tests::common::api_fixtures;
use crate::tests::common::api_fixtures::network_segment::{
    FIXTURE_ADMIN_NETWORK_SEGMENT_GATEWAY, FIXTURE_HOST_INBAND_NETWORK_SEGMENT_GATEWAY,
    FIXTURE_UNDERLAY_NETWORK_SEGMENT_GATEWAY, create_admin_network_segment,
    create_host_inband_network_segment, create_underlay_network_segment,
};

/// Creates a two-DPU host and moves its primary to a different host interface
/// via `set_primary_interface`, returning the host id and the promoted
/// interface targets before and after the move.
async fn host_with_moved_primary(
    env: &api_fixtures::TestEnv,
) -> Result<
    (
        StableHostMachineId,
        MachineBootInterfaceTarget,
        MachineBootInterfaceTarget,
    ),
    Box<dyn std::error::Error>,
> {
    let host =
        api_fixtures::site_explorer::new_host(env, ManagedHostConfig::default().with_dpu_count(2))
            .await?;
    let host_id: StableHostMachineId = host.host_snapshot.id.try_into()?;

    let (original_target, promote_id, promote_target) = {
        let mut txn = env.pool.begin().await?;
        let interfaces = db::machine_interface::find_by_machine_ids(txn.as_mut(), &[host_id])
            .await?
            .remove(&host_id)
            .expect("host should have interface rows");
        let original = interfaces
            .iter()
            .find(|i| i.primary_interface)
            .expect("host should start with a primary interface");
        let promote = interfaces
            .iter()
            .find(|i| !i.primary_interface && i.attached_dpu_machine_id.is_some())
            .expect("host should have a non-primary host interface to promote");
        let target = |interface: &model::machine::MachineInterfaceSnapshot| {
            MachineBootInterfaceTarget::from_parts(
                Some(interface.mac_address),
                interface.boot_interface_id.clone(),
            )
            .expect("a host interface always supplies a MAC")
        };
        (target(original), promote.id, target(promote))
    };

    env.api
        .set_primary_interface(tonic::Request::new(forge::SetPrimaryInterfaceRequest {
            host_machine_id: Some(host_id),
            interface_id: Some(promote_id),
            force_reconcile: false,
            ..Default::default()
        }))
        .await?;

    Ok((host_id, original_target, promote_target))
}

/// Clears any pending machine-controller wakeup for a test host.
async fn clear_controller_queue(
    pool: &sqlx::PgPool,
    machine_id: &HostMachineId,
) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM machine_state_controller_queued_objects WHERE object_id = $1")
        .bind(machine_id.to_string())
        .execute(pool)
        .await?;
    Ok(())
}

/// Returns the number of pending machine-controller wakeups for a test host.
async fn controller_queue_count(
    pool: &sqlx::PgPool,
    machine_id: &HostMachineId,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT count(*) FROM machine_state_controller_queued_objects WHERE object_id = $1",
    )
    .bind(machine_id.to_string())
    .fetch_one(pool)
    .await
}

// A managed `set_dpu_first_boot_order` request records the exact selected pair
// and wakes the controller. Redfish is deliberately untouched in the request
// path, including when the operator explicitly reapplies the current target.
#[crate::sqlx_test]
async fn test_set_dpu_first_persists_managed_host_intent_without_redfish(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = api_fixtures::create_test_env(pool).await;
    let (host_id, original_target, promote_target) = host_with_moved_primary(&env).await?;
    let before = db::machine_desired_boot_interface::get(&env.pool, &host_id)
        .await?
        .expect("moving the primary should persist its target");
    assert_eq!(before.value, promote_target);
    clear_controller_queue(&env.pool, &host_id).await?;

    let timepoint = env.redfish_sim.timepoint();
    env.api
        .set_dpu_first_boot_order(tonic::Request::new(forge::SetDpuFirstBootOrderRequest {
            machine_id: Some(host_id.to_string()),
            bmc_endpoint_request: None,
            boot_interface_mac: None,
        }))
        .await?;

    let actions = env.redfish_sim.actions_since(&timepoint).all_hosts();
    assert!(
        actions.is_empty(),
        "the machine controller, not the admin request, should perform Redfish work",
    );
    let queued = controller_queue_count(&env.pool, &host_id).await?;
    assert_eq!(
        queued, 1,
        "an unassigned Ready host should be enqueued after commit",
    );
    let reapplied = db::machine_desired_boot_interface::get(&env.pool, &host_id)
        .await?
        .expect("the explicit reapply should leave a desired target");
    assert_eq!(reapplied.value, promote_target);
    assert_ne!(
        reapplied.version, before.version,
        "an explicit setup request should force a fresh desired generation",
    );

    let timepoint = env.redfish_sim.timepoint();
    env.api
        .set_dpu_first_boot_order(tonic::Request::new(forge::SetDpuFirstBootOrderRequest {
            machine_id: Some(host_id.to_string()),
            bmc_endpoint_request: None,
            boot_interface_mac: Some(original_target.mac_address().to_string()),
        }))
        .await?;
    let actions = env.redfish_sim.actions_since(&timepoint).all_hosts();
    assert!(
        actions.is_empty(),
        "changing managed intent should not make request-path Redfish calls",
    );
    let desired = db::machine_desired_boot_interface::get(&env.pool, &host_id)
        .await?
        .expect("the managed request should persist its selected target");
    assert_eq!(desired.value, original_target);
    assert_ne!(desired.version, reapplied.version);

    Ok(())
}

// Reapplying an automatically selected target is not itself a selection. Only
// an entered MAC establishes explicit operator authority.
#[crate::sqlx_test]
async fn test_managed_reapply_preserves_selection_until_operator_enters_a_mac(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = api_fixtures::create_test_env(pool).await;
    let host =
        api_fixtures::site_explorer::new_host(&env, ManagedHostConfig::default().with_dpu_count(2))
            .await?;
    let host_id = host.host_snapshot.id;
    let desired = db::machine_desired_boot_interface::get(&env.pool, &host_id)
        .await?
        .expect("ingestion should select a desired target");
    let expected_pair = match &desired.value {
        MachineBootInterfaceTarget::Pair(pair) => pair.clone(),
        MachineBootInterfaceTarget::MacOnly(_) => {
            panic!("managed fixture should resolve a complete Redfish boot-interface pair")
        }
    };
    let sentinel =
        DateTime::from_timestamp(1_700_000_000, 123_000_000).expect("fixture selection timestamp");
    sqlx::query(
        "UPDATE machine_boot_interfaces
         SET desired_interface_id = NULL,
             selection_updated_at = $1
         WHERE machine_id = $2",
    )
    .bind(sentinel)
    .bind(host_id)
    .execute(&env.pool)
    .await?;
    let selection = || async {
        sqlx::query_as::<_, (BootInterfaceSelectionSource, Option<DateTime<Utc>>)>(
            "SELECT selection_source, selection_updated_at
             FROM machine_boot_interfaces
             WHERE machine_id = $1",
        )
        .bind(host_id)
        .fetch_one(&env.pool)
        .await
    };
    let automatic_selection = selection().await?;
    assert_ne!(
        automatic_selection.0,
        BootInterfaceSelectionSource::Operator
    );
    assert_eq!(automatic_selection.1, Some(sentinel));

    env.api
        .set_dpu_first_boot_order(tonic::Request::new(forge::SetDpuFirstBootOrderRequest {
            machine_id: Some(host_id.to_string()),
            bmc_endpoint_request: None,
            boot_interface_mac: None,
        }))
        .await?;
    assert_eq!(
        selection().await?,
        automatic_selection,
        "reapplying the resolved target must preserve why it was selected",
    );
    assert_eq!(
        db::machine_desired_boot_interface::get(&env.pool, &host_id)
            .await?
            .expect("reapply should retain a target")
            .value,
        MachineBootInterfaceTarget::Pair(expected_pair),
        "reapply should restore the resolved Redfish id without reselecting the interface",
    );

    env.api
        .set_dpu_first_boot_order(tonic::Request::new(forge::SetDpuFirstBootOrderRequest {
            machine_id: Some(host_id.to_string()),
            bmc_endpoint_request: None,
            boot_interface_mac: Some(desired.value.mac_address().to_string()),
        }))
        .await?;
    let operator_selection = selection().await?;
    assert_eq!(operator_selection.0, BootInterfaceSelectionSource::Operator);
    assert_ne!(
        operator_selection.1, automatic_selection.1,
        "an entered MAC must timestamp the operator authority change",
    );

    Ok(())
}

// A managed reapply refreshes a changed vendor native Redfish ID for the same
// physical interface. Learning that metadata does not reselect the interface,
// so the recorded selection source and decision time remain unchanged.
#[crate::sqlx_test]
async fn test_managed_reapply_refreshes_same_mac_redfish_id_without_reselecting(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = api_fixtures::create_test_env(pool).await;
    let host =
        api_fixtures::site_explorer::new_host(&env, ManagedHostConfig::default().with_dpu_count(2))
            .await?;
    let host_id = host.host_snapshot.id;
    let before = db::machine_desired_boot_interface::get(&env.pool, &host_id)
        .await?
        .expect("ingestion should select a desired target");
    let before_pair = match &before.value {
        MachineBootInterfaceTarget::Pair(pair) => pair,
        MachineBootInterfaceTarget::MacOnly(_) => {
            panic!("managed fixture should resolve a complete Redfish boot-interface pair")
        }
    };
    let selection_before =
        sqlx::query_as::<_, (BootInterfaceSelectionSource, Option<DateTime<Utc>>)>(
            "SELECT selection_source, selection_updated_at
         FROM machine_boot_interfaces
         WHERE machine_id = $1",
        )
        .bind(host_id)
        .fetch_one(&env.pool)
        .await?;
    let refreshed_interface_id = format!("{}.refreshed", before_pair.interface_id);
    let mut txn = env.pool.begin().await?;
    db::machine_interface::set_boot_interface_id(
        before_pair.mac_address,
        &refreshed_interface_id,
        txn.as_mut(),
    )
    .await?;
    txn.commit().await?;

    env.api
        .set_dpu_first_boot_order(tonic::Request::new(forge::SetDpuFirstBootOrderRequest {
            machine_id: Some(host_id.to_string()),
            bmc_endpoint_request: None,
            boot_interface_mac: None,
        }))
        .await?;

    let after = db::machine_desired_boot_interface::get(&env.pool, &host_id)
        .await?
        .expect("managed reapply should retain a desired target");
    assert_eq!(
        after.value,
        MachineBootInterfaceTarget::Pair(MachineBootInterface {
            mac_address: before_pair.mac_address,
            interface_id: refreshed_interface_id,
        }),
    );
    assert_ne!(after.version, before.version);
    let selection_after =
        sqlx::query_as::<_, (BootInterfaceSelectionSource, Option<DateTime<Utc>>)>(
            "SELECT selection_source, selection_updated_at
         FROM machine_boot_interfaces
         WHERE machine_id = $1",
        )
        .bind(host_id)
        .fetch_one(&env.pool)
        .await?;
    assert_eq!(selection_after, selection_before);

    Ok(())
}

// Assigned hosts retain operator intent but do not start boot reconciliation
// under a tenant. The new desired generation stays pending until the machine
// returns to an eligible unassigned `Ready` state.
#[crate::sqlx_test]
async fn test_set_dpu_first_does_not_enqueue_an_assigned_host(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = api_fixtures::create_test_env(pool).await;
    let (host_id, _, target) = host_with_moved_primary(&env).await?;
    let before = db::machine_desired_boot_interface::get(&env.pool, &host_id)
        .await?
        .expect("moving the primary should persist its target");

    sqlx::query("UPDATE machines SET controller_state = $1 WHERE id = $2")
        .bind(sqlx::types::Json(ManagedHostState::Assigned {
            instance_state: InstanceState::Ready,
        }))
        .bind(host_id)
        .execute(&env.pool)
        .await?;
    clear_controller_queue(&env.pool, &host_id).await?;

    let timepoint = env.redfish_sim.timepoint();
    env.api
        .set_dpu_first_boot_order(tonic::Request::new(forge::SetDpuFirstBootOrderRequest {
            machine_id: Some(host_id.to_string()),
            bmc_endpoint_request: None,
            boot_interface_mac: None,
        }))
        .await?;

    assert!(
        env.redfish_sim
            .actions_since(&timepoint)
            .all_hosts()
            .is_empty(),
        "assigned-host intent should not perform request-path Redfish",
    );
    let queued = controller_queue_count(&env.pool, &host_id).await?;
    assert_eq!(
        queued, 0,
        "an assigned host should not be enqueued for boot reconciliation",
    );
    let after = db::machine_desired_boot_interface::get(&env.pool, &host_id)
        .await?
        .expect("the assigned host should retain its desired target");
    assert_eq!(after.value, target);
    assert_ne!(
        after.version, before.version,
        "the assigned request should still record a fresh desired generation",
    );

    Ok(())
}

// `machine_setup` is also an explicit reapply. Even when resolution selects the
// target already stored by `set_primary_interface`, it creates a new desired
// generation for the controller and performs no request-path Redfish.
#[crate::sqlx_test]
async fn test_machine_setup_forces_managed_host_reconciliation_without_redfish(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = api_fixtures::create_test_env(pool).await;
    let (host_id, _, promote_target) = host_with_moved_primary(&env).await?;
    let before = db::machine_desired_boot_interface::get(&env.pool, &host_id)
        .await?
        .expect("moving the primary should persist its target");

    let timepoint = env.redfish_sim.timepoint();
    env.api
        .machine_setup(tonic::Request::new(forge::MachineSetupRequest {
            machine_id: Some(host_id.to_string()),
            bmc_endpoint_request: None,
            boot_interface_mac: None,
        }))
        .await?;

    let actions = env.redfish_sim.actions_since(&timepoint).all_hosts();
    assert!(
        actions.is_empty(),
        "the machine controller, not machine_setup, should perform Redfish work",
    );
    let desired = db::machine_desired_boot_interface::get(&env.pool, &host_id)
        .await?
        .expect("machine_setup should retain its resolved target");
    assert_eq!(desired.value, promote_target);
    assert_ne!(
        desired.version, before.version,
        "machine_setup should force a fresh desired generation",
    );

    Ok(())
}

// A zero-DPU host has no explored default, but its `HostInband` row still
// resolves an exact managed target. The request persists that target and leaves
// Redfish to the controller just like a DPU-backed host.
#[crate::sqlx_test]
async fn test_set_dpu_first_persists_a_zero_dpu_host_target_without_redfish(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    // Zero-DPU ingestion needs a HostInband segment with a routable relay
    // address; the default test env doesn't define one.
    let env = api_fixtures::create_test_env_with_overrides(
        pool,
        api_fixtures::TestEnvOverrides {
            site_prefixes: Some(vec![
                IpNetwork::new(
                    FIXTURE_ADMIN_NETWORK_SEGMENT_GATEWAY.network(),
                    FIXTURE_ADMIN_NETWORK_SEGMENT_GATEWAY.prefix(),
                )
                .unwrap(),
                IpNetwork::new(
                    FIXTURE_HOST_INBAND_NETWORK_SEGMENT_GATEWAY.network(),
                    FIXTURE_HOST_INBAND_NETWORK_SEGMENT_GATEWAY.prefix(),
                )
                .unwrap(),
                IpNetwork::new(
                    FIXTURE_UNDERLAY_NETWORK_SEGMENT_GATEWAY.network(),
                    FIXTURE_UNDERLAY_NETWORK_SEGMENT_GATEWAY.prefix(),
                )
                .unwrap(),
            ]),
            create_network_segments: Some(false),
            ..Default::default()
        },
    )
    .await;
    // HostInband segments must live in a Flat VPC.
    let flat_vpc_id = api_fixtures::network_segment::create_default_flat_vpc(
        &env.api,
        "boot-interface-resolution zero-dpu flat vpc",
    )
    .await;
    create_underlay_network_segment(&env.api).await;
    create_admin_network_segment(&env.api).await;
    create_host_inband_network_segment(&env.api, Some(flat_vpc_id)).await;
    env.run_network_segment_controller_iteration().await;
    env.run_network_segment_controller_iteration().await;

    let host = api_fixtures::site_explorer::new_host(&env, ManagedHostConfig::zero_dpu()).await?;
    let host_id: HostMachineId = host.host_snapshot.id;

    let inband_target = {
        let mut txn = env.pool.begin().await?;
        let interface = db::machine_interface::find_by_machine_ids(txn.as_mut(), &[host_id])
            .await?
            .remove(&host_id)
            .expect("zero-DPU host should have interface rows")
            .into_iter()
            .find(|i| i.network_segment_type == Some(NetworkSegmentType::HostInband))
            .expect("zero-DPU host should have a HostInband interface");
        MachineBootInterfaceTarget::from_parts(
            Some(interface.mac_address),
            interface.boot_interface_id,
        )
        .expect("the HostInband interface should resolve an exact target")
    };
    let timepoint = env.redfish_sim.timepoint();

    env.api
        .set_dpu_first_boot_order(tonic::Request::new(forge::SetDpuFirstBootOrderRequest {
            machine_id: Some(host_id.to_string()),
            bmc_endpoint_request: None,
            boot_interface_mac: None,
        }))
        .await?;

    let actions = env.redfish_sim.actions_since(&timepoint).all_hosts();
    assert!(
        actions.is_empty(),
        "managed zero-DPU hosts should also defer Redfish to the controller",
    );
    let desired = db::machine_desired_boot_interface::get(&env.pool, &host_id)
        .await?
        .expect("the zero-DPU request should persist its resolved target");
    assert_eq!(desired.value, inband_target);

    Ok(())
}

// Machine-row resolution applies to hosts (confirmed or predicted) only: a
// DPU machine's endpoint must not resolve a boot-interface target from
// interface rows -- a DPU's own setup runs without one, exactly like the
// machine-controller path.
#[crate::sqlx_test]
async fn test_boot_interface_candidates_skips_dpu_machines(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = api_fixtures::create_test_env(pool).await;

    let host =
        api_fixtures::site_explorer::new_host(&env, ManagedHostConfig::default().with_dpu_count(1))
            .await?;
    let host_id = host.host_snapshot.id;
    let dpu_id = host
        .dpu_snapshots
        .first()
        .expect("host should have a DPU snapshot")
        .id;

    let mut txn = env.pool.begin().await?;
    assert!(
        summarize_boot_interface_candidates_for_test(txn.as_mut(), Some(dpu_id.into()))
            .await?
            .is_none(),
        "a DPU machine should not resolve boot-interface rows",
    );
    assert!(
        summarize_boot_interface_candidates_for_test(txn.as_mut(), None)
            .await?
            .is_none(),
        "an unowned endpoint should not resolve boot-interface rows",
    );
    let (has_primary_interface, predicted_is_empty) =
        summarize_boot_interface_candidates_for_test(txn.as_mut(), Some(host_id.into()))
            .await?
            .expect("a host machine should resolve its boot-interface candidates");
    assert!(
        has_primary_interface,
        "the host's rows should include its primary interface",
    );
    assert!(
        predicted_is_empty,
        "a fully-leased DPU host should have no pending predictions",
    );
    txn.commit().await?;

    let timepoint = env.redfish_sim.timepoint();
    env.api
        .machine_setup(tonic::Request::new(forge::MachineSetupRequest {
            machine_id: Some(dpu_id.to_string()),
            bmc_endpoint_request: None,
            boot_interface_mac: None,
        }))
        .await?;
    let actions = env.redfish_sim.actions_since(&timepoint).all_hosts();
    let targeted = actions
        .iter()
        .find_map(|action| match action {
            RedfishSimAction::MachineSetup {
                boot_interface_mac, ..
            } => Some(boot_interface_mac.clone()),
            _ => None,
        })
        .expect("the DPU machine_setup should remain a direct Redfish action");
    assert_eq!(
        targeted, None,
        "a DPU setup should not target a host boot interface",
    );
    let persisted_rows: i64 =
        sqlx::query_scalar("SELECT count(*) FROM machine_boot_interfaces WHERE machine_id = $1")
            .bind(dpu_id)
            .fetch_one(&env.pool)
            .await?;
    assert_eq!(
        persisted_rows, 0,
        "a DPU admin action must remain a direct Redfish operation",
    );

    Ok(())
}

// An unowned endpoint is still part of Site Explorer's discovery path. With
// no actual machine owner, `machine_setup` must keep calling Redfish directly
// and must not reinterpret a caller-supplied machine id as ownership.
#[crate::sqlx_test]
async fn test_machine_setup_keeps_unowned_endpoint_redfish_direct(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = api_fixtures::create_test_env(pool).await;
    let host =
        api_fixtures::site_explorer::new_host(&env, ManagedHostConfig::default().with_dpu_count(1))
            .await?;
    let host_id = host.host_snapshot.id;
    let bmc_info = &host.host_snapshot.status.bmc_info;
    let bmc_ip = bmc_info.ip.expect("host should have a BMC IP");
    let bmc_interface_id = bmc_info
        .machine_interface_id
        .expect("host should have a BMC interface");
    let before = db::machine_desired_boot_interface::get(&env.pool, &host_id)
        .await?
        .expect("site explorer should initialize the host target");

    sqlx::query(
        "UPDATE machine_interfaces \
         SET machine_id = NULL, association_type = 'None'::association_type \
         WHERE id = $1",
    )
    .bind(bmc_interface_id)
    .execute(&env.pool)
    .await?;

    let timepoint = env.redfish_sim.timepoint();
    env.api
        .machine_setup(tonic::Request::new(forge::MachineSetupRequest {
            machine_id: Some(host_id.to_string()),
            bmc_endpoint_request: Some(forge::BmcEndpointRequest {
                ip_address: bmc_ip.to_string(),
                mac_address: None,
            }),
            boot_interface_mac: None,
        }))
        .await?;

    let actions = env.redfish_sim.actions_since(&timepoint).all_hosts();
    assert!(
        actions
            .iter()
            .any(|action| matches!(action, RedfishSimAction::MachineSetup { .. })),
        "an unowned endpoint should retain direct machine_setup behavior",
    );
    let after = db::machine_desired_boot_interface::get(&env.pool, &host_id)
        .await?
        .expect("the former owner's desired target should remain");
    assert_eq!(after.value, before.value);
    assert_eq!(after.version, before.version);

    Ok(())
}

// A BMC interface can outlive the candidate data needed to identify a host's
// boot NIC. Boot-order changes still require a target, while `machine_setup`
// preserves its target-less BIOS setup behavior without guessing at Site
// Explorer's stale explored default.
#[crate::sqlx_test]
async fn test_managed_host_without_a_resolvable_target_preserves_action_requirements(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = api_fixtures::create_test_env(pool).await;
    let host =
        api_fixtures::site_explorer::new_host(&env, ManagedHostConfig::default().with_dpu_count(1))
            .await?;
    let host_id: HostMachineId = host.host_snapshot.id;

    let mut txn = env.pool.begin().await?;
    sqlx::query("DELETE FROM machine_boot_interfaces WHERE machine_id = $1")
        .bind(host_id)
        .execute(txn.as_mut())
        .await?;
    sqlx::query(
        "UPDATE machine_interfaces \
         SET machine_id = NULL, association_type = 'None'::association_type \
         WHERE machine_id = $1 AND interface_type <> 'Bmc'::interface_type",
    )
    .bind(host_id)
    .execute(txn.as_mut())
    .await?;
    sqlx::query("DELETE FROM predicted_machine_interfaces WHERE machine_id = $1")
        .bind(host_id)
        .execute(txn.as_mut())
        .await?;
    txn.commit().await?;

    let timepoint = env.redfish_sim.timepoint();
    let error = env
        .api
        .set_dpu_first_boot_order(tonic::Request::new(forge::SetDpuFirstBootOrderRequest {
            machine_id: Some(host_id.to_string()),
            bmc_endpoint_request: None,
            boot_interface_mac: None,
        }))
        .await
        .expect_err("an owned host without a target should be rejected");

    assert_eq!(error.code(), tonic::Code::InvalidArgument);
    assert_eq!(
        error.message(),
        "no boot interface available: enter a MAC or explore the host first",
    );
    assert!(
        env.redfish_sim
            .actions_since(&timepoint)
            .all_hosts()
            .is_empty(),
        "a rejected managed request must not fall through to Redfish",
    );

    let timepoint = env.redfish_sim.timepoint();
    env.api
        .machine_setup(tonic::Request::new(forge::MachineSetupRequest {
            machine_id: Some(host_id.to_string()),
            bmc_endpoint_request: None,
            boot_interface_mac: None,
        }))
        .await?;
    let boot_interface_mac = env
        .redfish_sim
        .actions_since(&timepoint)
        .all_hosts()
        .into_iter()
        .find_map(|action| match action {
            RedfishSimAction::MachineSetup {
                boot_interface_mac, ..
            } => Some(boot_interface_mac),
            _ => None,
        })
        .expect("machine_setup should preserve target-less BIOS setup");
    assert_eq!(boot_interface_mac, None);

    Ok(())
}

// An explicit BMC request is authoritative in
// `validate_and_complete_bmc_endpoint_request`: its database owner wins over a
// mismatched `machine_id`. Persist and enqueue the actual owner only.
#[crate::sqlx_test]
async fn test_machine_setup_uses_the_bmc_endpoints_actual_owner(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = api_fixtures::create_test_env(pool).await;
    let actual =
        api_fixtures::site_explorer::new_host(&env, ManagedHostConfig::default().with_dpu_count(1))
            .await?;
    let caller_supplied =
        api_fixtures::site_explorer::new_host(&env, ManagedHostConfig::default().with_dpu_count(1))
            .await?;
    let actual_id: HostMachineId = actual.host_snapshot.id;
    let caller_supplied_id: HostMachineId = caller_supplied.host_snapshot.id;
    let actual_bmc_ip = actual
        .host_snapshot
        .status
        .bmc_info
        .ip
        .expect("host should have a BMC IP");
    let actual_before = db::machine_desired_boot_interface::get(&env.pool, &actual_id)
        .await?
        .expect("site explorer should initialize the actual owner");
    let caller_supplied_before =
        db::machine_desired_boot_interface::get(&env.pool, &caller_supplied_id)
            .await?
            .expect("site explorer should initialize the caller-supplied host");

    let timepoint = env.redfish_sim.timepoint();
    env.api
        .machine_setup(tonic::Request::new(forge::MachineSetupRequest {
            machine_id: Some(caller_supplied_id.to_string()),
            bmc_endpoint_request: Some(forge::BmcEndpointRequest {
                ip_address: actual_bmc_ip.to_string(),
                mac_address: None,
            }),
            boot_interface_mac: None,
        }))
        .await?;

    assert!(
        env.redfish_sim
            .actions_since(&timepoint)
            .all_hosts()
            .is_empty(),
        "the actual managed owner should keep the request declarative",
    );
    let actual_after = db::machine_desired_boot_interface::get(&env.pool, &actual_id)
        .await?
        .expect("the actual owner should retain desired state");
    assert_eq!(actual_after.value, actual_before.value);
    assert_ne!(
        actual_after.version, actual_before.version,
        "the actual owner should receive the forced generation",
    );
    let caller_supplied_after =
        db::machine_desired_boot_interface::get(&env.pool, &caller_supplied_id)
            .await?
            .expect("the caller-supplied host should retain desired state");
    assert_eq!(caller_supplied_after.value, caller_supplied_before.value);
    assert_eq!(
        caller_supplied_after.version, caller_supplied_before.version,
        "the mismatched machine id must not receive the forced generation",
    );

    Ok(())
}

// A predicted host can be managed before its in-band NIC takes its first DHCP
// lease. Its predicted MAC plus report-derived Redfish id form the exact
// desired pair; the request still does no Redfish work.
#[crate::sqlx_test]
async fn test_set_dpu_first_persists_predicted_host_intent_without_redfish(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = api_fixtures::create_test_env_with_host_inband(pool.clone()).await;

    let mock_host = ManagedHostConfig {
        dpus: vec![],
        ..ManagedHostConfig::default()
    };
    let inband_mac = *mock_host.non_dpu_macs.first().unwrap();

    let _mock =
        api_fixtures::site_explorer::ingest_zero_dpu_host_awaiting_first_lease(&env, mock_host)
            .await?;

    // Precondition: the machine owns a prediction for the NIC and no real
    // interface row. (The prediction's content is the site-explorer ingest
    // tests' contract; here it only locates the machine.)
    let (machine_id, predicted_target) = {
        let mut txn = env.pool.begin().await?;
        let predicted = db::predicted_machine_interface::find_by_mac_address(&mut txn, inband_mac)
            .await?
            .expect("zero-DPU ingest should have minted a predicted interface");
        assert!(
            db::machine_interface::find_by_mac_address(txn.as_mut(), inband_mac)
                .await?
                .is_empty(),
            "the in-band NIC should not have a machine_interfaces row yet",
        );
        let target = MachineBootInterfaceTarget::from_parts(
            Some(predicted.mac_address),
            predicted.boot_interface_id,
        )
        .expect("the prediction always supplies a MAC");
        (predicted.machine_id, target)
    };
    let machine_id = HostMachineId::try_from(machine_id)?;

    let timepoint = env.redfish_sim.timepoint();

    env.api
        .set_dpu_first_boot_order(tonic::Request::new(forge::SetDpuFirstBootOrderRequest {
            machine_id: Some(machine_id.to_string()),
            bmc_endpoint_request: None,
            boot_interface_mac: None,
        }))
        .await?;

    let actions = env.redfish_sim.actions_since(&timepoint).all_hosts();
    assert!(
        actions.is_empty(),
        "a predicted host should defer Redfish work to the machine controller",
    );
    let desired = db::machine_desired_boot_interface::get(&env.pool, &machine_id)
        .await?
        .expect("the predicted host should retain its resolved target");
    assert_eq!(desired.value, predicted_target);
    let report = env
        .api
        .get_machine_boot_interfaces(tonic::Request::new(
            forge::GetMachineBootInterfacesRequest {
                machine_id: Some(machine_id.into()),
            },
        ))
        .await?
        .into_inner();
    let reconciliation = report
        .reconciliation
        .expect("a predicted host should expose its pending reconciliation");
    assert_eq!(
        reconciliation.desired_version,
        desired.version.version_string(),
    );

    Ok(())
}

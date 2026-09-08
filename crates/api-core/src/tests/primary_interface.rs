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

use std::str::FromStr;

use carbide_redfish::libredfish::test_support::RedfishSimAction;
use carbide_uuid::machine::{MachineId, MachineInterfaceId};
use chrono::{DateTime, Utc};
use config_version::ConfigVersion;
use ipnetwork::IpNetwork;
use mac_address::MacAddress;
use model::allocation_type::AllocationType;
use model::machine::{InstanceState, ManagedHostState};
use model::machine_boot_interface::{BootInterfaceSelectionSource, MachineBootInterfaceTarget};
use model::network_segment::NetworkSegmentType;
use model::test_support::ManagedHostConfig;
use rpc::forge;
use rpc::forge::forge_server::Forge;
use sqlx::types::Json;

use crate::test_support::fixture_config::{FixtureDefault as _, ManagedHostConfigExt as _};
use crate::tests::common::api_fixtures;
use crate::tests::common::api_fixtures::network_segment::{
    FIXTURE_ADMIN_NETWORK_SEGMENT_GATEWAY, FIXTURE_HOST_INBAND_NETWORK_SEGMENT_GATEWAY,
    FIXTURE_UNDERLAY_NETWORK_SEGMENT_GATEWAY, create_admin_network_segment,
    create_host_inband_network_segment, create_underlay_network_segment,
};

#[derive(Debug, PartialEq, sqlx::FromRow)]
struct InterfaceIdentityState {
    id: String,
    machine_id: Option<String>,
    attached_dpu_machine_id: Option<String>,
    segment_id: String,
    mac_address: String,
    boot_interface_id: Option<String>,
    interface_type: String,
    association_type: Option<String>,
}

#[derive(Debug, PartialEq, sqlx::FromRow)]
struct InterfacePresentationState {
    id: String,
    primary_interface: bool,
    hostname: String,
    domain_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, sqlx::FromRow)]
struct InterfaceAddressState {
    id: String,
    interface_id: String,
    address: String,
    allocation_type: AllocationType,
}

#[derive(Debug, PartialEq, sqlx::FromRow)]
struct MachineNetworkConfigState {
    id: String,
    network_config: String,
    network_config_version: ConfigVersion,
}

#[derive(Debug, PartialEq, sqlx::FromRow)]
struct InstanceNetworkConfigState {
    id: String,
    network_config: String,
    network_config_version: ConfigVersion,
}

#[derive(Debug, PartialEq, sqlx::FromRow)]
struct BootInterfacePersistenceState {
    machine_version: ConfigVersion,
    desired_mac_address: String,
    desired_interface_id: Option<String>,
    desired_version: ConfigVersion,
    verified_version: Option<ConfigVersion>,
    observed_at: Option<DateTime<Utc>>,
    assumed: bool,
    selection_source: BootInterfaceSelectionSource,
    selection_updated_at: Option<DateTime<Utc>>,
}

#[derive(Debug, PartialEq)]
struct SetPrimaryPersistenceState {
    interface_identities: Vec<InterfaceIdentityState>,
    interface_presentations: Vec<InterfacePresentationState>,
    interface_addresses: Vec<InterfaceAddressState>,
    machine_network_configs: Vec<MachineNetworkConfigState>,
    instance_network_configs: Vec<InstanceNetworkConfigState>,
    desired_boot_interface: Option<BootInterfacePersistenceState>,
}

impl SetPrimaryPersistenceState {
    fn presentation(&self, interface_id: &str) -> &InterfacePresentationState {
        self.interface_presentations
            .iter()
            .find(|interface| interface.id == interface_id)
            .expect("selected interface should remain present")
    }
}

// Snapshot of every persisted field that a primary interface update can change.
async fn load_set_primary_persistence_state(
    pool: &sqlx::PgPool,
    host_id: MachineId,
) -> Result<SetPrimaryPersistenceState, sqlx::Error> {
    Ok(SetPrimaryPersistenceState {
        interface_identities: sqlx::query_as(
            "SELECT id::text,
                    machine_id::text,
                    attached_dpu_machine_id::text,
                    segment_id::text,
                    mac_address::text,
                    boot_interface_id,
                    interface_type::text,
                    association_type::text
             FROM machine_interfaces
             WHERE machine_id = $1
             ORDER BY id",
        )
        .bind(host_id)
        .fetch_all(pool)
        .await?,
        interface_presentations: sqlx::query_as(
            "SELECT id::text, primary_interface, hostname, domain_id::text
             FROM machine_interfaces
             WHERE machine_id = $1
             ORDER BY id",
        )
        .bind(host_id)
        .fetch_all(pool)
        .await?,
        interface_addresses: sqlx::query_as(
            "SELECT address.id::text,
                    address.interface_id::text,
                    address.address::text,
                    address.allocation_type
             FROM machine_interface_addresses address \
             JOIN machine_interfaces interface ON interface.id = address.interface_id \
             WHERE interface.machine_id = $1 \
             ORDER BY address.id",
        )
        .bind(host_id)
        .fetch_all(pool)
        .await?,
        machine_network_configs: sqlx::query_as(
            "SELECT id::text, network_config::text, network_config_version \
             FROM machines \
             WHERE id IN (SELECT id FROM machine_group_member_ids($1)) \
             ORDER BY id",
        )
        .bind(host_id)
        .fetch_all(pool)
        .await?,
        instance_network_configs: sqlx::query_as(
            "SELECT id::text, network_config::text, network_config_version
             FROM instances
             WHERE machine_id = $1
             ORDER BY id",
        )
        .bind(host_id)
        .fetch_all(pool)
        .await?,
        desired_boot_interface: sqlx::query_as(
            "SELECT machine.version AS machine_version,
                    boot_interface.desired_mac_address::text AS desired_mac_address,
                    boot_interface.desired_interface_id,
                    boot_interface.desired_version,
                    boot_interface.verified_version,
                    boot_interface.observed_at,
                    boot_interface.assumed,
                    boot_interface.selection_source,
                    boot_interface.selection_updated_at
             FROM machine_boot_interfaces boot_interface
             JOIN machines machine ON machine.id = boot_interface.machine_id
             WHERE boot_interface.machine_id = $1",
        )
        .bind(host_id)
        .fetch_optional(pool)
        .await?,
    })
}

// Unlike `set_primary_dpu`, `set_primary_interface` has no zero-DPU guard -- a
// zero-DPU host is a first-class target. So on a zero-DPU host the call must get
// PAST the would-be guard: it can still fail (here, because the interface id
// doesn't exist), but never with the `FailedPrecondition` "zero-DPU" rejection
// that `set_primary_dpu` returns for the same host.
#[crate::sqlx_test]
async fn test_set_primary_interface_does_not_apply_the_zero_dpu_guard(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    // Zero-DPU host ingestion needs a HostInband network segment whose CIDR
    // covers the relay address; the default test env doesn't define one.
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
    // HostInband segments must live in a Flat VPC. The test doesn't otherwise
    // need a non-Flat VPC, so create only a Flat one for the segment.
    let flat_vpc_id = api_fixtures::network_segment::create_default_flat_vpc(
        &env.api,
        "set-primary-interface flat vpc",
    )
    .await;
    create_underlay_network_segment(&env.api).await;
    create_admin_network_segment(&env.api).await;
    create_host_inband_network_segment(&env.api, Some(flat_vpc_id)).await;
    env.run_network_segment_controller_iteration().await;
    env.run_network_segment_controller_iteration().await;

    let zero_dpu_host =
        api_fixtures::site_explorer::new_host(&env, ManagedHostConfig::zero_dpu()).await?;

    // A well-formed but non-existent interface id: the handler must try to look
    // it up -- which is only reachable once it's past the would-be zero-DPU
    // guard -- and then fail because the interface isn't there.
    let missing_interface_id =
        MachineInterfaceId::from_str("11111111-1111-1111-1111-111111111111").unwrap();

    let result = env
        .api
        .set_primary_interface(tonic::Request::new(forge::SetPrimaryInterfaceRequest {
            host_machine_id: Some(zero_dpu_host.host_snapshot.id),
            interface_id: Some(missing_interface_id),
            force_reconcile: false,
            ..Default::default()
        }))
        .await;

    let err = result.expect_err("a non-existent interface id should still fail the request");
    // Getting PAST the (would-be) zero-DPU guard means we reach the interface
    // lookup and fail THERE -- an InvalidArgument about the missing interface,
    // never the FailedPrecondition "zero-DPU" rejection set_primary_dpu returns.
    assert_eq!(
        err.code(),
        tonic::Code::InvalidArgument,
        "a zero-DPU host should reach the interface lookup, not be rejected by a zero-DPU guard; got {}: {}",
        err.code(),
        err.message(),
    );
    assert!(
        err.message().contains("not found"),
        "expected the missing-interface error, got: {}",
        err.message(),
    );

    Ok(())
}

// Selecting the ingestion winner is still meaningful when the current
// source is automatic: it pins that same target as explicit operator intent.
// Since the target does not change, this must not manufacture controller work.
#[crate::sqlx_test]
async fn test_set_primary_interface_records_same_target_operator_authority(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = api_fixtures::create_test_env(pool).await;
    let host =
        api_fixtures::site_explorer::new_host(&env, ManagedHostConfig::default().with_dpu_count(2))
            .await?;
    let host_id = host.host_snapshot.id;

    let current_primary_id = {
        let mut txn = env.pool.begin().await?;
        db::machine_interface::find_by_machine_ids(txn.as_mut(), &[host_id])
            .await?
            .remove(&host_id)
            .expect("host should have interface rows")
            .into_iter()
            .find(|interface| interface.primary_interface)
            .expect("host should start with a primary interface")
            .id
    };
    let sentinel =
        DateTime::from_timestamp(1_722_000_000, 123_000_000).expect("fixture selection timestamp");
    sqlx::query(
        "UPDATE machine_boot_interfaces
         SET selection_source = 'redfish_serial_number',
             selection_updated_at = $1
         WHERE machine_id = $2",
    )
    .bind(sentinel)
    .bind(host_id)
    .execute(&env.pool)
    .await?;
    sqlx::query("DELETE FROM machine_state_controller_queued_objects WHERE object_id = $1")
        .bind(host_id.to_string())
        .execute(&env.pool)
        .await?;

    #[derive(sqlx::FromRow)]
    struct BootInterfaceState {
        machine_version: ConfigVersion,
        desired_version: ConfigVersion,
        verified_version: Option<ConfigVersion>,
        observed_at: Option<DateTime<Utc>>,
        assumed: bool,
        selection_source: BootInterfaceSelectionSource,
        selection_updated_at: Option<DateTime<Utc>>,
    }

    let state_query = "SELECT machine.version AS machine_version,
                              boot_interface.desired_version,
                              boot_interface.verified_version,
                              boot_interface.observed_at,
                              boot_interface.assumed,
                              boot_interface.selection_source,
                              boot_interface.selection_updated_at
                       FROM machines machine
                       JOIN machine_boot_interfaces boot_interface
                         ON boot_interface.machine_id = machine.id
                       WHERE machine.id = $1";
    let before = sqlx::query_as::<_, BootInterfaceState>(state_query)
        .bind(host_id)
        .fetch_one(&env.pool)
        .await?;
    assert_eq!(
        before.verified_version,
        Some(before.desired_version),
        "this case isolates a source-only change, so the target must already be verified",
    );

    env.api
        .set_primary_interface(tonic::Request::new(forge::SetPrimaryInterfaceRequest {
            host_machine_id: Some(host_id),
            interface_id: Some(current_primary_id),
            force_reconcile: false,
            ..Default::default()
        }))
        .await?;

    let after = sqlx::query_as::<_, BootInterfaceState>(state_query)
        .bind(host_id)
        .fetch_one(&env.pool)
        .await?;
    assert_eq!(
        after.machine_version.version_nr(),
        before.machine_version.version_nr() + 1,
        "the visible source change must advance the aggregate machine version",
    );
    assert_eq!(after.desired_version, before.desired_version);
    assert_eq!(after.verified_version, before.verified_version);
    assert_eq!(after.observed_at, before.observed_at);
    assert_eq!(after.assumed, before.assumed);
    assert_eq!(
        after.selection_source,
        BootInterfaceSelectionSource::Operator
    );
    let selection_updated_at = after
        .selection_updated_at
        .expect("operator decision timestamp");
    assert!(selection_updated_at > sentinel);

    let is_queued: bool = sqlx::query_scalar(
        "SELECT EXISTS (
            SELECT 1
            FROM machine_state_controller_queued_objects
            WHERE object_id = $1
        )",
    )
    .bind(host_id.to_string())
    .fetch_one(&env.pool)
    .await?;
    assert!(!is_queued, "source-only authority needs no Redfish work");

    let error = env
        .api
        .set_primary_interface(tonic::Request::new(forge::SetPrimaryInterfaceRequest {
            host_machine_id: Some(host_id),
            interface_id: Some(current_primary_id),
            force_reconcile: false,
            ..Default::default()
        }))
        .await
        .expect_err("repeating explicit operator intent should retain the API guard");
    assert_eq!(error.code(), tonic::Code::InvalidArgument);
    assert_eq!(
        error.message(),
        "requested interface is already the operator-selected primary interface",
    );

    let retry_selection: (BootInterfaceSelectionSource, Option<DateTime<Utc>>) = sqlx::query_as(
        "SELECT selection_source, selection_updated_at
         FROM machine_boot_interfaces
         WHERE machine_id = $1",
    )
    .bind(host_id)
    .fetch_one(&env.pool)
    .await?;
    assert_eq!(
        retry_selection,
        (
            BootInterfaceSelectionSource::Operator,
            Some(selection_updated_at),
        ),
        "an idempotent retry must not refresh the selection timestamp",
    );

    Ok(())
}

// BMC administration can leave the desired target on a different interface
// without moving the primary row. Selecting the current primary must repair
// that divergence even when the stored target was also selected by an operator.
#[crate::sqlx_test]
async fn test_set_primary_interface_repairs_divergent_operator_target(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = api_fixtures::create_test_env(pool).await;
    let host =
        api_fixtures::site_explorer::new_host(&env, ManagedHostConfig::default().with_dpu_count(2))
            .await?;
    let host_id = host.host_snapshot.id;

    let (current_primary_id, current_primary_target, divergent_target) = {
        let mut txn = env.pool.begin().await?;
        let interfaces = db::machine_interface::find_by_machine_ids(txn.as_mut(), &[host_id])
            .await?
            .remove(&host_id)
            .expect("host should have interface rows");
        let current_primary = interfaces
            .iter()
            .find(|interface| interface.primary_interface)
            .expect("host should start with a primary interface");
        let divergent = interfaces
            .iter()
            .find(|interface| {
                !interface.primary_interface && interface.attached_dpu_machine_id.is_some()
            })
            .expect("host with two DPUs should have an interface backed by another DPU");
        (
            current_primary.id,
            MachineBootInterfaceTarget::Pair(
                current_primary
                    .boot_interface()
                    .expect("primary DPU interface should have a Redfish id"),
            ),
            MachineBootInterfaceTarget::Pair(
                divergent
                    .boot_interface()
                    .expect("secondary DPU interface should have a Redfish id"),
            ),
        )
    };

    let mut txn = env.pool.begin().await?;
    db::machine_desired_boot_interface::force_set(
        txn.as_mut(),
        &host_id.try_into().unwrap(),
        &divergent_target,
        BootInterfaceSelectionSource::Operator,
    )
    .await?;
    txn.commit().await?;

    env.api
        .set_primary_interface(tonic::Request::new(forge::SetPrimaryInterfaceRequest {
            host_machine_id: Some(host_id),
            interface_id: Some(current_primary_id),
            force_reconcile: false,
            ..Default::default()
        }))
        .await?;

    let desired = db::machine_desired_boot_interface::get(&env.pool, &host_id.try_into().unwrap())
        .await?
        .expect("the repaired desired target should be persisted");
    assert_eq!(desired.value, current_primary_target);

    Ok(())
}

// A Redfish interface ID is part of a complete desired target. Selecting the
// current primary must refresh an obsolete ID even when the MAC and operator
// selection source have not changed.
#[crate::sqlx_test]
async fn test_set_primary_interface_refreshes_operator_target_interface_id(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = api_fixtures::create_test_env(pool).await;
    let host =
        api_fixtures::site_explorer::new_host(&env, ManagedHostConfig::default().with_dpu_count(2))
            .await?;
    let host_id = host.host_snapshot.id;

    let (current_primary_id, current_primary_target, obsolete_target) = {
        let mut txn = env.pool.begin().await?;
        let interfaces = db::machine_interface::find_by_machine_ids(txn.as_mut(), &[host_id])
            .await?
            .remove(&host_id)
            .expect("host should have interface rows");
        let current_primary = interfaces
            .iter()
            .find(|interface| interface.primary_interface)
            .expect("host should start with a primary interface");
        let current_primary_target = MachineBootInterfaceTarget::Pair(
            current_primary
                .boot_interface()
                .expect("primary DPU interface should have a Redfish id"),
        );
        let obsolete_target = MachineBootInterfaceTarget::from_parts(
            Some(current_primary.mac_address),
            Some("NIC.Obsolete.1-1-1".to_string()),
        )
        .expect("the obsolete complete target should be valid");
        (current_primary.id, current_primary_target, obsolete_target)
    };

    let mut txn = env.pool.begin().await?;
    let obsolete = db::machine_desired_boot_interface::force_set(
        txn.as_mut(),
        &host_id.try_into().unwrap(),
        &obsolete_target,
        BootInterfaceSelectionSource::Operator,
    )
    .await?;
    txn.commit().await?;

    env.api
        .set_primary_interface(tonic::Request::new(forge::SetPrimaryInterfaceRequest {
            host_machine_id: Some(host_id),
            interface_id: Some(current_primary_id),
            force_reconcile: false,
            ..Default::default()
        }))
        .await?;

    let desired = db::machine_desired_boot_interface::get(&env.pool, &host_id.try_into().unwrap())
        .await?
        .expect("the refreshed desired target should be persisted");
    assert_eq!(desired.value, current_primary_target);
    assert_eq!(
        desired.version.version_nr(),
        obsolete.desired.version.version_nr() + 1,
        "refreshing the Redfish interface ID should open a desired generation",
    );

    Ok(())
}

// `set_primary_interface` commits the primary row and desired target together.
// Redfish is deliberately absent from this request path: machine-controller
// picks the pending generation up after the transaction commits.
#[crate::sqlx_test]
#[allow(deprecated)] // The test verifies the compatibility behavior of `reboot`.
async fn test_set_primary_interface_promotes_a_non_primary_interface(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = api_fixtures::create_test_env(pool).await;

    let host =
        api_fixtures::site_explorer::new_host(&env, ManagedHostConfig::default().with_dpu_count(2))
            .await?;
    let host_id = host.host_snapshot.id;

    // One host interface is primary; pick a different (non-primary) host NIC to promote.
    let (original_primary_id, promote_id, promote_target) = {
        let mut txn = env.pool.begin().await?;
        let interfaces = db::machine_interface::find_by_machine_ids(txn.as_mut(), &[host_id])
            .await?
            .remove(&host_id)
            .expect("host should have interface rows");
        let original_primary_id = interfaces
            .iter()
            .find(|i| i.primary_interface)
            .expect("host should start with a primary interface")
            .id;
        let promote = interfaces
            .iter()
            .find(|i| !i.primary_interface && i.attached_dpu_machine_id.is_some())
            .expect("host should have a non-primary host interface to promote");
        let promote_target = MachineBootInterfaceTarget::from_parts(
            Some(promote.mac_address),
            promote.boot_interface_id.clone(),
        )
        .expect("a host interface always supplies a MAC");
        (original_primary_id, promote.id, promote_target)
    };
    let state_before = load_set_primary_persistence_state(&env.pool, host_id).await?;

    let timepoint = env.redfish_sim.timepoint();
    env.api
        .set_primary_interface(tonic::Request::new(forge::SetPrimaryInterfaceRequest {
            host_machine_id: Some(host_id),
            interface_id: Some(promote_id),
            force_reconcile: false,
            ..Default::default()
        }))
        .await?;

    let actions = env.redfish_sim.actions_since(&timepoint).all_hosts();
    assert_eq!(
        actions,
        Vec::<RedfishSimAction>::new(),
        "the managed request should leave Redfish convergence to machine-controller",
    );

    let state_after = load_set_primary_persistence_state(&env.pool, host_id).await?;
    let original_primary_id = original_primary_id.to_string();
    let promote_id_string = promote_id.to_string();

    // Comparing the immutable projection catches row recreation or a host/DPU
    // reassociation hidden by a correct primary flag.
    assert_eq!(
        state_after.interface_identities,
        state_before.interface_identities,
    );

    let primaries_now: Vec<_> = state_after
        .interface_presentations
        .iter()
        .filter(|interface| interface.primary_interface)
        .map(|interface| interface.id.as_str())
        .collect();
    assert_eq!(
        primaries_now,
        vec![promote_id_string.as_str()],
        "exactly the promoted interface should be primary",
    );
    assert!(
        !state_after
            .interface_presentations
            .iter()
            .find(|interface| interface.id == original_primary_id)
            .expect("the original interface should remain present")
            .primary_interface,
        "the previously-primary interface should no longer be primary",
    );

    let mut expected_addresses = state_before.interface_addresses.clone();
    let mut moved_address_count = 0;
    for address in &mut expected_addresses {
        if address.interface_id == original_primary_id
            && address.allocation_type == AllocationType::Dhcp
        {
            address.interface_id.clone_from(&promote_id_string);
            moved_address_count += 1;
        }
    }
    assert!(
        moved_address_count > 0,
        "the ingestion primary should own an Admin DHCP address",
    );
    assert_eq!(
        state_after.interface_addresses, expected_addresses,
        "the same Admin allocation rows should move to the promoted interface",
    );

    let original_before = state_before.presentation(&original_primary_id);
    let original_after = state_after.presentation(&original_primary_id);
    let promoted_after = state_after.presentation(&promote_id_string);
    assert_eq!(
        (promoted_after.hostname.as_str(), &promoted_after.domain_id),
        (
            original_before.hostname.as_str(),
            &original_before.domain_id
        ),
        "the Admin DNS identity should follow the address to the new primary",
    );
    assert_eq!(
        original_after.domain_id, None,
        "the dormant interface should no longer publish an Admin DNS name",
    );

    let mut expected_group_ids = vec![host_id.to_string()];
    expected_group_ids.extend(
        state_before
            .interface_identities
            .iter()
            .filter_map(|interface| interface.attached_dpu_machine_id.clone()),
    );
    expected_group_ids.sort();
    expected_group_ids.dedup();
    assert_eq!(
        state_before
            .machine_network_configs
            .iter()
            .map(|machine| machine.id.clone())
            .collect::<Vec<_>>(),
        expected_group_ids,
        "the network generation should cover the host and both attached DPUs",
    );
    assert_eq!(
        state_after.machine_network_configs.len(),
        state_before.machine_network_configs.len(),
    );
    for (before, after) in state_before
        .machine_network_configs
        .iter()
        .zip(&state_after.machine_network_configs)
    {
        assert_eq!(after.id, before.id);
        assert_eq!(after.network_config, before.network_config);
        assert_eq!(
            after.network_config_version.version_nr(),
            before.network_config_version.version_nr() + 1,
            "{} should advance exactly one network generation",
            after.id,
        );
    }

    let boot_before = state_before
        .desired_boot_interface
        .as_ref()
        .expect("ingestion should initialize the desired target");
    let boot_after = state_after
        .desired_boot_interface
        .as_ref()
        .expect("the update should retain the desired boot interface row");
    // Postgres renders `macaddr` in lowercase and `MacAddress`'s `Display` renders it in
    // uppercase, so compare parsed addresses rather than two spellings of the same value.
    // Comparing the strings only passes when the promoted interface happens to draw a MAC
    // with no hex letters in it, which made this assertion fail intermittently.
    assert_eq!(
        boot_after
            .desired_mac_address
            .parse::<MacAddress>()
            .expect("a stored MAC address is well formed"),
        promote_target.mac_address(),
    );
    assert_eq!(
        boot_after.desired_interface_id.as_deref(),
        promote_target.interface_id(),
    );
    assert_eq!(
        boot_after.desired_version.version_nr(),
        boot_before.desired_version.version_nr() + 1,
    );
    assert_eq!(boot_after.verified_version, boot_before.verified_version);
    assert_eq!(boot_after.observed_at, boot_before.observed_at);
    assert_eq!(boot_after.assumed, boot_before.assumed);
    assert_eq!(
        boot_after.selection_source,
        BootInterfaceSelectionSource::Operator,
    );
    assert!(
        boot_after.selection_updated_at > boot_before.selection_updated_at,
        "a new operator selection should record a later decision time",
    );
    assert_eq!(
        boot_after.machine_version.version_nr(),
        boot_before.machine_version.version_nr() + 1,
        "the desired-target change should advance the aggregate once",
    );

    let desired = db::machine_desired_boot_interface::get(&env.pool, &host_id.try_into().unwrap())
        .await?
        .expect("the selected target should be persisted");
    assert_eq!(desired.value, promote_target);

    let error = env
        .api
        .set_primary_interface(tonic::Request::new(forge::SetPrimaryInterfaceRequest {
            host_machine_id: Some(host_id),
            interface_id: Some(promote_id),
            force_reconcile: false,
            ..Default::default()
        }))
        .await
        .expect_err("selecting the current primary without force should retain the API guard");
    assert_eq!(error.code(), tonic::Code::InvalidArgument);
    assert_eq!(
        error.message(),
        "requested interface is already the operator-selected primary interface",
    );

    sqlx::query("DELETE FROM machine_state_controller_queued_objects WHERE object_id = $1")
        .bind(host_id.to_string())
        .execute(&env.pool)
        .await?;
    let timepoint = env.redfish_sim.timepoint();
    env.api
        .set_primary_interface(tonic::Request::new(forge::SetPrimaryInterfaceRequest {
            host_machine_id: Some(host_id),
            interface_id: Some(promote_id),
            force_reconcile: true,
            ..Default::default()
        }))
        .await?;
    let forced = db::machine_desired_boot_interface::get(&env.pool, &host_id.try_into().unwrap())
        .await?
        .expect("the forced request should retain the desired target");
    assert_eq!(forced.value, promote_target);
    assert_eq!(
        forced.version.version_nr(),
        desired.version.version_nr() + 1,
    );
    let forced_is_queued: bool = sqlx::query_scalar(
        "SELECT EXISTS (
            SELECT 1
            FROM machine_state_controller_queued_objects
            WHERE object_id = $1
        )",
    )
    .bind(host_id.to_string())
    .fetch_one(&env.pool)
    .await?;
    assert!(
        forced_is_queued,
        "a forced generation must wake the controller",
    );
    assert!(
        env.redfish_sim
            .actions_since(&timepoint)
            .all_hosts()
            .is_empty(),
        "force_reconcile should schedule controller work, not write Redfish directly",
    );

    // `reboot` remains a compatibility spelling for the same fresh controller
    // pass. It no longer means an unconditional restart in the RPC path.
    let timepoint = env.redfish_sim.timepoint();
    env.api
        .set_primary_interface(tonic::Request::new(forge::SetPrimaryInterfaceRequest {
            host_machine_id: Some(host_id),
            interface_id: Some(promote_id),
            reboot: true,
            force_reconcile: false,
        }))
        .await?;
    let legacy_forced =
        db::machine_desired_boot_interface::get(&env.pool, &host_id.try_into().unwrap())
            .await?
            .expect("the legacy alias should retain the desired target");
    assert_eq!(legacy_forced.value, promote_target);
    assert_eq!(
        legacy_forced.version.version_nr(),
        forced.version.version_nr() + 1,
    );
    assert!(
        env.redfish_sim
            .actions_since(&timepoint)
            .all_hosts()
            .is_empty(),
        "the deprecated reboot alias should not restart from the request path",
    );

    Ok(())
}

// `set_primary_interface` changes interface rows before it writes the desired
// target. A late database error must roll the whole transaction back so the
// machine controller can never see a primary/target mismatch.
#[crate::sqlx_test]
async fn test_set_primary_interface_rolls_back_primary_and_desired_together(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = api_fixtures::create_test_env(pool).await;
    let host =
        api_fixtures::site_explorer::new_host(&env, ManagedHostConfig::default().with_dpu_count(2))
            .await?;
    let host_id = host.host_snapshot.id;

    let promote_id = {
        let mut txn = env.pool.begin().await?;
        let interfaces = db::machine_interface::find_by_machine_ids(txn.as_mut(), &[host_id])
            .await?
            .remove(&host_id)
            .expect("host should have interface rows");
        let promote_id = interfaces
            .iter()
            .find(|interface| {
                !interface.primary_interface && interface.attached_dpu_machine_id.is_some()
            })
            .expect("host should have a non-primary DPU-backed interface")
            .id;
        txn.commit().await?;
        promote_id
    };
    let state_before = load_set_primary_persistence_state(&env.pool, host_id).await?;

    sqlx::raw_sql(
        r#"
        CREATE FUNCTION reject_desired_boot_interface_write()
        RETURNS trigger
        LANGUAGE plpgsql
        AS $$
        BEGIN
            RAISE EXCEPTION 'forced desired boot interface failure';
        END;
        $$;

        CREATE TRIGGER reject_desired_boot_interface_write
        BEFORE INSERT OR UPDATE ON machine_boot_interfaces
        FOR EACH ROW
        EXECUTE FUNCTION reject_desired_boot_interface_write();
        "#,
    )
    .execute(&env.pool)
    .await?;

    let error = env
        .api
        .set_primary_interface(tonic::Request::new(forge::SetPrimaryInterfaceRequest {
            host_machine_id: Some(host_id),
            interface_id: Some(promote_id),
            force_reconcile: false,
            ..Default::default()
        }))
        .await
        .expect_err("the injected desired-target write must fail the request");
    assert_eq!(error.code(), tonic::Code::Internal);

    let state_after = load_set_primary_persistence_state(&env.pool, host_id).await?;
    assert_eq!(
        state_after, state_before,
        "a late desired boot interface failure must roll back the transaction",
    );

    Ok(())
}

// The host row lock prevents version changes between the read and update. Returning no row is an
// internal consistency error, so the transaction must roll back the earlier writes.
#[crate::sqlx_test]
async fn test_set_primary_interface_rolls_back_when_network_update_returns_no_row(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = api_fixtures::create_test_env(pool).await;
    let host =
        api_fixtures::site_explorer::new_host(&env, ManagedHostConfig::default().with_dpu_count(2))
            .await?;
    let host_id = host.host_snapshot.id;

    let promote_id = {
        let mut txn = env.pool.begin().await?;
        let interfaces = db::machine_interface::find_by_machine_ids(txn.as_mut(), &[host_id])
            .await?
            .remove(&host_id)
            .expect("host should have interface rows");
        let promote_id = interfaces
            .iter()
            .find(|interface| {
                !interface.primary_interface && interface.attached_dpu_machine_id.is_some()
            })
            .expect("host should have a non-primary DPU-backed interface")
            .id;
        txn.commit().await?;
        promote_id
    };
    let state_before = load_set_primary_persistence_state(&env.pool, host_id).await?;

    // A NULL BEFORE-trigger result exercises the no-row path despite the host lock.
    sqlx::raw_sql(
        r#"
        CREATE FUNCTION suppress_machine_network_config_update()
        RETURNS trigger
        LANGUAGE plpgsql
        AS $$
        BEGIN
            RETURN NULL;
        END;
        $$;

        CREATE TRIGGER suppress_machine_network_config_update
        BEFORE UPDATE OF network_config ON machines
        FOR EACH ROW
        EXECUTE FUNCTION suppress_machine_network_config_update();
        "#,
    )
    .execute(&env.pool)
    .await?;

    let error = env
        .api
        .set_primary_interface(tonic::Request::new(forge::SetPrimaryInterfaceRequest {
            host_machine_id: Some(host_id),
            interface_id: Some(promote_id),
            force_reconcile: false,
            ..Default::default()
        }))
        .await
        .expect_err("the suppressed network configuration update should fail the request");
    assert_eq!(error.code(), tonic::Code::Internal);
    assert!(
        error.message().contains("network configuration update"),
        "expected the network configuration update error, got: {}",
        error.message(),
    );

    let state_after = load_set_primary_persistence_state(&env.pool, host_id).await?;
    assert_eq!(
        state_after, state_before,
        "a missing network configuration update must roll back the transaction",
    );

    Ok(())
}

// A deleted `Instance` remains associated with its host while lifecycle cleanup
// is pending. The locked lookup must reject both a primary move and a forced
// reconciliation before either operation can persist changes.
#[crate::sqlx_test]
async fn test_set_primary_interface_rejects_deleted_instance_and_rolls_back_writes(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = api_fixtures::create_test_env(pool).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let host = api_fixtures::create_managed_host_multi_dpu(&env, 2).await;
    let host_id = host.id;
    let instance = host
        .instance_builer(&env)
        .single_interface_network_config(segment_id)
        .build()
        .await;

    let (current_primary_id, promote_id) = {
        let mut txn = env.pool.begin().await?;
        let interfaces = db::machine_interface::find_by_machine_ids(txn.as_mut(), &[host_id])
            .await?
            .remove(&host_id)
            .expect("host should have interface rows");
        let current_primary_id = interfaces
            .iter()
            .find(|interface| interface.primary_interface)
            .expect("host should have a primary interface")
            .id;
        let promote_id = interfaces
            .into_iter()
            .find(|interface| {
                !interface.primary_interface && interface.attached_dpu_machine_id.is_some()
            })
            .expect("host should have a non-primary DPU-backed interface")
            .id;
        txn.commit().await?;
        (current_primary_id, promote_id)
    };

    let mut deletion = env.pool.begin().await?;
    db::instance::mark_as_deleted(instance.id, deletion.as_mut()).await?;
    deletion.commit().await?;
    let state_before = load_set_primary_persistence_state(&env.pool, host_id.into()).await?;

    struct Case {
        name: &'static str,
        interface_id: MachineInterfaceId,
        force_reconcile: bool,
    }
    let cases = [
        Case {
            name: "primary move",
            interface_id: promote_id,
            force_reconcile: false,
        },
        Case {
            name: "forced reconciliation",
            interface_id: current_primary_id,
            force_reconcile: true,
        },
    ];

    for case in cases {
        let error = env
            .api
            .set_primary_interface(tonic::Request::new(forge::SetPrimaryInterfaceRequest {
                host_machine_id: Some(host_id.into()),
                interface_id: Some(case.interface_id),
                force_reconcile: case.force_reconcile,
                ..Default::default()
            }))
            .await
            .expect_err(case.name);
        assert_eq!(error.code(), tonic::Code::FailedPrecondition);
        assert_eq!(
            error.message(),
            format!("instance {} is being deleted", instance.id),
        );

        let state_after = load_set_primary_persistence_state(&env.pool, host_id.into()).await?;
        assert_eq!(state_after, state_before, "{} persisted state", case.name);
    }

    Ok(())
}

// `set_primary_interface` wakes an unassigned Ready host only after its intent
// commits. Assigned hosts keep the same durable pending intent, but their
// current lifecycle owns when it is safe to act on it.
#[crate::sqlx_test]
async fn test_set_primary_interface_hands_ready_intent_to_the_controller(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = api_fixtures::create_test_env(pool).await;
    let host =
        api_fixtures::site_explorer::new_host(&env, ManagedHostConfig::default().with_dpu_count(2))
            .await?;
    let host_id = host.host_snapshot.id;
    assert_eq!(host.managed_state, ManagedHostState::Ready);

    let (original_primary_id, original_target, promote_id) = {
        let mut txn = env.pool.begin().await?;
        let interfaces = db::machine_interface::find_by_machine_ids(txn.as_mut(), &[host_id])
            .await?
            .remove(&host_id)
            .expect("host should have interface rows");
        let original = interfaces
            .iter()
            .find(|interface| interface.primary_interface)
            .expect("host should start with a primary interface");
        let promote = interfaces
            .iter()
            .find(|interface| {
                !interface.primary_interface && interface.attached_dpu_machine_id.is_some()
            })
            .expect("host should have a non-primary DPU-backed interface");
        let original_target = MachineBootInterfaceTarget::from_parts(
            Some(original.mac_address),
            original.boot_interface_id.clone(),
        )
        .expect("a host interface always supplies a MAC");
        txn.commit().await?;
        (original.id, original_target, promote.id)
    };

    sqlx::query("DELETE FROM machine_state_controller_queued_objects WHERE object_id = $1")
        .bind(host_id.to_string())
        .execute(&env.pool)
        .await?;
    env.api
        .set_primary_interface(tonic::Request::new(forge::SetPrimaryInterfaceRequest {
            host_machine_id: Some(host_id),
            interface_id: Some(promote_id),
            force_reconcile: false,
            ..Default::default()
        }))
        .await?;

    let ready_is_queued: bool = sqlx::query_scalar(
        "SELECT EXISTS (
            SELECT 1
            FROM machine_state_controller_queued_objects
            WHERE object_id = $1
        )",
    )
    .bind(host_id.to_string())
    .fetch_one(&env.pool)
    .await?;
    assert!(ready_is_queued, "the Ready host should be queued");
    let ready_pending: bool = sqlx::query_scalar(
        "SELECT desired_version IS DISTINCT FROM verified_version
         FROM machine_boot_interfaces
         WHERE machine_id = $1",
    )
    .bind(host_id)
    .fetch_one(&env.pool)
    .await?;
    assert!(ready_pending, "the committed target should remain pending");
    let ready_state: Json<ManagedHostState> =
        sqlx::query_scalar("SELECT controller_state FROM machines WHERE id = $1")
            .bind(host_id)
            .fetch_one(&env.pool)
            .await?;
    assert_eq!(ready_state.0, ManagedHostState::Ready);
    let ready_desired =
        db::machine_desired_boot_interface::get(&env.pool, &host_id.try_into().unwrap())
            .await?
            .expect("the Ready request should persist its target");

    sqlx::query("DELETE FROM machine_state_controller_queued_objects WHERE object_id = $1")
        .bind(host_id.to_string())
        .execute(&env.pool)
        .await?;
    let assigned_state = ManagedHostState::Assigned {
        instance_state: InstanceState::Ready,
    };
    sqlx::query("UPDATE machines SET controller_state = $1 WHERE id = $2")
        .bind(Json(assigned_state.clone()))
        .bind(host_id)
        .execute(&env.pool)
        .await?;

    env.api
        .set_primary_interface(tonic::Request::new(forge::SetPrimaryInterfaceRequest {
            host_machine_id: Some(host_id),
            interface_id: Some(original_primary_id),
            force_reconcile: false,
            ..Default::default()
        }))
        .await?;

    let assigned_is_queued: bool = sqlx::query_scalar(
        "SELECT EXISTS (
            SELECT 1
            FROM machine_state_controller_queued_objects
            WHERE object_id = $1
        )",
    )
    .bind(host_id.to_string())
    .fetch_one(&env.pool)
    .await?;
    assert!(
        !assigned_is_queued,
        "the Assigned host should stay with its current lifecycle",
    );
    let assigned_pending: bool = sqlx::query_scalar(
        "SELECT desired_version IS DISTINCT FROM verified_version
         FROM machine_boot_interfaces
         WHERE machine_id = $1",
    )
    .bind(host_id)
    .fetch_one(&env.pool)
    .await?;
    assert!(
        assigned_pending,
        "the Assigned host should retain its pending target",
    );
    let assigned_state_after: Json<ManagedHostState> =
        sqlx::query_scalar("SELECT controller_state FROM machines WHERE id = $1")
            .bind(host_id)
            .fetch_one(&env.pool)
            .await?;
    assert_eq!(assigned_state_after.0, assigned_state);
    let assigned_desired =
        db::machine_desired_boot_interface::get(&env.pool, &host_id.try_into().unwrap())
            .await?
            .expect("the Assigned request should persist its target");
    assert_eq!(assigned_desired.value, original_target);
    assert_eq!(
        assigned_desired.version.version_nr(),
        ready_desired.version.version_nr() + 1,
    );

    Ok(())
}

// Hosts with DPU-backed Admin interfaces reject primary targets outside Admin.
#[crate::sqlx_test]
async fn test_set_primary_interface_rejects_non_admin_interface_on_dpu_host(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = api_fixtures::create_test_env(pool).await;

    let host =
        api_fixtures::site_explorer::new_host(&env, ManagedHostConfig::default().with_dpu_count(2))
            .await?;
    let host_id = host.host_snapshot.id;

    // A non-primary host interface to target. The host's other DPU interface
    // stays the Admin primary, so the host still has a DPU-backed admin link.
    let promote_id = {
        let mut txn = env.pool.begin().await?;
        db::machine_interface::find_by_machine_ids(txn.as_mut(), &[host_id])
            .await?
            .remove(&host_id)
            .expect("host should have interface rows")
            .into_iter()
            .find(|i| !i.primary_interface && i.attached_dpu_machine_id.is_some())
            .expect("host should have a non-primary host interface")
            .id
    };

    // Craft the (off-happy-path) mixed shape: move that interface onto a
    // non-admin segment so it is no longer Admin-segment.
    sqlx::query(
        "UPDATE machine_interfaces SET segment_id = \
         (SELECT id FROM network_segments WHERE network_segment_type <> 'admin' LIMIT 1) \
         WHERE id = $1",
    )
    .bind(promote_id)
    .execute(&env.pool)
    .await?;

    let err = env
        .api
        .set_primary_interface(tonic::Request::new(forge::SetPrimaryInterfaceRequest {
            host_machine_id: Some(host_id),
            interface_id: Some(promote_id),
            force_reconcile: false,
            ..Default::default()
        }))
        .await
        .expect_err("promoting a non-admin interface on a DPU host should be rejected");

    assert_eq!(
        err.code(),
        tonic::Code::InvalidArgument,
        "expected an up-front InvalidArgument, got {}: {}",
        err.code(),
        err.message(),
    );
    assert!(
        err.message().contains("admin segment"),
        "expected the Admin-segment guard message, got: {}",
        err.message(),
    );

    Ok(())
}

// Success path on a ZERO-DPU host -- the case this feature exists to enable. A
// zero-DPU host has no DPU-backed admin interface, so neither the zero-DPU guard
// nor the Admin-segment constraint applies, and set_primary_interface can promote
// its plain HostInband NIC. (A zero-DPU host has no primary flag set at ingestion,
// so this records the first primary.)
#[crate::sqlx_test]
async fn test_set_primary_interface_promotes_a_zero_dpu_host_interface(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    // Zero-DPU host ingestion needs a HostInband segment whose CIDR covers the
    // relay address; the default test env doesn't define one.
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
        "set-primary-interface zero-dpu flat vpc",
    )
    .await;
    create_underlay_network_segment(&env.api).await;
    create_admin_network_segment(&env.api).await;
    create_host_inband_network_segment(&env.api, Some(flat_vpc_id)).await;
    env.run_network_segment_controller_iteration().await;
    env.run_network_segment_controller_iteration().await;

    let zero_dpu_host =
        api_fixtures::site_explorer::new_host(&env, ManagedHostConfig::zero_dpu()).await?;
    let host_id = zero_dpu_host.host_snapshot.id;

    // A zero-DPU host's plain NIC lands on the HostInband segment and is not flagged
    // primary at ingestion -- promote it by id.
    let promote_id = {
        let mut txn = env.pool.begin().await?;
        db::machine_interface::find_by_machine_ids(txn.as_mut(), &[host_id])
            .await?
            .remove(&host_id)
            .expect("zero-DPU host should have interface rows")
            .into_iter()
            .find(|i| {
                i.network_segment_type == Some(NetworkSegmentType::HostInband)
                    && !i.primary_interface
            })
            .expect("zero-DPU host should have a non-primary HostInband interface")
            .id
    };

    env.api
        .set_primary_interface(tonic::Request::new(forge::SetPrimaryInterfaceRequest {
            host_machine_id: Some(host_id),
            interface_id: Some(promote_id),
            force_reconcile: false,
            ..Default::default()
        }))
        .await?;

    // Exactly the promoted interface is now primary.
    let primaries_now: Vec<_> = {
        let mut txn = env.pool.begin().await?;
        db::machine_interface::find_by_machine_ids(txn.as_mut(), &[host_id])
            .await?
            .remove(&host_id)
            .expect("zero-DPU host should still have interface rows")
            .into_iter()
            .filter(|i| i.primary_interface)
            .map(|i| i.id)
            .collect()
    };
    assert_eq!(
        primaries_now,
        vec![promote_id],
        "exactly the promoted zero-DPU interface should be primary",
    );

    Ok(())
}

// A DPU-backed host can be left with no admin primary after an interrupted
// repair. Promoting a valid Admin interface must rebuild that ownership rather
// than fail the pre-move reconciliation on the already-broken state.
#[crate::sqlx_test]
async fn test_set_primary_interface_repairs_dpu_host_with_no_admin_primary(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = api_fixtures::create_test_env(pool).await;

    let host =
        api_fixtures::site_explorer::new_host(&env, ManagedHostConfig::default().with_dpu_count(2))
            .await?;
    let host_id = host.host_snapshot.id;

    // The current Admin primary, plus a non-primary Admin interface to promote.
    let (current_primary_id, promote_id) = {
        let mut txn = env.pool.begin().await?;
        let interfaces = db::machine_interface::find_by_machine_ids(txn.as_mut(), &[host_id])
            .await?
            .remove(&host_id)
            .expect("host should have interface rows");
        let current_primary_id = interfaces
            .iter()
            .find(|i| i.primary_interface)
            .expect("host should start with a primary interface")
            .id;
        let promote_id = interfaces
            .iter()
            .find(|i| !i.primary_interface && i.attached_dpu_machine_id.is_some())
            .expect("host should have a non-primary DPU-backed interface")
            .id;
        (current_primary_id, promote_id)
    };

    // Break the happy path: clear the host's primary flag, leaving its DPU-backed
    // admin interfaces with no primary -- the state the pre-move reconcile chokes on.
    sqlx::query("UPDATE machine_interfaces SET primary_interface = false WHERE id = $1")
        .bind(current_primary_id)
        .execute(&env.pool)
        .await?;

    env.api
        .set_primary_interface(tonic::Request::new(forge::SetPrimaryInterfaceRequest {
            host_machine_id: Some(host_id),
            interface_id: Some(promote_id),
            force_reconcile: false,
            ..Default::default()
        }))
        .await?;

    // The promoted interface is now the only primary.
    let primaries_now: Vec<_> = {
        let mut txn = env.pool.begin().await?;
        db::machine_interface::find_by_machine_ids(txn.as_mut(), &[host_id])
            .await?
            .remove(&host_id)
            .expect("host should still have interface rows")
            .into_iter()
            .filter(|i| i.primary_interface)
            .map(|i| i.id)
            .collect()
    };
    assert_eq!(
        primaries_now,
        vec![promote_id],
        "exactly the promoted interface should be primary after the repair",
    );

    Ok(())
}

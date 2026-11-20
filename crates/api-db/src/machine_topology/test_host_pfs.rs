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

use carbide_uuid::machine::{MachineIdSource, MachineType};
use model::hardware_info::{DpuData, DpuHostPf};
use model::machine::{CURRENT_STATE_MODEL_VERSION, ManagedHostState};
use sqlx::types::Json;

use super::*;

/// Creates a standalone BF4 topology without accepted PF facts so tests can exercise first binding.
async fn bf4_topology(pool: &sqlx::PgPool) -> (DpuMachineId, HardwareInfo) {
    // Give VPD a BF4 part number while leaving the new authority column absent.
    let dpu_id = DpuMachineId::try_from(MachineId::new(
        MachineIdSource::ProductBoardChassisSerial,
        [0x82; 32],
        MachineType::Dpu,
    ))
    .expect("DPU ID");
    let hardware_info = HardwareInfo {
        dpu_info: Some(DpuData {
            part_number: "900-9D4B4-CWAA-TSA".to_string(), // Classifies the mapping as required.
            factory_mac_address: "02:00:00:00:00:01".to_string(),
            ..Default::default()
        }),
        ..Default::default()
    };
    let mut txn = pool.begin().await.expect("fixture transaction");
    crate::machine::create(
        &mut txn,
        None,
        dpu_id.as_machine_id(),
        ManagedHostState::Created,
        None,
        CURRENT_STATE_MODEL_VERSION,
    )
    .await
    .expect("fixture DPU");
    create_or_update(&mut txn, dpu_id.as_machine_id(), &hardware_info)
        .await
        .expect("fixture topology");
    txn.commit().await.expect("commit fixture");
    (dpu_id, hardware_info)
}

/// Constructs a PF 0 fixture so tests vary controller identity without changing unrelated VPD.
fn host_pf(controller: u8, mac_address: &str) -> DpuHostPf {
    DpuHostPf {
        pci_controller_id: controller,
        pf_id: 0,
        mac_address: mac_address.parse().expect("fixture MAC"),
    }
}

/// Retries overlapping old JSON writes without waiting, then preserves PF facts alongside firmware and BMC updates through later replacements.
/// Releasing contention before retrying prevents registration from retaining its reporting interface behind an old writer.
#[crate::sqlx_test]
async fn host_pf_inventory_survives_old_json_writers(pool: sqlx::PgPool) {
    // Keep BOM refresh disabled so first-write authority cannot depend on topology_update_needed.
    let (dpu_id, mut hardware_info) = bf4_topology(&pool).await;
    let selected = host_pf(1, "02:00:00:00:00:10");
    hardware_info
        .dpu_info
        .as_mut()
        .expect("fixture DPU")
        .host_pfs = vec![selected.clone()];
    // Hold an old writer's unrelated firmware and BMC changes while the new fact writer starts.
    let mut legacy_hardware = hardware_info.clone();
    legacy_hardware
        .dpu_info
        .as_mut()
        .expect("fixture DPU")
        .host_pfs
        .clear();
    legacy_hardware
        .dpu_info
        .as_mut()
        .expect("fixture DPU")
        .firmware_version = "new-firmware".to_string();
    let replacement = TopologyData {
        discovery_data: DiscoveryData {
            info: legacy_hardware,
        },
        bmc_info: BmcInfo {
            port: Some(8443),
            ..Default::default()
        },
    };
    let mut old_writer = pool.begin().await.expect("old writer transaction");
    sqlx::query("UPDATE machine_topologies SET topology = $2 WHERE machine_id = $1")
        .bind(dpu_id)
        .bind(Json(&replacement))
        .execute(&mut *old_writer)
        .await
        .expect("old writer update");
    // Reject the overlapping attempt promptly and release its aborted transaction before retrying.
    let mut accepting = pool.begin().await.expect("fact transaction");
    let rejected = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        record_dpu_host_pfs(&mut accepting, &dpu_id, &hardware_info, 1),
    )
    .await
    .expect("topology contention must not wait")
    .expect_err("old writer holds topology");
    assert!(rejected.is_lock_not_available());
    accepting
        .rollback()
        .await
        .expect("release rejected attempt");
    old_writer.commit().await.expect("commit old writer");

    // Retry the actual fact write after the old writer releases topology.
    let mut accepting = pool.begin().await.expect("fact retry transaction");
    record_dpu_host_pfs(&mut accepting, &dpu_id, &hardware_info, 1)
        .await
        .expect("accept PF facts after contention");
    accepting.commit().await.expect("commit PF facts");

    // Reload before another replacement can hide a lost firmware or BMC update during first PF persistence.
    let mut read = pool.acquire().await.expect("first-write reader");
    let accepted = find_latest_by_machine_ids(&mut read, &[dpu_id])
        .await
        .expect("find accepted topology")
        .remove(&dpu_id)
        .expect("accepted DPU topology");
    assert_eq!(accepted.topology.bmc_info.port, Some(8443));
    let accepted_info = accepted
        .topology
        .discovery_data
        .info
        .dpu_info
        .expect("accepted DPU info");
    assert_eq!(accepted_info.firmware_version, "new-firmware");
    assert_eq!(accepted_info.host_pfs, vec![selected.clone()]);
    drop(read);

    // A later replacement must also preserve facts accepted after the overlapping old write.
    sqlx::query("UPDATE machine_topologies SET topology = $2 WHERE machine_id = $1")
        .bind(dpu_id)
        .bind(Json(&replacement))
        .execute(&pool)
        .await
        .expect("later old writer update");

    // Load persisted values through both direct topology and JSON-built machine snapshot reads.
    let mut reader = pool.acquire().await.expect("fresh reader");
    let found = find_latest_by_machine_ids(&mut reader, &[dpu_id])
        .await
        .expect("find topology");
    let persisted = &found[&dpu_id];
    assert!(!persisted.topology_update_needed);
    assert_eq!(persisted.topology.bmc_info.port, Some(8443));
    let persisted_info = persisted
        .topology
        .discovery_data
        .info
        .dpu_info
        .as_ref()
        .expect("DPU info");
    assert_eq!(persisted_info.host_pfs, vec![selected.clone()]);
    assert_eq!(persisted_info.firmware_version, "new-firmware");
    let machine = crate::machine::find_one(
        &mut *reader,
        &dpu_id,
        model::machine::machine_search_config::MachineSearchConfig {
            include_dpus: true,
            ..Default::default()
        },
    )
    .await
    .expect("find machine")
    .expect("persisted DPU");
    assert_eq!(
        machine
            .status
            .hardware_info
            .as_ref()
            .expect("hardware")
            .dpu_info
            .as_ref()
            .expect("DPU info")
            .host_pfs,
        vec![selected]
    );
}

/// Verifies nonselected PFs can change or disappear while configured mappings remain protected.
/// Restarting an agent must not fail because an unrelated PF observation changed.
#[crate::sqlx_test]
async fn host_pf_updates_protect_only_authoritative_mappings(pool: sqlx::PgPool) {
    // Accept the configured PF and one unrelated observation.
    let (dpu_id, mut hardware_info) = bf4_topology(&pool).await;
    let selected = host_pf(1, "02:00:00:00:00:10");
    let other = host_pf(2, "02:00:00:00:00:20");
    hardware_info
        .dpu_info
        .as_mut()
        .expect("fixture DPU")
        .host_pfs = vec![selected.clone(), other];
    let mut txn = pool.begin().await.expect("registration transaction");
    record_dpu_host_pfs(&mut txn, &dpu_id, &hardware_info, 1)
        .await
        .expect("initial binding");
    txn.commit().await.expect("commit initial binding");

    // Change the nonselected MAC while omitting the protected PF from the new observation.
    let changed_other = host_pf(2, "02:00:00:00:00:22");
    hardware_info
        .dpu_info
        .as_mut()
        .expect("fixture DPU")
        .host_pfs = vec![changed_other.clone()];
    let mut txn = pool.begin().await.expect("update transaction");
    record_dpu_host_pfs(&mut txn, &dpu_id, &hardware_info, 1)
        .await
        .expect("nonselected change");
    txn.commit().await.expect("commit nonselected change");

    // Verify the changed observation persisted and omission retained the protected mapping.
    let mut reader = pool.acquire().await.expect("fresh reader");
    let found = find_latest_by_machine_ids(&mut reader, &[dpu_id])
        .await
        .expect("find topology");
    assert_eq!(
        found[&dpu_id]
            .dpu_host_pf_inventory
            .as_ref()
            .expect("inventory")
            .host_pfs,
        vec![selected.clone(), changed_other]
    );

    // Remove the nonselected observation, then attempt to replace the configured MAC.
    hardware_info
        .dpu_info
        .as_mut()
        .expect("fixture DPU")
        .host_pfs = vec![selected.clone()];
    let mut txn = pool.begin().await.expect("removal transaction");
    record_dpu_host_pfs(&mut txn, &dpu_id, &hardware_info, 1)
        .await
        .expect("nonselected removal");
    txn.commit().await.expect("commit nonselected removal");
    hardware_info
        .dpu_info
        .as_mut()
        .expect("fixture DPU")
        .host_pfs = vec![host_pf(1, "02:00:00:00:00:11")];
    let mut txn = pool.begin().await.expect("conflicting transaction");
    let error = record_dpu_host_pfs(&mut txn, &dpu_id, &hardware_info, 1)
        .await
        .expect_err("configured MAC change");
    assert!(matches!(error, DatabaseError::FailedPrecondition(_)));
    txn.rollback().await.expect("rollback conflicting mapping");

    // A conflicting configured MAC must leave the accepted mapping and removal intact.
    let found = find_latest_by_machine_ids(&mut reader, &[dpu_id])
        .await
        .expect("find topology");
    assert_eq!(
        found[&dpu_id]
            .dpu_host_pf_inventory
            .as_ref()
            .expect("inventory")
            .host_pfs,
        vec![selected]
    );
}

/// Retains authenticated BF4 classification through missing selected facts and an older raw report, then binds the configured PF only when it is freshly reported.
/// Rejects admission for a valid configured-controller PF1 while retaining its observation, preserving the PF0 prerequisite for registration retries.
#[crate::sqlx_test]
async fn partial_registration_retains_classification_until_selected_pf_arrives(pool: sqlx::PgPool) {
    // Supply PF1 on the configured controller and PF0 on another controller without the required c1pf0.
    let (dpu_id, mut hardware_info) = bf4_topology(&pool).await;
    let partial_observations = vec![
        DpuHostPf {
            pf_id: 1, // Matching controller 1 alone cannot satisfy its PF0 requirement.
            ..host_pf(1, "02:00:00:00:00:11")
        },
        host_pf(2, "02:00:00:00:00:20"),
    ];
    hardware_info
        .dpu_info
        .as_mut()
        .expect("DPU hardware")
        .host_pfs = partial_observations.clone();
    // Register incomplete facts while keeping admission pending for the configured PF0.
    let mut txn = pool.begin().await.expect("partial registration");
    assert!(
        !record_dpu_host_pfs(&mut txn, &dpu_id, &hardware_info, 1)
            .await
            .expect("valid partial facts")
    );
    txn.commit().await.expect("commit classification");

    // Reload committed partial facts before an older report can replace them.
    let mut txn = pool.begin().await.expect("older writer");
    let mut topology = find_latest_by_machine_ids(&mut txn, &[dpu_id])
        .await
        .expect("topology")
        .remove(&dpu_id)
        .expect("DPU");
    let partial_inventory = topology
        .dpu_host_pf_inventory
        .as_ref()
        .expect("authenticated classification");
    assert!(partial_inventory.is_bf4);
    assert_eq!(partial_inventory.host_pfs, partial_observations);
    assert!(partial_inventory.protected_host_pfs.is_empty());

    // Replace the old JSON as an earlier writer can, then submit a report omitting BF4 and all PF observations.
    let mut older = hardware_info.clone();
    let older_dpu = older.dpu_info.as_mut().expect("DPU hardware");
    older_dpu.part_number.clear();
    older_dpu.host_pfs.clear();
    topology.topology.discovery_data.info = older.clone();
    sqlx::query("UPDATE machine_topologies SET topology = $2 WHERE machine_id = $1")
        .bind(dpu_id)
        .bind(Json(&topology.topology))
        .execute(&mut *txn)
        .await
        .expect("replace JSON");
    assert!(
        !record_dpu_host_pfs(&mut txn, &dpu_id, &older, 1)
            .await
            .expect("omitted facts")
    );
    txn.commit().await.expect("commit omitted facts");

    // Re-read persisted classification and prove the configured MAC was never inferred from the partial cache.
    let mut read = pool.begin().await.expect("read classification");
    let persisted = find_latest_by_machine_ids(&mut read, &[dpu_id])
        .await
        .expect("topology")
        .remove(&dpu_id)
        .expect("DPU");
    let inventory = persisted
        .dpu_host_pf_inventory
        .expect("retained classification");
    assert!(inventory.is_bf4);
    assert!(inventory.protected_host_pfs.is_empty());
    read.rollback().await.expect("close read");

    // Fresh selected evidence permits registration and becomes protected authority.
    hardware_info
        .dpu_info
        .as_mut()
        .expect("DPU hardware")
        .host_pfs = vec![host_pf(1, "02:00:00:00:00:10")];
    let mut txn = pool.begin().await.expect("fresh registration");
    assert!(
        record_dpu_host_pfs(&mut txn, &dpu_id, &hardware_info, 1)
            .await
            .expect("selected facts")
    );
    txn.commit().await.expect("commit binding");
    let mut read = pool.begin().await.expect("read binding");
    let bound = find_latest_by_machine_ids(&mut read, &[dpu_id])
        .await
        .expect("topology")
        .remove(&dpu_id)
        .expect("DPU");
    assert_eq!(
        bound
            .dpu_host_pf_inventory
            .expect("bound inventory")
            .protected_host_pfs,
        vec![host_pf(1, "02:00:00:00:00:10")]
    );
    read.rollback().await.expect("close read");
}

/// Keeps first PF facts mandatory through failed initial setup and reingestion while permitting reset teardown and retained post-initial recovery, without weakening protected MAC checks.
/// Defers implicit boot publication during the same initial states so an early failure cannot expose the provisional factory MAC.
#[crate::sqlx_test]
async fn missing_first_pf_registration_follows_initialization_and_recovery(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    use model::address_selection_strategy::AddressSelectionStrategy;
    use model::machine::{
        DpuDiscoveringState, DpuDiscoveringStates, FailureCause, FailureDetails, FailureSource,
        MachineState, ResetState,
    };
    use model::machine_interface_address::MachineInterfaceAssociation;
    use model::network_segment::NetworkSegmentControllerState;

    // Attach a never-initialized host without PF facts; the remaining inventory supplies real group membership.
    let (dpu_id, mut hardware_info) = bf4_topology(&pool).await;
    let host_id: HostMachineId =
        MachineId::new(MachineIdSource::Tpm, [0x84; 32], MachineType::Host).try_into()?;
    let mut setup = pool.begin().await?;
    crate::machine::create(
        &mut setup,
        None,
        &host_id,
        ManagedHostState::Created,
        None,
        CURRENT_STATE_MODEL_VERSION,
    )
    .await?;
    let segment = crate::network_segment::persist(
        crate::test_support::network_segment::admin_segment(
            "registration-recovery-pf",
            "192.0.2.0/24",
            "192.0.2.1",
            3,
        ),
        &mut setup,
        NetworkSegmentControllerState::Ready,
    )
    .await?;
    let link = crate::machine_interface::create(
        &mut setup,
        &[segment],
        &"02:00:00:00:00:01".parse()?,
        true,
        AddressSelectionStrategy::NextAvailableIp,
        None,
    )
    .await?;
    crate::machine_interface::associate_interface_with_machine(
        &link.id,
        MachineInterfaceAssociation::Machine(host_id.into()),
        &mut setup,
    )
    .await?;
    crate::machine_interface::associate_interface_with_dpu_machine(
        &link.id,
        dpu_id.as_machine_id(),
        &mut setup,
    )
    .await?;
    setup.commit().await?;
    let failed = ManagedHostState::Failed {
        details: FailureDetails {
            cause: FailureCause::Discovery {
                err: "registration recovery fixture".to_string(),
            },
            failed_at: Utc::now(),
            source: FailureSource::NoError, // Only actual retained lifecycle history can qualify this failure.
        },
        machine_id: host_id.into(),
        retry_count: 0,
    };

    // Advance the same host so the last case has actual HostInit history that the earlier failure lacks.
    for (scenario, state, accepted) in [
        // A newly attached host cannot stop the agent's acquisition retry before initialization.
        ("created", ManagedHostState::Created, false),
        // An interrupted first initialization is not evidence that the checkpoint completed.
        ("failed before initialization", failed.clone(), false),
        // Reset teardown needs a running agent to acknowledge Admin networking before reingestion.
        (
            "reset teardown",
            ManagedHostState::Reset {
                reset_state: ResetState::DeletingCrs,
            },
            true,
        ),
        // Returning from teardown reinstates first-binding requirements before another HostInit entry.
        (
            "reset reingestion",
            ManagedHostState::DpuDiscoveringState {
                dpu_states: DpuDiscoveringStates {
                    states: HashMap::from([(dpu_id, DpuDiscoveringState::Initializing)]),
                },
            },
            false,
        ),
        // HostInit is already beyond PF finalization and must permit older-agent registration.
        (
            "host initialization",
            ManagedHostState::HostInit {
                machine_state: MachineState::EnableIpmiOverLan,
            },
            true,
        ),
        // Persisted HostInit history distinguishes later failure recovery from the earlier initial failure.
        ("failed after initialization", failed, true),
    ] {
        let mut transition = pool.begin().await?;
        crate::machine::update_state(&mut transition, &host_id, &state).await?;
        transition.commit().await?;

        // Register missing first facts and commit valid BF4 classification even when admission remains pending.
        let mut registration = pool.begin().await?;
        assert_eq!(
            record_dpu_host_pfs(&mut registration, &dpu_id, &hardware_info, 1).await?,
            accepted,
            "{scenario}"
        );
        registration.commit().await?;

        // Reload retained facts to prove compatibility never invents a PF mapping or changes the current lifecycle state.
        let mut read = pool.acquire().await?;
        let inventory = find_latest_by_machine_ids(&mut read, &[dpu_id])
            .await?
            .remove(&dpu_id)
            .expect("DPU topology")
            .dpu_host_pf_inventory
            .expect("BF4 classification");
        assert!(inventory.is_bf4, "{scenario}");
        assert!(inventory.host_pfs.is_empty(), "{scenario}");
        assert!(inventory.protected_host_pfs.is_empty(), "{scenario}");
        assert_eq!(
            crate::machine::lookup_managed_host_state(&mut *read, host_id.into()).await?,
            Some(state),
            "{scenario}"
        );

        // Check publisher deferral against persisted lifecycle history, including failure before and after HostInit.
        assert_eq!(
            crate::machine::bf4_host_pf_initialization_pending(&mut read, &host_id).await?,
            !accepted,
            "{scenario}"
        );
    }

    // Bind PF facts during later recovery, then introduce a conflicting observation of that protected mapping.
    hardware_info
        .dpu_info
        .as_mut()
        .expect("BF4 hardware")
        .host_pfs = vec![host_pf(1, "02:00:00:00:00:10")];
    let mut accepted = pool.begin().await?;
    assert!(record_dpu_host_pfs(&mut accepted, &dpu_id, &hardware_info, 1).await?);
    accepted.commit().await?;
    hardware_info
        .dpu_info
        .as_mut()
        .expect("BF4 hardware")
        .host_pfs = vec![host_pf(1, "02:00:00:00:00:11")];
    let mut conflict = pool.begin().await?;
    // A newly configured controller lacks first facts, so the Failed-history exemption must still check the old protected PF.
    assert!(matches!(
        record_dpu_host_pfs(&mut conflict, &dpu_id, &hardware_info, 2).await,
        Err(DatabaseError::FailedPrecondition(_))
    ));
    conflict.rollback().await?;

    // A fresh read must retain the originally protected mapping after the failed replacement.
    let mut read = pool.acquire().await?;
    let retained = find_latest_by_machine_ids(&mut read, &[dpu_id])
        .await?
        .remove(&dpu_id)
        .expect("DPU topology")
        .dpu_host_pf_inventory
        .expect("protected inventory");
    assert_eq!(
        retained.protected_host_pfs,
        vec![host_pf(1, "02:00:00:00:00:10")]
    );
    Ok(())
}

/// Protects a previously nonselected authenticated PF after Operator boot intent names its MAC, so later reports cannot replace or erase accepted authority without changing networking.
/// Protected PF1 cannot admit registration after a controller change without PF0, so Operator boot intent cannot waive the fresh-PF prerequisite.
#[crate::sqlx_test]
async fn operator_boot_authority_protects_previously_nonselected_pf(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    use model::address_selection_strategy::AddressSelectionStrategy;
    use model::machine::{DpuInitState, DpuInitStates};
    use model::machine_boot_interface::{BootInterfaceSelectionSource, MachineBootInterfaceTarget};
    use model::machine_interface_address::MachineInterfaceAssociation;
    use model::network_segment::NetworkSegmentControllerState;

    // Keep the host in DPUInit with no Instance so admitted-state protection cannot mask the Operator predicate.
    let (dpu_id, mut hardware_info) = bf4_topology(&pool).await;
    let host_id: carbide_uuid::machine::HostMachineId =
        MachineId::new(MachineIdSource::Tpm, [0x83; 32], MachineType::Host).try_into()?;
    let state = ManagedHostState::DPUInit {
        dpu_states: DpuInitStates {
            states: std::collections::HashMap::from([(
                dpu_id,
                DpuInitState::WaitingForNetworkConfig,
            )]),
        },
    };
    let selected = host_pf(1, "02:00:00:00:00:10");
    let nonselected = DpuHostPf {
        pf_id: 1, // Keep Operator boot authority distinct from configured PF0 admission.
        ..host_pf(2, "02:00:00:00:00:20")
    };
    let mut setup = pool.begin().await?;
    crate::machine::create(
        &mut setup,
        None,
        &host_id,
        state.clone(),
        None,
        CURRENT_STATE_MODEL_VERSION,
    )
    .await?;
    let segment = crate::network_segment::persist(
        crate::test_support::network_segment::admin_segment(
            "operator-boot-pf",
            "192.0.2.0/24",
            "192.0.2.1",
            3,
        ),
        &mut setup,
        NetworkSegmentControllerState::Ready,
    )
    .await?;
    let link = crate::machine_interface::create(
        &mut setup,
        &[segment],
        &selected.mac_address,
        true,
        AddressSelectionStrategy::NextAvailableIp,
        None,
    )
    .await?;
    crate::machine_interface::associate_interface_with_machine(
        &link.id,
        MachineInterfaceAssociation::Machine(host_id.into()),
        &mut setup,
    )
    .await?;
    crate::machine_interface::associate_interface_with_dpu_machine(
        &link.id,
        dpu_id.as_machine_id(),
        &mut setup,
    )
    .await?;
    hardware_info
        .dpu_info
        .as_mut()
        .expect("DPU hardware")
        .host_pfs = vec![selected.clone(), nonselected.clone()];
    record_dpu_host_pfs(&mut setup, &dpu_id, &hardware_info, 1).await?;
    setup.commit().await?;

    // Reload the accepted observations to establish that the nonselected PF was mutable before boot authority was published.
    let mut read = pool.acquire().await?;
    let accepted = find_latest_by_machine_ids(&mut read, &[dpu_id])
        .await?
        .remove(&dpu_id)
        .expect("accepted topology")
        .dpu_host_pf_inventory
        .expect("accepted inventory");
    assert_eq!(
        accepted.host_pfs,
        vec![selected.clone(), nonselected.clone()]
    );
    assert_eq!(accepted.protected_host_pfs, vec![selected.clone()]);

    // Publish boot-only Operator intent after both observations are accepted, leaving networking on the configured PF.
    let mut intent = pool.begin().await?;
    crate::machine_desired_boot_interface::set(
        &mut intent,
        &host_id,
        &MachineBootInterfaceTarget::MacOnly(nonselected.mac_address),
        BootInterfaceSelectionSource::Operator,
    )
    .await?;
    intent.commit().await?;
    let before = crate::machine::find_one(&pool, &host_id, Default::default())
        .await?
        .expect("host with Operator intent");
    assert_eq!(before.state.value, state);
    assert_eq!(
        before
            .config
            .boot_interface_selection
            .expect("Operator source")
            .source,
        BootInterfaceSelectionSource::Operator
    );
    assert_eq!(
        before
            .config
            .desired_boot_interface
            .as_ref()
            .expect("Operator boot target")
            .value
            .mac_address(),
        nonselected.mac_address
    );

    // Replacement must fail before inventory publication even though this PF was not initially protected.
    hardware_info
        .dpu_info
        .as_mut()
        .expect("DPU hardware")
        .host_pfs = vec![
        selected.clone(),
        DpuHostPf {
            mac_address: "02:00:00:00:00:22".parse()?, // Change only the Operator-selected PF1 MAC.
            ..nonselected.clone()
        },
    ];
    let mut replacement = pool.begin().await?;
    let error = record_dpu_host_pfs(&mut replacement, &dpu_id, &hardware_info, 1)
        .await
        .expect_err("Operator-authoritative PF replacement");
    assert!(
        matches!(error, DatabaseError::FailedPrecondition(message) if message.contains("c2pf1"))
    );
    replacement.rollback().await?;
    let retained = find_latest_by_machine_ids(&mut read, &[dpu_id])
        .await?
        .remove(&dpu_id)
        .expect("topology after rejection")
        .dpu_host_pf_inventory
        .expect("inventory after rejection");
    assert_eq!(retained, accepted);

    // Omission must succeed while promoting the accepted mapping into persisted protection.
    hardware_info
        .dpu_info
        .as_mut()
        .expect("DPU hardware")
        .host_pfs = vec![selected.clone()];
    let mut omission = pool.begin().await?;
    record_dpu_host_pfs(&mut omission, &dpu_id, &hardware_info, 1).await?;
    omission.commit().await?;
    let retained = find_latest_by_machine_ids(&mut read, &[dpu_id])
        .await?
        .remove(&dpu_id)
        .expect("topology after omission")
        .dpu_host_pf_inventory
        .expect("inventory after omission");
    assert_eq!(retained.host_pfs, accepted.host_pfs);
    assert_eq!(
        retained.protected_host_pfs,
        vec![selected.clone(), nonselected]
    );

    // Configure controller 2 after c2pf1 protection commits, leaving no current or protected c2pf0.
    let mut reconfigured = pool.begin().await?;
    assert!(!record_dpu_host_pfs(&mut reconfigured, &dpu_id, &hardware_info, 2).await?);
    reconfigured.commit().await?;

    // Pending PF0 admission must retain the protected PF1 observation and its accepted MAC.
    let pending = find_latest_by_machine_ids(&mut read, &[dpu_id])
        .await?
        .remove(&dpu_id)
        .expect("topology after controller change")
        .dpu_host_pf_inventory
        .expect("inventory after controller change");
    assert_eq!(pending, retained);

    // Reload the host to prove registration preserved the independent boot decision and networking primary throughout.
    let after = crate::machine::find_one(&pool, &host_id, Default::default())
        .await?
        .expect("host after reports");
    assert_eq!(after.state.value, state);
    assert_eq!(
        after
            .status
            .interfaces
            .iter()
            .find(|row| row.primary_interface)
            .expect("networking primary")
            .mac_address,
        selected.mac_address
    );
    assert_eq!(
        after.config.boot_interface_selection,
        before.config.boot_interface_selection
    );
    assert_eq!(
        after
            .config
            .desired_boot_interface
            .as_ref()
            .map(|target| (&target.value, target.version)),
        before
            .config
            .desired_boot_interface
            .as_ref()
            .map(|target| (&target.value, target.version))
    );
    Ok(())
}

/// Protects previously nonselected PF observations in HostInit when automatic boot intent or the DPU-attached primary names their MAC, preventing independent networking and boot decisions from losing accepted facts.
#[crate::sqlx_test]
async fn admitted_host_authority_protects_previously_nonselected_pfs(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    use model::address_selection_strategy::AddressSelectionStrategy;
    use model::machine::MachineState;
    use model::machine_boot_interface::{BootInterfaceSelectionSource, MachineBootInterfaceTarget};
    use model::machine_interface_address::MachineInterfaceAssociation;
    use model::network_segment::NetworkSegmentControllerState;

    for (scenario, protects_network_primary, identity) in [
        // Automatic boot intent needs HostInit protection without relying on the Operator predicate or networking MAC.
        ("automatic boot authority after HostInit", false, 0x85),
        // An independent boot MAC leaves only the DPU-attached primary able to protect the nonselected PF.
        ("networking authority independent of boot", true, 0x86),
    ] {
        // Vary which published decision names the nonselected PF; the remaining fixture supplies a retained HostInit link without an Instance.
        let host_id: HostMachineId =
            MachineId::new(MachineIdSource::Tpm, [identity; 32], MachineType::Host).try_into()?;
        let dpu_id: DpuMachineId = MachineId::new(
            MachineIdSource::ProductBoardChassisSerial,
            [identity; 32],
            MachineType::Dpu,
        )
        .try_into()?;
        let state = ManagedHostState::HostInit {
            machine_state: MachineState::EnableIpmiOverLan,
        };
        let configured = host_pf(1, &format!("02:00:00:00:{identity:02x}:10"));
        let nonselected = host_pf(2, &format!("02:00:00:00:{identity:02x}:20"));
        let network_primary = if protects_network_primary {
            &nonselected
        } else {
            &configured
        };
        let mut hardware_info = HardwareInfo {
            dpu_info: Some(DpuData {
                part_number: "900-9D4B4-CWAA-TSA".to_string(),
                factory_mac_address: network_primary.mac_address.to_string(), // Retain a bootstrap link admitted before PF facts.
                host_pfs: vec![configured.clone(), nonselected.clone()], // Only controller 1 is configured.
                ..Default::default()
            }),
            ..Default::default()
        };
        let mut setup = pool.begin().await?;
        for id in [host_id.to_machine_id(), dpu_id.to_machine_id()] {
            crate::machine::create(
                &mut setup,
                None,
                &id,
                state.clone(),
                None,
                CURRENT_STATE_MODEL_VERSION,
            )
            .await?;
        }
        let segment = crate::network_segment::persist(
            crate::test_support::network_segment::admin_segment(
                &format!("published-pf-authority-{identity}"),
                &format!("192.0.{identity}.0/24"),
                &format!("192.0.{identity}.1"),
                3,
            ),
            &mut setup,
            NetworkSegmentControllerState::Ready,
        )
        .await?;
        let link = crate::machine_interface::create(
            &mut setup,
            &[segment],
            &network_primary.mac_address,
            true,
            AddressSelectionStrategy::NextAvailableIp,
            None,
        )
        .await?;
        crate::machine_interface::associate_interface_with_machine(
            &link.id,
            MachineInterfaceAssociation::Machine(host_id.into()),
            &mut setup,
        )
        .await?;
        crate::machine_interface::associate_interface_with_dpu_machine(
            &link.id,
            dpu_id.as_machine_id(),
            &mut setup,
        )
        .await?;
        create_or_update(&mut setup, dpu_id.as_machine_id(), &hardware_info).await?;

        // Accept both PF observations before the next report derives protection from the published decisions.
        record_dpu_host_pfs(&mut setup, &dpu_id, &hardware_info, 1).await?;
        setup.commit().await?;

        // Reload the first inventory so preexisting protection cannot make either authority predicate unnecessary.
        let mut read = pool.acquire().await?;
        let accepted = find_latest_by_machine_ids(&mut read, &[dpu_id])
            .await?
            .remove(&dpu_id)
            .expect("accepted topology")
            .dpu_host_pf_inventory
            .expect("accepted inventory");
        assert_eq!(
            accepted.host_pfs,
            vec![configured.clone(), nonselected.clone()],
            "{scenario}"
        );
        assert_eq!(accepted.protected_host_pfs, vec![configured.clone()]);
        assert!(
            crate::instance::find_id_by_machine_id(&mut *read, host_id.as_machine_id())
                .await?
                .is_none(),
            "{scenario}"
        );
        drop(read);

        // Publish an automatic nonselected boot target or Operator intent for a different MAC before the next report.
        let (boot_mac, boot_source) = if protects_network_primary {
            (
                format!("02:00:00:00:{identity:02x}:30").parse()?,
                BootInterfaceSelectionSource::Operator,
            )
        } else {
            (
                nonselected.mac_address,
                BootInterfaceSelectionSource::ExpectedMachine,
            )
        };
        let mut intent = pool.begin().await?;
        crate::machine_desired_boot_interface::set(
            &mut intent,
            &host_id,
            &MachineBootInterfaceTarget::MacOnly(boot_mac),
            boot_source,
        )
        .await?;
        intent.commit().await?;

        // Find both decisions after commit to isolate automatic boot authority from networking authority.
        let before = crate::machine::find_one(&pool, &host_id, Default::default())
            .await?
            .expect("host with published authority");
        assert_eq!(before.state.value, state, "{scenario}");
        let published_primary = before
            .status
            .interfaces
            .iter()
            .find(|row| row.primary_interface)
            .expect("networking primary");
        assert_eq!(
            (
                published_primary.mac_address,
                published_primary.attached_dpu_machine_id,
            ),
            (network_primary.mac_address, Some(dpu_id)),
            "{scenario}"
        );
        assert_eq!(
            before
                .config
                .desired_boot_interface
                .as_ref()
                .expect("boot target")
                .value,
            MachineBootInterfaceTarget::MacOnly(boot_mac),
            "{scenario}"
        );
        assert_eq!(
            before
                .config
                .boot_interface_selection
                .expect("boot source")
                .source,
            boot_source,
            "{scenario}"
        );

        // Reject replacement before an omission can persist protection and hide a missing authority check.
        hardware_info
            .dpu_info
            .as_mut()
            .expect("BF4 hardware")
            .host_pfs = vec![
            configured.clone(),
            host_pf(2, &format!("02:00:00:00:{identity:02x}:22")),
        ];
        let mut replacement = pool.begin().await?;
        let error = record_dpu_host_pfs(&mut replacement, &dpu_id, &hardware_info, 1)
            .await
            .expect_err("published PF replacement");
        assert!(
            matches!(error, DatabaseError::FailedPrecondition(message) if message.contains("c2pf0")),
            "{scenario}"
        );
        replacement.rollback().await?;

        // Find the committed inventory to prove the rejected replacement preserved every accepted PF.
        let mut read = pool.acquire().await?;
        let retained = find_latest_by_machine_ids(&mut read, &[dpu_id])
            .await?
            .remove(&dpu_id)
            .expect("topology after rejection")
            .dpu_host_pf_inventory
            .expect("inventory after rejection");
        assert_eq!(retained, accepted, "{scenario}");
        drop(read);

        // Commit omission to prove the published decision retains its original MAC in durable protection.
        hardware_info
            .dpu_info
            .as_mut()
            .expect("BF4 hardware")
            .host_pfs = vec![configured.clone()];
        let mut omission = pool.begin().await?;
        assert!(record_dpu_host_pfs(&mut omission, &dpu_id, &hardware_info, 1).await?);
        omission.commit().await?;

        // Reload durable protection so omission cannot erase the MAC named by either published decision.
        let mut read = pool.acquire().await?;
        let retained = find_latest_by_machine_ids(&mut read, &[dpu_id])
            .await?
            .remove(&dpu_id)
            .expect("topology after omission")
            .dpu_host_pf_inventory
            .expect("inventory after omission");
        assert_eq!(retained.host_pfs, accepted.host_pfs, "{scenario}");
        assert_eq!(
            retained.protected_host_pfs,
            vec![configured, nonselected],
            "{scenario}"
        );
    }
    Ok(())
}

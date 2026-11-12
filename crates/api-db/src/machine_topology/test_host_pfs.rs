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

/// Verifies PF facts survive overlapping and later old JSON writes without losing firmware or BMC updates.
/// The authority column must preserve both writers' fields during a rolling upgrade.
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
    let blocker: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *old_writer)
        .await
        .expect("old writer PID");
    sqlx::query("UPDATE machine_topologies SET topology = $2 WHERE machine_id = $1")
        .bind(dpu_id)
        .bind(Json(&replacement))
        .execute(&mut *old_writer)
        .await
        .expect("old writer update");
    let accepting_pool = pool.clone();
    let reported_hardware = hardware_info.clone();
    let accepting = tokio::spawn(async move {
        let mut txn = accepting_pool.begin().await.expect("fact transaction");
        record_dpu_host_pfs(&mut txn, &dpu_id, &reported_hardware, 1)
            .await
            .expect("accept PF facts");
        txn.commit().await.expect("commit PF facts");
    });

    // Confirm overlap before allowing the old writer to finish, then wait for the fact commit.
    crate::test_support::postgres::wait_for_blocked_query(&pool, blocker, "machine_topologies")
        .await;
    old_writer.commit().await.expect("commit old writer");
    accepting.await.expect("fact writer task");

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
#[crate::sqlx_test]
async fn partial_registration_retains_classification_until_selected_pf_arrives(pool: sqlx::PgPool) {
    // Supply a valid nonselected observation without the configured controller's PF.
    let (dpu_id, mut hardware_info) = bf4_topology(&pool).await;
    hardware_info
        .dpu_info
        .as_mut()
        .expect("DPU hardware")
        .host_pfs = vec![host_pf(2, "02:00:00:00:00:20")];
    let mut txn = pool.begin().await.expect("partial registration");
    assert!(
        !record_dpu_host_pfs(&mut txn, &dpu_id, &hardware_info, 1)
            .await
            .expect("valid partial facts")
    );
    txn.commit().await.expect("commit classification");

    // Replace the old JSON as an earlier writer can, then submit a report omitting BF4 and all PF observations.
    let mut txn = pool.begin().await.expect("older writer");
    let mut topology = find_latest_by_machine_ids(&mut txn, &[dpu_id])
        .await
        .expect("topology")
        .remove(&dpu_id)
        .expect("DPU");
    assert!(
        topology
            .dpu_host_pf_inventory
            .as_ref()
            .expect("authenticated classification")
            .is_bf4
    );
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

/// Protects a previously nonselected authenticated PF after Operator boot intent names its MAC, so later reports cannot replace or erase accepted authority without changing networking.
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
    let nonselected = host_pf(2, "02:00:00:00:00:20");
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
        .host_pfs = vec![selected.clone(), host_pf(2, "02:00:00:00:00:22")];
    let mut replacement = pool.begin().await?;
    let error = record_dpu_host_pfs(&mut replacement, &dpu_id, &hardware_info, 1)
        .await
        .expect_err("Operator-authoritative PF replacement");
    assert!(
        matches!(error, DatabaseError::FailedPrecondition(message) if message.contains("c2pf0"))
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

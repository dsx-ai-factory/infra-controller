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
use model::machine::machine_search_config::MachineSearchConfig;
use model::machine::{CURRENT_STATE_MODEL_VERSION, DpuInitStates};
use model::network_segment::NetworkSegmentControllerState;

use super::*;

/// Supplies one unallocated BF4 host with leased provisional networking so repairs exercise real address ownership.
struct HostPfFixture {
    host_id: HostMachineId,
    dpu_id: DpuMachineId,
    old_interface: MachineInterfaceSnapshot,
    state_version: ConfigVersion,
    network_version: ConfigVersion,
    hardware_info: HardwareInfo,
    plan: HostPfInterfacePlan,
}

/// Commits a provisional host/DPU link and authenticated corrected MAC so tests vary only the repair's failure or allocation condition.
async fn fixture(pool: &sqlx::PgPool) -> Result<HostPfFixture, Box<dyn std::error::Error>> {
    let host_id: HostMachineId =
        MachineId::new(MachineIdSource::Tpm, [0x91; 32], MachineType::Host)
            .try_into()
            .expect("host ID");
    let dpu_id: DpuMachineId = MachineId::new(
        MachineIdSource::ProductBoardChassisSerial,
        [0x92; 32],
        MachineType::Dpu,
    )
    .try_into()
    .expect("DPU ID");
    let state = ManagedHostState::DPUInit {
        dpu_states: DpuInitStates {
            states: HashMap::from([(dpu_id, DpuInitState::WaitingForNetworkConfig)]),
        },
    };
    let old_mac = MacAddress::new([2, 0, 0, 0, 0x91, 1]);
    let corrected_mac = MacAddress::new([2, 0, 0, 0, 0x91, 2]);
    let hardware_info = HardwareInfo {
        dpu_info: Some(DpuData {
            part_number: "900-9D4B4-CWAA-TSA".to_string(), // BF4 requires authenticated PF facts.
            factory_mac_address: old_mac.to_string(),
            host_pfs: vec![DpuHostPf {
                pci_controller_id: 1,
                pf_id: 0,
                mac_address: corrected_mac,
            }],
            ..Default::default()
        }),
        ..Default::default()
    };
    // Create both machines and a ready Admin segment before leasing the provisional host link.
    let mut txn = pool.begin().await?;
    for id in [host_id.to_machine_id(), dpu_id.to_machine_id()] {
        crate::machine::create(
            &mut txn,
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
            "host-pf-admin",
            "192.0.2.0/24",
            "192.0.2.1",
            3,
        ),
        &mut txn,
        NetworkSegmentControllerState::Ready,
    )
    .await?;
    // Lease the factory-MAC link and associate it with the host and DPU to model provisional Admin networking.
    let old_interface = create(
        &mut txn,
        &[segment],
        &old_mac,
        true,
        AddressSelectionStrategy::NextAvailableIp,
        None,
    )
    .await?;
    associate_interface_with_machine(
        &old_interface.id,
        MachineInterfaceAssociation::Machine(host_id.to_machine_id()),
        &mut txn,
    )
    .await?;
    associate_interface_with_dpu_machine(&old_interface.id, dpu_id.as_machine_id(), &mut txn)
        .await?;
    set_boot_interface_id(old_mac, "NIC.Old", &mut txn).await?;
    // Record accepted PF facts separately from topology because discovery data alone cannot authorize repair.
    crate::machine_topology::create_or_update(&mut txn, dpu_id.as_machine_id(), &hardware_info)
        .await?;
    crate::machine_topology::record_dpu_host_pfs(&mut txn, &dpu_id, &hardware_info, 1).await?;

    // Give the provisional link independent automatic boot and networking decisions to model Explorer ingestion.
    crate::machine_desired_boot_interface::set(
        &mut txn,
        &host_id,
        &MachineBootInterfaceTarget::Pair(model::machine_boot_interface::MachineBootInterface {
            mac_address: old_mac,
            interface_id: "NIC.Old".to_string(),
        }),
        BootInterfaceSelectionSource::RedfishSerialNumber,
    )
    .await?;
    crate::machine_interface::set_primary_interface_selection(
        &mut txn,
        &host_id,
        BootInterfaceSelectionSource::RedfishSerialNumber,
        true,
    )
    .await?;
    let host = crate::machine::find_one(&mut *txn, &host_id, MachineSearchConfig::default())
        .await?
        .expect("host");
    txn.commit().await?;
    Ok(HostPfFixture {
        host_id,
        dpu_id,
        old_interface,
        state_version: host.state.version,
        network_version: host.network_config.version,
        hardware_info,
        plan: HostPfInterfacePlan {
            links: vec![RegisteredHostPf {
                dpu_id,
                mac_address: corrected_mac,
                boot_interface_id: None,
            }],
            network_primary_mac: corrected_mac,
            network_selection_source: BootInterfaceSelectionSource::RedfishSerialNumber,
            boot_target: MachineBootInterfaceTarget::MacOnly(corrected_mac),
            boot_selection_source: BootInterfaceSelectionSource::RedfishSerialNumber,
        },
    })
}

/// Moves DHCP to the corrected MAC with MAC-only boot targeting, enriches settled PF metadata without republishing networking, and keeps repeat repair and registration idempotent so bootstrap data cannot undo finalization.
///
/// Preserving the network version avoids requiring another DPU acknowledgment for ID or source enrichment.
#[crate::sqlx_test]
async fn repair_moves_dhcp_and_repeated_registration_retains_corrected_link(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    // Keep the old Redfish ID distinct, and model a migrated primary whose decision time is unknown.
    let mut fixture = fixture(&pool).await?;
    fixture.plan.network_selection_source = BootInterfaceSelectionSource::LegacyUnknown;
    sqlx::query("UPDATE machine_interfaces SET primary_selection_source = 'legacy_unknown', primary_selection_updated_at = NULL WHERE id = $1")
        .bind(fixture.old_interface.id).execute(&pool).await?;
    let before = crate::machine::find_one(&pool, &fixture.host_id, MachineSearchConfig::default())
        .await?
        .expect("host before repair")
        .config
        .primary_interface_selection;
    let admission = admin_lock_admission().await;
    let mut txn = pool.begin().await?;
    assert!(
        lock_host_pf_initialization(
            &mut txn,
            &fixture.host_id,
            fixture.state_version,
            &[fixture.dpu_id],
            &[fixture.plan.network_primary_mac]
        )
        .await?
    );

    // Commit one repair before reading the persisted rows through the normal find APIs.
    assert!(repair_host_pf_interfaces(&mut txn, &fixture.host_id, &fixture.plan, None).await?);
    txn.commit().await?;
    drop(admission);
    let mut read = pool.begin().await?;
    let corrected = find_by_mac_address(&mut *read, fixture.plan.network_primary_mac)
        .await?
        .remove(0);
    let retired = find_one(&mut *read, fixture.old_interface.id).await?;
    let host =
        crate::machine::find_one(&mut *read, &fixture.host_id, MachineSearchConfig::default())
            .await?
            .expect("host");
    assert_eq!(corrected.machine_id, Some(fixture.host_id.to_machine_id()));
    assert_eq!(corrected.attached_dpu_machine_id, Some(fixture.dpu_id));
    assert!(corrected.primary_interface);
    assert_eq!(corrected.addresses, fixture.old_interface.addresses);
    assert!(corrected.boot_interface_id.is_none());
    assert!(!retired.primary_interface);
    assert!(retired.addresses.is_empty());
    assert!(retired.attached_dpu_machine_id.is_none());
    assert_eq!(retired.boot_interface_id.as_deref(), Some("NIC.Old"));
    assert_ne!(host.network_config.version, fixture.network_version);
    // A corrected PF changes the MAC while preserving the selected DPU and its unknown legacy decision time.
    assert_eq!(host.config.primary_interface_selection, before);
    assert!(
        host.config
            .primary_interface_selection
            .expect("migrated decision")
            .updated_at
            .is_none()
    );
    // Inspect both physical rows so a machine-level value cannot mask a missing transfer or stale authority.
    assert_eq!(
        persisted_primary_selection(&mut read, corrected.id).await?,
        before
    );
    assert!(
        persisted_primary_selection(&mut read, retired.id)
            .await?
            .is_none()
    );
    assert_eq!(
        crate::machine_desired_boot_interface::get(&mut *read, &fixture.host_id)
            .await?
            .expect("desired")
            .value,
        fixture.plan.boot_target
    );
    read.commit().await?;

    // Supply a report-resolved Redfish ID and UEFI selection sources while retaining the settled PF MAC.
    fixture.plan.links[0].boot_interface_id = Some("NIC.Corrected.1".to_string());
    fixture.plan.network_selection_source = BootInterfaceSelectionSource::RedfishUefiPci;
    fixture.plan.boot_target =
        MachineBootInterfaceTarget::Pair(model::machine_boot_interface::MachineBootInterface {
            mac_address: fixture.plan.network_primary_mac,
            interface_id: "NIC.Corrected.1".to_string(),
        });
    fixture.plan.boot_selection_source = BootInterfaceSelectionSource::RedfishUefiPci;

    // Commit metadata changes under the same production locks used for the initial network repair.
    let admission = admin_lock_admission().await;
    let mut txn = pool.begin().await?;
    assert!(
        lock_host_pf_initialization(
            &mut txn,
            &fixture.host_id,
            fixture.state_version,
            &[fixture.dpu_id],
            &[fixture.plan.network_primary_mac]
        )
        .await?
    );
    assert!(repair_host_pf_interfaces(&mut txn, &fixture.host_id, &fixture.plan, None).await?);
    txn.commit().await?;
    drop(admission);

    // Reload the interface and host to prove metadata persisted without another network publication or lease move.
    let mut read = pool.begin().await?;
    let enriched = find_by_mac_address(&mut *read, fixture.plan.network_primary_mac)
        .await?
        .remove(0);
    let enriched_host =
        crate::machine::find_one(&mut *read, &fixture.host_id, MachineSearchConfig::default())
            .await?
            .expect("host after metadata enrichment");
    assert_eq!(enriched.id, corrected.id);
    assert_eq!(enriched.mac_address, corrected.mac_address);
    assert_eq!(enriched.machine_id, corrected.machine_id);
    assert_eq!(
        enriched.attached_dpu_machine_id,
        corrected.attached_dpu_machine_id
    );
    assert_eq!(enriched.primary_interface, corrected.primary_interface);
    assert_eq!(enriched.addresses, corrected.addresses);
    assert_eq!(
        enriched.boot_interface_id.as_deref(),
        Some("NIC.Corrected.1")
    );
    let primary_selection = enriched_host
        .config
        .primary_interface_selection
        .expect("enriched primary decision");
    assert_eq!(
        primary_selection.source,
        fixture.plan.network_selection_source
    );
    assert!(primary_selection.updated_at.is_some());
    assert_eq!(
        persisted_primary_selection(&mut read, enriched.id).await?,
        Some(primary_selection)
    );
    let desired = enriched_host
        .config
        .desired_boot_interface
        .as_ref()
        .expect("enriched desired target");
    assert_eq!(desired.value, fixture.plan.boot_target);
    assert_eq!(
        desired.version.version_nr(),
        host.config
            .desired_boot_interface
            .as_ref()
            .expect("settled desired target")
            .version
            .version_nr()
            + 1
    );
    assert_eq!(
        enriched_host
            .config
            .boot_interface_selection
            .expect("enriched boot decision")
            .source,
        fixture.plan.boot_selection_source
    );
    assert_eq!(
        enriched_host.network_config.version,
        host.network_config.version
    );
    read.commit().await?;

    // A later controller pass and a full factory-MAC registration must leave this same networking version and association intact.
    let admission = admin_lock_admission().await;
    let mut txn = pool.begin().await?;
    assert!(
        lock_host_pf_initialization(
            &mut txn,
            &fixture.host_id,
            fixture.state_version,
            &[fixture.dpu_id],
            &[fixture.plan.network_primary_mac]
        )
        .await?
    );
    assert!(!repair_host_pf_interfaces(&mut txn, &fixture.host_id, &fixture.plan, None).await?);
    let registration_link = create_host_machine_dpu_interface_proactively(
        &mut txn,
        Some(&fixture.hardware_info),
        fixture.dpu_id.as_machine_id(),
        false,
        None,
    )
    .await?;
    assert_eq!(registration_link.id, corrected.id);
    txn.commit().await?;
    drop(admission);
    let mut read = pool.begin().await?;
    let unchanged =
        crate::machine::find_one(&mut *read, &fixture.host_id, MachineSearchConfig::default())
            .await?
            .expect("host");
    assert_eq!(
        unchanged.network_config.version,
        host.network_config.version
    );
    assert!(
        find_one(&mut *read, fixture.old_interface.id)
            .await?
            .attached_dpu_machine_id
            .is_none()
    );
    read.commit().await?;
    Ok(())
}

/// Rejects two registered DPUs claiming the same host PF MAC without persisting interface, allocation or selection changes, preventing one DPU from taking the other's corrected link.
#[crate::sqlx_test]
async fn repair_rejects_shared_mac_across_registered_dpus_without_mutation(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    // Add a second provisional DPU link while retaining the fixture's leased primary and boot decision.
    let mut fixture = fixture(&pool).await?;
    let second_dpu: DpuMachineId = MachineId::new(
        MachineIdSource::ProductBoardChassisSerial,
        [0x94; 32],
        MachineType::Dpu,
    )
    .try_into()
    .expect("second DPU ID");
    let dpu_ids = [fixture.dpu_id, second_dpu];
    let waiting_state = ManagedHostState::DPUInit {
        dpu_states: DpuInitStates {
            states: dpu_ids
                .into_iter()
                .map(|id| (id, DpuInitState::WaitingForNetworkConfig))
                .collect(),
        },
    };
    let shared_mac = fixture.plan.network_primary_mac;
    let second_factory_mac = MacAddress::new([2, 0, 0, 0, 0x91, 3]);
    let mut setup = pool.begin().await?;
    crate::machine::create(
        &mut setup,
        None,
        second_dpu.as_machine_id(),
        waiting_state.clone(),
        None,
        CURRENT_STATE_MODEL_VERSION,
    )
    .await?;
    let segment = load_admin_segments(&mut setup).await?.remove(0);
    let second_interface = create_without_addresses(
        &mut setup,
        &segment,
        &second_factory_mac,
        false,
        InterfaceType::Data,
        None,
    )
    .await?;
    associate_interface_with_machine(
        &second_interface.id,
        MachineInterfaceAssociation::Machine(fixture.host_id.to_machine_id()),
        &mut setup,
    )
    .await?;
    associate_interface_with_dpu_machine(
        &second_interface.id,
        second_dpu.as_machine_id(),
        &mut setup,
    )
    .await?;

    // Accept the same configured PF MAC for a distinct DPU through the per-DPU inventory writer.
    let mut second_hardware = fixture.hardware_info.clone();
    second_hardware
        .dpu_info
        .as_mut()
        .expect("second DPU facts")
        .factory_mac_address = second_factory_mac.to_string();
    crate::machine_topology::create_or_update(
        &mut setup,
        second_dpu.as_machine_id(),
        &second_hardware,
    )
    .await?;
    assert!(
        crate::machine_topology::record_dpu_host_pfs(&mut setup, &second_dpu, &second_hardware, 1)
            .await?
    );
    crate::machine::update_state(&mut setup, &fixture.host_id, &waiting_state).await?;
    setup.commit().await?;
    // Build the plan from both accepted assignments while keeping their DPU IDs distinct.
    fixture.plan.links.push(RegisteredHostPf {
        dpu_id: second_dpu, // Distinct DPUs make the shared MAC the only duplicate assignment.
        ..fixture.plan.links[0].clone()
    });

    // Reload accepted mappings and the current host token before capturing every row repair could change.
    let machine_ids = [
        fixture.host_id.to_machine_id(),
        fixture.dpu_id.to_machine_id(),
        second_dpu.to_machine_id(),
    ];
    let mut read = pool.begin().await?;
    let topologies =
        crate::machine_topology::find_latest_by_machine_ids(&mut read, &dpu_ids).await?;
    for dpu_id in &dpu_ids {
        let inventory = topologies
            .get(dpu_id)
            .and_then(|topology| topology.dpu_host_pf_inventory.as_ref())
            .expect("accepted PF inventory");
        assert_eq!(
            inventory.host_pfs,
            fixture
                .hardware_info
                .dpu_info
                .as_ref()
                .expect("PF facts")
                .host_pfs
        );
    }
    let host = crate::machine::find_one(&mut *read, &fixture.host_id, Default::default())
        .await?
        .expect("two-DPU host");
    assert_eq!(host.state.value, waiting_state);
    let query = "SELECT jsonb_build_object(
        'interfaces', (SELECT jsonb_agg(to_jsonb(i) ORDER BY i.id) FROM machine_interfaces i
            WHERE i.machine_id = ANY($1) OR i.attached_dpu_machine_id = ANY($1) OR i.mac_address = $2),
        'allocations', (SELECT jsonb_agg(to_jsonb(a) ORDER BY a.id) FROM machine_interface_addresses a
            JOIN machine_interfaces i ON i.id = a.interface_id WHERE i.machine_id = ANY($1)),
        'boot', (SELECT to_jsonb(b) FROM machine_boot_interfaces b WHERE b.machine_id = $3),
        'machine_versions', (SELECT jsonb_agg(jsonb_build_object('id', m.id, 'controller_state', m.controller_state,
            'controller_state_version', m.controller_state_version, 'network_config_version', m.network_config_version,
            'version', m.version) ORDER BY m.id) FROM machines m WHERE m.id = ANY($1)))";
    let before: serde_json::Value = sqlx::query_scalar(query)
        .bind(&machine_ids[..])
        .bind(shared_mac)
        .bind(fixture.host_id)
        .fetch_one(&mut *read)
        .await?;
    read.commit().await?;

    // Acquire the production initialization locks before submitting the two-DPU plan with its shared MAC.
    let admission = admin_lock_admission().await;
    let mut repair = pool.begin().await?;
    assert!(
        lock_host_pf_initialization(
            &mut repair,
            &fixture.host_id,
            host.state.version,
            &dpu_ids,
            &[shared_mac]
        )
        .await?
    );
    let error = repair_host_pf_interfaces(&mut repair, &fixture.host_id, &fixture.plan, None)
        .await
        .expect_err("shared registered MAC");
    assert!(matches!(error, DatabaseError::FailedPrecondition(message)
        if message == "duplicate registered host PF assignment"));
    repair.rollback().await?;
    drop(admission);

    // Reload full persisted rows to include allocation ownership, both selection timestamps and all machine versions.
    let after: serde_json::Value = sqlx::query_scalar(query)
        .bind(&machine_ids[..])
        .bind(shared_mac)
        .bind(fixture.host_id)
        .fetch_one(&pool)
        .await?;
    assert_eq!(after, before);
    let mut read = pool.begin().await?;
    assert!(
        find_by_mac_address(&mut *read, shared_mac)
            .await?
            .is_empty()
    );
    read.rollback().await?;
    Ok(())
}

/// Rolls back conflicting ownership and preserves Static allocations and independent Operator intent when a later repair succeeds, preventing cross-host MAC adoption and boot-intent loss.
#[crate::sqlx_test]
async fn repair_preserves_static_and_operator_intent_and_rolls_back_conflicts(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    // Convert the bootstrap lease to Static and place the corrected MAC on a different host.
    let fixture = fixture(&pool).await?;
    let mut setup = pool.begin().await?;
    sqlx::query(
        "UPDATE machine_interface_addresses SET allocation_type = $1 WHERE interface_id = $2",
    )
    .bind(AllocationType::Static)
    .bind(fixture.old_interface.id)
    .execute(&mut *setup)
    .await
    .map_err(|error| DatabaseError::new("static fixture", error))?;
    let operator_target =
        MachineBootInterfaceTarget::MacOnly(MacAddress::new([2, 0, 0, 0, 0x91, 9]));
    crate::machine_desired_boot_interface::set(
        &mut setup,
        &fixture.host_id,
        &operator_target,
        BootInterfaceSelectionSource::Operator,
    )
    .await?;
    let foreign_id = MachineId::new(MachineIdSource::Tpm, [0x93; 32], MachineType::Host);
    crate::machine::create(
        &mut setup,
        None,
        &foreign_id,
        ManagedHostState::Ready,
        None,
        CURRENT_STATE_MODEL_VERSION,
    )
    .await?;
    let segment = load_admin_segments(&mut setup).await?.remove(0);
    let conflicting = create_without_addresses(
        &mut setup,
        &segment,
        &fixture.plan.network_primary_mac,
        false,
        InterfaceType::Data,
        None,
    )
    .await?;
    associate_interface_with_machine(
        &conflicting.id,
        MachineInterfaceAssociation::Machine(foreign_id),
        &mut setup,
    )
    .await?;
    setup.commit().await?;

    // Reject before publishing any repaired rows or desired boot generation.
    let admission = admin_lock_admission().await;
    let mut txn = pool.begin().await?;
    assert!(
        lock_host_pf_initialization(
            &mut txn,
            &fixture.host_id,
            fixture.state_version,
            &[fixture.dpu_id],
            &[fixture.plan.network_primary_mac]
        )
        .await?
    );
    let error = repair_host_pf_interfaces(&mut txn, &fixture.host_id, &fixture.plan, None)
        .await
        .expect_err("foreign target");
    assert!(matches!(error, DatabaseError::FailedPrecondition(_)));
    txn.rollback().await?;
    drop(admission);
    let mut read = pool.begin().await?;
    assert!(
        find_one(&mut *read, fixture.old_interface.id)
            .await?
            .primary_interface
    );
    assert_eq!(
        find_one(&mut *read, conflicting.id).await?.machine_id,
        Some(foreign_id)
    );
    assert_eq!(
        crate::machine::get_network_config(&mut *read, fixture.host_id.as_machine_id())
            .await?
            .version,
        fixture.network_version
    );
    assert_eq!(
        crate::machine_desired_boot_interface::get(&mut *read, &fixture.host_id)
            .await?
            .expect("desired")
            .value,
        operator_target
    );
    read.commit().await?;

    // Remove the conflicting assignment, then prove repair adopts that row without moving the old Static address or Operator target.
    sqlx::query(
        "UPDATE machine_interfaces SET machine_id = NULL, association_type = 'None' WHERE id = $1",
    )
    .bind(conflicting.id)
    .execute(&pool)
    .await
    .map_err(|error| DatabaseError::new("release conflicting fixture", error))?;
    let admission = admin_lock_admission().await;
    let mut txn = pool.begin().await?;
    assert!(
        lock_host_pf_initialization(
            &mut txn,
            &fixture.host_id,
            fixture.state_version,
            &[fixture.dpu_id],
            &[fixture.plan.network_primary_mac]
        )
        .await?
    );
    assert!(repair_host_pf_interfaces(&mut txn, &fixture.host_id, &fixture.plan, None).await?);
    txn.commit().await?;
    drop(admission);
    let mut read = pool.begin().await?;
    let retired = find_one(&mut *read, fixture.old_interface.id).await?;
    let adopted = find_one(&mut *read, conflicting.id).await?;
    assert_eq!(retired.addresses, fixture.old_interface.addresses);
    assert!(retired.attached_dpu_machine_id.is_none());
    let allocations =
        crate::machine_interface_address::find_for_interface(&mut read, retired.id).await?;
    assert!(
        allocations
            .iter()
            .all(|address| address.allocation_type == AllocationType::Static)
    );
    assert!(adopted.primary_interface);
    assert!(!adopted.addresses.is_empty());
    assert_ne!(adopted.addresses, retired.addresses);
    assert_eq!(
        crate::machine_desired_boot_interface::get(&mut *read, &fixture.host_id)
            .await?
            .expect("desired")
            .value,
        operator_target
    );
    read.commit().await?;
    Ok(())
}

/// Rejects a stale controller token or changed attached DPU before HostInit publication while the network version and other admission checks match, preventing stale state or membership from authorizing the transition.
#[crate::sqlx_test]
async fn publication_fence_rejects_stale_state_and_changed_dpu_membership(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    // Advance the controller token without leaving WaitingForNetworkConfig or changing its DPU membership.
    let fixture = fixture(&pool).await?;
    let waiting_state = ManagedHostState::DPUInit {
        dpu_states: DpuInitStates {
            states: HashMap::from([(fixture.dpu_id, DpuInitState::WaitingForNetworkConfig)]),
        },
    };
    let mut setup = pool.begin().await?;
    crate::machine::update_state(&mut setup, &fixture.host_id, &waiting_state).await?;
    setup.commit().await?;
    let current = crate::machine::find_one(&pool, &fixture.host_id, Default::default())
        .await?
        .expect("initializing host");
    assert_eq!(current.state.value, waiting_state);
    assert_ne!(current.state.version, fixture.state_version);
    let replacement_dpu: DpuMachineId = MachineId::new(
        MachineIdSource::ProductBoardChassisSerial,
        [0x95; 32],
        MachineType::Dpu,
    )
    .try_into()
    .expect("replacement DPU ID");
    for (scenario, expected_version, attached_dpu) in [
        // An unchanged state and DPU attachment cannot make an earlier controller token current.
        (
            "stale controller token",
            fixture.state_version,
            fixture.dpu_id,
        ),
        // A current token cannot authorize HostInit publication after the actual attachment differs from the snapshot's DPU.
        (
            "changed DPU membership",
            current.state.version,
            replacement_dpu,
        ),
    ] {
        if attached_dpu == replacement_dpu {
            // Replace the real interface attachment without changing the host token or its expected DPUInit membership.
            let mut setup = pool.begin().await?;
            crate::machine::create(
                &mut setup,
                None,
                replacement_dpu.as_machine_id(),
                waiting_state.clone(),
                None,
                CURRENT_STATE_MODEL_VERSION,
            )
            .await?;
            associate_interface_with_dpu_machine(
                &fixture.old_interface.id,
                replacement_dpu.as_machine_id(),
                &mut setup,
            )
            .await?;
            setup.commit().await?;
        }

        // Reload the unchanged eligible state so each rejection isolates its token or attachment mismatch.
        let before = crate::machine::find_one(&pool, &fixture.host_id, Default::default())
            .await?
            .expect("host before rejected publication");
        assert_eq!(before.state.value, waiting_state, "{scenario}");
        assert_eq!(before.state.version, current.state.version, "{scenario}");

        // Attempt HostInit publication with the snapshot's expected DPU and the current network version.
        let mut publication = pool.begin().await?;
        assert!(
            !crate::machine::lock_host_init_network_config(
                &mut publication,
                &fixture.host_id,
                expected_version,
                before.network_config.version,
                &[fixture.dpu_id]
            )
            .await?,
            "{scenario}"
        );
        publication.rollback().await?;

        // Find persisted state and interfaces after rollback to prove rejection preserved controller and network versions.
        let after = crate::machine::find_one(&pool, &fixture.host_id, Default::default())
            .await?
            .expect("host after rejected publication");
        assert_eq!(after.state.value, before.state.value, "{scenario}");
        assert_eq!(after.state.version, before.state.version, "{scenario}");
        assert_eq!(
            after.network_config.version, fixture.network_version,
            "{scenario}"
        );
        let mut read = pool.begin().await?;
        let provisional = find_one(&mut *read, fixture.old_interface.id).await?;
        assert!(provisional.primary_interface, "{scenario}");
        assert_eq!(
            provisional.attached_dpu_machine_id,
            Some(attached_dpu),
            "{scenario}"
        );
        assert!(
            find_by_mac_address(&mut *read, fixture.plan.network_primary_mac)
                .await?
                .is_empty(),
            "{scenario}"
        );
        read.rollback().await?;
    }
    Ok(())
}

/// Defers repair while allocation holds the host and after an instance persists, including deleted instances, so state text alone cannot authorize automatic migration.
#[crate::sqlx_test]
async fn repair_serializes_with_allocation_and_rejects_later_lifecycle(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let fixture = fixture(&pool).await?;
    // Mirror allocation's host-first lock and commit an instance without changing the visible controller state.
    let mut allocation = pool.begin().await?;
    sqlx::query("SELECT id FROM machines WHERE id = $1 FOR UPDATE")
        .bind(fixture.host_id)
        .execute(&mut *allocation)
        .await
        .map_err(|error| DatabaseError::new("allocation host lock", error))?;
    sqlx::query("INSERT INTO instances (machine_id) VALUES ($1)")
        .bind(fixture.host_id)
        .execute(&mut *allocation)
        .await
        .map_err(|error| DatabaseError::new("instance fixture", error))?;
    let admission = admin_lock_admission().await;
    let mut repair = pool.begin().await?;
    assert!(
        !lock_host_pf_initialization(
            &mut repair,
            &fixture.host_id,
            fixture.state_version,
            &[fixture.dpu_id],
            &[fixture.plan.network_primary_mac]
        )
        .await?
    );
    repair.rollback().await?;
    allocation.commit().await?;

    // Both live and deleted persisted instances reject repair after the allocation transaction releases its lock.
    for deleted in [
        // A committed live instance excludes the host from unallocated initialization repair.
        false,
        // A deleted instance still records prior allocation and must not reopen automatic repair.
        true,
    ] {
        if deleted {
            sqlx::query("UPDATE instances SET deleted = clock_timestamp() WHERE machine_id = $1")
                .bind(fixture.host_id)
                .execute(&pool)
                .await
                .map_err(|error| DatabaseError::new("deleted instance fixture", error))?;
        }
        let mut repair = pool.begin().await?;
        assert!(
            !lock_host_pf_initialization(
                &mut repair,
                &fixture.host_id,
                fixture.state_version,
                &[fixture.dpu_id],
                &[fixture.plan.network_primary_mac]
            )
            .await?
        );
        repair.rollback().await?;
    }
    // With no instance left, use each later state's current token to exercise the phase guard itself.
    sqlx::query("DELETE FROM instances WHERE machine_id = $1")
        .bind(fixture.host_id)
        .execute(&pool)
        .await?;
    for next in [
        // HostInit has already crossed the checkpoint and cannot be repaired again.
        ManagedHostState::HostInit {
            machine_state: model::machine::MachineState::EnableIpmiOverLan,
        },
        // Ready stays outside automatic initialization repair even when unallocated.
        ManagedHostState::Ready,
    ] {
        let mut setup = pool.begin().await?;
        crate::machine::update_state(&mut setup, &fixture.host_id, &next).await?;
        setup.commit().await?;
        let mut repair = pool.begin().await?;
        let current = crate::machine::find_one(
            &mut *repair,
            &fixture.host_id,
            MachineSearchConfig::default(),
        )
        .await?
        .expect("host");
        assert_eq!(current.state.value, next);
        assert!(
            !lock_host_pf_initialization(
                &mut repair,
                &fixture.host_id,
                current.state.version,
                &[fixture.dpu_id],
                &[fixture.plan.network_primary_mac]
            )
            .await?
        );
        repair.rollback().await?;
    }
    drop(admission);
    let mut read = pool.begin().await?;
    assert!(
        find_one(&mut *read, fixture.old_interface.id)
            .await?
            .primary_interface
    );
    assert!(
        find_by_mac_address(&mut *read, fixture.plan.network_primary_mac)
            .await?
            .is_empty()
    );
    assert_eq!(
        crate::machine::get_network_config(&mut *read, fixture.host_id.as_machine_id())
            .await?
            .version,
        fixture.network_version
    );
    read.commit().await?;
    Ok(())
}

/// Rejects an old networking version after repair and retains the publication lock through the HostInit CAS so concurrent networking cannot invalidate the acknowledged version before admission.
#[crate::sqlx_test]
async fn host_init_fence_rejects_old_version_and_serializes_publication(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let fixture = fixture(&pool).await?;
    // Publish repaired networking before attempting admission with the pre-repair version.
    let admission = admin_lock_admission().await;
    let mut repair = pool.begin().await?;
    assert!(
        lock_host_pf_initialization(
            &mut repair,
            &fixture.host_id,
            fixture.state_version,
            &[fixture.dpu_id],
            &[fixture.plan.network_primary_mac]
        )
        .await?
    );
    assert!(repair_host_pf_interfaces(&mut repair, &fixture.host_id, &fixture.plan, None).await?);
    repair.commit().await?;
    drop(admission);
    let mut stale = pool.begin().await?;
    assert!(
        !crate::machine::lock_host_init_network_config(
            &mut stale,
            &fixture.host_id,
            fixture.state_version,
            fixture.network_version,
            &[fixture.dpu_id]
        )
        .await?
    );
    stale.rollback().await?;

    // Hold a fresh publication fence while a real network writer attempts to update the host.
    let mut publication = pool.begin().await?;
    let current =
        crate::machine::get_network_config(&mut *publication, fixture.host_id.as_machine_id())
            .await?;
    assert!(
        crate::machine::lock_host_init_network_config(
            &mut publication,
            &fixture.host_id,
            fixture.state_version,
            current.version,
            &[fixture.dpu_id]
        )
        .await?
    );
    let blocker: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *publication)
        .await
        .map_err(|error| DatabaseError::new("publication blocker", error))?;
    let writer_pool = pool.clone();
    let host_id = fixture.host_id;
    let writer = tokio::spawn(async move {
        let mut txn = writer_pool.begin().await.expect("network writer");
        let result = crate::machine::try_update_network_config(
            &mut txn,
            host_id.as_machine_id(),
            current.version,
            &current.value,
        )
        .await
        .expect("network update");
        txn.commit().await.expect("commit network update");
        result
    });
    crate::test_support::postgres::wait_for_blocked_query(&pool, blocker, "machines").await;
    let next = ManagedHostState::HostInit {
        machine_state: model::machine::MachineState::EnableIpmiOverLan,
    };
    assert!(matches!(
        crate::machine::try_update_controller_state(
            &mut publication,
            &fixture.host_id,
            fixture.state_version,
            fixture.state_version.increment(),
            &next
        )
        .await?,
        crate::ConditionalWrite::Applied(())
    ));
    publication.commit().await?;
    assert!(matches!(
        writer.await.expect("writer task"),
        crate::ConditionalWrite::Applied(())
    ));

    // Find persisted state to prove publication committed while the writer was serialized behind it.
    let mut read = pool.begin().await?;
    let admitted =
        crate::machine::find_one(&mut *read, &fixture.host_id, MachineSearchConfig::default())
            .await?
            .expect("host");
    assert_eq!(admitted.state.value, next);
    assert!(
        find_one(&mut *read, fixture.old_interface.id)
            .await?
            .attached_dpu_machine_id
            .is_none()
    );
    read.commit().await?;
    Ok(())
}

/// Exercises stale primary-marked boot predictions against corrected BF4 networking or legacy promotion so generation-specific behavior cannot silently exchange primary authority.
async fn check_prediction_primary_selection(
    pool: sqlx::PgPool,
    bf4: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    // Vary the DPU generation while the common fixture supplies leased networking and independent boot intent.
    let fixture = fixture(&pool).await?;
    let admission = admin_lock_admission().await;
    let mut setup = pool.begin().await?;
    if bf4 {
        // Finalize PF networking before Ready so the stale prediction is tested beyond the initialization-only guard.
        assert!(
            lock_host_pf_initialization(
                &mut setup,
                &fixture.host_id,
                fixture.state_version,
                &[fixture.dpu_id],
                &[fixture.plan.network_primary_mac]
            )
            .await?
        );
        repair_host_pf_interfaces(&mut setup, &fixture.host_id, &fixture.plan, None).await?;
        crate::machine::update_state(&mut setup, &fixture.host_id, &ManagedHostState::Ready)
            .await?;
    } else {
        // Construct the legacy predecessor without BF4 hardware or retained classification; this is fixture setup, not a classification withdrawal operation.
        let mut hardware = fixture.hardware_info.clone();
        let dpu = hardware.dpu_info.as_mut().expect("legacy hardware");
        dpu.part_number = "900-9D3B6-00CV-AA0".to_string();
        dpu.host_pfs.clear();
        crate::machine_topology::set_topology_update_needed(
            &mut setup,
            fixture.dpu_id.as_machine_id(),
            true,
        )
        .await?;
        crate::machine_topology::create_or_update(
            &mut setup,
            fixture.dpu_id.as_machine_id(),
            &hardware,
        )
        .await?;
        sqlx::query(
            "UPDATE machine_topologies SET dpu_host_pf_inventory = NULL WHERE machine_id = $1",
        )
        .bind(fixture.dpu_id)
        .execute(&mut *setup)
        .await?;
    }
    assert_eq!(
        crate::machine::host_has_bf4_dpu(&mut setup, &fixture.host_id).await?,
        bf4,
        "generation-specific promotion precondition"
    );
    crate::machine_interface::set_primary_interface_selection(
        &mut setup,
        &fixture.host_id,
        BootInterfaceSelectionSource::Operator,
        true,
    )
    .await?;
    let mut inband = crate::test_support::network_segment::admin_segment(
        "predicted-boot-nic",
        "198.51.100.0/24",
        "198.51.100.1",
        3,
    );
    inband.segment_type = NetworkSegmentType::HostInband;
    let inband =
        crate::network_segment::persist(inband, &mut setup, NetworkSegmentControllerState::Ready)
            .await?;
    let boot_mac = MacAddress::new([2, 0, 0, 0, 0x91, 9]);
    crate::predicted_machine_interface::create(
        NewPredictedMachineInterface {
            machine_id: &fixture.host_id,
            mac_address: boot_mac,
            expected_network_segment_type: NetworkSegmentType::HostInband,
            boot_interface_id: Some("NIC.Embedded.1-1-1"),
            primary_interface: true,
        },
        &mut setup,
    )
    .await?;
    setup.commit().await?;
    drop(admission);
    let before = crate::machine::find_one(&pool, &fixture.host_id, Default::default())
        .await?
        .expect("host");

    // Promote the prediction under the required segment locks before publishing the new primary's provenance.
    let admission = admin_lock_admission().await;
    let mut txn = pool.begin().await?;
    let admin = load_admin_segments(&mut txn).await?.remove(0);
    let locked_segments = vec![admin.id, inband.id];
    lock_network_segments_exclusive(&mut txn, &locked_segments).await?;
    let prediction = crate::predicted_machine_interface::find_by_mac_address(&mut txn, boot_mac)
        .await?
        .expect("prediction");
    move_predicted_machine_interface_to_machine(
        &mut txn,
        &prediction,
        "198.51.100.1".parse()?,
        None,
        &locked_segments,
    )
    .await?;
    txn.commit().await?;
    drop(admission);

    // Reload the actual primary and both decisions so a retained Operator source cannot hide behind the new MAC.
    let after = crate::machine::find_one(&pool, &fixture.host_id, Default::default())
        .await?
        .expect("promoted host");
    let promoted = after
        .status
        .interfaces
        .iter()
        .find(|row| row.primary_interface)
        .expect("primary");
    if bf4 {
        // An older boot prediction must leave the actual Ready networking primary and decision untouched.
        let previous = before
            .status
            .interfaces
            .iter()
            .find(|row| row.primary_interface)
            .expect("previous primary");
        assert_eq!(promoted.id, previous.id);
        assert_eq!(promoted.addresses, previous.addresses);
        assert_eq!(
            after.config.primary_interface_selection,
            before.config.primary_interface_selection
        );
        let boot_nic = after
            .status
            .interfaces
            .iter()
            .find(|row| row.mac_address == boot_mac)
            .expect("adopted boot NIC");
        assert!(!boot_nic.primary_interface);
    } else {
        // Legacy promotion selects the new NIC without inheriting the previous NIC's explicit authority.
        assert_eq!(promoted.mac_address, boot_mac);
        let decision = after
            .config
            .primary_interface_selection
            .expect("promoted source");
        assert_eq!(decision.source, BootInterfaceSelectionSource::LegacyUnknown);
        assert!(
            decision.updated_at
                > before
                    .config
                    .primary_interface_selection
                    .expect("Operator primary")
                    .updated_at
        );
    }
    assert_eq!(
        after.config.boot_interface_selection,
        before.config.boot_interface_selection
    );
    assert_eq!(
        after
            .config
            .desired_boot_interface
            .as_ref()
            .map(|desired| (&desired.value, desired.version)),
        before
            .config
            .desired_boot_interface
            .as_ref()
            .map(|desired| (&desired.value, desired.version))
    );
    Ok(())
}

/// Retains the existing BF2/BF3 promotion behavior while discarding the former primary's Operator authority, preventing the BF4 exception from changing legacy networking semantics.
#[crate::sqlx_test]
async fn prediction_promotion_does_not_inherit_primary_operator_authority(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    check_prediction_primary_selection(pool, false).await
}

/// Ignores an older boot prediction's primary flag after BF4 initialization, preserving the selected HBN link and explicit networking decision during later DHCP.
#[crate::sqlx_test]
async fn bf4_boot_prediction_preserves_networking_after_initialization(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    check_prediction_primary_selection(pool, true).await
}

/// Publishes an applicable retained ID into Operator boot intent without reselecting its MAC or source, so direct desired-target boot keeps a usable Pair and initialization can progress on retry.
#[crate::sqlx_test]
async fn repair_enriches_operator_desired_target_with_retained_same_mac_id(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    // Select the registered PF MAC before it has an owned row, and retain that same MAC's valid Redfish ID.
    let fixture = fixture(&pool).await?;
    let mut setup = pool.begin().await?;
    crate::machine_desired_boot_interface::set(
        &mut setup,
        &fixture.host_id,
        &fixture.plan.boot_target,
        BootInterfaceSelectionSource::Operator,
    )
    .await?;
    crate::retained_boot_interface::upsert(
        &mut setup,
        fixture.plan.network_primary_mac,
        "NIC.Retained.1",
    )
    .await?;
    setup.commit().await?;
    let before = crate::machine::find_one(&pool, &fixture.host_id, Default::default())
        .await?
        .expect("host before repair");
    let previous_boot = before
        .config
        .desired_boot_interface
        .as_ref()
        .expect("Operator MAC target");

    // Repair under production locks with a finite retention window before publishing initial boot intent.
    let admission = admin_lock_admission().await;
    let mut txn = pool.begin().await?;
    assert!(
        lock_host_pf_initialization(
            &mut txn,
            &fixture.host_id,
            fixture.state_version,
            &[fixture.dpu_id],
            &[fixture.plan.network_primary_mac]
        )
        .await?
    );
    assert!(
        repair_host_pf_interfaces(
            &mut txn,
            &fixture.host_id,
            &fixture.plan,
            Some(chrono::Duration::hours(1))
        )
        .await?
    );
    txn.commit().await?;
    drop(admission);

    // Find both the row and the desired target so a restored ID cannot remain confined to the networking interface.
    let corrected = find_by_mac_address(&pool, fixture.plan.network_primary_mac)
        .await?
        .remove(0);
    let after = crate::machine::find_one(&pool, &fixture.host_id, Default::default())
        .await?
        .expect("host after repair");
    let desired = after
        .config
        .desired_boot_interface
        .as_ref()
        .expect("paired desired target");
    assert_eq!(
        desired.value,
        MachineBootInterfaceTarget::Pair(corrected.boot_interface().expect("restored pair"))
    );
    assert_eq!(
        desired.value.mac_address(),
        previous_boot.value.mac_address()
    );
    assert_eq!(desired.value.interface_id(), Some("NIC.Retained.1"));
    assert_eq!(
        desired.version.version_nr(),
        previous_boot.version.version_nr() + 1
    );
    assert_eq!(
        after.config.boot_interface_selection,
        before.config.boot_interface_selection
    );

    // A second checkpoint must settle without another desired generation, rather than continually re-enriching the Pair.
    let admission = admin_lock_admission().await;
    let mut txn = pool.begin().await?;
    assert!(
        lock_host_pf_initialization(
            &mut txn,
            &fixture.host_id,
            fixture.state_version,
            &[fixture.dpu_id],
            &[fixture.plan.network_primary_mac]
        )
        .await?
    );
    assert!(
        !repair_host_pf_interfaces(
            &mut txn,
            &fixture.host_id,
            &fixture.plan,
            Some(chrono::Duration::hours(1))
        )
        .await?
    );
    txn.commit().await?;
    drop(admission);
    let retried = crate::machine::find_one(&pool, &fixture.host_id, Default::default())
        .await?
        .expect("host after retry");
    let retained = retried
        .config
        .desired_boot_interface
        .as_ref()
        .expect("retained Pair");
    assert_eq!(retained.value, desired.value);
    assert_eq!(retained.version, desired.version);
    assert_eq!(
        retried.config.boot_interface_selection,
        after.config.boot_interface_selection
    );
    Ok(())
}

/// Reads interface-row attribution so lifecycle tests can distinguish clearing from snapshot filtering.
async fn persisted_primary_selection(
    txn: &mut PgConnection,
    interface_id: MachineInterfaceId,
) -> Result<Option<model::machine_boot_interface::InterfaceSelection>, sqlx::Error> {
    let (source, updated_at): (Option<BootInterfaceSelectionSource>, Option<DateTime<Utc>>) =
        sqlx::query_as("SELECT primary_selection_source, primary_selection_updated_at FROM machine_interfaces WHERE id = $1")
            .bind(interface_id)
            .fetch_one(txn)
            .await?;
    Ok(source
        .map(|source| model::machine_boot_interface::InterfaceSelection { source, updated_at }))
}

/// Clears outgoing authority on primary changes and records a fresh decision on reselection, preventing an old Operator choice from returning with a reused row.
#[crate::sqlx_test]
async fn primary_selection_reselection_cannot_restore_stale_operator_authority(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    // Give the initial owned primary explicit networking authority before exercising row reuse.
    let fixture = fixture(&pool).await?;
    let mut txn = pool.begin().await?;
    set_primary_interface_selection(
        &mut txn,
        &fixture.host_id,
        BootInterfaceSelectionSource::Operator,
        true,
    )
    .await?;
    let original = persisted_primary_selection(&mut txn, fixture.old_interface.id)
        .await?
        .expect("Operator decision");

    // A repeated primary and association write must preserve the original decision time.
    set_primary_interface(&fixture.old_interface.id, true, &mut txn).await?;
    associate_interface_with_machine(
        &fixture.old_interface.id,
        MachineInterfaceAssociation::Machine(fixture.host_id.to_machine_id()),
        &mut txn,
    )
    .await?;
    assert!(
        !set_primary_interface_selection(
            &mut txn,
            &fixture.host_id,
            BootInterfaceSelectionSource::Operator,
            false
        )
        .await?
    );
    let unchanged = crate::machine::find_one(&mut *txn, &fixture.host_id, Default::default())
        .await?
        .expect("unchanged host");
    assert_eq!(unchanged.config.primary_interface_selection, Some(original));

    // Select a different owned row through the bulk setters, which must discard outgoing authority.
    let segments = load_admin_segments(&mut txn).await?;
    let replacement = create(
        &mut txn,
        &segments,
        &MacAddress::new([2, 0, 0, 0, 0x91, 8]),
        false,
        AddressSelectionStrategy::NextAvailableIp,
        None,
    )
    .await?;
    associate_interface_with_machine(
        &replacement.id,
        MachineInterfaceAssociation::Machine(fixture.host_id.to_machine_id()),
        &mut txn,
    )
    .await?;
    set_primary_interfaces(&[fixture.old_interface.id], false, &mut txn).await?;
    set_primary_interfaces(&[replacement.id], true, &mut txn).await?;
    set_primary_interface_selection(
        &mut txn,
        &fixture.host_id,
        BootInterfaceSelectionSource::RedfishSerialNumber,
        true,
    )
    .await?;
    assert!(
        persisted_primary_selection(&mut txn, fixture.old_interface.id)
            .await?
            .is_none()
    );

    // Reselect the former Operator row automatically after demoting the replacement.
    demote_primary_interfaces_for_machine(fixture.host_id.as_machine_id(), &mut txn).await?;
    set_primary_interface(&fixture.old_interface.id, true, &mut txn).await?;
    assert!(
        persisted_primary_selection(&mut txn, fixture.old_interface.id)
            .await?
            .is_none()
    );
    set_primary_interface_selection(
        &mut txn,
        &fixture.host_id,
        BootInterfaceSelectionSource::LegacyUnknown,
        true,
    )
    .await?;
    txn.commit().await?;

    // Reload the selected interface and the physical retired row to prove the old Operator decision is gone.
    let selected = crate::machine::find_one(&pool, &fixture.host_id, Default::default())
        .await?
        .expect("reselected host");
    let decision = selected
        .config
        .primary_interface_selection
        .expect("new decision");
    assert_eq!(decision.source, BootInterfaceSelectionSource::LegacyUnknown);
    assert_ne!(decision.updated_at, original.updated_at);
    assert_eq!(
        selected
            .status
            .interfaces
            .iter()
            .find(|row| row.primary_interface)
            .expect("primary")
            .id,
        fixture.old_interface.id
    );
    let mut read = pool.begin().await?;
    assert!(
        persisted_primary_selection(&mut read, replacement.id)
            .await?
            .is_none()
    );
    read.rollback().await?;
    Ok(())
}

/// Clears a prior machine's primary decision on reassignment and BMC conversion while preserving idempotent associations, preventing explicit intent from following a reused interface.
#[crate::sqlx_test]
async fn primary_selection_follows_current_machine_association(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    // A second machine makes same-kind ownership reassignment observable independently of boot intent.
    let fixture = fixture(&pool).await?;
    let other_host = MachineId::new(MachineIdSource::Tpm, [0x95; 32], MachineType::Host);
    let mut txn = pool.begin().await?;
    crate::machine::create(
        &mut txn,
        None,
        &other_host,
        ManagedHostState::Created,
        None,
        CURRENT_STATE_MODEL_VERSION,
    )
    .await?;

    // Both association APIs preserve identical ownership but clear attribution on a real handoff.
    for (scenario, bulk) in [
        // The single-row path compares the old association inside its update.
        ("single-row machine reassignment", false),
        // The batch path must apply the same rule to every affected interface.
        ("batch machine reassignment", true),
    ] {
        associate_interface_with_machine(
            &fixture.old_interface.id,
            MachineInterfaceAssociation::Machine(fixture.host_id.to_machine_id()),
            &mut txn,
        )
        .await?;
        set_primary_interface_selection(
            &mut txn,
            &fixture.host_id,
            BootInterfaceSelectionSource::Operator,
            true,
        )
        .await?;
        let before = persisted_primary_selection(&mut txn, fixture.old_interface.id).await?;
        if bulk {
            associate_interfaces_with_machine(
                &[fixture.old_interface.id],
                fixture.host_id.as_machine_id(),
                &mut txn,
            )
            .await?;
        } else {
            associate_interface_with_machine(
                &fixture.old_interface.id,
                MachineInterfaceAssociation::Machine(fixture.host_id.to_machine_id()),
                &mut txn,
            )
            .await?;
        }
        assert_eq!(
            persisted_primary_selection(&mut txn, fixture.old_interface.id).await?,
            before,
            "{scenario}"
        );
        if bulk {
            associate_interfaces_with_machine(&[fixture.old_interface.id], &other_host, &mut txn)
                .await?;
        } else {
            associate_interface_with_machine(
                &fixture.old_interface.id,
                MachineInterfaceAssociation::Machine(other_host),
                &mut txn,
            )
            .await?;
        }
        let reassigned = crate::machine::find_one(&mut *txn, &other_host, Default::default())
            .await?
            .expect("new owner");
        assert!(reassigned.config.primary_interface_selection.is_none());
        assert!(
            persisted_primary_selection(&mut txn, fixture.old_interface.id)
                .await?
                .is_none()
        );
    }

    // Both BMC conversion paths must discard networking attribution, even when ownership stays the same.
    for (scenario, direct_type) in [
        // Type-only updates exclude BMC rows from networking-primary reads.
        ("BMC conversion through type update", true),
        // BMC association also forces the primary flag off.
        ("BMC conversion through association", false),
    ] {
        set_interface_type(&fixture.old_interface.id, InterfaceType::Data, &mut txn).await?;
        set_primary_interface(&fixture.old_interface.id, true, &mut txn).await?;
        set_primary_interface_selection(
            &mut txn,
            &other_host,
            BootInterfaceSelectionSource::Operator,
            true,
        )
        .await?;
        if direct_type {
            set_interface_type(&fixture.old_interface.id, InterfaceType::Bmc, &mut txn).await?;
        } else {
            associate_bmc_interface(
                &fixture.old_interface.id,
                MachineInterfaceAssociation::Machine(other_host),
                &mut txn,
            )
            .await?;
        }
        let management = crate::machine::find_one(&mut *txn, &other_host, Default::default())
            .await?
            .expect("BMC owner");
        assert!(
            management.config.primary_interface_selection.is_none(),
            "{scenario}"
        );
        assert!(
            persisted_primary_selection(&mut txn, fixture.old_interface.id)
                .await?
                .is_none()
        );
    }
    txn.rollback().await?;
    Ok(())
}

/// Finishes a remaining prediction for an already-owned primary without demoting it or changing its decision, preventing idempotent DHCP bookkeeping from erasing Operator intent.
#[crate::sqlx_test]
async fn prediction_bookkeeping_preserves_owned_primary_selection(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    // Keep a prediction alongside the same owned primary, as permitted by the promotion path.
    let fixture = fixture(&pool).await?;
    let mut setup = pool.begin().await?;
    set_primary_interface_selection(
        &mut setup,
        &fixture.host_id,
        BootInterfaceSelectionSource::Operator,
        true,
    )
    .await?;
    let before = crate::machine::find_one(&mut *setup, &fixture.host_id, Default::default())
        .await?
        .expect("owned primary");
    crate::predicted_machine_interface::create(
        NewPredictedMachineInterface {
            machine_id: &fixture.host_id,
            mac_address: fixture.old_interface.mac_address,
            expected_network_segment_type: NetworkSegmentType::Admin,
            boot_interface_id: Some("NIC.Old"),
            primary_interface: true,
        },
        &mut setup,
    )
    .await?;
    setup.commit().await?;

    // Promote under the existing segment lock so the operation only completes prediction bookkeeping.
    let admission = admin_lock_admission().await;
    let mut txn = pool.begin().await?;
    let segments = vec![fixture.old_interface.segment_id];
    lock_network_segments_exclusive(&mut txn, &segments).await?;
    let prediction = crate::predicted_machine_interface::find_by_mac_address(
        &mut txn,
        fixture.old_interface.mac_address,
    )
    .await?
    .expect("prediction");
    move_predicted_machine_interface_to_machine(
        &mut txn,
        &prediction,
        "192.0.2.1".parse()?,
        None,
        &segments,
    )
    .await?;
    txn.commit().await?;
    drop(admission);

    // Reload both records to prove the prediction disappeared while the existing primary and decision survived.
    let after = crate::machine::find_one(&pool, &fixture.host_id, Default::default())
        .await?
        .expect("host after bookkeeping");
    assert_eq!(
        after.config.primary_interface_selection,
        before.config.primary_interface_selection
    );
    assert!(
        after
            .status
            .interfaces
            .iter()
            .any(|row| row.id == fixture.old_interface.id && row.primary_interface)
    );
    let mut read = pool.begin().await?;
    assert!(
        crate::predicted_machine_interface::find_by_mac_address(
            &mut read,
            fixture.old_interface.mac_address
        )
        .await?
        .is_none()
    );
    read.rollback().await?;
    Ok(())
}

/// Restores interfaces, allocations, boot decisions and networking versions when repair fails at its final network publication, preventing partial PF correction from surviving rollback.
#[crate::sqlx_test]
async fn repair_rolls_back_mutations_when_network_publication_fails(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    // Set both plan sources to UEFI, distinct from the fixture's persisted serial-based decisions.
    let mut fixture = fixture(&pool).await?;
    fixture.plan.network_selection_source = BootInterfaceSelectionSource::RedfishUefiPci;
    fixture.plan.boot_selection_source = BootInterfaceSelectionSource::RedfishUefiPci;
    let query = "SELECT jsonb_build_object(
        'interfaces', (SELECT jsonb_agg(to_jsonb(i) ORDER BY i.id) FROM machine_interfaces i
            WHERE i.machine_id = $1 OR i.attached_dpu_machine_id = $2 OR i.mac_address = $3),
        'allocations', (SELECT jsonb_agg(to_jsonb(a) ORDER BY a.id) FROM machine_interface_addresses a
            JOIN machine_interfaces i ON i.id = a.interface_id WHERE i.machine_id = $1),
        'boot', (SELECT to_jsonb(b) FROM machine_boot_interfaces b WHERE b.machine_id = $1),
        'network_versions', (SELECT jsonb_agg(jsonb_build_object('id', m.id, 'network_config_version', m.network_config_version, 'version', m.version) ORDER BY m.id)
            FROM machines m WHERE m.id = $1 OR m.id = $2))";
    let before: serde_json::Value = sqlx::query_scalar(query)
        .bind(fixture.host_id)
        .bind(fixture.dpu_id)
        .bind(fixture.plan.network_primary_mac)
        .fetch_one(&pool)
        .await?;
    // The generated MachineId and ConfigVersion literals contain no SQL quotes; CHECK definitions cannot use bind parameters.
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "ALTER TABLE machines ADD CONSTRAINT test_reject_host_pf_network_update
        CHECK (id != '{}' OR network_config_version = '{}')",
        fixture.host_id, fixture.network_version
    )))
    .execute(&pool)
    .await?;

    // Fail inside repair after row adoption, primary/provenance changes, address transfer and desired-boot publication.
    let admission = admin_lock_admission().await;
    let mut txn = pool.begin().await?;
    assert!(
        lock_host_pf_initialization(
            &mut txn,
            &fixture.host_id,
            fixture.state_version,
            &[fixture.dpu_id],
            &[fixture.plan.network_primary_mac]
        )
        .await?
    );
    let error = repair_host_pf_interfaces(&mut txn, &fixture.host_id, &fixture.plan, None)
        .await
        .expect_err("reject network publication");
    let DatabaseError::Sqlx(error) = error else {
        panic!("expected database check failure");
    };
    assert_eq!(
        error
            .source
            .as_database_error()
            .and_then(|error| error.constraint()),
        Some("test_reject_host_pf_network_update")
    );
    txn.rollback().await?;
    drop(admission);

    // Reload complete persisted rows to verify both selection timestamps and versions after rollback.
    let after: serde_json::Value = sqlx::query_scalar(query)
        .bind(fixture.host_id)
        .bind(fixture.dpu_id)
        .bind(fixture.plan.network_primary_mac)
        .fetch_one(&pool)
        .await?;
    assert_eq!(after, before);
    let mut read = pool.begin().await?;
    assert!(
        find_by_mac_address(&mut *read, fixture.plan.network_primary_mac)
            .await?
            .is_empty()
    );
    let host = crate::machine::find_one(&mut *read, &fixture.host_id, Default::default())
        .await?
        .expect("rolled-back host");
    assert_eq!(host.network_config.version, fixture.network_version);
    assert_eq!(
        host.config
            .primary_interface_selection
            .expect("restored source")
            .source,
        BootInterfaceSelectionSource::RedfishSerialNumber
    );
    read.rollback().await?;
    Ok(())
}

/// Retains an explicitly selected owned HostInband primary while consuming a secondary BF4 boot prediction, preventing automatic bookkeeping from undoing an Operator networking choice.
#[crate::sqlx_test]
async fn bf4_boot_prediction_preserves_explicit_owned_nic_primary(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    // An owned NIC can be explicitly selected after its boot-only prediction was created.
    let fixture = fixture(&pool).await?;
    let mut setup = pool.begin().await?;
    let mut inband = crate::test_support::network_segment::admin_segment(
        "explicit-boot-nic",
        "198.51.100.0/24",
        "198.51.100.1",
        3,
    );
    inband.segment_type = NetworkSegmentType::HostInband;
    let inband =
        crate::network_segment::persist(inband, &mut setup, NetworkSegmentControllerState::Ready)
            .await?;
    let mac = MacAddress::new([2, 0, 0, 0, 0x91, 9]);
    let nic = create(
        &mut setup,
        std::slice::from_ref(&inband),
        &mac,
        false,
        AddressSelectionStrategy::NextAvailableIp,
        None,
    )
    .await?;
    associate_interface_with_machine(
        &nic.id,
        MachineInterfaceAssociation::Machine(fixture.host_id.to_machine_id()),
        &mut setup,
    )
    .await?;
    demote_primary_interfaces_for_machine(fixture.host_id.as_machine_id(), &mut setup).await?;
    set_primary_interface(&nic.id, true, &mut setup).await?;
    set_primary_interface_selection(
        &mut setup,
        &fixture.host_id,
        BootInterfaceSelectionSource::Operator,
        true,
    )
    .await?;
    crate::machine_desired_boot_interface::set(
        &mut setup,
        &fixture.host_id,
        &MachineBootInterfaceTarget::MacOnly(mac),
        BootInterfaceSelectionSource::Operator,
    )
    .await?;
    crate::predicted_machine_interface::create(
        NewPredictedMachineInterface {
            machine_id: &fixture.host_id,
            mac_address: mac,
            expected_network_segment_type: NetworkSegmentType::HostInband,
            boot_interface_id: Some("NIC.Embedded.1-1-1"),
            primary_interface: false,
        },
        &mut setup,
    )
    .await?;
    setup.commit().await?;
    let before = crate::machine::find_one(&pool, &fixture.host_id, Default::default())
        .await?
        .expect("selected host");

    // Finish the remaining prediction under the ordinary promotion locks, retaining the actual owned selection.
    let admission = admin_lock_admission().await;
    let mut txn = pool.begin().await?;
    let segments = vec![fixture.old_interface.segment_id, inband.id];
    lock_network_segments_exclusive(&mut txn, &segments).await?;
    let prediction = crate::predicted_machine_interface::find_by_mac_address(&mut txn, mac)
        .await?
        .expect("boot prediction");
    move_predicted_machine_interface_to_machine(
        &mut txn,
        &prediction,
        "198.51.100.1".parse()?,
        None,
        &segments,
    )
    .await?;
    txn.commit().await?;
    drop(admission);

    // Reload networking, boot and prediction state so neither a demotion nor lost Operator attribution can be hidden.
    let after = crate::machine::find_one(&pool, &fixture.host_id, Default::default())
        .await?
        .expect("host after bookkeeping");
    assert_eq!(
        after
            .status
            .interfaces
            .iter()
            .find(|row| row.primary_interface)
            .expect("explicit primary")
            .id,
        nic.id
    );
    assert_eq!(
        after.config.primary_interface_selection,
        before.config.primary_interface_selection
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
    let mut read = pool.begin().await?;
    assert!(
        crate::predicted_machine_interface::find_by_mac_address(&mut read, mac)
            .await?
            .is_none()
    );
    read.rollback().await?;
    Ok(())
}

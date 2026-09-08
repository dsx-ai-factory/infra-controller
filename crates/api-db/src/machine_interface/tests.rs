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

use carbide_uuid::machine::{MachineId, MachineIdSource, MachineType};
use carbide_uuid::network::NetworkSegmentId;
use model::allocation_type::AllocationType;
use model::expected_machine::{
    ExpectedInterface, ExpectedInterfaceIpAllocation, ExpectedInterfaceRole,
};
use model::machine_interface::InterfaceType;
use model::network_prefix::NewNetworkPrefix;
use model::network_segment::{
    AllocationStrategy, NetworkSegmentControllerState, NetworkSegmentType, NewNetworkSegment,
};

use super::*;
use crate as db;

async fn create_static_assignments_segment(
    pool: &sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = db::Transaction::begin(pool).await?;
    db::network_segment::persist(
        NewNetworkSegment {
            id: uuid::Uuid::new_v4().into(),
            name: db::network_segment::STATIC_ASSIGNMENTS_SEGMENT_NAME.to_string(),
            subdomain_id: None,
            vpc_id: None,
            mtu: 1500,
            prefixes: vec![NewNetworkPrefix {
                prefix: "169.254.254.254/32".parse().unwrap(),
                gateway: None,
                dhcpv6_link_address: None,
                num_reserved: 1,
            }],
            vlan_id: None,
            vni: None,
            segment_type: NetworkSegmentType::Underlay,
            can_stretch: Some(false),
            allocation_strategy: AllocationStrategy::Reserved,
            infer_slaac_eui64_addresses: false,
        },
        txn.as_pgconn(),
        NetworkSegmentControllerState::Ready,
    )
    .await?;
    txn.commit().await?;

    Ok(())
}

async fn create_test_segment(
    pool: &sqlx::PgPool,
    name: &str,
) -> Result<NetworkSegmentId, Box<dyn std::error::Error>> {
    let segment_id = NetworkSegmentId::new();
    let mut txn = db::Transaction::begin(pool).await?;
    db::network_segment::persist(
        NewNetworkSegment {
            id: segment_id,
            name: name.to_string(),
            subdomain_id: None,
            vpc_id: None,
            mtu: 1500,
            prefixes: Vec::new(),
            vlan_id: None,
            vni: None,
            segment_type: NetworkSegmentType::HostInband,
            can_stretch: Some(false),
            allocation_strategy: AllocationStrategy::Reserved,
            infer_slaac_eui64_addresses: false,
        },
        txn.as_pgconn(),
        NetworkSegmentControllerState::Ready,
    )
    .await?;
    txn.commit().await?;

    Ok(segment_id)
}

async fn create_managed_segment(
    pool: &sqlx::PgPool,
    name: &str,
    prefix: &str,
    segment_type: NetworkSegmentType,
    allocation_strategy: AllocationStrategy,
) -> Result<NetworkSegmentId, Box<dyn std::error::Error>> {
    let segment_id = NetworkSegmentId::new();
    let mut txn = db::Transaction::begin(pool).await?;
    db::network_segment::persist(
        NewNetworkSegment {
            id: segment_id,
            name: name.to_string(),
            subdomain_id: None,
            vpc_id: None,
            mtu: 1500,
            prefixes: vec![NewNetworkPrefix {
                prefix: prefix.parse()?,
                gateway: None,
                dhcpv6_link_address: None,
                num_reserved: 0,
            }],
            vlan_id: None,
            vni: None,
            segment_type,
            can_stretch: Some(false),
            allocation_strategy,
            infer_slaac_eui64_addresses: false,
        },
        txn.as_pgconn(),
        NetworkSegmentControllerState::Ready,
    )
    .await?;
    txn.commit().await?;

    Ok(segment_id)
}

#[crate::sqlx_test]
async fn find_by_machine_id_for_update_locks_non_bmc_interfaces_in_id_order(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let segment_id = create_test_segment(&pool, "host-interface-locks").await?;
    let machine_id = MachineId::new(
        MachineIdSource::ProductBoardChassisSerial,
        [0x45; 32],
        MachineType::Host,
    );
    let first_interface_id = MachineInterfaceId::new();
    let second_interface_id = first_interface_id.offset(1);
    let bmc_interface_id = first_interface_id.offset(2);

    let mut setup_txn = pool.begin().await?;
    sqlx::query("INSERT INTO machines (id, dpf) VALUES ($1, '{}'::jsonb)")
        .bind(machine_id)
        .execute(setup_txn.as_mut())
        .await?;
    let query = r#"
        INSERT INTO machine_interfaces (
            id,
            machine_id,
            segment_id,
            mac_address,
            primary_interface,
            hostname,
            association_type,
            interface_type
        )
        VALUES
            ($1, $3, $4, '7A:7B:7C:7D:7E:53', false, 'second', 'Machine', 'Data'),
            ($2, $3, $4, '7A:7B:7C:7D:7E:52', false, 'first', 'Machine', 'Data'),
            ($5, $3, $4, '7A:7B:7C:7D:7E:54', false, 'bmc', 'Machine', 'Bmc')
    "#;
    sqlx::query(query)
        .bind(second_interface_id)
        .bind(first_interface_id)
        .bind(machine_id)
        .bind(segment_id)
        .bind(bmc_interface_id)
        .execute(setup_txn.as_mut())
        .await?;
    setup_txn.commit().await?;

    let mut lock_txn = pool.begin().await?;
    let interfaces = find_by_machine_id_for_update(lock_txn.as_mut(), &machine_id).await?;
    assert_eq!(
        interfaces
            .iter()
            .map(|interface| interface.id)
            .collect::<Vec<_>>(),
        vec![first_interface_id, second_interface_id],
    );

    let mut bmc_writer = pool.begin().await?;
    sqlx::query("SET LOCAL lock_timeout = '100ms'")
        .execute(bmc_writer.as_mut())
        .await?;
    sqlx::query("UPDATE machine_interfaces SET hostname = hostname WHERE id = $1")
        .bind(bmc_interface_id)
        .execute(bmc_writer.as_mut())
        .await?;
    bmc_writer.commit().await?;

    let mut host_writer = pool.begin().await?;
    sqlx::query("SET LOCAL lock_timeout = '100ms'")
        .execute(host_writer.as_mut())
        .await?;
    let error = sqlx::query("UPDATE machine_interfaces SET hostname = hostname WHERE id = $1")
        .bind(first_interface_id)
        .execute(host_writer.as_mut())
        .await
        .expect_err("a concurrent host-interface writer must wait for the row lock");
    assert_eq!(
        error
            .as_database_error()
            .and_then(sqlx::error::DatabaseError::code)
            .as_deref(),
        Some("55P03"),
    );
    host_writer.rollback().await?;
    lock_txn.rollback().await?;

    Ok(())
}

/// A MAC identifies one physical interface even when stale or transitional
/// rows represent it on more than one segment. Site Explorer learns one
/// vendor-native Redfish id for that interface, so `set_boot_interface_id`
/// updates every row for the MAC rather than whichever segment happened to
/// report first.
#[crate::sqlx_test]
async fn set_boot_interface_id_updates_every_segment_row_for_mac(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let segment_a = create_test_segment(&pool, "boot-id-segment-a").await?;
    let segment_b = create_test_segment(&pool, "boot-id-segment-b").await?;
    let boot_mac: MacAddress = "7A:7B:7C:7D:7E:41".parse()?;
    let other_mac: MacAddress = "7A:7B:7C:7D:7E:42".parse()?;

    let mut txn = db::Transaction::begin(&pool).await?;
    let query = "
INSERT INTO machine_interfaces
    (segment_id, mac_address, primary_interface, hostname)
VALUES
    ($1, $2, false, 'boot-a'),
    ($3, $2, false, 'boot-b'),
    ($1, $4, false, 'other')";
    sqlx::query(query)
        .bind(segment_a)
        .bind(boot_mac)
        .bind(segment_b)
        .bind(other_mac)
        .execute(txn.as_pgconn())
        .await?;

    set_boot_interface_id(boot_mac, "NIC.Slot.7-1-1", txn.as_pgconn()).await?;

    let boot_ids: Vec<Option<String>> = sqlx::query_scalar(
        "SELECT boot_interface_id FROM machine_interfaces WHERE mac_address=$1 ORDER BY hostname",
    )
    .bind(boot_mac)
    .fetch_all(txn.as_pgconn())
    .await?;
    let other_id: Option<String> =
        sqlx::query_scalar("SELECT boot_interface_id FROM machine_interfaces WHERE mac_address=$1")
            .bind(other_mac)
            .fetch_one(txn.as_pgconn())
            .await?;

    assert_eq!(
        boot_ids,
        vec![
            Some("NIC.Slot.7-1-1".to_string()),
            Some("NIC.Slot.7-1-1".to_string())
        ]
    );
    assert_eq!(other_id, None, "a different MAC must remain unchanged");

    Ok(())
}

/// Verify `preallocate_machine_interface` is idempotent.
/// AddExpectedMachine, expected_machines.json, and the DHCP discover() flow can
/// all fire against the same (ip, mac) pair, including after state has already
/// converged, which is both on purpose and to help flexibly adjust where we
/// find these calls fit best.
///
/// A repeat call must be Ok without changing rows.
#[crate::sqlx_test]
async fn test_preallocate_machine_interface_is_idempotent(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    create_static_assignments_segment(&pool).await?;
    let mac: MacAddress = "7A:7B:7C:7D:7E:31".parse().unwrap();
    let ip: std::net::IpAddr = "192.0.2.241".parse().unwrap();

    let mut txn = db::Transaction::begin(&pool).await?;
    preallocate_machine_interface(txn.as_pgconn(), mac, ip, None).await?;
    txn.commit().await?;

    let mut txn = db::Transaction::begin(&pool).await?;
    preallocate_machine_interface(txn.as_pgconn(), mac, ip, None).await?;
    let interfaces = find_by_mac_address(&mut txn, mac).await?;
    txn.commit().await?;

    assert_eq!(
        interfaces.len(),
        1,
        "second preallocate should be a no-op, not create a duplicate row"
    );
    assert!(
        interfaces[0].addresses.contains(&ip),
        "interface should still carry the static IP"
    );

    Ok(())
}

/// Legacy installations can have an external static reservation on an
/// ordinary segment without the newer static-assignments anchor. Reapplying
/// that exact reservation remains an idempotent no-op.
#[crate::sqlx_test]
async fn test_preallocate_machine_interface_is_idempotent_without_static_assignments(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let segment_id = create_test_segment(&pool, "legacy-external-static").await?;
    let mac: MacAddress = "7A:7B:7C:7D:7E:30".parse()?;
    let ip: IpAddr = "203.0.113.230".parse()?;

    let mut txn = db::Transaction::begin(&pool).await?;
    let interface_id: MachineInterfaceId = sqlx::query_scalar(
        "INSERT INTO machine_interfaces
            (segment_id, mac_address, primary_interface, hostname)
         VALUES ($1, $2, true, 'legacy-external-static')
         RETURNING id",
    )
    .bind(segment_id)
    .bind(mac)
    .fetch_one(txn.as_pgconn())
    .await?;
    crate::machine_interface_address::insert(
        txn.as_pgconn(),
        interface_id,
        ip,
        AllocationType::Static,
    )
    .await?;

    preallocate_machine_interface(txn.as_pgconn(), mac, ip, None).await?;
    let legacy_expected_interface = ExpectedInterface {
        mac_address: mac,
        fixed_ip: Some(ip),
        ..Default::default()
    };
    preallocate_expected_machine_interface(txn.as_pgconn(), &legacy_expected_interface, None)
        .await?;

    // An explicit policy requires a managed prefix even when the exact
    // external reservation already exists. Resolve it before the idempotent
    // `(MAC, IP)` path can return success.
    let explicit_expected_interface = ExpectedInterface {
        ip_allocation: Some(ExpectedInterfaceIpAllocation::Fixed),
        ..legacy_expected_interface
    };
    let error =
        preallocate_expected_machine_interface(txn.as_pgconn(), &explicit_expected_interface, None)
            .await
            .expect_err("an explicit fixed policy should require a managed prefix");
    assert!(matches!(error, DatabaseError::InvalidArgument(_)));

    let interfaces = find_by_mac_address(txn.as_pgconn(), mac).await?;
    txn.commit().await?;

    assert_eq!(interfaces.len(), 1);
    assert_eq!(interfaces[0].segment_id, segment_id);
    assert_eq!(interfaces[0].addresses, vec![ip]);

    Ok(())
}

#[crate::sqlx_test]
async fn test_expected_declaration_does_not_reclassify_attached_dpu_interface(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let segment_name = "attached-dpu-interface";
    create_managed_segment(
        &pool,
        segment_name,
        "192.0.2.0/24",
        NetworkSegmentType::Underlay,
        AllocationStrategy::Dynamic,
    )
    .await?;
    let dpu_id = MachineId::new(
        MachineIdSource::ProductBoardChassisSerial,
        [0x43; 32],
        MachineType::Dpu,
    );
    let mac: MacAddress = "7A:7B:7C:7D:7E:2F".parse()?;
    let relay: IpAddr = "192.0.2.1".parse()?;

    let mut txn = db::Transaction::begin(&pool).await?;
    sqlx::query("INSERT INTO machines (id, dpf) VALUES ($1, '{}'::jsonb)")
        .bind(dpu_id)
        .execute(txn.as_pgconn())
        .await?;
    let segment = db::network_segment::find_by_name(txn.as_pgconn(), segment_name).await?;
    let interface = create_without_addresses(
        txn.as_pgconn(),
        &segment,
        &mac,
        true,
        InterfaceType::Data,
        None,
    )
    .await?;
    associate_interface_with_dpu_machine(&interface.id, &dpu_id, txn.as_pgconn()).await?;

    let expected_interface = ExpectedInterface {
        mac_address: mac,
        role: ExpectedInterfaceRole::DpuBmc,
        ..Default::default()
    };
    let reconciled = find_or_create_machine_interface(
        txn.as_pgconn(),
        None,
        mac,
        &[relay],
        Some(expected_interface),
        Some(false),
        None,
    )
    .await?;
    txn.commit().await?;

    assert_eq!(reconciled.attached_dpu_machine_id, Some(dpu_id.try_into()?));
    assert_eq!(reconciled.interface_type, InterfaceType::Data);
    assert!(reconciled.primary_interface);

    Ok(())
}

/// An association that commits after DHCP reads an interface must still win
/// over ExpectedMachine settings derived from that stale snapshot.
#[crate::sqlx_test]
async fn test_expected_interface_settings_do_not_overwrite_concurrent_dpu_association(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let segment_name = "concurrently-attached-dpu-interface";
    create_managed_segment(
        &pool,
        segment_name,
        "198.51.100.0/24",
        NetworkSegmentType::Underlay,
        AllocationStrategy::Dynamic,
    )
    .await?;
    let dpu_id = MachineId::new(
        MachineIdSource::ProductBoardChassisSerial,
        [0x44; 32],
        MachineType::Dpu,
    );
    let mac: MacAddress = "7A:7B:7C:7D:7E:2E".parse()?;

    let mut setup_txn = db::Transaction::begin(&pool).await?;
    sqlx::query("INSERT INTO machines (id, dpf) VALUES ($1, '{}'::jsonb)")
        .bind(dpu_id)
        .execute(setup_txn.as_pgconn())
        .await?;
    let segment = db::network_segment::find_by_name(setup_txn.as_pgconn(), segment_name).await?;
    let interface = create_without_addresses(
        setup_txn.as_pgconn(),
        &segment,
        &mac,
        true,
        InterfaceType::Data,
        None,
    )
    .await?;
    setup_txn.commit().await?;

    let mut snapshot_txn = db::Transaction::begin(&pool).await?;
    let mut stale_interface = find_one(snapshot_txn.as_pgconn(), interface.id).await?;
    let updated = update_unassociated_expected_interface_settings(
        snapshot_txn.as_pgconn(),
        interface.id,
        Some(InterfaceType::Data),
        Some(true),
    )
    .await?;
    assert!(
        !updated,
        "steady-state ExpectedInterface settings should not write a new row version",
    );
    snapshot_txn.commit().await?;

    let mut association_txn = db::Transaction::begin(&pool).await?;
    associate_interface_with_dpu_machine(&interface.id, &dpu_id, association_txn.as_pgconn())
        .await?;
    association_txn.commit().await?;

    let mut expected_txn = db::Transaction::begin(&pool).await?;
    reconcile_unassociated_expected_interface_settings(
        expected_txn.as_pgconn(),
        &mut stale_interface,
        Some(InterfaceType::Bmc),
        Some(false),
    )
    .await?;
    assert_eq!(
        stale_interface.attached_dpu_machine_id,
        Some(dpu_id.try_into()?)
    );
    assert_eq!(stale_interface.interface_type, InterfaceType::Data);
    assert!(stale_interface.primary_interface);
    expected_txn.commit().await?;

    let mut verify_txn = db::Transaction::begin(&pool).await?;
    let reconciled = find_one(verify_txn.as_pgconn(), interface.id).await?;
    verify_txn.commit().await?;

    assert_eq!(reconciled.attached_dpu_machine_id, Some(dpu_id.try_into()?));
    assert_eq!(reconciled.interface_type, InterfaceType::Data);
    assert!(reconciled.primary_interface);

    Ok(())
}

/// Pre-allocating a different IP for an existing MAC must error, rather than
/// silently reassigning. If an `expected_machine.bmc_ip_address` (or an
/// `ExpectedInterface.fixed_ip`) drifts from its `machine_interface` row,
/// operators should see the conflict instead of an automatic rewrite.
#[crate::sqlx_test]
async fn test_preallocate_machine_interface_rejects_conflicting_ip(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    create_static_assignments_segment(&pool).await?;
    let mac: MacAddress = "7A:7B:7C:7D:7E:32".parse().unwrap();
    let ip1: std::net::IpAddr = "192.0.2.242".parse().unwrap();
    let ip2: std::net::IpAddr = "192.0.2.243".parse().unwrap();

    let mut txn = db::Transaction::begin(&pool).await?;
    preallocate_machine_interface(txn.as_pgconn(), mac, ip1, None).await?;
    txn.commit().await?;

    let mut txn = db::Transaction::begin(&pool).await?;
    let result = preallocate_machine_interface(txn.as_pgconn(), mac, ip2, None).await;
    assert!(
        matches!(result, Err(DatabaseError::InvalidArgument(_))),
        "preallocating a different IP for the same MAC should be rejected, got {result:?}"
    );

    Ok(())
}

/// Symmetric to `test_preallocate_machine_interface_rejects_conflicting_ip`: pre-allocating
/// an IP that another MAC already owns must error rather than silently reassigning. Covers
/// the `find_by_address`-branch in `preallocate_machine_interface_with_type`.
#[crate::sqlx_test]
async fn test_preallocate_machine_interface_rejects_ip_owned_by_different_mac(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    create_static_assignments_segment(&pool).await?;
    let mac_a: MacAddress = "7A:7B:7C:7D:7E:35".parse().unwrap();
    let mac_b: MacAddress = "7A:7B:7C:7D:7E:36".parse().unwrap();
    let ip: std::net::IpAddr = "192.0.2.248".parse().unwrap();

    let mut txn = db::Transaction::begin(&pool).await?;
    preallocate_machine_interface(txn.as_pgconn(), mac_a, ip, None).await?;
    txn.commit().await?;

    let mut txn = db::Transaction::begin(&pool).await?;
    let result = preallocate_machine_interface(txn.as_pgconn(), mac_b, ip, None).await;
    assert!(
        matches!(result, Err(DatabaseError::InvalidArgument(_))),
        "preallocating an IP owned by a different MAC should be rejected, got {result:?}"
    );

    Ok(())
}

/// After a `machine_interface` row gets deleted (e.g. force-delete
/// --delete-interfaces), a subsequent `preallocate_machine_interface` call
/// must successfully recreate it with the same static IP. This is the
/// deferred-allocation flow that we rely on with DHCP discover(...).
#[crate::sqlx_test]
async fn test_preallocate_machine_interface_recreates_after_deletion(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    create_static_assignments_segment(&pool).await?;
    let mac: MacAddress = "7A:7B:7C:7D:7E:33".parse().unwrap();
    let ip: std::net::IpAddr = "192.0.2.244".parse().unwrap();

    let mut txn = db::Transaction::begin(&pool).await?;
    preallocate_machine_interface(txn.as_pgconn(), mac, ip, None).await?;
    let interfaces_before = find_by_mac_address(&mut txn, mac).await?;
    let interface_id = interfaces_before[0].id;
    delete(&interface_id, txn.as_pgconn()).await?;
    txn.commit().await?;

    let mut txn = db::Transaction::begin(&pool).await?;
    preallocate_machine_interface(txn.as_pgconn(), mac, ip, None).await?;
    let interfaces_after = find_by_mac_address(&mut txn, mac).await?;
    txn.commit().await?;

    assert_eq!(
        interfaces_after.len(),
        1,
        "interface should be re-created after deletion"
    );
    assert!(
        interfaces_after[0].addresses.contains(&ip),
        "re-created interface should carry the same static IP"
    );

    Ok(())
}

/// When an interface row already exists for the right (MAC, IP) but with the wrong
/// `interface_type`, a subsequent preallocate call should promote the type rather than
/// erroring or creating a duplicate. Covers the case where a host NIC initially DHCPs in as
/// `InterfaceType::Data`, then the operator's expected_machine config later marks the same
/// MAC as the BMC (or vice versa), and the next reconciliation pass (or discover hook)
/// reconciles.
#[crate::sqlx_test]
async fn test_preallocate_machine_interface_promotes_interface_type(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    create_static_assignments_segment(&pool).await?;
    let mac: MacAddress = "7A:7B:7C:7D:7E:34".parse().unwrap();
    let ip: std::net::IpAddr = "192.0.2.247".parse().unwrap();

    // Initial preallocation lands as InterfaceType::Data.
    let mut txn = db::Transaction::begin(&pool).await?;
    preallocate_machine_interface(txn.as_pgconn(), mac, ip, None).await?;
    let before = find_by_mac_address(&mut txn, mac).await?;
    assert_eq!(
        before[0].interface_type,
        InterfaceType::Data,
        "Data-variant preallocate should start as InterfaceType::Data"
    );
    txn.commit().await?;

    // Re-preallocate the same (MAC, IP) but as the BMC variant. Helper should promote
    // the existing row's interface_type rather than erroring or creating a duplicate.
    let mut txn = db::Transaction::begin(&pool).await?;
    preallocate_bmc_machine_interface(txn.as_pgconn(), mac, ip, None).await?;
    let after = find_by_mac_address(&mut txn, mac).await?;
    txn.commit().await?;

    assert_eq!(after.len(), 1, "no duplicate row should have been created");
    assert_eq!(
        after[0].interface_type,
        InterfaceType::Bmc,
        "Bmc-variant preallocate should promote the existing row to InterfaceType::Bmc"
    );
    assert!(
        after[0].addresses.contains(&ip),
        "promoted row should still carry the same IP"
    );

    Ok(())
}

/// A retained Host BMC promotes its DHCP address to `Static`, remains
/// idempotent, and then survives the DHCP-scoped expiry delete path.
#[crate::sqlx_test]
async fn test_retained_host_bmc_address_pins_dhcp_and_survives_expiry(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    use model::allocation_type::AllocationType;

    create_static_assignments_segment(&pool).await?;
    let mac: MacAddress = "7A:7B:7C:7D:7E:37".parse().unwrap();
    let ip: std::net::IpAddr = "192.0.2.250".parse().unwrap();

    // Create a BMC interface (preallocate lands a Static address), then swap that
    // address for a Dhcp one so we have a BMC interface holding a DHCP lease --
    // the state a BMC reaches when it auto-allocates over DHCP.
    let mut txn = db::Transaction::begin(&pool).await?;
    preallocate_bmc_machine_interface(txn.as_pgconn(), mac, ip, None).await?;
    let interfaces = find_by_mac_address(&mut txn, mac).await?;
    let interface_id = interfaces[0].id;
    assert_eq!(
        interfaces[0].interface_type,
        InterfaceType::Bmc,
        "preallocated interface should be the BMC type"
    );
    crate::machine_interface_address::delete(txn.as_pgconn(), &interface_id).await?;
    crate::machine_interface_address::insert(
        txn.as_pgconn(),
        interface_id,
        ip,
        AllocationType::Dhcp,
    )
    .await?;
    txn.commit().await?;

    let expected_interface = ExpectedInterface {
        mac_address: mac,
        role: ExpectedInterfaceRole::HostBmc,
        ip_allocation: Some(ExpectedInterfaceIpAllocation::Retained),
        ..Default::default()
    };

    // Retain: the DHCP address is promoted to Static.
    let mut txn = db::Transaction::begin(&pool).await?;
    retain_expected_machine_interface_address(txn.as_pgconn(), &expected_interface).await?;
    let addrs =
        crate::machine_interface_address::find_for_interface(txn.as_pgconn(), interface_id).await?;
    txn.commit().await?;
    assert_eq!(addrs.len(), 1, "retain must not duplicate the address row");
    assert_eq!(
        addrs[0].allocation_type,
        AllocationType::Static,
        "retain should promote the DHCP address to Static"
    );

    // Idempotent: a second retain is a no-op (the row is already Static).
    let mut txn = db::Transaction::begin(&pool).await?;
    retain_expected_machine_interface_address(txn.as_pgconn(), &expected_interface).await?;
    let addrs =
        crate::machine_interface_address::find_for_interface(txn.as_pgconn(), interface_id).await?;
    txn.commit().await?;
    assert_eq!(addrs.len(), 1, "second retain must remain a single row");
    assert_eq!(
        addrs[0].allocation_type,
        AllocationType::Static,
        "second retain should leave the address Static"
    );

    // The promoted Static address survives the DHCP-scoped expiry delete path:
    // delete_by_address(.., Dhcp) finds nothing to delete and the row remains.
    let mut txn = db::Transaction::begin(&pool).await?;
    let deleted = crate::machine_interface_address::delete_by_address(
        txn.as_pgconn(),
        ip,
        AllocationType::Dhcp,
    )
    .await?;
    let addrs =
        crate::machine_interface_address::find_for_interface(txn.as_pgconn(), interface_id).await?;
    txn.commit().await?;
    assert!(
        deleted.is_empty(),
        "DHCP-scoped expiry delete should not match a Static address"
    );
    assert_eq!(
        addrs.len(),
        1,
        "the retained Static address should survive DHCP lease expiry"
    );
    assert_eq!(addrs[0].allocation_type, AllocationType::Static);

    Ok(())
}

#[crate::sqlx_test]
async fn test_expected_interface_role_controls_fixed_preallocation(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    create_static_assignments_segment(&pool).await?;
    let segment_id = create_managed_segment(
        &pool,
        "expected-interface-underlay",
        "192.0.2.0/24",
        NetworkSegmentType::Underlay,
        AllocationStrategy::Reserved,
    )
    .await?;

    struct Case {
        name: &'static str,
        suffix: u8,
        role: ExpectedInterfaceRole,
        primary: Option<bool>,
        expected_type: InterfaceType,
        expected_primary: bool,
    }

    for Case {
        name,
        suffix,
        role,
        primary,
        expected_type,
        expected_primary,
    } in [
        Case {
            name: "host default",
            suffix: 0x51,
            role: ExpectedInterfaceRole::Host,
            primary: None,
            expected_type: InterfaceType::Data,
            expected_primary: true,
        },
        Case {
            name: "Host false retains legacy creation default",
            suffix: 0x52,
            role: ExpectedInterfaceRole::Host,
            primary: Some(false),
            expected_type: InterfaceType::Data,
            expected_primary: true,
        },
        Case {
            name: "DPU OS",
            suffix: 0x53,
            role: ExpectedInterfaceRole::DpuOs,
            primary: None,
            expected_type: InterfaceType::Data,
            expected_primary: true,
        },
        Case {
            name: "DPU BMC",
            suffix: 0x54,
            role: ExpectedInterfaceRole::DpuBmc,
            primary: None,
            expected_type: InterfaceType::Bmc,
            expected_primary: false,
        },
    ] {
        let expected_interface = ExpectedInterface {
            mac_address: MacAddress::new([0x7a, 0x7b, 0x7c, 0x7d, 0x7e, suffix]),
            role,
            ip_allocation: Some(ExpectedInterfaceIpAllocation::Fixed),
            network_segment_type: Some(NetworkSegmentType::Underlay),
            fixed_ip: Some(format!("192.0.2.{suffix}").parse()?),
            primary,
            ..Default::default()
        };

        let mut txn = db::Transaction::begin(&pool).await?;
        preallocate_expected_machine_interface(txn.as_pgconn(), &expected_interface, None).await?;
        let interfaces =
            find_by_mac_address(txn.as_pgconn(), expected_interface.mac_address).await?;
        txn.commit().await?;

        assert_eq!(interfaces.len(), 1, "case: {name}");
        assert_eq!(interfaces[0].interface_type, expected_type, "case: {name}");
        assert_eq!(
            interfaces[0].primary_interface, expected_primary,
            "case: {name}",
        );
        assert_eq!(interfaces[0].segment_id, segment_id, "case: {name}");
        assert_eq!(
            interfaces[0].addresses,
            vec![expected_interface.fixed_ip.unwrap()],
            "case: {name}",
        );
    }

    Ok(())
}

#[crate::sqlx_test]
async fn test_fixed_host_preallocation_does_not_override_machine_wide_primary_selection(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    create_static_assignments_segment(&pool).await?;
    create_managed_segment(
        &pool,
        "fixed-host-primary",
        "192.0.2.0/24",
        NetworkSegmentType::Underlay,
        AllocationStrategy::Reserved,
    )
    .await?;
    let expected_interface = ExpectedInterface {
        mac_address: "7A:7B:7C:7D:7E:59".parse()?,
        ip_allocation: Some(ExpectedInterfaceIpAllocation::Fixed),
        fixed_ip: Some("192.0.2.59".parse()?),
        ..Default::default()
    };

    let mut txn = db::Transaction::begin(&pool).await?;
    preallocate_expected_machine_interface(txn.as_pgconn(), &expected_interface, None).await?;
    let interface_id = find_by_mac_address(txn.as_pgconn(), expected_interface.mac_address)
        .await?
        .pop()
        .expect("fixed Host preallocation should create an interface")
        .id;

    // DHCP applies the ExpectedMachine-wide primary declaration. A later Site
    // Explorer pass must not replace it with the Host creation default.
    set_primary_interface(&interface_id, false, txn.as_pgconn()).await?;
    preallocate_expected_machine_interface(txn.as_pgconn(), &expected_interface, None).await?;
    let interface = find_one(txn.as_pgconn(), interface_id).await?;
    txn.commit().await?;

    assert!(!interface.primary_interface);

    Ok(())
}

#[crate::sqlx_test]
async fn test_fixed_preallocation_resolves_managed_prefix_and_segment_type(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    create_static_assignments_segment(&pool).await?;
    let underlay_segment = create_managed_segment(
        &pool,
        "fixed-address-underlay",
        "198.51.100.0/24",
        NetworkSegmentType::Underlay,
        AllocationStrategy::Reserved,
    )
    .await?;
    let non_target_segment = create_test_segment(&pool, "fixed-address-non-target").await?;

    let mut txn = db::Transaction::begin(&pool).await?;
    let legacy_hint = ExpectedInterface {
        mac_address: "7A:7B:7C:7D:7E:61".parse()?,
        ip_allocation: Some(ExpectedInterfaceIpAllocation::Fixed),
        nic_type: Some("onboard".to_string()),
        fixed_ip: Some("198.51.100.61".parse()?),
        ..Default::default()
    };
    preallocate_expected_machine_interface(txn.as_pgconn(), &legacy_hint, None).await?;
    let interface = find_by_mac_address(txn.as_pgconn(), legacy_hint.mac_address)
        .await?
        .pop()
        .expect("legacy fixed interface should be preallocated");
    assert_eq!(interface.segment_id, underlay_segment);
    let reconciled = validate_existing_mac_and_create(
        txn.as_pgconn(),
        legacy_hint.mac_address,
        &["198.51.100.1".parse()?],
        Some(legacy_hint.clone()),
        None,
    )
    .await?;
    assert_eq!(
        reconciled.segment_id, underlay_segment,
        "legacy nic_type must not reject an existing row whose fixed IP selected the segment",
    );

    let legacy_typed_segment = ExpectedInterface {
        mac_address: "7A:7B:7C:7D:7E:64".parse()?,
        network_segment_type: Some(NetworkSegmentType::Underlay),
        fixed_ip: Some("203.0.113.64".parse()?),
        ..Default::default()
    };
    preallocate_expected_machine_interface(txn.as_pgconn(), &legacy_typed_segment, None).await?;
    let legacy_interface = find_by_mac_address(txn.as_pgconn(), legacy_typed_segment.mac_address)
        .await?
        .pop()
        .expect("legacy fixed interface should be preallocated");
    let static_assignments = db::network_segment::static_assignments(txn.as_pgconn()).await?;
    assert_eq!(
        legacy_interface.segment_id, static_assignments.id,
        "a legacy Host declaration must not turn its DHCP selector into a fixed-address guard",
    );

    let legacy_host_bmc = ExpectedInterface {
        mac_address: "7A:7B:7C:7D:7E:69".parse()?,
        role: ExpectedInterfaceRole::HostBmc,
        fixed_ip: Some("203.0.113.69".parse()?),
        ..Default::default()
    };
    preallocate_expected_machine_interface(txn.as_pgconn(), &legacy_host_bmc, None).await?;
    let legacy_host_bmc_interface =
        find_by_mac_address(txn.as_pgconn(), legacy_host_bmc.mac_address)
            .await?
            .pop()
            .expect("legacy Host BMC fixed interface should be preallocated");
    assert_eq!(legacy_host_bmc_interface.segment_id, static_assignments.id);
    assert_eq!(legacy_host_bmc_interface.interface_type, InterfaceType::Bmc);
    assert!(!legacy_host_bmc_interface.primary_interface);

    // The address alone cannot make an exact row idempotent when an explicit
    // policy resolves it to a different managed segment.
    let misplaced_mac: MacAddress = "7A:7B:7C:7D:7E:68".parse()?;
    let misplaced_ip: IpAddr = "198.51.100.68".parse()?;
    let misplaced_interface_id: MachineInterfaceId = sqlx::query_scalar(
        "INSERT INTO machine_interfaces
            (segment_id, mac_address, primary_interface, hostname)
         VALUES ($1, $2, true, 'fixed-address-non-target')
         RETURNING id",
    )
    .bind(non_target_segment)
    .bind(misplaced_mac)
    .fetch_one(txn.as_pgconn())
    .await?;
    crate::machine_interface_address::insert(
        txn.as_pgconn(),
        misplaced_interface_id,
        misplaced_ip,
        AllocationType::Static,
    )
    .await?;
    let misplaced_reservation = ExpectedInterface {
        mac_address: misplaced_mac,
        ip_allocation: Some(ExpectedInterfaceIpAllocation::Fixed),
        fixed_ip: Some(misplaced_ip),
        ..Default::default()
    };
    let error =
        preallocate_expected_machine_interface(txn.as_pgconn(), &misplaced_reservation, None)
            .await
            .expect_err("an exact reservation on another segment should be rejected");
    assert!(matches!(error, DatabaseError::InvalidArgument(_)));
    let misplaced_interface = find_one(txn.as_pgconn(), misplaced_interface_id).await?;
    assert_eq!(misplaced_interface.segment_id, non_target_segment);

    /// One generalized fixed-address declaration that must reject the
    /// out-of-prefix address used by this test.
    struct OutsidePrefixCase {
        name: &'static str,
        suffix: u8,
        role: ExpectedInterfaceRole,
        ip_allocation: Option<ExpectedInterfaceIpAllocation>,
        network_segment_type: Option<NetworkSegmentType>,
    }

    for case in [
        OutsidePrefixCase {
            name: "explicit Host Fixed policy",
            suffix: 0x65,
            role: ExpectedInterfaceRole::Host,
            ip_allocation: Some(ExpectedInterfaceIpAllocation::Fixed),
            network_segment_type: None,
        },
        OutsidePrefixCase {
            name: "DPU OS inferred Fixed policy",
            suffix: 0x66,
            role: ExpectedInterfaceRole::DpuOs,
            ip_allocation: None,
            network_segment_type: None,
        },
        OutsidePrefixCase {
            name: "DPU BMC inferred Fixed policy",
            suffix: 0x67,
            role: ExpectedInterfaceRole::DpuBmc,
            ip_allocation: None,
            network_segment_type: None,
        },
        OutsidePrefixCase {
            name: "explicit Host BMC Fixed policy",
            suffix: 0x6a,
            role: ExpectedInterfaceRole::HostBmc,
            ip_allocation: Some(ExpectedInterfaceIpAllocation::Fixed),
            network_segment_type: None,
        },
        OutsidePrefixCase {
            name: "inferred Host BMC Fixed policy with a segment guard",
            suffix: 0x6b,
            role: ExpectedInterfaceRole::HostBmc,
            ip_allocation: None,
            network_segment_type: Some(NetworkSegmentType::Underlay),
        },
    ] {
        let expected_interface = ExpectedInterface {
            mac_address: MacAddress::new([0x7a, 0x7b, 0x7c, 0x7d, 0x7e, case.suffix]),
            role: case.role,
            ip_allocation: case.ip_allocation,
            fixed_ip: Some(format!("203.0.113.{}", case.suffix).parse()?),
            network_segment_type: case.network_segment_type,
            ..Default::default()
        };

        let error =
            preallocate_expected_machine_interface(txn.as_pgconn(), &expected_interface, None)
                .await
                .expect_err(case.name);
        assert!(
            matches!(error, DatabaseError::InvalidArgument(_)),
            "case: {}",
            case.name,
        );
        assert!(
            find_by_mac_address(txn.as_pgconn(), expected_interface.mac_address)
                .await?
                .is_empty(),
            "case: {}",
            case.name,
        );
    }

    let wrong_guard = ExpectedInterface {
        mac_address: "7A:7B:7C:7D:7E:62".parse()?,
        ip_allocation: Some(ExpectedInterfaceIpAllocation::Fixed),
        network_segment_type: Some(NetworkSegmentType::Admin),
        fixed_ip: Some("198.51.100.62".parse()?),
        ..Default::default()
    };
    let error = preallocate_expected_machine_interface(txn.as_pgconn(), &wrong_guard, None)
        .await
        .expect_err("the typed segment guard should reject a different segment type");
    assert!(matches!(error, DatabaseError::InvalidArgument(_)));

    let outside_guard = ExpectedInterface {
        mac_address: "7A:7B:7C:7D:7E:63".parse()?,
        ip_allocation: Some(ExpectedInterfaceIpAllocation::Fixed),
        network_segment_type: Some(NetworkSegmentType::Underlay),
        fixed_ip: Some("203.0.113.63".parse()?),
        ..Default::default()
    };
    let error = preallocate_expected_machine_interface(txn.as_pgconn(), &outside_guard, None)
        .await
        .expect_err("a guarded fixed IP should belong to a configured segment");
    assert!(matches!(error, DatabaseError::InvalidArgument(_)));
    txn.rollback().await?;

    Ok(())
}

/// Existing Host declarations used `network_segment_type` only to narrow the
/// first DHCP segment choice. An explicit allocation policy turns that same
/// field into a guard for later DHCP reconciliation.
#[crate::sqlx_test]
async fn test_explicit_policy_opts_existing_host_interface_into_segment_guard(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    create_static_assignments_segment(&pool).await?;
    let segment_id = create_managed_segment(
        &pool,
        "legacy-host-segment-selector",
        "198.51.100.0/24",
        NetworkSegmentType::Underlay,
        AllocationStrategy::Dynamic,
    )
    .await?;
    let mac_address: MacAddress = "7A:7B:7C:7D:7E:65".parse()?;
    let fixed_ip = "198.51.100.65".parse()?;
    let relay = "198.51.100.1".parse()?;

    let mut txn = db::Transaction::begin(&pool).await?;
    preallocate_machine_interface(txn.as_pgconn(), mac_address, fixed_ip, None).await?;

    let legacy_interface = ExpectedInterface {
        mac_address,
        network_segment_type: Some(NetworkSegmentType::Admin),
        ..Default::default()
    };
    let reconciled = validate_existing_mac_and_create(
        txn.as_pgconn(),
        mac_address,
        &[relay],
        Some(legacy_interface.clone()),
        None,
    )
    .await?;
    assert_eq!(reconciled.segment_id, segment_id);

    let explicit_policy = ExpectedInterface {
        ip_allocation: Some(ExpectedInterfaceIpAllocation::Dynamic),
        ..legacy_interface
    };
    let error = validate_existing_mac_and_create(
        txn.as_pgconn(),
        mac_address,
        &[relay],
        Some(explicit_policy),
        None,
    )
    .await
    .expect_err("an explicit policy should enforce the typed segment guard");
    assert!(matches!(error, DatabaseError::FailedPrecondition(_)));
    assert!(
        error
            .to_string()
            .contains("do not identify the expected admin network segment type"),
        "{error}",
    );
    txn.rollback().await?;

    Ok(())
}

#[crate::sqlx_test]
async fn test_expected_interface_retained_policy_pins_all_dhcp_address_families(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    create_static_assignments_segment(&pool).await?;
    create_managed_segment(
        &pool,
        "retained-wrong-interface-type",
        "203.0.113.0/24",
        NetworkSegmentType::Underlay,
        AllocationStrategy::Dynamic,
    )
    .await?;
    create_managed_segment(
        &pool,
        "retained-wrong-segment-type",
        "198.51.100.0/24",
        NetworkSegmentType::Admin,
        AllocationStrategy::Dynamic,
    )
    .await?;
    let mac_address: MacAddress = "7A:7B:7C:7D:7E:71".parse()?;
    let wrong_segment_mac: MacAddress = "7A:7B:7C:7D:7E:72".parse()?;
    let addresses = ["192.0.2.71".parse()?, "2001:db8::71".parse()?];
    let bmc_address = "203.0.113.71".parse()?;
    let wrong_segment_address = "198.51.100.72".parse()?;

    let mut txn = db::Transaction::begin(&pool).await?;
    preallocate_machine_interface(txn.as_pgconn(), mac_address, addresses[0], None).await?;
    let interface_id = find_by_mac_address(txn.as_pgconn(), mac_address)
        .await?
        .pop()
        .expect("preallocation should create an interface")
        .id;
    crate::machine_interface_address::delete(txn.as_pgconn(), &interface_id).await?;
    for address in addresses {
        crate::machine_interface_address::insert(
            txn.as_pgconn(),
            interface_id,
            address,
            AllocationType::Dhcp,
        )
        .await?;
    }

    let bmc_segment =
        crate::network_segment::for_prefix_containing_address(txn.as_pgconn(), bmc_address)
            .await?
            .expect("BMC test address should belong to the managed segment");
    let bmc_interface = create_with_type(
        txn.as_pgconn(),
        &[bmc_segment],
        &mac_address,
        false,
        AddressSelectionStrategy::StaticAddress(bmc_address),
        InterfaceType::Bmc,
        None,
    )
    .await?;
    crate::machine_interface_address::delete(txn.as_pgconn(), &bmc_interface.id).await?;
    crate::machine_interface_address::insert(
        txn.as_pgconn(),
        bmc_interface.id,
        bmc_address,
        AllocationType::Dhcp,
    )
    .await?;

    preallocate_machine_interface(
        txn.as_pgconn(),
        wrong_segment_mac,
        wrong_segment_address,
        None,
    )
    .await?;
    let wrong_segment_interface_id = find_by_mac_address(txn.as_pgconn(), wrong_segment_mac)
        .await?
        .pop()
        .expect("wrong-segment preallocation should create an interface")
        .id;
    crate::machine_interface_address::delete(txn.as_pgconn(), &wrong_segment_interface_id).await?;
    crate::machine_interface_address::insert(
        txn.as_pgconn(),
        wrong_segment_interface_id,
        wrong_segment_address,
        AllocationType::Dhcp,
    )
    .await?;

    let expected_interface = ExpectedInterface {
        mac_address,
        role: ExpectedInterfaceRole::DpuOs,
        ip_allocation: Some(ExpectedInterfaceIpAllocation::Retained),
        ..Default::default()
    };
    retain_expected_machine_interface_address(txn.as_pgconn(), &expected_interface).await?;
    retain_expected_machine_interface_address(txn.as_pgconn(), &expected_interface).await?;
    let retained =
        crate::machine_interface_address::find_for_interface(txn.as_pgconn(), interface_id).await?;
    let bmc_addresses =
        crate::machine_interface_address::find_for_interface(txn.as_pgconn(), bmc_interface.id)
            .await?;

    let wrong_segment_expected_interface = ExpectedInterface {
        mac_address: wrong_segment_mac,
        role: ExpectedInterfaceRole::DpuOs,
        ip_allocation: Some(ExpectedInterfaceIpAllocation::Retained),
        network_segment_type: Some(NetworkSegmentType::Underlay),
        ..Default::default()
    };
    let error = retain_expected_machine_interface_address(
        txn.as_pgconn(),
        &wrong_segment_expected_interface,
    )
    .await
    .expect_err("the typed segment guard should reject a DHCP address on another segment type");
    assert!(matches!(error, DatabaseError::FailedPrecondition(_)));
    let wrong_segment_addresses = crate::machine_interface_address::find_for_interface(
        txn.as_pgconn(),
        wrong_segment_interface_id,
    )
    .await?;
    txn.commit().await?;

    assert_eq!(retained.len(), 2);
    assert!(
        retained
            .iter()
            .all(|address| address.allocation_type == AllocationType::Static),
    );
    assert_eq!(bmc_addresses.len(), 1);
    assert_eq!(bmc_addresses[0].allocation_type, AllocationType::Dhcp);
    assert_eq!(wrong_segment_addresses.len(), 1);
    assert_eq!(
        wrong_segment_addresses[0].allocation_type,
        AllocationType::Dhcp,
    );

    Ok(())
}

#[crate::sqlx_test]
async fn test_expected_interface_role_controls_observed_interface_creation(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let segment_id = create_managed_segment(
        &pool,
        "observed-interface-underlay",
        "203.0.113.0/24",
        NetworkSegmentType::Underlay,
        AllocationStrategy::Dynamic,
    )
    .await?;
    let relay = "203.0.113.1".parse()?;

    struct Case {
        name: &'static str,
        suffix: u8,
        role: ExpectedInterfaceRole,
        primary: Option<bool>,
        expected_type: InterfaceType,
        expected_primary: bool,
    }

    for Case {
        name,
        suffix,
        role,
        primary,
        expected_type,
        expected_primary,
    } in [
        Case {
            name: "Host false retains legacy creation default",
            suffix: 0x81,
            role: ExpectedInterfaceRole::Host,
            primary: Some(false),
            expected_type: InterfaceType::Data,
            expected_primary: true,
        },
        Case {
            name: "DPU OS",
            suffix: 0x82,
            role: ExpectedInterfaceRole::DpuOs,
            primary: None,
            expected_type: InterfaceType::Data,
            expected_primary: true,
        },
        Case {
            name: "DPU BMC",
            suffix: 0x83,
            role: ExpectedInterfaceRole::DpuBmc,
            primary: None,
            expected_type: InterfaceType::Bmc,
            expected_primary: false,
        },
    ] {
        let expected_interface = ExpectedInterface {
            mac_address: MacAddress::new([0x7a, 0x7b, 0x7c, 0x7d, 0x7e, suffix]),
            role,
            ip_allocation: Some(ExpectedInterfaceIpAllocation::Dynamic),
            network_segment_type: Some(NetworkSegmentType::Underlay),
            primary,
            ..Default::default()
        };
        let mut txn = db::Transaction::begin(&pool).await?;
        let interface = find_or_create_observed_machine_interface(
            txn.as_pgconn(),
            None,
            expected_interface.mac_address,
            &[relay],
            Some(expected_interface),
            None,
            None,
        )
        .await?;
        txn.commit().await?;

        assert_eq!(interface.segment_id, segment_id, "case: {name}");
        assert_eq!(interface.interface_type, expected_type, "case: {name}");
        assert_eq!(
            interface.primary_interface, expected_primary,
            "case: {name}",
        );
        assert!(interface.addresses.is_empty(), "case: {name}");
    }

    Ok(())
}

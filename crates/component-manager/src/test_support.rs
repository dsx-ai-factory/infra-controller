// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use std::net::IpAddr;

use carbide_uuid::machine::{MachineId, MachineIdSource, MachineInterfaceId, MachineType};
use carbide_uuid::network::NetworkSegmentId;
use carbide_uuid::power_shelf::{PowerShelfId, PowerShelfIdSource, PowerShelfType};
use carbide_uuid::rack::{RackId, RackProfileId};
use carbide_uuid::switch::{SwitchId, SwitchIdSource, SwitchType};
use mac_address::MacAddress;
use model::expected_machine::{ExpectedMachine, ExpectedMachineData};
use model::expected_power_shelf::ExpectedPowerShelf;
use model::expected_switch::ExpectedSwitch;
use model::machine::ManagedHostState;
use model::metadata::Metadata;
use model::power_shelf::{NewPowerShelf, PowerShelfConfig};
use model::rack::{RackConfig, RackState};
use model::switch::{NewSwitch, SwitchConfig};
use sqlx::PgPool;

pub(crate) const PS_MAC_1: &str = "AA:BB:CC:DD:EE:01";
pub(crate) const PS_MAC_2: &str = "AA:BB:CC:DD:EE:02";
pub(crate) const SW_MAC_1: &str = "AA:BB:CC:DD:FF:01";
pub(crate) const SW_MAC_2: &str = "AA:BB:CC:DD:FF:02";
pub(crate) const CT_MAC_1: &str = "AA:BB:CC:DD:CC:01";
pub(crate) const CT_MAC_2: &str = "AA:BB:CC:DD:CC:02";
pub(crate) const CT_IP_1: &str = "10.0.1.1";
pub(crate) const CT_IP_2: &str = "10.0.1.2";
pub(crate) const UNKNOWN_MAC: &str = "FF:FF:FF:FF:FF:FF";
pub(crate) const TEST_RACK_PROFILE_ID: &str = "NVL72";

pub(crate) fn test_power_shelf_id(label: &str) -> PowerShelfId {
    let mut hash = [0u8; 32];
    let bytes = label.as_bytes();
    hash[..bytes.len().min(32)].copy_from_slice(&bytes[..bytes.len().min(32)]);
    PowerShelfId::new(
        PowerShelfIdSource::ProductBoardChassisSerial,
        hash,
        PowerShelfType::Rack,
    )
}

pub(crate) fn test_machine_id(label: &str) -> MachineId {
    let mut hash = [0u8; 32];
    let bytes = label.as_bytes();
    hash[..bytes.len().min(32)].copy_from_slice(&bytes[..bytes.len().min(32)]);
    MachineId::new(
        MachineIdSource::ProductBoardChassisSerial,
        hash,
        MachineType::Host,
    )
}

pub(crate) fn test_switch_id(label: &str) -> SwitchId {
    let mut hash = [0u8; 32];
    let bytes = label.as_bytes();
    hash[..bytes.len().min(32)].copy_from_slice(&bytes[..bytes.len().min(32)]);
    SwitchId::new(SwitchIdSource::Tpm, hash, SwitchType::NvLink)
}

/// Seed a rack + two power shelves + two switches into the database. Returns
/// the IDs so tests can assert against them. The rack is transitioned into
/// `Ready` so component-manager wrapper preflight accepts it.
pub(crate) async fn seed_test_data(
    pool: &PgPool,
) -> (RackId, PowerShelfId, PowerShelfId, SwitchId, SwitchId) {
    let mut txn = pool.begin().await.unwrap();

    let rack_id = RackId::new(uuid::Uuid::new_v4().to_string());
    let rack_profile_id = RackProfileId::new(TEST_RACK_PROFILE_ID);
    let rack = db::rack::create(
        &mut txn,
        &rack_id,
        Some(&rack_profile_id),
        &RackConfig::default(),
        None,
    )
    .await
    .expect("failed to create rack");

    let ps1 = seed_power_shelf(&mut txn, PS_MAC_1, "PS-001", &rack_id).await;
    let ps2 = seed_power_shelf(&mut txn, PS_MAC_2, "PS-002", &rack_id).await;
    let sw1 = seed_switch(&mut txn, SW_MAC_1, "SW-001", &rack_id).await;
    let sw2 = seed_switch(&mut txn, SW_MAC_2, "SW-002", &rack_id).await;

    // Advance the freshly-created rack into Ready so on-demand-maintenance
    // preflight accepts it.
    let next_version = rack.controller_state.version.increment();
    let advanced = db::rack::try_update_controller_state(
        &mut txn,
        &rack_id,
        rack.controller_state.version,
        next_version,
        &RackState::Ready,
    )
    .await
    .expect("failed to advance rack to Ready");
    assert_eq!(
        advanced,
        db::ConditionalWrite::Applied(()),
        "rack controller_state version mismatch"
    );

    txn.commit().await.unwrap();
    (rack_id, ps1, ps2, sw1, sw2)
}

pub(crate) async fn seed_power_shelf(
    txn: &mut sqlx::PgConnection,
    mac: &str,
    label: &str,
    rack_id: &RackId,
) -> PowerShelfId {
    let ps_id = test_power_shelf_id(label);
    let mac: MacAddress = mac.parse().unwrap();

    db::expected_power_shelf::create(
        &mut *txn,
        ExpectedPowerShelf {
            expected_power_shelf_id: None,
            bmc_mac_address: mac,
            serial_number: label.to_owned(),
            bmc_username: "admin".into(),
            bmc_password: "pass".into(),
            bmc_ip_address: None,
            metadata: Metadata::default(),
            rack_id: Some(rack_id.clone()),
            bmc_retain_credentials: None,
        },
    )
    .await
    .expect("failed to create expected power shelf");

    db::power_shelf::create(
        &mut *txn,
        &NewPowerShelf {
            id: ps_id,
            config: PowerShelfConfig {
                name: label.to_owned(),
                capacity: None,
                voltage: None,
            },
            bmc_mac_address: Some(mac),
            metadata: Some(Metadata::default()),
            rack_id: Some(rack_id.clone()),
        },
    )
    .await
    .expect("failed to create power shelf");

    ps_id
}

pub(crate) async fn seed_switch(
    txn: &mut sqlx::PgConnection,
    mac: &str,
    label: &str,
    rack_id: &RackId,
) -> SwitchId {
    let sw_id = test_switch_id(label);
    let mac: MacAddress = mac.parse().unwrap();

    db::expected_switch::create(
        &mut *txn,
        ExpectedSwitch {
            expected_switch_id: None,
            serial_number: label.to_owned(),
            bmc_mac_address: mac,
            bmc_ip_address: None,
            nvos_ip_address: None,
            bmc_username: "admin".into(),
            bmc_password: "pass".into(),
            nvos_username: None,
            nvos_password: None,
            nvos_mac_addresses: vec![],
            metadata: Metadata::default(),
            rack_id: Some(rack_id.clone()),
            bmc_retain_credentials: None,
        },
    )
    .await
    .expect("failed to create expected switch");

    db::switch::create(
        &mut *txn,
        &NewSwitch {
            id: sw_id,
            config: SwitchConfig {
                name: label.to_owned(),
                enable_nmxc: false,
                fabric_manager_config: None,
            },
            bmc_mac_address: Some(mac),
            metadata: Some(Metadata::default()),
            rack_id: Some(rack_id.clone()),
            slot_number: None,
            tray_index: None,
        },
    )
    .await
    .expect("failed to create switch");

    sw_id
}

/// Ensure an admin network segment exists for component-manager sqlx tests.
/// Migrated template DBs used by `#[carbide_macros::sqlx_test]` do not seed
/// network segments, but compute tray RMS tests need BMC interface rows.
async fn ensure_admin_network_segment(txn: &mut sqlx::PgConnection) -> NetworkSegmentId {
    if let Ok(segments) = db::network_segment::admin(&mut *txn).await
        && let Some(segment) = segments.into_iter().next()
    {
        return segment.id;
    }

    let segment_id: NetworkSegmentId = sqlx::query_scalar(
        "INSERT INTO network_segments (name, version, network_segment_type) \
         VALUES ($1, 'V1-T0', 'admin') RETURNING id",
    )
    .bind(format!("cm-test-admin-{}", uuid::Uuid::new_v4()))
    .fetch_one(&mut *txn)
    .await
    .expect("failed to create admin network segment");

    sqlx::query(
        "INSERT INTO network_prefixes (segment_id, prefix, gateway, num_reserved) \
         VALUES ($1, '10.0.0.0/8'::cidr, '10.0.0.1'::inet, 0)",
    )
    .bind(segment_id)
    .execute(&mut *txn)
    .await
    .expect("failed to create admin network prefix");

    segment_id
}

async fn seed_bmc_interface(
    txn: &mut sqlx::PgConnection,
    segment_id: NetworkSegmentId,
    machine_id: &MachineId,
    mac: MacAddress,
    bmc_ip: IpAddr,
    hostname: &str,
) {
    let interface_id: MachineInterfaceId = sqlx::query_scalar(
        "INSERT INTO machine_interfaces \
            (segment_id, mac_address, primary_interface, hostname, machine_id, interface_type, association_type) \
         VALUES ($1, $2, false, $3, $4, 'Bmc', 'Machine') \
         RETURNING id",
    )
    .bind(segment_id)
    .bind(mac)
    .bind(hostname)
    .bind(machine_id.to_string())
    .fetch_one(&mut *txn)
    .await
    .expect("failed to create BMC machine interface");

    sqlx::query(
        "INSERT INTO machine_interface_addresses (interface_id, address, allocation_type) \
         VALUES ($1, $2, 'static')",
    )
    .bind(interface_id)
    .bind(bmc_ip)
    .execute(&mut *txn)
    .await
    .expect("failed to create BMC machine interface address");
}

pub(crate) async fn seed_machine(
    txn: &mut sqlx::PgConnection,
    mac: &str,
    bmc_ip: &str,
    label: &str,
    rack_id: &RackId,
) -> MachineId {
    let machine_id = test_machine_id(label);
    let mac: MacAddress = mac.parse().unwrap();
    let bmc_ip: IpAddr = bmc_ip.parse().unwrap();
    let expected_data = ExpectedMachineData {
        serial_number: label.to_owned(),
        rack_id: Some(rack_id.clone()),
        bmc_ip_address: Some(bmc_ip),
        ..Default::default()
    };

    db::expected_machine::create(
        &mut *txn,
        ExpectedMachine {
            id: None,
            bmc_mac_address: mac,
            data: expected_data.clone(),
        },
    )
    .await
    .expect("failed to create expected machine");

    db::machine::create(
        &mut *txn,
        None,
        &machine_id,
        ManagedHostState::Ready,
        Some(&expected_data),
        2,
    )
    .await
    .expect("failed to create machine");

    let segment_id = ensure_admin_network_segment(&mut *txn).await;
    seed_bmc_interface(&mut *txn, segment_id, &machine_id, mac, bmc_ip, label).await;

    machine_id
}

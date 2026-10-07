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

use std::collections::HashMap;

use carbide_uuid::switch::{SwitchIdSource, SwitchType};
use mac_address::MacAddress;
use model::address_selection_strategy::AddressSelectionStrategy;
use model::expected_switch::ExpectedSwitch;
use model::machine_interface_address::MachineInterfaceAssociation;
use model::metadata::Metadata;
use model::network_prefix::NewNetworkPrefix;
use model::network_segment::{
    AllocationStrategy, NetworkSegmentControllerState, NetworkSegmentType, NewNetworkSegment,
};
use model::switch::{NewSwitch, Switch, SwitchConfig, switch_id};
use sqlx::PgConnection;

use crate::{DatabaseError, expected_switch, machine_interface, network_segment, switch};

fn seeded_mac(prefix: [u8; 4], seed: u8) -> MacAddress {
    MacAddress::new([prefix[0], prefix[1], prefix[2], prefix[3], seed, 0])
}

async fn ensure_network_segment(
    txn: &mut PgConnection,
    name: &str,
    prefix: &str,
    gateway: &str,
    segment_type: NetworkSegmentType,
) -> Result<(), DatabaseError> {
    const FIND_SEGMENT: &str = "SELECT EXISTS(SELECT 1 FROM network_segments WHERE name = $1)";
    let exists: bool = sqlx::query_scalar(FIND_SEGMENT)
        .bind(name)
        .fetch_one(&mut *txn)
        .await
        .map_err(|error| DatabaseError::query(FIND_SEGMENT, error))?;
    if exists {
        return Ok(());
    }

    network_segment::persist(
        NewNetworkSegment {
            id: uuid::Uuid::new_v4().into(),
            name: name.to_string(),
            subdomain_id: None,
            vpc_id: None,
            mtu: 1500,
            prefixes: vec![NewNetworkPrefix {
                prefix: prefix.parse().expect("test prefix should parse"),
                gateway: Some(gateway.parse().expect("test gateway should parse")),
                dhcpv6_link_address: None,
                num_reserved: 3,
            }],
            vlan_id: None,
            vni: None,
            segment_type,
            can_stretch: None,
            allocation_strategy: AllocationStrategy::Dynamic,
            infer_slaac_eui64_addresses: false,
        },
        txn,
        NetworkSegmentControllerState::Ready,
    )
    .await?;

    Ok(())
}

async fn ensure_network_segments(txn: &mut PgConnection) -> Result<(), DatabaseError> {
    ensure_network_segment(
        txn,
        "ADMIN",
        "192.0.2.0/24",
        "192.0.2.1",
        NetworkSegmentType::Admin,
    )
    .await?;
    ensure_network_segment(
        txn,
        "UNDERLAY",
        "192.0.1.0/24",
        "192.0.1.1",
        NetworkSegmentType::Underlay,
    )
    .await
}

/// Creates a discovered switch and its expected-switch and interface records.
///
/// Reusing the same seed in the same database or transaction will collide with
/// the deterministic BMC and NVOS MAC addresses and switch ID.
pub async fn create_seeded_discovered(
    txn: &mut PgConnection,
    seed: u8,
    name: &str,
) -> Result<Switch, DatabaseError> {
    ensure_network_segments(txn).await?;

    let bmc_mac_address = seeded_mac([0x44, 0x44, 0x11, 0x11], seed);
    let nvos_mac_address = seeded_mac([0x44, 0x44, 0x33, 0x33], seed);
    let serial_number = format!("SW-SN-{seed:03}");
    let expected = expected_switch::create(
        txn,
        ExpectedSwitch {
            expected_switch_id: None,
            bmc_mac_address,
            nvos_mac_addresses: vec![nvos_mac_address],
            serial_number: serial_number.clone(),
            bmc_username: "ADMIN".into(),
            bmc_password: "Pwd2023x0x0x0x7".into(),
            nvos_username: None,
            nvos_password: None,
            bmc_ip_address: None,
            nvos_ip_address: None,
            metadata: Metadata {
                name: name.to_string(),
                description: format!("Test Switch {seed}"),
                labels: HashMap::new(),
            },
            rack_id: None,
            bmc_retain_credentials: None,
        },
    )
    .await?;

    let admin_segments = network_segment::admin(txn).await?;
    machine_interface::create(
        txn,
        &admin_segments,
        &nvos_mac_address,
        false,
        AddressSelectionStrategy::NextAvailableIp,
        None,
    )
    .await?;

    let underlay = network_segment::find_by_name(txn, "UNDERLAY").await?;
    let bmc_interface = machine_interface::create(
        txn,
        std::slice::from_ref(&underlay),
        &bmc_mac_address,
        false,
        AddressSelectionStrategy::NextAvailableIp,
        None,
    )
    .await?;

    let id = switch_id::from_hardware_info(
        &serial_number,
        "NVIDIA",
        "Switch",
        SwitchIdSource::ProductBoardChassisSerial,
        SwitchType::NvLink,
    )
    .map_err(|error| DatabaseError::internal(format!("failed to create switch ID: {error:?}")))?;
    let created = switch::create(
        txn,
        &NewSwitch {
            id,
            config: SwitchConfig {
                name: expected.metadata.name,
                enable_nmxc: false,
                fabric_manager_config: None,
            },
            bmc_mac_address: Some(bmc_mac_address),
            metadata: None,
            rack_id: None,
            slot_number: Some(0),
            tray_index: Some(0),
        },
    )
    .await?;

    // Mirror site-explorer ingestion: link the switch's BMC interface back to
    // the switch and annotate it `Bmc`, so that `bmc_info` resolves through
    // the production database path.
    machine_interface::associate_bmc_interface(
        &bmc_interface.id,
        MachineInterfaceAssociation::Switch(id),
        txn,
    )
    .await?;

    Ok(created)
}

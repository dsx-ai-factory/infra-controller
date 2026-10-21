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

use carbide_uuid::machine::{MachineId, MachineIdSource, MachineInterfaceId, MachineType};
use carbide_uuid::network::NetworkSegmentId;
use chrono::{Duration, Utc};
use mac_address::MacAddress;
use model::allocation_type::AllocationType;
use model::attestation::spdm::SlotInfo;
use model::bmc_info::BmcInfo;
use model::machine::topology::{DiscoveryData, TopologyData};
use model::machine::{CURRENT_STATE_MODEL_VERSION, ManagedHostState};
use sqlx::Connection;

use super::*;

#[crate::sqlx_test]
async fn spdm_snapshot_survives_added_columns(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut api_connection = pool.acquire().await?;
    exercise_snapshot(&mut api_connection).await?;
    assert!(api_connection.cached_statements_size() > 0);

    // Keep the API's prepared statements while another connection commits DDL.
    let mut migration = pool.begin().await?;
    sqlx::raw_sql(
        "SET LOCAL lock_timeout = '5s';
         ALTER TABLE spdm_machine_devices_attestation ADD COLUMN test_added_column text;",
    )
    .execute(&mut *migration)
    .await?;
    migration.commit().await?;

    exercise_snapshot(&mut api_connection).await?;
    Ok(())
}

#[crate::sqlx_test]
async fn spdm_snapshot_uses_topology_selected_bmc(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    // `machine_interface_id` selects the BMC for an SPDM snapshot. If that
    // interface becomes unavailable, the attestation remains loadable without
    // falling back to another BMC. A topology without an ID still uses an
    // attached BMC.
    let mut txn = pool.begin().await?;
    let machine_id = MachineId::new(
        MachineIdSource::ProductBoardChassisSerial,
        [0x70; 32],
        MachineType::Host,
    );
    crate::machine::create(
        txn.as_mut(),
        None,
        &machine_id,
        ManagedHostState::Ready,
        None,
        CURRENT_STATE_MODEL_VERSION,
    )
    .await?;
    let segment_id: NetworkSegmentId = sqlx::query_scalar(
        "INSERT INTO network_segments (name, version, network_segment_type)
         VALUES ('selector-spdm-bmc', 'V1-T0', 'tenant') RETURNING id",
    )
    .fetch_one(txn.as_mut())
    .await?;

    let selected_created = Utc::now();
    let stale_interface_id = insert_bmc_interface(
        txn.as_mut(),
        machine_id,
        segment_id,
        "stale-bmc",
        "02:00:00:00:70:68".parse()?,
        "192.0.2.70".parse()?,
        selected_created - Duration::days(1),
    )
    .await?;
    let selected_interface_id = insert_bmc_interface(
        txn.as_mut(),
        machine_id,
        segment_id,
        "selected-bmc",
        "02:00:00:00:70:69".parse()?,
        "192.0.2.71".parse()?,
        selected_created,
    )
    .await?;

    let topology = TopologyData {
        discovery_data: DiscoveryData {
            info: Default::default(),
        },
        bmc_info: BmcInfo {
            machine_interface_id: Some(selected_interface_id),
            port: Some(8443),
            ..Default::default()
        },
    };
    sqlx::query("INSERT INTO machine_topologies (machine_id, topology) VALUES ($1, $2)")
        .bind(machine_id)
        .bind(sqlx::types::Json(topology))
        .execute(txn.as_mut())
        .await?;

    let attestation = SpdmDeviceAttestation {
        machine_id,
        device_id: "selector-device".to_string(),
        bmc_info: Default::default(),
        nonce: "11111111-2222-3333-4444-666666666666".parse()?,
        state: SpdmAttestationState::FetchMetadata,
        state_version: "V1-T0".parse()?,
        state_outcome: None,
        metadata: None,
        ca_certificate_link: None,
        ca_certificate: None,
        evidence_target: None,
        evidence: None,
        started_at: Utc::now(),
        cancelled_at: None,
        completed_at: None,
    };
    insert_device_attestations(txn.as_mut(), &machine_id, vec![attestation.clone()]).await?;

    let selected =
        load_snapshot_for_machine_and_device_id(txn.as_mut(), &machine_id, &attestation.device_id)
            .await?;
    assert_eq!(
        selected.bmc_info,
        BmcInfo {
            machine_interface_id: Some(selected_interface_id),
            ip: Some("192.0.2.71".parse()?),
            port: Some(8443),
            mac: Some("02:00:00:00:70:69".parse()?),
            ..Default::default()
        }
    );

    sqlx::query(
        "UPDATE machine_interfaces
         SET machine_id = NULL, association_type = 'None'
         WHERE id = $1",
    )
    .bind(selected_interface_id)
    .execute(txn.as_mut())
    .await?;
    let unavailable =
        load_snapshot_for_machine_and_device_id(txn.as_mut(), &machine_id, &attestation.device_id)
            .await?;
    assert_eq!(
        unavailable.bmc_info,
        BmcInfo {
            machine_interface_id: Some(selected_interface_id),
            port: Some(8443),
            ..Default::default()
        }
    );

    sqlx::query(
        "UPDATE machine_topologies
         SET topology = jsonb_set(
             topology,
             '{bmc_info,machine_interface_id}',
             'null'::jsonb
         )
         WHERE machine_id = $1",
    )
    .bind(machine_id)
    .execute(txn.as_mut())
    .await?;
    let fallback =
        load_snapshot_for_machine_and_device_id(txn.as_mut(), &machine_id, &attestation.device_id)
            .await?;
    assert_eq!(
        fallback.bmc_info,
        BmcInfo {
            machine_interface_id: Some(stale_interface_id),
            ip: Some("192.0.2.70".parse()?),
            port: Some(8443),
            mac: Some("02:00:00:00:70:68".parse()?),
            ..Default::default()
        }
    );

    txn.rollback().await?;
    Ok(())
}

async fn exercise_snapshot(
    connection: &mut PgConnection,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = connection.begin().await?;
    let machine_id = MachineId::new(
        MachineIdSource::ProductBoardChassisSerial,
        [0x69; 32],
        MachineType::Host,
    );
    crate::machine::create(
        txn.as_mut(),
        None,
        &machine_id,
        ManagedHostState::Ready,
        None,
        CURRENT_STATE_MODEL_VERSION,
    )
    .await?;
    let segment_id: NetworkSegmentId = sqlx::query_scalar(
        "INSERT INTO network_segments (name, version, network_segment_type)
         VALUES ('projection-spdm-bmc', 'V1-T0', 'tenant') RETURNING id",
    )
    .fetch_one(txn.as_mut())
    .await?;
    let mac_address: MacAddress = "02:00:00:00:68:69".parse()?;
    let interface_id: MachineInterfaceId = sqlx::query_scalar(
        "INSERT INTO machine_interfaces
         (machine_id, association_type, segment_id, mac_address,
          primary_interface, hostname, interface_type)
         VALUES ($1, 'Machine', $2, $3, false, 'bmc', 'Bmc') RETURNING id",
    )
    .bind(machine_id)
    .bind(segment_id)
    .bind(mac_address)
    .fetch_one(txn.as_mut())
    .await?;
    let bmc_ip = "192.0.2.69".parse()?;
    crate::machine_interface_address::insert(
        txn.as_mut(),
        interface_id,
        bmc_ip,
        AllocationType::Dhcp,
    )
    .await?;
    let bmc_info = BmcInfo {
        machine_interface_id: Some(interface_id),
        ip: Some(bmc_ip),
        mac: Some(mac_address),
        port: Some(8443),
        version: Some("BMC model".to_string()),
        firmware_version: Some("1.2.3".to_string()),
    };
    let topology = TopologyData {
        discovery_data: DiscoveryData {
            info: Default::default(),
        },
        bmc_info: BmcInfo {
            ip: Some("198.51.100.69".parse()?),
            mac: Some("02:00:00:00:00:01".parse()?),
            ..bmc_info.clone()
        },
    };
    sqlx::query("INSERT INTO machine_topologies (machine_id, topology) VALUES ($1, $2)")
        .bind(machine_id)
        .bind(sqlx::types::Json(topology))
        .execute(txn.as_mut())
        .await?;

    let expected = SpdmDeviceAttestation {
        machine_id,
        device_id: "HGX_IRoT_GPU_0".to_string(),
        bmc_info,
        nonce: "11111111-2222-3333-4444-555555555555".parse()?,
        state: SpdmAttestationState::Cancelled,
        state_version: "V7-T0".parse()?,
        state_outcome: Some(PersistentStateHandlerOutcome::Wait {
            reason: "waiting for attestation evidence".to_string(),
            source_ref: None,
        }),
        metadata: Some(SpdmMachineDeviceMetadata {
            firmware_version: Some("gpu-firmware-2".to_string()),
        }),
        ca_certificate_link: Some("/redfish/v1/Certificates/1".to_string()),
        ca_certificate: Some(CaCertificate {
            certificate_string: "projection-certificate".to_string(),
            certificate_type: "PEM".to_string(),
            certificate_usage_types: vec!["DigitalSignature".to_string()],
            id: "certificate-1".to_string(),
            name: "device certificate".to_string(),
            spdm: SlotInfo { slot_id: 2 },
        }),
        evidence_target: Some("/redfish/v1/ComponentIntegrity/1/Actions/GetEvidence".to_string()),
        evidence: Some(Evidence {
            hashing_algorithm: "SHA384".to_string(),
            signed_measurements: "projection-measurements".to_string(),
            signing_algorithm: "ECDSA".to_string(),
            version: "1.2".to_string(),
        }),
        started_at: "2026-09-01T12:00:00Z".parse()?,
        cancelled_at: Some("2026-09-01T12:01:00Z".parse()?),
        completed_at: Some("2026-09-01T12:02:00Z".parse()?),
    };
    assert_eq!(
        insert_device_attestations(txn.as_mut(), &machine_id, vec![expected.clone()]).await?,
        1
    );
    sqlx::query(
        "UPDATE spdm_machine_devices_attestation
         SET metadata = $1, ca_certificate = $2, evidence = $3,
             state_outcome = $4, cancelled_at = $5, completed_at = $6
         WHERE machine_id = $7 AND device_id = $8",
    )
    .bind(sqlx::types::Json(&expected.metadata))
    .bind(sqlx::types::Json(&expected.ca_certificate))
    .bind(sqlx::types::Json(&expected.evidence))
    .bind(sqlx::types::Json(&expected.state_outcome))
    .bind(expected.cancelled_at)
    .bind(expected.completed_at)
    .bind(machine_id)
    .bind(&expected.device_id)
    .execute(txn.as_mut())
    .await?;

    let found =
        load_snapshot_for_machine_and_device_id(txn.as_mut(), &machine_id, &expected.device_id)
            .await?;
    assert_eq!(
        serde_json::to_value(&found)?,
        serde_json::to_value(&expected)?
    );
    txn.rollback().await?;
    Ok(())
}

/// Adds an addressed BMC fixture with a controlled creation time for selection
/// checks.
async fn insert_bmc_interface(
    txn: &mut PgConnection,
    machine_id: MachineId,
    segment_id: NetworkSegmentId,
    hostname: &str,
    mac_address: MacAddress,
    ip_address: IpAddr,
    created: chrono::DateTime<Utc>,
) -> Result<MachineInterfaceId, Box<dyn std::error::Error>> {
    let interface_id = sqlx::query_scalar(
        "INSERT INTO machine_interfaces
         (machine_id, association_type, segment_id, mac_address,
          primary_interface, hostname, interface_type, created)
         VALUES ($1, 'Machine', $2, $3, false, $4, 'Bmc', $5)
         RETURNING id",
    )
    .bind(machine_id)
    .bind(segment_id)
    .bind(mac_address)
    .bind(hostname)
    .bind(created)
    .fetch_one(&mut *txn)
    .await?;
    crate::machine_interface_address::insert(txn, interface_id, ip_address, AllocationType::Dhcp)
        .await?;
    Ok(interface_id)
}

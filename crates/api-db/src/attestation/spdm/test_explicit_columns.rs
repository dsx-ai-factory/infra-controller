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

use carbide_uuid::machine::{MachineIdSource, MachineInterfaceId, MachineType};
use carbide_uuid::network::NetworkSegmentId;
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

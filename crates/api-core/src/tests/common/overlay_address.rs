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

use carbide_uuid::instance::InstanceId;
use carbide_uuid::machine::{MachineId, MachineIdSource, MachineType};
use carbide_uuid::network::NetworkSegmentId;
use carbide_uuid::vpc::VpcId;
use ipnetwork::IpNetwork;
use model::instance_address::InstanceAddress;
use sha2::{Digest, Sha256};

/// `seed_overlay_address_owner` writes the smallest set of valid rows needed
/// to represent one tenant overlay address owner. The instance configs remain
/// decodable as an `InstanceSnapshot`, while raw address-lookup tests can skip
/// controllers, network prefixes, and the full managed-host lifecycle.
pub(in crate::tests) async fn seed_overlay_address_owner(
    conn: &mut sqlx::PgConnection,
    fixture_name: &str,
    address: IpAddr,
) -> InstanceAddress {
    let vpc_id: VpcId =
        sqlx::query_scalar("INSERT INTO vpcs (name, version) VALUES ($1, $2) RETURNING id")
            .bind(format!("vpc-{fixture_name}"))
            .bind("1")
            .fetch_one(&mut *conn)
            .await
            .unwrap();

    let machine_hash: [u8; 32] = Sha256::digest(fixture_name.as_bytes()).into();
    let machine_id = MachineId::new(
        MachineIdSource::ProductBoardChassisSerial,
        machine_hash,
        MachineType::Host,
    );
    sqlx::query("INSERT INTO machines (id, dpf) VALUES ($1, '{}'::jsonb)")
        .bind(machine_id)
        .execute(&mut *conn)
        .await
        .unwrap();

    let instance_id: InstanceId = sqlx::query_scalar(
        "INSERT INTO instances \
         (machine_id, os_ipxe_script, network_config, nvlink_config) \
         VALUES ($1, $2, '{\"interfaces\": []}'::jsonb, '{\"gpu_configs\": []}'::jsonb) \
         RETURNING id",
    )
    .bind(machine_id)
    .bind("#!ipxe\nexit")
    .fetch_one(&mut *conn)
    .await
    .unwrap();

    let segment_id: NetworkSegmentId = sqlx::query_scalar(
        "INSERT INTO network_segments (name, version, network_segment_type, vpc_id) \
         VALUES ($1, $2, 'tenant', $3) RETURNING id",
    )
    .bind(format!("segment-{fixture_name}"))
    .bind("1")
    .bind(vpc_id)
    .fetch_one(&mut *conn)
    .await
    .unwrap();

    let prefix_length = if address.is_ipv4() { 32 } else { 128 };
    let prefix = IpNetwork::new(address, prefix_length).unwrap();
    sqlx::query_as(
        "INSERT INTO instance_addresses \
         (instance_id, address, segment_id, prefix, vpc_id) \
         VALUES ($1, $2, $3, $4, $5) \
         RETURNING instance_id, segment_id, vpc_id, address, prefix, hostname",
    )
    .bind(instance_id)
    .bind(address)
    .bind(segment_id)
    .bind(prefix)
    .bind(vpc_id)
    .fetch_one(conn)
    .await
    .unwrap()
}

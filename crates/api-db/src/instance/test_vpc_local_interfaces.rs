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

//! Which VPCs a DPU serves through VPC-local interfaces, read straight from
//! the rows the query joins: machines, the host interfaces attached to DPUs,
//! and the instance's persisted network and attachment documents.

use carbide_test_support::Outcome::Yields;
use carbide_test_support::{Case, check_cases_async};
use carbide_uuid::extension_service::ExtensionServiceId;
use carbide_uuid::machine::{DpuMachineId, MachineId, MachineIdSource, MachineType};
use carbide_uuid::vpc::VpcId;
use chrono::Utc;
use mac_address::MacAddress;
use model::instance::config::extension_services::{
    InstanceExtensionServiceConfig, InstanceExtensionServicesConfig,
};
use model::instance::config::network::{
    InstanceNetworkConfig, InstanceServiceInterfaceConfig, InstanceVpcLocalInterfaceConfig,
};
use sqlx::PgPool;

const SEGMENT_ID: &str = "20000000-0000-0000-0000-000000000002";
/// Fixed VPC ids in ascending order, so the query's ordered answer needs no sorting.
const VPC_IDS: [&str; 3] = [
    "10000000-0000-0000-0000-00000000000a",
    "10000000-0000-0000-0000-00000000000b",
    "10000000-0000-0000-0000-00000000000c",
];

fn host(n: u8) -> MachineId {
    MachineId::new(
        MachineIdSource::ProductBoardChassisSerial,
        [n; 32],
        MachineType::Host,
    )
}

fn dpu(n: u8) -> DpuMachineId {
    MachineId::new(
        MachineIdSource::ProductBoardChassisSerial,
        [n; 32],
        MachineType::Dpu,
    )
    .try_into()
    .expect("a DPU machine id")
}

/// An attachment as the registration path records it; `terminating` sets `removed`.
fn attachment(id: uuid::Uuid, terminating: bool) -> InstanceExtensionServiceConfig {
    InstanceExtensionServiceConfig {
        id: Some(id),
        service_id: ExtensionServiceId::new(),
        version: "V1-T0".parse().expect("fixture version"),
        dpu_target: None,
        removed: terminating.then(Utc::now),
    }
}

fn vpc_local(
    attachment_id: uuid::Uuid,
    dpu_id: DpuMachineId,
    vpc_id: VpcId,
) -> InstanceVpcLocalInterfaceConfig {
    InstanceVpcLocalInterfaceConfig {
        attachment_id,
        interface_ordinal: 0,
        dpu_id,
        slot_index: 0,
        vpc_id,
        mac_address: MacAddress::new([0x02, 0, 0, 0, 0, 2]),
        internal_uuid: uuid::Uuid::new_v4(),
    }
}

/// A service-VPC interface for `vpc_id` on `dpu_id`: the other kind of server-owned record.
fn service_interface(
    attachment_id: uuid::Uuid,
    dpu_id: DpuMachineId,
    vpc_id: VpcId,
) -> InstanceServiceInterfaceConfig {
    InstanceServiceInterfaceConfig {
        attachment_id,
        interface_ordinal: 0,
        dpu_id,
        slot_index: 0,
        vpc_id,
        vpc_prefix_id: carbide_uuid::vpc::VpcPrefixId::new(),
        network_segment_id: SEGMENT_ID.parse().expect("fixture segment id"),
        network_prefix_id: carbide_uuid::network::NetworkPrefixId::new(),
        link_prefix: "192.0.2.0/31".parse().expect("valid service prefix"),
        mac_address: MacAddress::new([0x02, 0, 0, 0, 0, 1]),
        internal_uuid: uuid::Uuid::new_v4(),
    }
}

/// Host `n` with one data interface per DPU in `dpus`, running one instance
/// (released when `released`) that persisted `network` and `attachments`.
async fn seed_host(
    pool: &PgPool,
    n: u8,
    dpus: &[u8],
    network: &InstanceNetworkConfig,
    attachments: &InstanceExtensionServicesConfig,
    released: bool,
) {
    let host = host(n);
    sqlx::query("INSERT INTO machines (id, dpf) VALUES ($1, '{}'::jsonb)")
        .bind(host)
        .execute(pool)
        .await
        .unwrap();
    for (position, &d) in dpus.iter().enumerate() {
        let dpu = dpu(d);
        sqlx::query("INSERT INTO machines (id, dpf) VALUES ($1, '{}'::jsonb)")
            .bind(dpu)
            .execute(pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO machine_interfaces
                 (machine_id, attached_dpu_machine_id, segment_id, mac_address,
                  primary_interface, hostname, association_type)
             VALUES ($1, $2, $3::uuid, $4::macaddr, $5, $1, 'Machine')",
        )
        .bind(host)
        .bind(dpu)
        .bind(SEGMENT_ID)
        .bind(format!("02:00:00:00:{n:02x}:{d:02x}"))
        .bind(position == 0)
        .execute(pool)
        .await
        .unwrap();
    }
    sqlx::query(
        "INSERT INTO instances (machine_id, network_config, extension_services_config, deleted)
         VALUES ($1, $2, $3, CASE WHEN $4 THEN now() END)",
    )
    .bind(host)
    .bind(sqlx::types::Json(network))
    .bind(sqlx::types::Json(attachments))
    .bind(released)
    .execute(pool)
    .await
    .unwrap();
}

async fn served_by(pool: &PgPool, dpu: DpuMachineId) -> Result<Vec<VpcId>, String> {
    super::find_vpc_local_vpc_ids_for_dpu(pool, dpu)
        .await
        .map_err(|error| error.to_string())
}

/// A DPU serves exactly the VPCs named by live VPC-local interface records on
/// the live instance of its host. A record of a terminating or missing
/// attachment, a service-VPC interface, a released instance, another host's
/// records and a sibling DPU's records grant nothing.
#[crate::sqlx_test]
async fn dpu_serves_the_vpcs_of_its_live_vpc_local_interfaces(pool: PgPool) {
    sqlx::query(
        "INSERT INTO network_segments (id, name, version) VALUES ($1::uuid, 'seg', 'test')",
    )
    .bind(SEGMENT_ID)
    .execute(&pool)
    .await
    .unwrap();
    let [vpc_a, vpc_b, vpc_c] = VPC_IDS.map(|id| id.parse::<VpcId>().expect("fixture VPC id"));

    // DPU 1 serves A and C through two live records; its service-VPC
    // interface for B is the other kind of record and grants nothing.
    let (a1, c1, b1) = (
        uuid::Uuid::new_v4(),
        uuid::Uuid::new_v4(),
        uuid::Uuid::new_v4(),
    );
    seed_host(
        &pool,
        1,
        &[1],
        &InstanceNetworkConfig {
            vpc_local_interfaces: vec![vpc_local(a1, dpu(1), vpc_a), vpc_local(c1, dpu(1), vpc_c)],
            service_interfaces: vec![service_interface(b1, dpu(1), vpc_b)],
            ..Default::default()
        },
        &InstanceExtensionServicesConfig {
            service_configs: vec![
                attachment(a1, false),
                attachment(c1, false),
                attachment(b1, false),
            ],
        },
        false,
    )
    .await;
    // DPU 2 serves B alone.
    let b2 = uuid::Uuid::new_v4();
    seed_host(
        &pool,
        2,
        &[2],
        &InstanceNetworkConfig {
            vpc_local_interfaces: vec![vpc_local(b2, dpu(2), vpc_b)],
            ..Default::default()
        },
        &InstanceExtensionServicesConfig {
            service_configs: vec![attachment(b2, false)],
        },
        false,
    )
    .await;
    // DPU 3 serves C; its record for A belongs to a terminating attachment
    // and its record for B to no attachment at all.
    let (c3, a3) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
    seed_host(
        &pool,
        3,
        &[3],
        &InstanceNetworkConfig {
            vpc_local_interfaces: vec![
                vpc_local(c3, dpu(3), vpc_c),
                vpc_local(a3, dpu(3), vpc_a),
                vpc_local(uuid::Uuid::new_v4(), dpu(3), vpc_b),
            ],
            ..Default::default()
        },
        &InstanceExtensionServicesConfig {
            service_configs: vec![attachment(c3, false), attachment(a3, true)],
        },
        false,
    )
    .await;
    // DPU 4's instance was released: its record survives in the soft-deleted
    // row until the host is wiped, but it serves nothing.
    let a4 = uuid::Uuid::new_v4();
    seed_host(
        &pool,
        4,
        &[4],
        &InstanceNetworkConfig {
            vpc_local_interfaces: vec![vpc_local(a4, dpu(4), vpc_a)],
            ..Default::default()
        },
        &InstanceExtensionServicesConfig {
            service_configs: vec![attachment(a4, false)],
        },
        true,
    )
    .await;
    // Host 5 has two DPUs; each serves only the VPC whose record names it.
    let (a5, b6) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
    seed_host(
        &pool,
        5,
        &[5, 6],
        &InstanceNetworkConfig {
            vpc_local_interfaces: vec![vpc_local(a5, dpu(5), vpc_a), vpc_local(b6, dpu(6), vpc_b)],
            ..Default::default()
        },
        &InstanceExtensionServicesConfig {
            service_configs: vec![attachment(a5, false), attachment(b6, false)],
        },
        false,
    )
    .await;

    check_cases_async(
        [
            Case {
                scenario: "two VPCs on one DPU, service interface ignored",
                input: dpu(1),
                expect: Yields(vec![vpc_a, vpc_c]),
            },
            Case {
                scenario: "a VPC served only by another DPU",
                input: dpu(2),
                expect: Yields(vec![vpc_b]),
            },
            Case {
                scenario: "a terminating or missing attachment grants nothing",
                input: dpu(3),
                expect: Yields(vec![vpc_c]),
            },
            Case {
                scenario: "a released instance grants nothing",
                input: dpu(4),
                expect: Yields(vec![]),
            },
            Case {
                scenario: "the first DPU of a two-DPU host",
                input: dpu(5),
                expect: Yields(vec![vpc_a]),
            },
            Case {
                scenario: "the second DPU of a two-DPU host",
                input: dpu(6),
                expect: Yields(vec![vpc_b]),
            },
            Case {
                scenario: "a DPU without a host",
                input: dpu(9),
                expect: Yields(vec![]),
            },
        ],
        |dpu| served_by(&pool, dpu),
    )
    .await;
}

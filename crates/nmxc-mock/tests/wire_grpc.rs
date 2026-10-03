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

//! Wire-level tests.
//!
//! These drive the mock through NICo's own `libnmxc` client over a real
//! socket, rather than calling the trait methods directly, because the thing
//! most likely to break is the transport: codec, HTTP/2 framing, the router
//! path the service is mounted on, and the authority the domain is chosen by.
//! Using the production client also proves the client's own response checks
//! accept what the mock sends.

use std::sync::Arc;

use libnmxc::nmxc_model::nmx_controller_client::NmxControllerClient;
use libnmxc::{Endpoint, NMX_C_GATEWAY_ID, Nmxc, NmxcClientPool, nmxc_model as nmx};
use nmxc_mock::{
    NmxcMock, NmxcMockConfig, SimComputeNode, SimDomain, SimGpu, SimSwitch, StaticInventory,
};
use uuid::Uuid;

const RACK_A_UUID: Uuid = Uuid::from_u128(0xa000_0000_0000_0000_0000_0000_0000_000a);
const RACK_B_UUID: Uuid = Uuid::from_u128(0xb000_0000_0000_0000_0000_0000_0000_000b);

/// A rack with one switch at NVOS address `nvos_ip` and two compute trays of
/// two GPUs each, whose uids start at `first_uid`.
fn rack(key: &str, domain_uuid: Uuid, nvos_ip: &str, first_uid: u64) -> SimDomain {
    SimDomain {
        key: key.into(),
        domain_uuid,
        nvos_ips: vec![nvos_ip.parse().unwrap()],
        switches: vec![SimSwitch {
            chassis_serial: format!("{key}-switch"),
            slot_number: 19,
            tray_index: 0,
            num_switches: 2,
        }],
        compute_nodes: (0..2)
            .map(|tray| SimComputeNode {
                chassis_serial: format!("{key}-tray-{tray}"),
                slot_number: 11 + tray,
                tray_index: tray,
                host_id: 1,
                gpus: (0..2)
                    .map(|gpu| SimGpu {
                        uid: first_uid + u64::from(tray * 2 + gpu),
                        module_id: gpu + 1,
                    })
                    .collect(),
            })
            .collect(),
    }
}

fn rack_a() -> SimDomain {
    rack("rack-a", RACK_A_UUID, "10.0.0.1", 0xa0)
}

fn rack_b() -> SimDomain {
    rack("rack-b", RACK_B_UUID, "10.0.0.2", 0xb0)
}

/// Serve the mock on an ephemeral port and return its base URL.
async fn serve(domains: Vec<SimDomain>) -> String {
    let mock = Arc::new(NmxcMock::new(
        Arc::new(StaticInventory::new(domains.into())),
        NmxcMockConfig::default(),
    ));
    let router = nmxc_mock::router(mock);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });

    format!("http://{addr}")
}

/// NICo's client, exactly as `nvlink-manager` builds it without TLS.
async fn nico_client(url: &str) -> Box<dyn Nmxc> {
    NmxcClientPool::builder()
        .build()
        .unwrap()
        .create_client(Endpoint::new(url).unwrap())
        .await
        .unwrap()
}

/// A raw client whose requests carry `authority` instead of the socket
/// address, as they do once a per-switch Service fronts the listener.
async fn client_addressing(
    url: &str,
    authority: &str,
) -> NmxControllerClient<tonic::transport::Channel> {
    let channel = tonic::transport::Endpoint::from_shared(url.to_string())
        .unwrap()
        .origin(format!("http://{authority}").parse().unwrap())
        .connect()
        .await
        .unwrap();
    NmxControllerClient::new(channel)
}

fn list_all() -> nmx::GetPartitionInfoListRequest {
    nmx::GetPartitionInfoListRequest {
        context: None,
        partition_id_list: Vec::new(),
        partition_name_list: Vec::new(),
        gateway_id: NMX_C_GATEWAY_ID.into(),
    }
}

fn partition_id(id: u32) -> Option<nmx::PartitionId> {
    Some(nmx::PartitionId { partition_id: id })
}

fn create(name: &str, uids: &[u64]) -> nmx::CreatePartitionRequest {
    nmx::CreatePartitionRequest {
        context: None,
        name: name.into(),
        gpu_resource_id: uids
            .iter()
            .map(|&uid| nmx::GpuResourceId {
                resource_id: Some(nmx::gpu_resource_id::ResourceId::GpuUid(uid)),
            })
            .collect(),
        attr: Some(nmx::PartitionAttr {
            resiliency_mode: 0,
            multicast_groups_limit: 16,
        }),
        partition_id: None,
        gateway_id: NMX_C_GATEWAY_ID.into(),
    }
}

fn delete(id: u32) -> nmx::DeletePartitionRequest {
    nmx::DeletePartitionRequest {
        context: None,
        partition_id: partition_id(id),
        gateway_id: NMX_C_GATEWAY_ID.into(),
        name: String::new(),
    }
}

fn update(id: u32, uids: &[u64]) -> nmx::UpdatePartitionRequest {
    nmx::UpdatePartitionRequest {
        context: None,
        partition_id: partition_id(id),
        location_list: Vec::new(),
        gpu_uid: uids.to_vec(),
        gateway_id: NMX_C_GATEWAY_ID.into(),
        name: String::new(),
        reroute: true,
    }
}

#[tokio::test]
async fn hello_reports_the_domain_uuid_to_an_unmodified_client() {
    let url = serve(vec![rack_a()]).await;
    let mut client = nico_client(&url).await;

    let hello = client
        .hello(NMX_C_GATEWAY_ID)
        .await
        .expect("Hello is the first call NICo makes and must succeed");

    let header = hello.server_header.expect("server_header");
    assert_eq!(header.domain_uuid, RACK_A_UUID.to_string());
    assert!(
        hello
            .components_ver
            .iter()
            .any(|kv| kv.value.starts_with("machine-a-tron-nmxc-mock/")),
        "components_ver should identify the mock: {:?}",
        hello.components_ver
    );
}

/// The lifecycle NICo's partition monitor drives: find the factory partition,
/// delete it, provision partitions by GPU uid, adjust membership, tear down.
#[tokio::test]
async fn partition_lifecycle_through_nicos_client() {
    let url = serve(vec![rack_a()]).await;
    let mut client = nico_client(&url).await;

    let partitions = client.get_partition_info_list(list_all()).await.unwrap();
    let [factory] = partitions.partition_info_list.as_slice() else {
        panic!("a fresh domain has exactly the factory partition");
    };
    assert_eq!(factory.partition_id, partition_id(32766));
    assert_eq!(factory.name, "Default");
    assert_eq!(factory.gpu_uid_list, [0xa0, 0xa1, 0xa2, 0xa3]);

    let held = client.create_partition(create("early", &[0xa0])).await;
    assert_eq!(
        held.unwrap_err().nmx_return_code(),
        Some(nmx::StReturnCode::NmxStResourceInUse as i32),
        "GPUs stay in the factory partition until it is deleted"
    );

    client.delete_partition(delete(32766)).await.unwrap();
    client
        .delete_partition(delete(32766))
        .await
        .expect("deleting an id that is gone is not an error");

    let created = client
        .create_partition(create("tenant-1", &[0xa0, 0xa1]))
        .await
        .unwrap();
    let id = created.partition_id.expect("new partition id").partition_id;
    assert_eq!(id, 1, "ids are allocated from 1");

    client
        .add_gpus_to_partition(update(id, &[0xa2]))
        .await
        .unwrap();
    client
        .remove_gpus_from_partition(update(id, &[0xa0]))
        .await
        .unwrap();

    let by_id = client
        .get_partition_info_list(nmx::GetPartitionInfoListRequest {
            partition_id_list: vec![nmx::PartitionId { partition_id: id }],
            ..list_all()
        })
        .await
        .unwrap();
    assert_eq!(by_id.partition_info_list.len(), 1);
    assert_eq!(by_id.partition_info_list[0].name, "tenant-1");
    assert_eq!(by_id.partition_info_list[0].gpu_uid_list, [0xa1, 0xa2]);

    let gpus = client
        .get_gpu_info_list(nmx::GetGpuInfoListRequest {
            attr: nmx::GpuAttr::NmxGpuAttrAll as i32,
            gateway_id: NMX_C_GATEWAY_ID.into(),
            ..Default::default()
        })
        .await
        .unwrap();
    let placements: Vec<(u64, Option<u32>)> = gpus
        .gpu_info_list
        .iter()
        .map(|gpu| (gpu.gpu_uid, gpu.partition_id.map(|p| p.partition_id)))
        .collect();
    assert_eq!(
        placements,
        [(0xa0, None), (0xa1, Some(1)), (0xa2, Some(1)), (0xa3, None)]
    );

    client.delete_partition(delete(id)).await.unwrap();
    let remaining = client.get_partition_info_list(list_all()).await.unwrap();
    assert!(remaining.partition_info_list.is_empty());
}

/// Two racks behind one listener are told apart by the address the client
/// used, and each keeps its own partitions.
#[tokio::test]
async fn domains_are_selected_by_authority() {
    let url = serve(vec![rack_a(), rack_b()]).await;
    let mut a = client_addressing(&url, "10.0.0.1:9370").await;
    let mut b = client_addressing(&url, "10.0.0.2:9370").await;

    let hello_a = a
        .hello(nmx::ClientHello::default())
        .await
        .unwrap()
        .into_inner();
    let hello_b = b
        .hello(nmx::ClientHello::default())
        .await
        .unwrap()
        .into_inner();
    assert_eq!(
        hello_a.server_header.unwrap().domain_uuid,
        RACK_A_UUID.to_string()
    );
    assert_eq!(
        hello_b.server_header.unwrap().domain_uuid,
        RACK_B_UUID.to_string()
    );

    a.delete_partition(delete(32766)).await.unwrap();
    let in_b = b
        .get_partition_info_list(list_all())
        .await
        .unwrap()
        .into_inner();
    assert_eq!(
        in_b.partition_info_list[0].partition_id,
        partition_id(32766),
        "deleting rack A's factory partition leaves rack B's alone"
    );

    let mut stranger = client_addressing(&url, "10.9.9.9:9370").await;
    let status = stranger
        .hello(nmx::ClientHello::default())
        .await
        .expect_err("no domain has that NVOS address");
    assert_eq!(status.code(), tonic::Code::NotFound);

    // The bare socket address selects nothing once more than one rack exists.
    let mut unaddressed = NmxControllerClient::connect(url).await.unwrap();
    let status = unaddressed
        .hello(nmx::ClientHello::default())
        .await
        .expect_err("two racks cannot share an unnamed authority");
    assert_eq!(status.code(), tonic::Code::NotFound);
}

#[tokio::test]
async fn out_of_scope_methods_report_unimplemented() {
    // A missing route would surface as a router 404, which tonic reports as
    // `Unimplemented` too -- so assert on the message, which only the mock's
    // own handler produces.
    let url = serve(vec![rack_a()]).await;
    let mut client = NmxControllerClient::connect(url).await.unwrap();

    let status = client
        .subscribe(nmx::SubscribeRequest::default())
        .await
        .expect_err("subscribe is out of scope");

    assert_eq!(status.code(), tonic::Code::Unimplemented);
    assert!(
        status.message().contains("machine-a-tron NMX-C mock"),
        "expected the mock's own handler to answer, not a router 404; got: {}",
        status.message()
    );
}

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

//! Shared `find_*_by_ids` request guards, proven once across representative RPCs.
//!
//! Every `find_*_by_ids` RPC validates its ID list with the same two API-layer
//! guards before touching configuration or the database: it rejects an empty list
//! with `"at least one ID must be provided"`, and a list longer than
//! `max_find_by_ids` with `"no more than {max} IDs can be accepted"`. The messages
//! are byte-identical across handlers (see `handlers/*.rs`), so each guard is
//! exercised once here over a representative spread of RPCs rather than re-proven
//! per entity in every `*_find.rs`. Each row builds and calls one RPC; the row is
//! labeled by RPC name so a failure says which RPC drifted from the shared guard.

use std::future::Future;
use std::pin::Pin;

use carbide_test_harness::prelude::*;
use carbide_uuid::infiniband::IBPartitionId;
use carbide_uuid::instance::InstanceId;
use carbide_uuid::machine::MachineId;
use carbide_uuid::network::NetworkSegmentId;
use carbide_uuid::power_shelf::PowerShelfId;
use carbide_uuid::switch::SwitchId;
use carbide_uuid::vpc::VpcId;
use data_encoding::BASE32_DNSSEC;
use rpc::forge::forge_server::Forge;
use sha2::{Digest, Sha256};
use tonic::{Code, Request, Status};

/// The shared empty-list guard message, returned ahead of any DB work.
const EMPTY_MESSAGE: &str = "at least one ID must be provided";

/// One representative RPC, plus the `find_*_by_ids` call to exercise its guard.
///
/// `call` builds and dispatches a single RPC against the shared `env`, discarding
/// the success payload — the guard cases never reach a successful response, so the
/// only thing that matters is the error each RPC returns.
struct RpcGuardCase {
    /// The RPC under test; used as the failure label so a regression names the RPC.
    rpc: &'static str,
    /// The `find_*_by_ids` call, already bound to its request.
    call: Pin<Box<dyn Future<Output = Result<(), Status>>>>,
}

/// Builds machine IDs the way the machine handler expects (TPM-derived, base32),
/// since the empty case below shares this file's representative spread.
fn machine_id(index: u32) -> MachineId {
    let serial = format!("machine_{index}");
    let hash: [u8; 32] = Sha256::new_with_prefix(serial.as_bytes()).finalize().into();
    let encoded = BASE32_DNSSEC.encode(&hash);
    format!(
        "{}s{}",
        carbide_uuid::machine::MachineType::Dpu.id_prefix(),
        encoded
    )
    .parse()
    .unwrap()
}

/// The representative RPCs exercised for the over-max guard, each handed a list of
/// `max_find_by_ids + 1` IDs. The IDs need not be real: the guard fires on length
/// before any lookup.
fn over_max_cases(env: &TestHarness) -> Vec<RpcGuardCase> {
    use rpc::forge as rpc;
    let over = env.api().runtime_config.max_find_by_ids + 1;
    let api = env.api_arc();

    vec![
        RpcGuardCase {
            rpc: "find_machines_by_ids",
            call: Box::pin({
                let api = api.clone();
                async move {
                    api.find_machines_by_ids(Request::new(rpc::MachinesByIdsRequest {
                        machine_ids: (1..=over).map(machine_id).collect(),
                        ..Default::default()
                    }))
                    .await
                    .map(|_| ())
                }
            }),
        },
        RpcGuardCase {
            rpc: "find_instances_by_ids",
            call: Box::pin({
                let api = api.clone();
                async move {
                    let instance_ids: Vec<InstanceId> =
                        (1..=over).map(|_| uuid::Uuid::new_v4().into()).collect();
                    api.find_instances_by_ids(Request::new(rpc::InstancesByIdsRequest {
                        instance_ids,
                    }))
                    .await
                    .map(|_| ())
                }
            }),
        },
        RpcGuardCase {
            rpc: "find_switches_by_ids",
            call: Box::pin({
                let api = api.clone();
                async move {
                    let switch_ids: Vec<SwitchId> = (0..over)
                        .map(|_| SwitchId::from(uuid::Uuid::new_v4()))
                        .collect();
                    api.find_switches_by_ids(Request::new(rpc::SwitchesByIdsRequest { switch_ids }))
                        .await
                        .map(|_| ())
                }
            }),
        },
        RpcGuardCase {
            rpc: "find_power_shelves_by_ids",
            call: Box::pin({
                let api = api.clone();
                async move {
                    let power_shelf_ids: Vec<PowerShelfId> = (0..over)
                        .map(|_| PowerShelfId::from(uuid::Uuid::new_v4()))
                        .collect();
                    api.find_power_shelves_by_ids(Request::new(rpc::PowerShelvesByIdsRequest {
                        power_shelf_ids,
                    }))
                    .await
                    .map(|_| ())
                }
            }),
        },
        RpcGuardCase {
            rpc: "find_vpcs_by_ids",
            call: Box::pin({
                let api = api.clone();
                async move {
                    api.find_vpcs_by_ids(Request::new(rpc::VpcsByIdsRequest {
                        vpc_ids: (0..over)
                            .map(|_| VpcId::from(uuid::Uuid::new_v4()))
                            .collect(),
                    }))
                    .await
                    .map(|_| ())
                }
            }),
        },
        RpcGuardCase {
            rpc: "find_network_segments_by_ids",
            call: Box::pin({
                let api = api.clone();
                async move {
                    let network_segments_ids: Vec<NetworkSegmentId> =
                        (1..=over).map(|_| uuid::Uuid::new_v4().into()).collect();
                    api.find_network_segments_by_ids(Request::new(
                        rpc::NetworkSegmentsByIdsRequest {
                            network_segments_ids,
                            include_history: false,
                            include_num_free_ips: false,
                        },
                    ))
                    .await
                    .map(|_| ())
                }
            }),
        },
        RpcGuardCase {
            rpc: "find_ib_partitions_by_ids",
            call: Box::pin({
                let api = api.clone();
                async move {
                    let ib_partition_ids: Vec<IBPartitionId> =
                        (1..=over).map(|_| uuid::Uuid::new_v4().into()).collect();
                    api.find_ib_partitions_by_ids(Request::new(rpc::IbPartitionsByIdsRequest {
                        ib_partition_ids,
                        include_history: false,
                    }))
                    .await
                    .map(|_| ())
                }
            }),
        },
        RpcGuardCase {
            rpc: "find_tenant_keysets_by_ids",
            call: Box::pin({
                let api = api.clone();
                async move {
                    api.find_tenant_keysets_by_ids(Request::new(rpc::TenantKeysetsByIdsRequest {
                        keyset_ids: (1..=over)
                            .map(|i| rpc::TenantKeysetIdentifier {
                                organization_id: "tenant_org_1".to_string(),
                                keyset_id: format!("keyset_id_{i}"),
                            })
                            .collect(),
                        include_key_data: false,
                    }))
                    .await
                    .map(|_| ())
                }
            }),
        },
        RpcGuardCase {
            rpc: "find_explored_managed_hosts_by_ids",
            call: Box::pin({
                let api = api.clone();
                async move {
                    api.find_explored_managed_hosts_by_ids(Request::new(
                        ::rpc::site_explorer::ExploredManagedHostsByIdsRequest {
                            host_ids: (1..=over).map(|i| format!("141.219.24.{i}")).collect(),
                        },
                    ))
                    .await
                    .map(|_| ())
                }
            }),
        },
        RpcGuardCase {
            rpc: "find_explored_endpoints_by_ids",
            call: Box::pin(async move {
                api.find_explored_endpoints_by_ids(Request::new(
                    ::rpc::site_explorer::ExploredEndpointsByIdsRequest {
                        endpoint_ids: (1..=over).map(|i| format!("141.219.24.{i}")).collect(),
                    },
                ))
                .await
                .map(|_| ())
            }),
        },
    ]
}

/// The same representative RPCs exercised for the empty-list guard, each handed an
/// empty ID list (a default request).
fn empty_cases(env: &TestHarness) -> Vec<RpcGuardCase> {
    use rpc::forge as rpc;
    let api = env.api_arc();

    vec![
        RpcGuardCase {
            rpc: "find_machines_by_ids",
            call: Box::pin({
                let api = api.clone();
                async move {
                    api.find_machines_by_ids(Request::new(rpc::MachinesByIdsRequest::default()))
                        .await
                        .map(|_| ())
                }
            }),
        },
        RpcGuardCase {
            rpc: "find_instances_by_ids",
            call: Box::pin({
                let api = api.clone();
                async move {
                    api.find_instances_by_ids(Request::new(rpc::InstancesByIdsRequest::default()))
                        .await
                        .map(|_| ())
                }
            }),
        },
        RpcGuardCase {
            rpc: "find_switches_by_ids",
            call: Box::pin({
                let api = api.clone();
                async move {
                    api.find_switches_by_ids(Request::new(rpc::SwitchesByIdsRequest {
                        switch_ids: vec![],
                    }))
                    .await
                    .map(|_| ())
                }
            }),
        },
        RpcGuardCase {
            rpc: "find_power_shelves_by_ids",
            call: Box::pin({
                let api = api.clone();
                async move {
                    api.find_power_shelves_by_ids(Request::new(rpc::PowerShelvesByIdsRequest {
                        power_shelf_ids: vec![],
                    }))
                    .await
                    .map(|_| ())
                }
            }),
        },
        RpcGuardCase {
            rpc: "find_vpcs_by_ids",
            call: Box::pin({
                let api = api.clone();
                async move {
                    api.find_vpcs_by_ids(Request::new(rpc::VpcsByIdsRequest::default()))
                        .await
                        .map(|_| ())
                }
            }),
        },
        RpcGuardCase {
            rpc: "find_network_segments_by_ids",
            call: Box::pin({
                let api = api.clone();
                async move {
                    api.find_network_segments_by_ids(Request::new(
                        rpc::NetworkSegmentsByIdsRequest::default(),
                    ))
                    .await
                    .map(|_| ())
                }
            }),
        },
        RpcGuardCase {
            rpc: "find_ib_partitions_by_ids",
            call: Box::pin({
                let api = api.clone();
                async move {
                    api.find_ib_partitions_by_ids(Request::new(rpc::IbPartitionsByIdsRequest {
                        ib_partition_ids: Vec::new(),
                        include_history: false,
                    }))
                    .await
                    .map(|_| ())
                }
            }),
        },
        RpcGuardCase {
            rpc: "find_tenant_keysets_by_ids",
            call: Box::pin({
                let api = api.clone();
                async move {
                    api.find_tenant_keysets_by_ids(Request::new(
                        rpc::TenantKeysetsByIdsRequest::default(),
                    ))
                    .await
                    .map(|_| ())
                }
            }),
        },
        RpcGuardCase {
            rpc: "find_explored_managed_hosts_by_ids",
            call: Box::pin({
                let api = api.clone();
                async move {
                    api.find_explored_managed_hosts_by_ids(Request::new(
                        ::rpc::site_explorer::ExploredManagedHostsByIdsRequest::default(),
                    ))
                    .await
                    .map(|_| ())
                }
            }),
        },
        RpcGuardCase {
            rpc: "find_explored_endpoints_by_ids",
            call: Box::pin(async move {
                api.find_explored_endpoints_by_ids(Request::new(
                    ::rpc::site_explorer::ExploredEndpointsByIdsRequest::default(),
                ))
                .await
                .map(|_| ())
            }),
        },
    ]
}

#[sqlx_test]
async fn find_by_ids_rejects_empty_id_list(pool: PgPool) {
    let env = TestHarness::builder(pool).build().await;

    for case in empty_cases(&env) {
        let err = case.call.await.expect_err(&format!(
            "{}: expected an error for an empty ID list",
            case.rpc
        ));
        assert_eq!(err.code(), Code::InvalidArgument, "{}", case.rpc);
        assert_eq!(err.message(), EMPTY_MESSAGE, "{}", case.rpc);
    }
}

#[sqlx_test]
async fn find_by_ids_rejects_too_many_ids(pool: PgPool) {
    let env = TestHarness::builder(pool).build().await;
    let expected = format!(
        "no more than {} IDs can be accepted",
        env.api().runtime_config.max_find_by_ids
    );

    for case in over_max_cases(&env) {
        let err = case.call.await.expect_err(&format!(
            "{}: expected an error when passing more than max IDs",
            case.rpc
        ));
        assert_eq!(err.code(), Code::InvalidArgument, "{}", case.rpc);
        assert_eq!(err.message(), expected, "{}", case.rpc);
    }
}

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

//! `NMX_Controller` service implementation.
//!
//! The generated trait has a required method per RPC and tonic emits no
//! default bodies, so every method must exist here even when it is out of
//! scope. The out-of-scope ones are generated rather than written out, so
//! that a proto change which adds an RPC fails to compile in one obvious
//! place instead of silently returning a router 404.
//!
//! NMX-C reports operation failures in `server_header.return_code`, not as a
//! gRPC status, and NICo's client reads them from there: a rejected partition
//! operation is an `Ok` response carrying the controller's return code. A
//! gRPC status is reserved for a request the mock cannot attribute to any
//! domain at all.

use libnmxc::nmxc_model::nmx_controller_server::NmxController;
use tonic::{Request, Response, Status};

use crate::inventory::{SimComputeNode, SimSwitch};
use crate::state::{DomainState, PartitionError, PartitionRef};
use crate::{NmxcMock, SimDomain, nmx};

type RpcResult<T> = std::result::Result<Response<T>, Status>;

/// Builds the whole `NmxController` impl.
///
/// The macro emits the `#[tonic::async_trait]` attribute itself rather than
/// being invoked underneath one. Attribute macros expand before the
/// function-like macros in their body, so an invocation placed inside an
/// already-annotated impl block would emit plain `async fn` methods that
/// `async_trait` never desugars, and every one of them would fail to match
/// the trait signature.
macro_rules! nmx_controller_impl {
    (
        implemented { $($implemented:tt)* }
        unimplemented { $($method:ident($req:ident) -> $res:ident,)* }
    ) => {
        #[tonic::async_trait]
        impl NmxController for NmxcMock {
            $($implemented)*

            $(
                /// Out of scope for the mock; `UNIMPLEMENTED` is what a real
                /// controller returns for an RPC it does not serve.
                async fn $method(&self, _request: Request<nmx::$req>) -> RpcResult<nmx::$res> {
                    Err(Status::unimplemented(concat!(
                        "the machine-a-tron NMX-C mock does not implement ",
                        stringify!($method),
                    )))
                }
            )*
        }
    };
}

nmx_controller_impl! {
    implemented {
        /// NICo reads only `server_header.domain_uuid` from this, and labels
        /// metrics with `components_ver`.
        async fn hello(&self, request: Request<nmx::ClientHello>) -> RpcResult<nmx::ServerHello> {
            let hello = self.with_domain(&request, |domain, _| nmx::ServerHello {
                server_header: Some(self.header(domain, Ok(()))),
                components_ver: vec![nmx::KeyValPair {
                    key: "nmx-controller".into(),
                    value: self.config.version_string.clone(),
                }],
                capabilities: Vec::new(),
                host_os_details: "machine-a-tron".into(),
                major_version: nmx::ProtoMsgMajorVersion::ProtoMsgMajorVersion as i32,
                minor_version: nmx::ProtoMsgMinorVersion::ProtoMsgMinorVersion as i32,
            })?;
            Ok(Response::new(hello))
        }

        async fn get_domain_properties(
            &self,
            request: Request<nmx::GetDomainPropertiesRequest>,
        ) -> RpcResult<nmx::DomainProperties> {
            let properties = self.with_domain(&request, |domain, _| nmx::DomainProperties {
                server_header: Some(self.header(domain, Ok(()))),
                max_compute_nodes: domain.compute_nodes.len() as u32,
                max_gpus_per_compute_node: domain
                    .compute_nodes
                    .iter()
                    .map(|node| node.gpus.len() as u32)
                    .max()
                    .unwrap_or_default(),
                max_switch_nodes: domain.switches.len() as u32,
                max_switches_per_switch_node: domain
                    .switches
                    .iter()
                    .map(|switch| switch.num_switches)
                    .max()
                    .unwrap_or_default(),
                ..Default::default()
            })?;
            Ok(Response::new(properties))
        }

        async fn get_compute_node_count(
            &self,
            request: Request<nmx::GetComputeNodeCountRequest>,
        ) -> RpcResult<nmx::GetComputeNodeCountResponse> {
            let response = self.with_domain(&request, |domain, _| nmx::GetComputeNodeCountResponse {
                server_header: Some(self.header(domain, Ok(()))),
                context: None,
                num_nodes: domain.compute_nodes.len() as u32,
            })?;
            Ok(Response::new(response))
        }

        async fn get_compute_node_info_list(
            &self,
            request: Request<nmx::GetComputeNodeInfoListRequest>,
        ) -> RpcResult<nmx::GetComputeNodeInfoListResponse> {
            let response = self.with_domain(&request, |domain, state| {
                nmx::GetComputeNodeInfoListResponse {
                    server_header: Some(self.header(domain, Ok(()))),
                    context: None,
                    node_info_list: domain
                        .compute_nodes
                        .iter()
                        .map(|node| compute_node_info(node, state))
                        .collect(),
                }
            })?;
            Ok(Response::new(response))
        }

        /// Every GPU in the domain, or those of one partition when the
        /// request names it.
        async fn get_gpu_info_list(
            &self,
            request: Request<nmx::GetGpuInfoListRequest>,
        ) -> RpcResult<nmx::GetGpuInfoListResponse> {
            let only_partition = request
                .get_ref()
                .partition_id
                .as_ref()
                .map(|partition| partition.partition_id);
            let response = self.with_domain(&request, |domain, state| nmx::GetGpuInfoListResponse {
                server_header: Some(self.header(domain, Ok(()))),
                context: None,
                gpu_info_list: domain
                    .compute_nodes
                    .iter()
                    .flat_map(|node| {
                        let state = &*state;
                        node.gpus.iter().map(move |gpu| {
                            let partition_id = state.partition_of(gpu.uid);
                            (node, gpu, partition_id)
                        })
                    })
                    .filter(|(_, _, partition_id)| {
                        only_partition.is_none_or(|only| *partition_id == Some(only))
                    })
                    .map(|(node, gpu, partition_id)| nmx::GpuInfo {
                        loc: Some(location_info(
                            &node.chassis_serial,
                            node.slot_number,
                            node.tray_index,
                            node.host_id,
                        )),
                        gpu_id: gpu.module_id,
                        gpu_uid: gpu.uid,
                        gpu_health: nmx::GpuHealth::NmxGpuHealthHealthy as i32,
                        partition_id: partition_id.map(partition_id_message),
                    })
                    .collect(),
            })?;
            Ok(Response::new(response))
        }

        async fn get_switch_node_count(
            &self,
            request: Request<nmx::GetSwitchNodeCountRequest>,
        ) -> RpcResult<nmx::GetSwitchNodeCountResponse> {
            let response = self.with_domain(&request, |domain, _| nmx::GetSwitchNodeCountResponse {
                server_header: Some(self.header(domain, Ok(()))),
                context: None,
                num_nodes: domain.switches.len() as u32,
            })?;
            Ok(Response::new(response))
        }

        async fn get_switch_node_info_list(
            &self,
            request: Request<nmx::GetSwitchNodeInfoListRequest>,
        ) -> RpcResult<nmx::GetSwitchNodeInfoListResponse> {
            let response = self.with_domain(&request, |domain, state| {
                // Every switch tray carries every partition: an NVLink
                // partition spans the whole fabric.
                let partition_id_list: Vec<_> = state
                    .partitions()
                    .map(|partition| partition_id_message(partition.id))
                    .collect();
                nmx::GetSwitchNodeInfoListResponse {
                    server_header: Some(self.header(domain, Ok(()))),
                    context: None,
                    node_info_list: domain
                        .switches
                        .iter()
                        .map(|switch| switch_node_info(switch, partition_id_list.clone()))
                        .collect(),
                }
            })?;
            Ok(Response::new(response))
        }

        async fn get_partition_count(
            &self,
            request: Request<nmx::GetPartitionCountRequest>,
        ) -> RpcResult<nmx::GetPartitionCountResponse> {
            let response = self.with_domain(&request, |domain, state| nmx::GetPartitionCountResponse {
                server_header: Some(self.header(domain, Ok(()))),
                context: None,
                num_partitions: state.partitions().count() as u32,
            })?;
            Ok(Response::new(response))
        }

        async fn get_partition_id_list(
            &self,
            request: Request<nmx::GetPartitionIdListRequest>,
        ) -> RpcResult<nmx::GetPartitionIdListResponse> {
            let response = self.with_domain(&request, |domain, state| nmx::GetPartitionIdListResponse {
                server_header: Some(self.header(domain, Ok(()))),
                context: None,
                partition_list: state
                    .partitions()
                    .map(|partition| nmx::Partition {
                        partition_id: Some(partition_id_message(partition.id)),
                        num_gpus: partition.gpu_uids.len() as u32,
                    })
                    .collect(),
            })?;
            Ok(Response::new(response))
        }

        /// The partitions NICo reconciles against. `gpu_uid_list` is what it
        /// joins to machines, by the fabric GUID each machine reported.
        async fn get_partition_info_list(
            &self,
            request: Request<nmx::GetPartitionInfoListRequest>,
        ) -> RpcResult<nmx::GetPartitionInfoListResponse> {
            let ids: Vec<u32> = request
                .get_ref()
                .partition_id_list
                .iter()
                .map(|partition| partition.partition_id)
                .collect();
            let names = &request.get_ref().partition_name_list;
            let response = self.with_domain(&request, |domain, state| {
                nmx::GetPartitionInfoListResponse {
                    server_header: Some(self.header(domain, Ok(()))),
                    context: None,
                    partition_info_list: state
                        .find(&ids, names)
                        .into_iter()
                        .map(|partition| nmx::PartitionInfo {
                            partition_id: Some(partition_id_message(partition.id)),
                            name: partition.name.clone(),
                            num_gpus: partition.gpu_uids.len() as u32,
                            gpu_location_list: Vec::new(),
                            gpu_uid_list: partition.gpu_uids.clone(),
                            health: nmx::PartitionHealth::NmxPartitionHealthHealthy as i32,
                            partition_type: nmx::PartitionType::NmxPartitionTypeGpuuidBased as i32,
                            num_allocated_multicast_groups: 0,
                            attr: None,
                        })
                        .collect(),
                }
            })?;
            Ok(Response::new(response))
        }

        /// NICo does not read the returned id; it re-lists partitions and
        /// finds the new one by name.
        async fn create_partition(
            &self,
            request: Request<nmx::CreatePartitionRequest>,
        ) -> RpcResult<nmx::CreatePartitionResponse> {
            let create = request.get_ref();
            let requested_id = create.partition_id.as_ref().map(|id| id.partition_id);
            let response = self.with_domain(&request, |domain, state| {
                let result = gpu_uids(&create.gpu_resource_id)
                    .and_then(|uids| state.create(&create.name, &uids, requested_id));
                nmx::CreatePartitionResponse {
                    server_header: Some(self.header(domain, result.as_ref().map(drop))),
                    context: None,
                    partition_id: result.ok().map(partition_id_message),
                }
            })?;
            Ok(Response::new(response))
        }

        async fn delete_partition(
            &self,
            request: Request<nmx::DeletePartitionRequest>,
        ) -> RpcResult<nmx::DeletePartitionResponse> {
            let delete = request.get_ref();
            let target = PartitionRef::new(
                delete.partition_id.as_ref().map(|id| id.partition_id),
                &delete.name,
            );
            let response = self.with_domain(&request, |domain, state| {
                let result = target.and_then(|target| state.delete(target));
                nmx::DeletePartitionResponse {
                    server_header: Some(self.header(domain, result.as_ref().map(drop))),
                    context: None,
                    partition_id: result.ok().map(partition_id_message),
                }
            })?;
            Ok(Response::new(response))
        }

        async fn add_gpus_to_partition(
            &self,
            request: Request<nmx::UpdatePartitionRequest>,
        ) -> RpcResult<nmx::UpdatePartitionResponse> {
            self.update_partition(request, DomainState::add_gpus)
        }

        async fn remove_gpus_from_partition(
            &self,
            request: Request<nmx::UpdatePartitionRequest>,
        ) -> RpcResult<nmx::UpdatePartitionResponse> {
            self.update_partition(request, DomainState::remove_gpus)
        }

        type SubscribeStream =
            tonic::codegen::tokio_stream::Empty<std::result::Result<nmx::ServerNotification, Status>>;

        /// Out of scope for the mock; the health collectors that subscribe
        /// treat `UNIMPLEMENTED` as an endpoint without telemetry.
        async fn subscribe(
            &self,
            _request: Request<nmx::SubscribeRequest>,
        ) -> RpcResult<Self::SubscribeStream> {
            Err(Status::unimplemented(
                "the machine-a-tron NMX-C mock does not implement subscribe",
            ))
        }
    }

    unimplemented {
        factory_reset(FactoryResetRequest) -> ReturnCode,
        get_static_config(GetStaticConfigRequest) -> StaticConfigResponse,
        set_static_config(SetStaticConfigRequest) -> ReturnCode,
        get_admin_state(GetAdminStateRequest) -> GetAdminStateResponse,
        set_admin_state(SetAdminStateRequest) -> SetAdminStateResponse,
        get_domain_state_info(GetDomainStateInfoRequest) -> DomainStateInfo,
        get_topology_info(GetTopologyInfoRequest) -> FmTopologyInfo,
        get_compute_node_location_list(GetComputeNodeLocationListRequest) -> GetComputeNodeLocationListResponse,
        get_switch_node_location_list(GetSwitchNodeLocationListRequest) -> GetSwitchNodeLocationListResponse,
        get_switch_info_list(GetSwitchInfoListRequest) -> GetSwitchInfoListResponse,
        get_conn_count(GetConnCountRequest) -> GetConnCountResponse,
        get_conn_info_list(GetConnInfoListRequest) -> GetConnInfoListResponse,
        get_conn_info_combined(GetConnInfoCombinedRequest) -> ConnInfoCombined,
        get_state_report(GetStateReportRequest) -> GetStateReportResponse,
    }
}

impl NmxcMock {
    /// The header every response carries. A failed partition operation is
    /// reported here, with the controller's return code, and logged once.
    fn header(&self, domain: &SimDomain, result: Result<(), &PartitionError>) -> nmx::ServerHeader {
        let return_code = match result {
            Ok(()) => nmx::StReturnCode::NmxStSuccess,
            Err(error) => {
                tracing::warn!(
                    domain = %domain.key,
                    error = %error,
                    "NMX-C mock rejected a partition operation"
                );
                error.return_code()
            }
        };
        nmx::ServerHeader {
            domain_uuid: domain.domain_uuid.to_string(),
            app_uuid: String::new(),
            app_ver: self.config.version_string.clone(),
            return_code: return_code as i32,
        }
    }

    fn update_partition(
        &self,
        request: Request<nmx::UpdatePartitionRequest>,
        apply: fn(&mut DomainState, PartitionRef, &[u64]) -> Result<u32, PartitionError>,
    ) -> RpcResult<nmx::UpdatePartitionResponse> {
        let update = request.get_ref();
        let target = PartitionRef::new(
            update.partition_id.as_ref().map(|id| id.partition_id),
            &update.name,
        );
        let response = self.with_domain(&request, |domain, state| {
            let result = target.and_then(|target| {
                if !update.location_list.is_empty() {
                    return Err(PartitionError::LocationBased);
                }
                apply(state, target, &update.gpu_uid)
            });
            nmx::UpdatePartitionResponse {
                server_header: Some(self.header(domain, result.as_ref().map(drop))),
                context: None,
                partition_id: result.ok().map(partition_id_message),
            }
        })?;
        Ok(Response::new(response))
    }
}

/// The uids a create request names. NICo always sends uids; a location-based
/// reference has no meaning to a mock that places GPUs by uid.
fn gpu_uids(resource_ids: &[nmx::GpuResourceId]) -> Result<Vec<u64>, PartitionError> {
    resource_ids
        .iter()
        .map(|resource| match resource.resource_id {
            Some(nmx::gpu_resource_id::ResourceId::GpuUid(uid)) => Ok(uid),
            Some(nmx::gpu_resource_id::ResourceId::GpuLocation(_)) | None => {
                Err(PartitionError::LocationBased)
            }
        })
        .collect()
}

fn partition_id_message(partition_id: u32) -> nmx::PartitionId {
    nmx::PartitionId { partition_id }
}

fn location_info(
    chassis_serial: &str,
    slot_number: u32,
    tray_index: u32,
    host_id: u32,
) -> nmx::LocationInfo {
    nmx::LocationInfo {
        chassis_serial_number: chassis_serial.to_string(),
        tray_index: u64::from(tray_index),
        location: Some(nmx::Location {
            chassis_id: 0,
            slot_id: u64::from(slot_number),
            host_id: u64::from(host_id),
        }),
    }
}

fn compute_node_info(node: &SimComputeNode, state: &DomainState) -> nmx::ComputeNodeInfo {
    let mut partition_ids: Vec<u32> = node
        .gpus
        .iter()
        .filter_map(|gpu| state.partition_of(gpu.uid))
        .collect();
    partition_ids.sort_unstable();
    partition_ids.dedup();
    nmx::ComputeNodeInfo {
        loc: Some(location_info(
            &node.chassis_serial,
            node.slot_number,
            node.tray_index,
            node.host_id,
        )),
        num_gpus: node.gpus.len() as u32,
        node_health: nmx::ComputeNodeHealth::NmxComputeNodeHealthHealthy as i32,
        partition_id_list: partition_ids
            .into_iter()
            .map(partition_id_message)
            .collect(),
    }
}

fn switch_node_info(
    switch: &SimSwitch,
    partition_id_list: Vec<nmx::PartitionId>,
) -> nmx::SwitchNodeInfo {
    nmx::SwitchNodeInfo {
        loc: Some(location_info(
            &switch.chassis_serial,
            switch.slot_number,
            switch.tray_index,
            0,
        )),
        num_switches: switch.num_switches,
        node_health: nmx::SwitchNodeHealth::NmxSwitchNodeHealthHealthy as i32,
        partition_id_list,
    }
}

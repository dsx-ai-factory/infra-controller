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
use tonic::transport::Channel;
use trace_propagation::TraceInjectService;

use crate::nmxc_model::nmx_controller_client::NmxControllerClient;
use crate::response::check_server_header_success;
use crate::{Nmxc, NmxcError, nmxc_model};

macro_rules! nmx_c_checked {
    ($operation:literal, $future:expr) => {{
        let response = $future.await?.into_inner();
        check_server_header_success(response.server_header.as_ref(), $operation)?;
        response
    }};
}

fn default_context() -> nmxc_model::Context {
    nmxc_model::Context {
        context: String::new(),
    }
}

pub(super) struct NmxcApi {
    client: NmxControllerClient<TraceInjectService<Channel>>,
}

impl NmxcApi {
    pub(super) fn new(client: NmxControllerClient<TraceInjectService<Channel>>) -> Self {
        Self { client }
    }
}

#[async_trait::async_trait]
impl Nmxc for NmxcApi {
    async fn hello(&mut self, gateway_id: &str) -> Result<nmxc_model::ServerHello, NmxcError> {
        let req = nmxc_model::ClientHello {
            gateway_id: gateway_id.to_string(),
            major_version: nmxc_model::ProtoMsgMajorVersion::ProtoMsgMajorVersion as i32,
            minor_version: nmxc_model::ProtoMsgMinorVersion::ProtoMsgMinorVersion as i32,
        };
        Ok(nmx_c_checked!(
            "Hello",
            self.client.hello(tonic::Request::new(req))
        ))
    }

    async fn get_domain_properties(
        &mut self,
        context: Option<nmxc_model::Context>,
        gateway_id: &str,
    ) -> Result<nmxc_model::DomainProperties, NmxcError> {
        let req = nmxc_model::GetDomainPropertiesRequest {
            context: Some(context.unwrap_or_else(default_context)),
            gateway_id: gateway_id.to_string(),
        };
        Ok(nmx_c_checked!(
            "GetDomainProperties",
            self.client.get_domain_properties(tonic::Request::new(req))
        ))
    }

    async fn get_domain_state_info(
        &mut self,
        context: Option<nmxc_model::Context>,
        gateway_id: &str,
    ) -> Result<nmxc_model::DomainStateInfo, NmxcError> {
        let req = nmxc_model::GetDomainStateInfoRequest {
            context: Some(context.unwrap_or_else(default_context)),
            gateway_id: gateway_id.to_string(),
        };
        Ok(nmx_c_checked!(
            "GetDomainStateInfo",
            self.client.get_domain_state_info(tonic::Request::new(req))
        ))
    }

    async fn get_topology_info(
        &mut self,
        context: Option<nmxc_model::Context>,
        gateway_id: &str,
    ) -> Result<nmxc_model::FmTopologyInfo, NmxcError> {
        let req = nmxc_model::GetTopologyInfoRequest {
            context: Some(context.unwrap_or_else(default_context)),
            gateway_id: gateway_id.to_string(),
        };
        Ok(nmx_c_checked!(
            "GetTopologyInfo",
            self.client.get_topology_info(tonic::Request::new(req))
        ))
    }

    async fn get_compute_node_count(
        &mut self,
        req: nmxc_model::GetComputeNodeCountRequest,
    ) -> Result<nmxc_model::GetComputeNodeCountResponse, NmxcError> {
        Ok(nmx_c_checked!(
            "GetComputeNodeCount",
            self.client.get_compute_node_count(tonic::Request::new(req))
        ))
    }

    async fn get_compute_node_info_list(
        &mut self,
        req: nmxc_model::GetComputeNodeInfoListRequest,
    ) -> Result<nmxc_model::GetComputeNodeInfoListResponse, NmxcError> {
        Ok(nmx_c_checked!(
            "GetComputeNodeInfoList",
            self.client
                .get_compute_node_info_list(tonic::Request::new(req))
        ))
    }

    async fn get_gpu_info_list(
        &mut self,
        req: nmxc_model::GetGpuInfoListRequest,
    ) -> Result<nmxc_model::GetGpuInfoListResponse, NmxcError> {
        Ok(nmx_c_checked!(
            "GetGpuInfoList",
            self.client.get_gpu_info_list(tonic::Request::new(req))
        ))
    }

    async fn get_switch_node_count(
        &mut self,
        req: nmxc_model::GetSwitchNodeCountRequest,
    ) -> Result<nmxc_model::GetSwitchNodeCountResponse, NmxcError> {
        Ok(nmx_c_checked!(
            "GetSwitchNodeCount",
            self.client.get_switch_node_count(tonic::Request::new(req))
        ))
    }

    async fn get_switch_node_info_list(
        &mut self,
        req: nmxc_model::GetSwitchNodeInfoListRequest,
    ) -> Result<nmxc_model::GetSwitchNodeInfoListResponse, NmxcError> {
        Ok(nmx_c_checked!(
            "GetSwitchNodeInfoList",
            self.client
                .get_switch_node_info_list(tonic::Request::new(req))
        ))
    }

    async fn get_partition_count(
        &mut self,
        req: nmxc_model::GetPartitionCountRequest,
    ) -> Result<nmxc_model::GetPartitionCountResponse, NmxcError> {
        Ok(nmx_c_checked!(
            "GetPartitionCount",
            self.client.get_partition_count(tonic::Request::new(req))
        ))
    }

    async fn get_partition_id_list(
        &mut self,
        req: nmxc_model::GetPartitionIdListRequest,
    ) -> Result<nmxc_model::GetPartitionIdListResponse, NmxcError> {
        Ok(nmx_c_checked!(
            "GetPartitionIdList",
            self.client.get_partition_id_list(tonic::Request::new(req))
        ))
    }

    async fn get_partition_info_list(
        &mut self,
        req: nmxc_model::GetPartitionInfoListRequest,
    ) -> Result<nmxc_model::GetPartitionInfoListResponse, NmxcError> {
        Ok(nmx_c_checked!(
            "GetPartitionInfoList",
            self.client
                .get_partition_info_list(tonic::Request::new(req))
        ))
    }

    async fn create_partition(
        &mut self,
        req: nmxc_model::CreatePartitionRequest,
    ) -> Result<nmxc_model::CreatePartitionResponse, NmxcError> {
        Ok(nmx_c_checked!(
            "CreatePartition",
            self.client.create_partition(tonic::Request::new(req))
        ))
    }

    async fn delete_partition(
        &mut self,
        req: nmxc_model::DeletePartitionRequest,
    ) -> Result<nmxc_model::DeletePartitionResponse, NmxcError> {
        Ok(nmx_c_checked!(
            "DeletePartition",
            self.client.delete_partition(tonic::Request::new(req))
        ))
    }

    async fn add_gpus_to_partition(
        &mut self,
        req: nmxc_model::UpdatePartitionRequest,
    ) -> Result<nmxc_model::UpdatePartitionResponse, NmxcError> {
        Ok(nmx_c_checked!(
            "AddGpusToPartition",
            self.client.add_gpus_to_partition(tonic::Request::new(req))
        ))
    }

    async fn remove_gpus_from_partition(
        &mut self,
        req: nmxc_model::UpdatePartitionRequest,
    ) -> Result<nmxc_model::UpdatePartitionResponse, NmxcError> {
        Ok(nmx_c_checked!(
            "RemoveGpusFromPartition",
            self.client
                .remove_gpus_from_partition(tonic::Request::new(req))
        ))
    }
}

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
use std::string::String;

use reqwest::StatusCode;

use crate::nmxm_model::*;
use crate::{Endpoint, Nmxm, NmxmApiClient, NmxmApiError};

// Macro for GET operations that return a vector of items
macro_rules! get_list {
    ($self:expr, $base_url:literal, $return_type:ty, $id:expr) => {{
        let mut result = Vec::new();
        let url = if let Some(id) = $id {
            format!("{}/{}", $base_url, id)
        } else {
            $base_url.to_string()
        };
        let (_status, items): (StatusCode, $return_type) = $self.client.get(&url).await?;
        result.extend(items);
        Ok(result)
    }};
    ($self:expr, $base_url:literal, $return_type:ty) => {{
        let mut result = Vec::new();
        let url = $base_url.to_string();
        let (_status, items): (StatusCode, $return_type) = $self.client.get(&url).await?;
        result.extend(items);
        Ok(result)
    }};
}

// Macro for GET operations that return a count
macro_rules! get_count {
    ($self:expr, $base_url:literal) => {{
        let url = $base_url.to_string();
        let (_status, count): (StatusCode, CountResponse) = $self.client.get(&url).await?;
        Ok(count.total.unwrap_or(0))
    }};
}

// Macro for async operations (create, update, delete)
macro_rules! async_operation {
    ($self:expr, $method:ident, $url:expr, $data:expr) => {{
        let (_status, response): (StatusCode, AsyncResponse) =
            $self.client.$method(&$url, $data).await?;
        Ok(response)
    }};
    ($self:expr, $method:ident, $url:expr) => {{
        let (_status, response): (StatusCode, AsyncResponse) = $self.client.$method(&$url).await?;
        Ok(response)
    }};
}

#[derive(Clone)]
pub(super) struct NmxmApi {
    client: NmxmApiClient,
}
impl NmxmApi {
    pub(super) fn new(client: &NmxmApiClient) -> Self {
        Self {
            client: client.clone(),
        }
    }
}

#[async_trait::async_trait]
impl Nmxm for NmxmApi {
    async fn create(&self, _endpoint: Endpoint) -> Result<Box<dyn Nmxm>, NmxmApiError> {
        Ok(Box::new(self.clone()))
    }

    async fn raw_get(&self, api: &str) -> Result<RawResponse, NmxmApiError> {
        let response = self.client.get_raw(api).await?;

        Ok(response)
    }

    async fn get_chassis_count(
        &self,
        _domain: Option<Vec<uuid::Uuid>>,
    ) -> Result<i64, NmxmApiError> {
        get_count!(self, "nmx/v1/chassis/count")
    }

    async fn get_chassis(&self, _id: String) -> Result<Vec<Chassis>, NmxmApiError> {
        let mut c = Vec::new();
        let url = "nmx/v1/chassis".to_string();
        let (_status, chassis): (StatusCode, Vec<Chassis>) = self.client.get(&url).await?;
        c.extend(chassis);

        Ok(c)
    }

    async fn get_gpu(&self, id: Option<String>) -> Result<Vec<Gpu>, NmxmApiError> {
        let mut g = Vec::new();
        let mut url = String::from("nmx/v1/gpus");
        if let Some(id) = id {
            url = format!("{}/{}", url, id);
        }
        let (_status, gpus): (StatusCode, Vec<Gpu>) = self.client.get(&url).await?;
        g.extend(gpus);
        Ok(g)
    }

    async fn get_gpu_count(&self, _domain: Option<Vec<uuid::Uuid>>) -> Result<i64, NmxmApiError> {
        let url = String::from("nmx/v1/gpus/count");
        let (_status, count): (StatusCode, CountResponse) = self.client.get(&url).await?;
        if let Some(c) = count.total {
            return Ok(c);
        }
        Ok(0)
    }

    async fn get_partition(&self, id: String) -> Result<Partition, NmxmApiError> {
        let url = format!("nmx/v1/partitions/{}", id);
        let (_status, partition): (StatusCode, Partition) = self.client.get(&url).await?;
        Ok(partition)
    }

    async fn get_partitions_list(&self) -> Result<Vec<Partition>, NmxmApiError> {
        let mut c = Vec::new();
        let url = String::from("nmx/v1/partitions");

        let (_status, partitions): (StatusCode, Vec<Partition>) = self.client.get(&url).await?;
        c.extend(partitions);

        Ok(c)
    }

    async fn get_compute_node(&self, id: Option<String>) -> Result<Vec<ComputeNode>, NmxmApiError> {
        let mut n = Vec::new();
        let mut url = String::from("nmx/v1/compute-nodes");
        if let Some(id) = id {
            url = format!("{}/{}", url, id);
        }
        let (_status, nodes): (StatusCode, Vec<ComputeNode>) = self.client.get(&url).await?;
        n.extend(nodes);
        Ok(n)
    }

    async fn get_compute_nodes_count(
        &self,
        _domain: Option<Vec<uuid::Uuid>>,
    ) -> Result<i64, NmxmApiError> {
        let url = String::from("nmx/v1/compute-nodes/count");
        let (_status, count): (StatusCode, CountResponse) = self.client.get(&url).await?;
        if let Some(c) = count.total {
            return Ok(c);
        }
        Ok(0)
    }

    async fn get_port(&self, id: Option<String>) -> Result<Vec<Port>, NmxmApiError> {
        get_list!(self, "nmx/v1/ports", Vec<Port>, id)
    }

    async fn get_ports_count(&self, _domain: Option<Vec<uuid::Uuid>>) -> Result<i64, NmxmApiError> {
        get_count!(self, "nmx/v1/ports/count")
    }

    async fn get_switch_node(&self, id: Option<String>) -> Result<Vec<SwitchNode>, NmxmApiError> {
        get_list!(self, "nmx/v1/switch-nodes", Vec<SwitchNode>, id)
    }

    async fn get_switch_nodes_count(
        &self,
        _domain: Option<Vec<uuid::Uuid>>,
    ) -> Result<i64, NmxmApiError> {
        get_count!(self, "nmx/v1/switch-nodes/count")
    }

    async fn create_partition(
        &self,
        req: Option<CreatePartitionRequest>,
    ) -> Result<AsyncResponse, NmxmApiError> {
        let url = String::from("nmx/v1/partitions");

        let (_status, response): (StatusCode, AsyncResponse) = self.client.post(&url, req).await?;
        Ok(response)
    }

    async fn delete_partition(&self, id: String) -> Result<AsyncResponse, NmxmApiError> {
        async_operation!(self, delete, format!("nmx/v1/partitions/{}", id))
    }

    async fn update_partition(
        &self,
        id: String,
        req: UpdatePartitionRequest,
    ) -> Result<AsyncResponse, NmxmApiError> {
        async_operation!(self, put, format!("nmx/v1/partitions/{}", id), req)
    }

    async fn get_operation(&self, id: String) -> Result<Operation, NmxmApiError> {
        let url = format!("nmx/v1/operations/{}", id);
        let (_status, op): (StatusCode, Operation) = self.client.get(&url).await?;
        Ok(op)
    }

    async fn get_operations_list(&self) -> Result<Vec<Operation>, NmxmApiError> {
        let url = "nmx/v1/operations".to_string();
        let mut op_list = Vec::new();
        let (_status, operations): (StatusCode, Vec<Operation>) = self.client.get(&url).await?;
        op_list.extend(operations);
        Ok(op_list)
    }

    async fn cancel_operation(&self, id: String) -> Result<AsyncResponse, NmxmApiError> {
        let url = format!("nmx/v1/operations/{}", id);
        let (_status, response): (StatusCode, AsyncResponse) = self.client.delete(&url).await?;
        Ok(response)
    }
}

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

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use async_trait::async_trait;
use model::ib::{IBNetwork, IBPort, IBPortState, IBQosConf};

use crate::errors::IbError;
use crate::ib::IBFabricManagerConfig;

#[derive(Default)]
pub struct Filter {
    pub guids: Option<HashSet<String>>,
    pub pkey: Option<u16>,
    pub state: Option<IBPortState>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IBFabricVersions {
    pub ufm_version: String,
}

pub struct IBFabricRawResponse {
    /// Response body
    pub body: String,
    /// Response status code
    pub code: u16,
    /// Response headers
    pub headers: http::HeaderMap,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IBFabricConfig {
    /// The subnet_prefix of UFM
    /// Subnet prefix used on the subnet.
    /// Default: 0xfe80000000000000
    pub subnet_prefix: String,
    /// The m_key of UFM
    /// M_Key value sent to all ports qualifying all Set(PortInfo).
    /// Default: 0x0000000000000000
    pub m_key: String,
    /// The sm_key of UFM
    /// SM_Key value of the Subnet Manager used for authentication.
    /// Default: 0x0000000000000001
    pub sm_key: String,
    /// The sa_key of UFM
    /// SM_Key value used to qualify received Subnet Administrator queries as trusted.
    /// Default: 0x0000000000000001
    pub sa_key: String,
    /// The m_key_per_port of UFM
    /// When m_key_per_port is enabled, OpenSM will generate an M_Key for each port.
    /// Default: false
    pub m_key_per_port: bool,
}

impl Default for IBFabricConfig {
    fn default() -> Self {
        Self {
            subnet_prefix: "0xfe80000000000000".to_string(),
            m_key: "0x0000000000000000".to_string(),
            sm_key: "0x0000000000000001".to_string(),
            sa_key: "0x0000000000000001".to_string(),
            m_key_per_port: false,
        }
    }
}

#[async_trait]
pub trait IBFabricManager: Send + Sync {
    async fn new_client(&self, fabric_name: &str) -> Result<Arc<dyn IBFabric>, IbError>;
    fn get_config(&self) -> IBFabricManagerConfig;
}

#[derive(Default, Debug, Copy, Clone)]
pub struct GetPartitionOptions {
    /// Whether to include `guids` associated with each partition in the response
    pub include_guids_data: bool,
    /// Whether the response should contain the `qos_conf` and `ip_over_ib` parameters
    pub include_qos_conf: bool,
}

#[async_trait]
pub trait IBFabric: Send + Sync {
    /// Get fabric configuration
    async fn get_fabric_config(&self) -> Result<IBFabricConfig, IbError>;

    /// Update an IB Partitions QoS configuration
    async fn update_partition_qos_conf(
        &self,
        pkey: u16,
        qos_conf: &IBQosConf,
    ) -> Result<(), IbError>;

    /// Get all IB Networks
    async fn get_ib_networks(
        &self,
        options: GetPartitionOptions,
    ) -> Result<HashMap<u16, IBNetwork>, IbError>;

    /// Get IBNetwork by ID
    async fn get_ib_network(
        &self,
        pkey: u16,
        options: GetPartitionOptions,
    ) -> Result<IBNetwork, IbError>;

    /// Create IBPort
    async fn bind_ib_ports(&self, ibnetwork: IBNetwork, ports: Vec<String>) -> Result<(), IbError>;

    /// Delete IBPort
    async fn unbind_ib_ports(&self, pkey: u16, id: Vec<String>) -> Result<(), IbError>;

    /// Find IBPort
    async fn find_ib_port(&self, filter: Option<Filter>) -> Result<Vec<IBPort>, IbError>;

    /// Returns IB fabric related versions
    async fn versions(&self) -> Result<IBFabricVersions, IbError>;

    /// Make a raw HTTP GET request to the Fabric Manager using the given path,
    /// and return the response body.
    async fn raw_get(&self, path: &str) -> Result<IBFabricRawResponse, IbError>;
}

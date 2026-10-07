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

use std::collections::HashMap;

use async_trait::async_trait;
use model::ib::{IBNetwork, IBPort, IBQosConf};

use super::iface::{Filter, GetPartitionOptions, IBFabricRawResponse};
use super::{IBFabric, IBFabricConfig, IBFabricVersions};
use crate::errors::IbError;

pub(super) struct DisableIBFabric {}

#[async_trait]
impl IBFabric for DisableIBFabric {
    /// Get fabric configuration
    async fn get_fabric_config(&self) -> Result<IBFabricConfig, IbError> {
        Err(IbError::IBFabricError("ib fabric is disabled".to_string()))
    }

    /// Get IBNetwork by ID
    async fn get_ib_network(
        &self,
        _: u16,
        _options: GetPartitionOptions,
    ) -> Result<IBNetwork, IbError> {
        Err(IbError::IBFabricError("ib fabric is disabled".to_string()))
    }

    async fn get_ib_networks(
        &self,
        _options: GetPartitionOptions,
    ) -> Result<HashMap<u16, IBNetwork>, IbError> {
        Err(IbError::IBFabricError("ib fabric is disabled".to_string()))
    }

    async fn bind_ib_ports(&self, _: IBNetwork, _: Vec<String>) -> Result<(), IbError> {
        Err(IbError::IBFabricError("ib fabric is disabled".to_string()))
    }

    /// Update an IB Partitions QoS configuration
    async fn update_partition_qos_conf(
        &self,
        _pkey: u16,
        _qos_conf: &IBQosConf,
    ) -> Result<(), IbError> {
        Err(IbError::IBFabricError("ib fabric is disabled".to_string()))
    }

    /// Find IBPort
    async fn find_ib_port(&self, _: Option<Filter>) -> Result<Vec<IBPort>, IbError> {
        Err(IbError::IBFabricError("ib fabric is disabled".to_string()))
    }

    /// Delete IBPort
    async fn unbind_ib_ports(&self, _: u16, _: Vec<String>) -> Result<(), IbError> {
        Err(IbError::IBFabricError("ib fabric is disabled".to_string()))
    }

    /// Returns IB fabric related versions
    async fn versions(&self) -> Result<IBFabricVersions, IbError> {
        Err(IbError::IBFabricError("ib fabric is disabled".to_string()))
    }

    /// Make a raw HTTP GET request to the Fabric Manager using the given path,
    /// and return the response body.
    async fn raw_get(&self, _path: &str) -> Result<IBFabricRawResponse, IbError> {
        Err(IbError::IBFabricError("ib fabric is disabled".to_string()))
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::*;
    use carbide_test_support::{Case, check_cases_async};

    use super::*;

    #[derive(Clone, Copy, Debug)]
    enum DisabledOperation {
        GetFabricConfig,
        GetNetwork,
        GetNetworks,
        BindPorts,
        UpdateQos,
        FindPort,
        UnbindPorts,
        Versions,
        RawGet,
    }

    fn ib_network() -> IBNetwork {
        IBNetwork {
            name: "tenant".to_string(),
            pkey: 1,
            ipoib: true,
            qos_conf: None,
            associated_guids: None,
            membership: None,
        }
    }

    fn qos_conf() -> IBQosConf {
        IBQosConf {
            mtu: Default::default(),
            service_level: Default::default(),
            rate_limit: Default::default(),
        }
    }

    async fn run_disabled_operation(operation: DisabledOperation) -> Result<(), String> {
        let fabric = DisableIBFabric {};
        match operation {
            DisabledOperation::GetFabricConfig => fabric.get_fabric_config().await.map(|_| ()),
            DisabledOperation::GetNetwork => fabric
                .get_ib_network(1, GetPartitionOptions::default())
                .await
                .map(|_| ()),
            DisabledOperation::GetNetworks => fabric
                .get_ib_networks(GetPartitionOptions::default())
                .await
                .map(|_| ()),
            DisabledOperation::BindPorts => {
                fabric
                    .bind_ib_ports(ib_network(), vec!["guid-1".to_string()])
                    .await
            }
            DisabledOperation::UpdateQos => fabric.update_partition_qos_conf(1, &qos_conf()).await,
            DisabledOperation::FindPort => fabric.find_ib_port(None).await.map(|_| ()),
            DisabledOperation::UnbindPorts => {
                fabric.unbind_ib_ports(1, vec!["guid-1".to_string()]).await
            }
            DisabledOperation::Versions => fabric.versions().await.map(|_| ()),
            DisabledOperation::RawGet => fabric.raw_get("/app/ufm_version").await.map(|_| ()),
        }
        .map_err(|error| error.to_string())
    }

    #[tokio::test]
    async fn disabled_fabric_rejects_operations() {
        let disabled = "failed to call IBFabricManager: ib fabric is disabled".to_string();
        check_cases_async(
            [
                Case {
                    scenario: "get fabric config",
                    input: DisabledOperation::GetFabricConfig,
                    expect: FailsWith(disabled.clone()),
                },
                Case {
                    scenario: "get network",
                    input: DisabledOperation::GetNetwork,
                    expect: FailsWith(disabled.clone()),
                },
                Case {
                    scenario: "get networks",
                    input: DisabledOperation::GetNetworks,
                    expect: FailsWith(disabled.clone()),
                },
                Case {
                    scenario: "bind ports",
                    input: DisabledOperation::BindPorts,
                    expect: FailsWith(disabled.clone()),
                },
                Case {
                    scenario: "update qos",
                    input: DisabledOperation::UpdateQos,
                    expect: FailsWith(disabled.clone()),
                },
                Case {
                    scenario: "find port",
                    input: DisabledOperation::FindPort,
                    expect: FailsWith(disabled.clone()),
                },
                Case {
                    scenario: "unbind ports",
                    input: DisabledOperation::UnbindPorts,
                    expect: FailsWith(disabled.clone()),
                },
                Case {
                    scenario: "versions",
                    input: DisabledOperation::Versions,
                    expect: FailsWith(disabled.clone()),
                },
                Case {
                    scenario: "raw get",
                    input: DisabledOperation::RawGet,
                    expect: FailsWith(disabled),
                },
            ],
            run_disabled_operation,
        )
        .await;
    }
}

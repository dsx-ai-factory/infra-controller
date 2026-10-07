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
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use model::ib::{
    IBMtu, IBNetwork, IBPort, IBPortMembership, IBPortState, IBQosConf, IBRateLimit, IBServiceLevel,
};

use super::iface::{Filter, GetPartitionOptions, IBFabricRawResponse};
use super::ufmclient::{
    self, Partition, PartitionKey, PartitionQoS, Port, PortConfig, PortMembership, SmConfig,
    UFMCert, UFMConfig, UFMError, Ufm,
};
use super::{IBFabric, IBFabricConfig, IBFabricVersions};
use crate::errors::IbError;

pub(super) struct RestIBFabric {
    ufm: Ufm,
}

const DEFAULT_INDEX0: bool = true;
const DEFAULT_MEMBERSHIP: PortMembership = PortMembership::Full;

/// Detects the authentication method `auth` selects: a bearer token, or
/// client-certificate files on disk. Certificate authentication is chosen for
/// an empty `auth` string (the SPIFFE default paths) or one naming an existing
/// path. The returned [`UFMCert`] paths are exactly what a client built from
/// `auth` loads, so callers can also watch them for rotation.
pub(super) fn auth_method(auth: &str) -> (Option<String>, Option<UFMCert>) {
    if auth.trim().is_empty() {
        (
            None,
            Some(UFMCert {
                ca_crt: "/var/run/secrets/spiffe.io/ca.crt".to_string(),
                tls_key: "/var/run/secrets/spiffe.io/tls.key".to_string(),
                tls_crt: "/var/run/secrets/spiffe.io/tls.crt".to_string(),
            }),
        )
    } else if Path::new(auth).exists() {
        (
            None,
            Some(UFMCert {
                ca_crt: format!("{auth}/ca.crt"),
                tls_key: format!("{auth}/tls.key"),
                tls_crt: format!("{auth}/tls.crt"),
            }),
        )
    } else {
        (Some(auth.to_string()), None)
    }
}

pub(super) fn new_client(addr: &str, auth: &str) -> Result<Arc<dyn IBFabric>, IbError> {
    let (token, cert) = auth_method(auth);

    let conf = UFMConfig {
        address: addr.to_string(),
        username: None,
        password: None,
        token,
        cert,
    };

    let ufm = ufmclient::new_client(conf).map_err(IbError::from)?;

    Ok(Arc::new(RestIBFabric { ufm }))
}

#[async_trait]
impl IBFabric for RestIBFabric {
    /// Get fabric configuration
    async fn get_fabric_config(&self) -> Result<IBFabricConfig, IbError> {
        self.ufm
            .get_sm_config()
            .await
            .map(IBFabricConfig::from)
            .map_err(IbError::from)
    }

    /// Get all IB Networks
    async fn get_ib_networks(
        &self,
        options: GetPartitionOptions,
    ) -> Result<HashMap<u16, IBNetwork>, IbError> {
        let partitions = self
            .ufm
            .list_partitions(ufmclient::GetPartitionOptions {
                include_guids_data: options.include_guids_data,
                include_qos_conf: options.include_qos_conf,
            })
            .await?;

        let mut results = HashMap::with_capacity(partitions.len());
        for (pkey, partition) in partitions.into_iter() {
            let pkey = pkey.into();
            let network = IBNetwork::try_from(partition)?;
            results.insert(pkey, network);
        }

        Ok(results)
    }

    /// Get IBNetwork by ID
    async fn get_ib_network(
        &self,
        pkey: u16,
        options: GetPartitionOptions,
    ) -> Result<IBNetwork, IbError> {
        let pkey = PartitionKey::try_from(pkey)?;

        let partition = self
            .ufm
            .get_partition(
                pkey,
                ufmclient::GetPartitionOptions {
                    include_guids_data: options.include_guids_data,
                    include_qos_conf: options.include_qos_conf,
                },
            )
            .await?;

        IBNetwork::try_from(partition)
    }

    /// Create IBPort
    async fn bind_ib_ports(&self, ibnetwork: IBNetwork, ports: Vec<String>) -> Result<(), IbError> {
        let partition = Partition::try_from(ibnetwork)?;
        let ports = ports.iter().map(PortConfig::from).collect();

        self.ufm
            .bind_ports(partition, ports)
            .await
            .map_err(Into::into)
    }

    /// Update an IB Partitions QoS configuration
    async fn update_partition_qos_conf(
        &self,
        pkey: u16,
        qos_conf: &IBQosConf,
    ) -> Result<(), IbError> {
        let qos = PartitionQoS::try_from(qos_conf)?;
        let pkey = PartitionKey::try_from(pkey)?;

        self.ufm
            .update_partition_qos(pkey, qos)
            .await
            .map_err(Into::into)
    }

    /// Delete IBPort
    async fn unbind_ib_ports(&self, pkey: u16, ids: Vec<String>) -> Result<(), IbError> {
        let pkey = PartitionKey::try_from(pkey)?;

        self.ufm.unbind_ports(pkey, ids).await.map_err(Into::into)
    }

    /// Find IBPort
    async fn find_ib_port(&self, filter: Option<Filter>) -> Result<Vec<IBPort>, IbError> {
        let filter = filter.map(ufmclient::Filter::try_from).transpose()?;
        self.ufm
            .list_port(filter)
            .await
            .map(|p| p.iter().map(IBPort::from).collect())
            .map_err(Into::into)
    }

    /// Returns IB fabric related versions
    async fn versions(&self) -> Result<IBFabricVersions, IbError> {
        let ufm_version = self.ufm.version().await?;

        Ok(IBFabricVersions { ufm_version })
    }

    /// Make a raw HTTP GET request to the Fabric Manager using the given path,
    /// and return the response body.
    async fn raw_get(&self, path: &str) -> Result<IBFabricRawResponse, IbError> {
        let response = match self.ufm.raw_get(path).await {
            Ok((body, details)) => IBFabricRawResponse {
                body,
                code: details.status_code,
                headers: details.headers,
            },
            Err(UFMError::HttpError {
                status_code,
                body,
                headers,
            }) => IBFabricRawResponse {
                body,
                code: status_code,
                headers: *headers,
            },
            Err(e) => return Err(e.into()),
        };
        Ok(response)
    }
}

impl From<UFMError> for IbError {
    fn from(e: UFMError) -> Self {
        match e {
            // This is required to let the IB partition handler move on into the final deletion state
            UFMError::NotFound {
                path,
                status_code: _,
                body: _,
                headers: _,
            } => IbError::NotFoundError {
                kind: "ufm_path",
                id: path,
            },
            _ => IbError::IBFabricError(e.to_string()),
        }
    }
}

impl From<SmConfig> for IBFabricConfig {
    fn from(c: SmConfig) -> Self {
        Self {
            subnet_prefix: c.subnet_prefix.clone(),
            m_key: c.m_key.clone(),
            sm_key: c.sm_key.clone(),
            sa_key: c.sa_key.clone(),
            m_key_per_port: c.m_key_per_port,
        }
    }
}

impl TryFrom<PartitionQoS> for IBQosConf {
    type Error = IbError;
    fn try_from(qos: PartitionQoS) -> Result<Self, Self::Error> {
        IBQosConf::try_from(&qos)
    }
}

impl TryFrom<&PartitionQoS> for IBQosConf {
    type Error = IbError;
    fn try_from(qos: &PartitionQoS) -> Result<Self, Self::Error> {
        let rate_limit_value = if qos.rate_limit == (qos.rate_limit as i32) as f32 {
            qos.rate_limit as i32
        } else if qos.rate_limit == 2.5 {
            // It is special case for SDR as 2.5
            2
        } else {
            return Err(IbError::InvalidArgument(format!(
                "{0} is an invalid rate limit",
                qos.rate_limit
            )));
        };
        Ok(IBQosConf {
            mtu: IBMtu::try_from(qos.mtu_limit as i32)?,
            service_level: IBServiceLevel::try_from(qos.service_level as i32)?,
            rate_limit: IBRateLimit::try_from(rate_limit_value)?,
        })
    }
}

impl TryFrom<Partition> for IBNetwork {
    type Error = IbError;
    fn try_from(p: Partition) -> Result<Self, Self::Error> {
        IBNetwork::try_from(&p)
    }
}

impl TryFrom<&Partition> for IBNetwork {
    type Error = IbError;
    fn try_from(p: &Partition) -> Result<Self, Self::Error> {
        Ok(IBNetwork {
            name: p.name.clone(),
            pkey: p.pkey.into(),
            ipoib: p.ipoib,
            qos_conf: p.qos.as_ref().map(|qos| qos.try_into()).transpose()?,
            associated_guids: p.guids.clone(),
            membership: p.membership.map(Into::into),
            // Not implemented yet
            // enable_sharp: false,
            // index0: IBNETWORK_DEFAULT_INDEX0,
        })
    }
}

impl TryFrom<Filter> for ufmclient::Filter {
    type Error = IbError;
    fn try_from(filter: Filter) -> Result<Self, Self::Error> {
        Ok(Self {
            guids: filter.guids.clone(),
            pkey: filter
                .pkey
                .map(ufmclient::PartitionKey::try_from)
                .transpose()?,
            logical_state: filter.state.map(|state| format!("{state:?}")),
        })
    }
}

impl TryFrom<IBQosConf> for PartitionQoS {
    type Error = IbError;
    fn try_from(qos: IBQosConf) -> Result<Self, Self::Error> {
        PartitionQoS::try_from(&qos)
    }
}

impl TryFrom<&IBQosConf> for PartitionQoS {
    type Error = IbError;
    fn try_from(qos: &IBQosConf) -> Result<Self, Self::Error> {
        let rate_limit_value = if qos.rate_limit == IBRateLimit(2) {
            // It is special case for SDR as 2.5
            2.5_f32
        } else {
            Into::<i32>::into(qos.rate_limit.clone()) as f32
        };
        Ok(PartitionQoS {
            mtu_limit: Into::<i32>::into(qos.mtu.clone()) as u16,
            service_level: Into::<i32>::into(qos.service_level.clone()) as u8,
            rate_limit: rate_limit_value,
        })
    }
}

impl TryFrom<IBNetwork> for Partition {
    type Error = IbError;
    fn try_from(p: IBNetwork) -> Result<Self, Self::Error> {
        Partition::try_from(&p)
    }
}

impl TryFrom<&IBNetwork> for Partition {
    type Error = IbError;
    fn try_from(p: &IBNetwork) -> Result<Self, Self::Error> {
        Ok(Partition {
            name: p.name.clone(),
            pkey: PartitionKey::try_from(p.pkey)
                .map_err(|_| IbError::IBFabricError("invalid pkey".to_string()))?,
            ipoib: p.ipoib,
            qos: p.qos_conf.as_ref().map(|qos| qos.try_into()).transpose()?,
            guids: Default::default(),
            membership: p.membership.map(Into::into),
        })
    }
}

impl From<&Port> for IBPort {
    fn from(p: &Port) -> Self {
        IBPort {
            name: p.name.clone(),
            guid: p.guid.clone(),
            lid: p.lid,
            state: IBPortState::try_from(p.logical_state.clone()).ok(),
        }
    }
}

impl From<Port> for IBPort {
    fn from(p: Port) -> Self {
        IBPort::from(&p)
    }
}

impl From<&IBPort> for PortConfig {
    fn from(p: &IBPort) -> Self {
        PortConfig {
            guid: p.guid.clone(),
            index0: DEFAULT_INDEX0,
            membership: DEFAULT_MEMBERSHIP,
        }
    }
}

impl From<IBPort> for PortConfig {
    fn from(p: IBPort) -> Self {
        PortConfig::from(&p)
    }
}

impl From<&String> for PortConfig {
    fn from(guid: &String) -> Self {
        PortConfig {
            guid: guid.clone(),
            index0: DEFAULT_INDEX0,
            membership: DEFAULT_MEMBERSHIP,
        }
    }
}

impl From<String> for PortConfig {
    fn from(guid: String) -> Self {
        PortConfig::from(&guid)
    }
}

impl From<IBPortMembership> for PortMembership {
    fn from(m: IBPortMembership) -> Self {
        match m {
            IBPortMembership::Full => PortMembership::Full,
            IBPortMembership::Limited => PortMembership::Limited,
        }
    }
}

impl From<PortMembership> for IBPortMembership {
    fn from(m: PortMembership) -> Self {
        match m {
            PortMembership::Full => IBPortMembership::Full,
            PortMembership::Limited => IBPortMembership::Limited,
        }
    }
}

#[cfg(test)]
mod tests {
    use model::errors::ModelError;
    use opentelemetry::global;
    use ufm_mock::{
        InfinibandPortState, InventoryMachine, InventoryPort, InventorySnapshot, UfmAuthToken,
        UfmMock, UfmMockConfig,
    };

    use super::*;

    #[tokio::test]
    async fn rest_client_is_compatible_with_the_ufm_mock() {
        let auth_token = UfmAuthToken::new("test-token".to_string()).unwrap();
        let ufm_mock = UfmMock::new(
            &UfmMockConfig::default(),
            &auth_token,
            &global::meter("ib-fabric-ufm-mock-compatibility-test"),
        )
        .unwrap();
        ufm_mock
            .apply_inventory(InventorySnapshot {
                inventory_id: "inventory-a".into(),
                epoch_id: "epoch-a".into(),
                generation: 1.into(),
                machines: vec![InventoryMachine {
                    mat_id: "mat-a".into(),
                    machine_id: None,
                    infiniband_ports: Some(vec![InventoryPort {
                        guid: "0000000000000001".parse().unwrap(),
                        state: InfinibandPortState::Active,
                    }]),
                }],
            })
            .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, ufm_mock.router()).await.unwrap();
        });
        let client = new_client(&format!("http://{address}"), "test-token").unwrap();

        assert_eq!(client.versions().await.unwrap().ufm_version, "6.18.0");
        assert_eq!(client.find_ib_port(None).await.unwrap().len(), 1);
        let partitions = client
            .get_ib_networks(GetPartitionOptions {
                include_guids_data: true,
                include_qos_conf: false,
            })
            .await
            .unwrap();
        assert!(
            partitions[&0x7fff]
                .associated_guids
                .as_ref()
                .unwrap()
                .is_empty()
        );

        server.abort();
        assert!(server.await.unwrap_err().is_cancelled());
    }

    #[test]
    fn ib_rest_type_conversion() {
        let value = 0x7;
        let result = PartitionKey::try_from(value);
        assert!(result.is_ok());
        let pkey = result.unwrap();

        // Valid Partition
        let value = Partition {
            name: "PartitionTest".to_string(),
            pkey,
            ipoib: true,
            qos: Some(PartitionQoS {
                mtu_limit: 2,
                service_level: 0,
                rate_limit: 10.0,
            }),
            guids: Default::default(),
            membership: None,
        };
        let result = IBNetwork::try_from(value);
        assert!(result.is_ok());

        // Invalid Partition (mtu)
        let value = Partition {
            name: "PartitionTest".to_string(),
            pkey,
            ipoib: true,
            qos: Some(PartitionQoS {
                mtu_limit: 8,
                service_level: 0,
                rate_limit: 10.0,
            }),
            guids: Default::default(),
            membership: None,
        };
        let result = IBNetwork::try_from(value);
        assert!(result.is_err());
        assert!(matches!(
            result,
            Err(IbError::ModelError(ModelError::InvalidArgument(_)))
        ));

        // Invalid Partition (service level)
        let value = Partition {
            name: "PartitionTest".to_string(),
            pkey,
            ipoib: true,
            qos: Some(PartitionQoS {
                mtu_limit: 2,
                service_level: 20,
                rate_limit: 10.0,
            }),
            guids: Default::default(),
            membership: None,
        };
        let result = IBNetwork::try_from(value);
        assert!(result.is_err());
        assert!(matches!(
            result,
            Err(IbError::ModelError(ModelError::InvalidArgument(_)))
        ));

        // Invalid Partition (rate limit)
        let value = Partition {
            name: "PartitionTest".to_string(),
            pkey,
            ipoib: true,
            qos: Some(PartitionQoS {
                mtu_limit: 2,
                service_level: 0,
                rate_limit: 15.0,
            }),
            guids: Default::default(),
            membership: None,
        };
        let result = IBNetwork::try_from(value);
        assert!(result.is_err());
        assert!(matches!(
            result,
            Err(IbError::ModelError(ModelError::InvalidArgument(_)))
        ));

        // Check special (rate limit as 2(2.5))
        let expected_partition = Partition {
            name: "PartitionTest".to_string(),
            pkey,
            ipoib: true,
            qos: Some(PartitionQoS {
                mtu_limit: 2,
                service_level: 0,
                rate_limit: 2.5,
            }),
            guids: Default::default(),
            membership: None,
        };
        let value = IBNetwork::try_from(expected_partition.clone());
        assert!(value.is_ok(), "IBNetwork::try_from() failure");
        let v = value.unwrap();
        assert_eq!(v.qos_conf.as_ref().unwrap().rate_limit, IBRateLimit(2));
        let result = Partition::try_from(v);
        assert!(result.is_ok(), "Partition::try_from() failure");
        let v = result.unwrap();
        assert_eq!(v.qos.as_ref().unwrap().rate_limit, 2.5_f32);
        assert_eq!(expected_partition, v);

        // Partition <-> IBNetwork
        let expected_partition = Partition {
            name: "PartitionTest".to_string(),
            pkey,
            ipoib: true,
            qos: Some(PartitionQoS {
                mtu_limit: 2,
                service_level: 5,
                rate_limit: 10.0,
            }),
            guids: Default::default(),
            membership: Some(PortMembership::Full),
        };
        let value = IBNetwork::try_from(expected_partition.clone());
        assert!(value.is_ok(), "IBNetwork::try_from() failure");
        let result = Partition::try_from(value.unwrap());
        assert!(result.is_ok(), "Partition::try_from() failure");
        assert_eq!(expected_partition, result.unwrap());

        // Check IBPortState
        assert_eq!(
            Some(IBPortState::Active),
            IBPortState::try_from("active".to_string()).ok()
        );
        assert_eq!(
            Some(IBPortState::Active),
            IBPortState::try_from("Active".to_string()).ok()
        );
        assert_eq!(
            Some(IBPortState::Active),
            IBPortState::try_from(" Active".to_string()).ok()
        );
        assert_eq!(
            Some(IBPortState::Active),
            IBPortState::try_from(" Active ".to_string()).ok()
        );

        assert_eq!(
            Some(IBPortState::Down),
            IBPortState::try_from("Down".to_string()).ok()
        );
        assert_eq!(
            Some(IBPortState::Armed),
            IBPortState::try_from("Armed".to_string()).ok()
        );
        assert_eq!(
            Some(IBPortState::Initialize),
            IBPortState::try_from("Initialize".to_string()).ok()
        );

        assert_eq!(None, IBPortState::try_from("".to_string()).ok());
        assert_eq!(None, IBPortState::try_from("Actived".to_string()).ok());
        assert_eq!(None, IBPortState::try_from("Polling".to_string()).ok());

        // Port <-> IBPort
        let expected_port = Port {
            guid: "1070fd0300176625".to_string(),
            name: "1070fd0300176625_2".to_string(),
            system_id: "1070fd0300176624".to_string(),
            lid: 4,
            dname: "2".to_string(),
            system_name: "MT4119 ConnectX5   Mellanox Technologies".to_string(),
            physical_state: "Link Up".to_string(),
            logical_state: "Active".to_string(),
        };
        let value = IBPort::from(expected_port);
        assert_eq!(
            value,
            IBPort {
                name: "1070fd0300176625_2".to_string(),
                guid: "1070fd0300176625".to_string(),
                lid: 4,
                state: Some(IBPortState::Active),
            }
        );

        let expected_port = Port {
            guid: "1070fd0300176374".to_string(),
            name: "1070fd0300176374_1".to_string(),
            system_id: "1070fd0300176374".to_string(),
            lid: 1,
            dname: "HCA-1/1".to_string(),
            system_name: "ufm02".to_string(),
            physical_state: "Link Up".to_string(),
            logical_state: "Active".to_string(),
        };
        let value = IBPort::from(expected_port);
        assert_eq!(
            value,
            IBPort {
                name: "1070fd0300176374_1".to_string(),
                guid: "1070fd0300176374".to_string(),
                lid: 1,
                state: Some(IBPortState::Active),
            }
        );

        // Port <-> IBPort (Logical state is invalid)
        let expected_port = Port {
            guid: "1070fd0300176625".to_string(),
            name: "1070fd0300176625_2".to_string(),
            system_id: "1070fd0300176624".to_string(),
            lid: 4,
            dname: "2".to_string(),
            system_name: "MT4119 ConnectX5   Mellanox Technologies".to_string(),
            physical_state: "Link Up".to_string(),
            logical_state: "Unknown".to_string(),
        };
        let value = IBPort::from(expected_port);
        assert_eq!(
            value,
            IBPort {
                name: "1070fd0300176625_2".to_string(),
                guid: "1070fd0300176625".to_string(),
                lid: 4,
                state: None,
            }
        );
    }

    #[test]
    fn check_find_ib_port() {
        let ports: Vec<Port> = vec![
            Port {
                guid: "1070fd0300176374".to_string(),
                name: "1070fd0300176374_1".to_string(),
                system_id: "1070fd0300176374".to_string(),
                lid: 1,
                dname: "HCA-1/1".to_string(),
                system_name: "ufm02".to_string(),
                physical_state: "Link Up".to_string(),
                logical_state: "Active".to_string(),
            },
            Port {
                guid: "1070fd0300176624".to_string(),
                name: "1070fd0300176624_1".to_string(),
                system_id: "1070fd0300176624".to_string(),
                lid: 3,
                dname: "1".to_string(),
                system_name: "MT4119 ConnectX5   Mellanox Technologies".to_string(),
                physical_state: "Link Up".to_string(),
                logical_state: "Down".to_string(),
            },
            Port {
                guid: "1070fd0300176625".to_string(),
                name: "1070fd0300176625_2".to_string(),
                system_id: "1070fd0300176624".to_string(),
                lid: 4,
                dname: "2".to_string(),
                system_name: "MT4119 ConnectX5   Mellanox Technologies".to_string(),
                physical_state: "Link Up".to_string(),
                logical_state: "".to_string(),
            },
        ];
        assert_eq!(ports.len(), 3);

        // No filter by state
        let result_ports: Result<Vec<Port>, UFMError> = Ok(ports.clone());
        let result: Result<Vec<IBPort>, IbError> = result_ports
            .map(|p| p.iter().map(IBPort::from).collect())
            .map_err(Into::into);
        assert!(result.is_ok());
        let ibports = result.unwrap();
        assert_eq!(ibports.len(), 3);

        // Filter devices in Active state
        let result_ports: Result<Vec<Port>, UFMError> = Ok(ports);
        let result: Result<Vec<IBPort>, IbError> = result_ports
            .map(|p| {
                p.iter()
                    .map(IBPort::from)
                    .filter(|v| v.state == Some(IBPortState::Active))
                    .collect()
            })
            .map_err(Into::into);
        assert!(result.is_ok());
        let ibports = result.unwrap();
        assert_eq!(ibports.len(), 1);
        assert_eq!(
            ibports[0],
            IBPort {
                name: "1070fd0300176374_1".to_string(),
                guid: "1070fd0300176374".to_string(),
                lid: 1,
                state: Some(IBPortState::Active),
            }
        );
    }
}

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
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use model::ib::{
    IBMtu, IBNetwork, IBPort, IBPortMembership, IBPortState, IBQosConf, IBRateLimit, IBServiceLevel,
};

use super::iface::{Filter, GetPartitionOptions, IBFabricRawResponse};
use super::{IBFabric, IBFabricConfig, IBFabricVersions};
use crate::errors::IbError;

pub struct MockIBFabric {
    state: Arc<Mutex<State>>,
}

struct State {
    /// Maps from pkey to subnet state
    subnets: HashMap<u16, IBNetwork>,
    /// Maps from GUID to port state
    ports: HashMap<String, IBPort>,
    /// Map from pkey to associated ports/GUIDs
    subnets_to_ports: HashMap<u16, HashSet<String>>,
    /// The next LID that will be used
    next_lid: i32,
    /// Whether unbind requests fail without changing fabric state.
    fail_unbind: bool,
}

#[async_trait]
impl IBFabric for MockIBFabric {
    /// Get fabric configuration
    async fn get_fabric_config(&self) -> Result<IBFabricConfig, IbError> {
        Ok(IBFabricConfig {
            subnet_prefix: "0xfe80000000000000".to_string(),
            m_key: "0x10".to_string(),
            sm_key: "0x20".to_string(),
            sa_key: "0x30".to_string(),
            m_key_per_port: true,
        })
    }

    /// Get all IB Networks
    async fn get_ib_networks(
        &self,
        options: GetPartitionOptions,
    ) -> Result<HashMap<u16, IBNetwork>, IbError> {
        if options.include_guids_data && options.include_qos_conf {
            return Err(IbError::internal(
                "returning qos_conf and guids_data together is not supported by UFM".to_string(),
            ));
        };
        assert!(
            options.include_qos_conf || options.include_guids_data,
            "include_qos_conf or include_guids_data must be set in order to match restrictions on the REST API"
        );

        let state = self
            .state
            .lock()
            .map_err(|_| IbError::IBFabricError("state lock".to_string()))?;

        let mut results = HashMap::new();
        for (&pkey, subnet) in &state.subnets {
            let mut subnet = subnet.clone();
            if options.include_guids_data {
                let guids = state
                    .subnets_to_ports
                    .get(&pkey)
                    .cloned()
                    .unwrap_or_default();
                subnet.associated_guids = Some(guids);
            } else {
                subnet.associated_guids = None;
            }
            results.insert(pkey, subnet);
        }

        Ok(results)
    }

    /// Get IBNetwork by ID
    async fn get_ib_network(
        &self,
        pkey: u16,
        options: GetPartitionOptions,
    ) -> Result<IBNetwork, IbError> {
        assert!(
            options.include_qos_conf,
            "include_qos_conf must be to set to match the real/rest path"
        );

        let state = self
            .state
            .lock()
            .map_err(|_| IbError::IBFabricError("state lock".to_string()))?;

        let mut ib = match state.subnets.get(&pkey) {
            None => {
                return Err(IbError::NotFoundError {
                    kind: "ufm_path",
                    id: format!("/resources/pkeys/0x{pkey:x}"),
                });
            }
            Some(ib) => ib.clone(),
        };
        if options.include_guids_data {
            let guids = state
                .subnets_to_ports
                .get(&pkey)
                .cloned()
                .unwrap_or_default();
            ib.associated_guids = Some(guids);
        } else {
            ib.associated_guids = None;
        }

        Ok(ib)
    }

    async fn bind_ib_ports(&self, mut ib: IBNetwork, ports: Vec<String>) -> Result<(), IbError> {
        println!(
            "bind_ib_ports(pkey: 0x{:x}, ports: {})",
            ib.pkey,
            ports.join(",")
        );
        ib.associated_guids = None; // Nothing can be associated by caller
        // The initial QOS config is always coming from UFM. The caller can't specify it
        ib.qos_conf = Some(IBQosConf {
            mtu: IBMtu::default(),
            service_level: IBServiceLevel::default(),
            rate_limit: IBRateLimit::default(),
        });
        ib.membership = None;

        let mut state = self
            .state
            .lock()
            .map_err(|_| IbError::IBFabricError("state lock".to_string()))?;

        for port in &ports {
            if !state.ports.contains_key(port) {
                return Err(IbError::IBFabricError(format!(
                    "Port with GUID {port} is not found"
                )));
            }
        }

        let pkey = ib.pkey;
        // Create partition on demand. This matches what UFM does
        state.subnets.entry(pkey).or_insert(ib);
        let associated_ports = state.subnets_to_ports.entry(pkey).or_default();
        for port in ports {
            associated_ports.insert(port);
        }

        Ok(())
    }

    /// Update an IB Partitions QoS configuration
    async fn update_partition_qos_conf(
        &self,
        pkey: u16,
        qos_conf: &IBQosConf,
    ) -> Result<(), IbError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| IbError::IBFabricError("state lock".to_string()))?;

        match state.subnets.get_mut(&pkey) {
            Some(ib) => {
                // Update QoS accordingly
                ib.qos_conf = Some(qos_conf.clone());
                Ok(())
            }
            None => Err(IbError::IBFabricError("ib subnet not found".to_string())),
        }
    }

    /// Find IBPort
    async fn find_ib_port(&self, filter: Option<Filter>) -> Result<Vec<IBPort>, IbError> {
        let state = self
            .state
            .lock()
            .map_err(|_| IbError::IBFabricError("state lock".to_string()))?;

        let ports = state.ports.values().cloned().collect();

        let f = filter.unwrap_or_default();
        let pkey_guids = match &f.pkey {
            Some(pkey) => {
                let associated_guids = state
                    .subnets_to_ports
                    .get(pkey)
                    .cloned()
                    .unwrap_or_default();
                Some(associated_guids)
            }
            None => None,
        };

        Ok(filter_ports(ports, pkey_guids, f.guids, f.state))
    }

    /// Delete IBPort
    async fn unbind_ib_ports(&self, pkey: u16, ids: Vec<String>) -> Result<(), IbError> {
        println!(
            "bind_ib_ports(pkey: 0x{:x}, ports: {})",
            pkey,
            ids.join(",")
        );
        let mut state = self
            .state
            .lock()
            .map_err(|_| IbError::IBFabricError("state lock".to_string()))?;

        if state.fail_unbind {
            return Err(IbError::IBFabricError(
                "simulated UFM unbind failure".to_string(),
            ));
        }

        for id in &ids {
            if !state.ports.contains_key(id) {
                return Err(IbError::IBFabricError(format!(
                    "Port with GUID {id} is not found"
                )));
            }
        }

        match state.subnets_to_ports.get_mut(&pkey) {
            Some(associated_ports) => {
                for id in &ids {
                    associated_ports.remove(id);
                }

                // If the partition is empty, then remove knowledge about it
                // This applies to all partitions except the default one
                if associated_ports.is_empty() && pkey != DEFAULT_PARTITION_KEY {
                    state.subnets.remove(&pkey);
                }
            }
            None => {
                // Nothing to do.
                // TODO: Would UFM return an error here?
            }
        }

        Ok(())
    }

    /// Returns IB fabric related versions
    async fn versions(&self) -> Result<IBFabricVersions, IbError> {
        let ufm_version = "mock_ufm_1.0".to_string();

        Ok(IBFabricVersions { ufm_version })
    }

    /// Make a raw HTTP GET request to the Fabric Manager using the given path,
    /// and return the response body.
    async fn raw_get(&self, _path: &str) -> Result<IBFabricRawResponse, IbError> {
        Err(IbError::NotImplemented)
    }
}

impl MockIBFabric {
    pub fn new() -> Self {
        let default_partition = IBNetwork {
            name: "management".to_string(),
            pkey: DEFAULT_PARTITION_KEY,
            ipoib: true,
            qos_conf: Some(IBQosConf {
                mtu: IBMtu(2),
                service_level: IBServiceLevel(0),
                rate_limit: IBRateLimit(2),
            }),
            associated_guids: None,
            membership: Some(IBPortMembership::Limited),
        };

        Self {
            state: Arc::new(std::sync::Mutex::new(State {
                subnets: HashMap::from_iter([(DEFAULT_PARTITION_KEY, default_partition)]),
                ports: HashMap::new(),
                subnets_to_ports: HashMap::new(),
                next_lid: 1,
                fail_unbind: false,
            })),
        }
    }

    /// Registers a port with a given GUID at the mocked IB Fabric
    pub fn register_port(&self, guid: String) {
        let mut state = self.state.lock().unwrap();
        if state.ports.contains_key(&guid) {
            panic!("IB port with GUID {guid} is already registered");
        }

        let lid = state.next_lid;
        state.next_lid += 1;

        state.ports.insert(
            guid.clone(),
            IBPort {
                name: guid.clone(),
                guid,
                lid,
                state: Some(IBPortState::Active),
            },
        );
    }

    /// Configures whether a port shows up as active or inactive
    pub fn set_port_state(&self, guid: &str, is_active: bool) {
        let mut state = self.state.lock().unwrap();

        let port = match state.ports.get_mut(guid) {
            Some(port) => port,
            None => panic!("IB port with GUID {guid} is not known to Mock"),
        };

        port.state = Some(if is_active {
            IBPortState::Active
        } else {
            IBPortState::Down
        });
    }

    /// Sets the membership parameter of the default partition
    pub fn set_default_partition_membership(&self, membership: IBPortMembership) {
        let mut state: std::sync::MutexGuard<'_, State> = self.state.lock().unwrap();
        let partition = state.subnets.get_mut(&DEFAULT_PARTITION_KEY).unwrap();
        partition.membership = Some(membership);
    }

    /// Configures whether unbind requests fail without changing fabric state.
    pub fn set_unbind_failure(&self, fail: bool) {
        self.state.lock().unwrap().fail_unbind = fail;
    }
}

impl Default for MockIBFabric {
    fn default() -> Self {
        Self::new()
    }
}

fn filter_ports(
    ports: Vec<IBPort>,
    pkey_guids: Option<HashSet<String>>,
    guids: Option<HashSet<String>>,
    state: Option<IBPortState>,
) -> Vec<IBPort> {
    let guid_filter = match (pkey_guids, guids) {
        // If both are None, means no filter, return all ports.
        (None, None) => None,
        // If just one is None, filter ports by the other guids set.
        (Some(pkey_guids), None) => Some(pkey_guids),
        (None, Some(guids)) => Some(guids),
        // If both are Some, filter ports by the intersection.
        (Some(pkey_guids), Some(guids)) => Some(pkey_guids.intersection(&guids).cloned().collect()),
    };

    let ports = match guid_filter {
        // If no filter, return all ports;
        None => ports,
        // otherwise, filter ports accordingly.
        Some(filter) => ports
            .into_iter()
            .filter(|p: &IBPort| filter.contains(&p.guid))
            .collect(),
    };

    match state {
        None => ports,
        Some(state) => ports
            .into_iter()
            .filter(|v| v.state.as_ref() == Some(&state))
            .collect(),
    }
}

const DEFAULT_PARTITION_KEY: u16 = 0x7fff;

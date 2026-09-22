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
use std::fmt;

use base64::prelude::*;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use url::Url;

use self::rest::{RestClient, RestClientConfig, RestError, RestScheme};
use crate::ib::ufmclient::rest::ResponseDetails;

mod rest;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub(super) struct SmConfig {
    /// The subnet_prefix of openSM
    pub(super) subnet_prefix: String,
    /// The m_key of openSM
    pub(super) m_key: String,
    /// The sm_key of openSM
    pub(super) sm_key: String,
    /// The sa_key of openSM
    pub(super) sa_key: String,
    /// The m_key_per_port of openSM
    pub(super) m_key_per_port: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub(super) struct PartitionQoS {
    // Default 2k; one of 2k or 4k; the MTU of the services.
    pub(super) mtu_limit: u16,
    // Default is None, value can be range from 0-15
    pub(super) service_level: u8,
    /// Supported values: 10, 30, 5, 20, 40, 60, 80, 120, 14, 56, 112, 168, 25, 100, 200, or 300.
    /// 2 is also valid but is used internally to represent rate limit 2.5 that is possible in UFM for lagecy hardware.
    /// It is done to avoid floating point data type usage for rate limit w/o obvious benefits.
    /// 2 to 2.5 and back conversion is done just on REST API operations.
    pub(super) rate_limit: f32,
}

#[derive(Serialize, Deserialize, Debug, Copy, Clone, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub(super) enum PortMembership {
    Limited,
    Full,
}

#[derive(Serialize, Deserialize, Debug)]
pub(super) struct PortConfig {
    /// The GUID of Port.
    pub(super) guid: String,
    /// Default false; store the PKey at index 0 of the PKey table of the GUID.
    pub(super) index0: bool,
    /// Default is full:
    ///   "full"    - members with full membership can communicate with all hosts (members) within the network/partition
    ///   "limited" - members with limited membership cannot communicate with other members with limited membership.
    ///               However, communication is allowed between every other combination of membership types.
    pub(super) membership: PortMembership,
}

#[derive(Serialize, Deserialize, Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub(super) struct PartitionKey(u16);

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub(super) struct Partition {
    /// The name of Partition.
    pub(super) name: String,
    /// The pkey of Partition.
    pub(super) pkey: PartitionKey,
    /// Default false
    pub(super) ipoib: bool,
    /// The QoS of Partition.
    pub(super) qos: Option<PartitionQoS>,
    /// GUIDS attached to the Partition. Only available if explictly queried for
    pub(super) guids: Option<HashSet<String>>,
    /// The default membership status of ports on this partition
    /// The value is only available if all of these things are true:
    /// - The partition is the default partition
    /// - associated ports/guid are queried
    /// - UFM version is 6.19 or newer
    pub(super) membership: Option<PortMembership>,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone, Default)]
pub(super) struct Port {
    pub(super) guid: String,
    pub(super) name: String,
    #[serde(rename = "systemID")]
    pub(super) system_id: String,
    pub(super) lid: i32,
    pub(super) dname: String,
    pub(super) system_name: String,
    pub(super) physical_state: String,
    pub(super) logical_state: String,
}

#[derive(Default)]
pub(super) struct Filter {
    pub(super) guids: Option<HashSet<String>>,
    pub(super) pkey: Option<PartitionKey>,
    pub(super) logical_state: Option<String>,
}

#[derive(Default, Debug, Copy, Clone)]
pub(super) struct GetPartitionOptions {
    /// Whether to include `guids` associated with each partition in the response
    pub(super) include_guids_data: bool,
    /// Whether the response should contain the `qos_conf` and `ip_over_ib` parameters
    pub(super) include_qos_conf: bool,
}

/// Partition data with extra options as presented by UFM
#[derive(Serialize, Deserialize, Debug)]
struct PartitionData {
    partition: String,
    ip_over_ib: bool,
    /// Quality of Service related data. Only available if `qos_conf==true`
    qos_conf: Option<PartitionQoS>,
    /// Ports attached to a partition. Only available if `guids_data==true`
    #[serde(default)]
    guids: Vec<PortConfig>,
    /// The default membership status of ports on this partition
    /// The value is only available if all of these things are true:
    /// - The partition is the default partition
    /// - associated ports/guid are queried with `guids_data==true`
    /// - UFM version is 6.19 or newer
    membership: Option<PortMembership>,
}

const HEX_PRE: &str = "0x";

impl TryFrom<u16> for PartitionKey {
    type Error = UFMError;

    fn try_from(pkey: u16) -> Result<Self, Self::Error> {
        if pkey != (pkey & 0x7fff) {
            return Err(UFMError::InvalidPKey(pkey.to_string()));
        }

        Ok(PartitionKey(pkey))
    }
}

impl TryFrom<String> for PartitionKey {
    type Error = UFMError;

    fn try_from(pkey: String) -> Result<Self, Self::Error> {
        let pkey = pkey.to_lowercase();
        let base = if pkey.starts_with(HEX_PRE) { 16 } else { 10 };
        let p = pkey.trim_start_matches(HEX_PRE);
        let k = u16::from_str_radix(p, base);

        match k {
            Ok(v) => Ok(PartitionKey(v)),
            Err(_e) => Err(UFMError::InvalidPKey(pkey.to_string())),
        }
    }
}

impl TryFrom<&String> for PartitionKey {
    type Error = UFMError;

    fn try_from(pkey: &String) -> Result<Self, Self::Error> {
        PartitionKey::try_from(pkey.to_string())
    }
}

impl TryFrom<&str> for PartitionKey {
    type Error = UFMError;

    fn try_from(pkey: &str) -> Result<Self, Self::Error> {
        PartitionKey::try_from(pkey.to_string())
    }
}

impl fmt::Display for PartitionKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> Result<(), fmt::Error> {
        write!(f, "{HEX_PRE}{:x}", self.0)
    }
}

impl From<PartitionKey> for u16 {
    fn from(v: PartitionKey) -> u16 {
        v.0
    }
}

pub(super) struct Ufm {
    client: RestClient,
}

#[derive(Error, Debug)]
pub(super) enum UFMError {
    #[error("invalid argument: {0}")]
    InvalidArgument(String),
    #[error("invalid pkey '{0}'")]
    InvalidPKey(String),
    #[error("invalid configuration: '{0}'")]
    InvalidConfig(String),
    #[error("response body can not be deserialized: {body}")]
    MalformedResponse {
        status_code: u16,
        body: String,
        headers: Box<http::HeaderMap>,
    },
    #[error("failed to execute HTTP request: {0}")]
    HttpConnectionError(String),
    #[error("HTTP error code {status_code}")]
    HttpError {
        status_code: u16,
        body: String,
        headers: Box<http::HeaderMap>,
    },
    /// This error type is just needed because UFM in some cases does not return a 404 status
    /// code but a 200 status code with a body containing {}
    #[error(
        "resource at path {path} was not found. UFM returned: '{body}'. status code: {status_code}"
    )]
    NotFound {
        path: String,
        status_code: u16,
        body: String,
        headers: Box<http::HeaderMap>,
    },
}

impl From<RestError> for UFMError {
    fn from(e: RestError) -> Self {
        match e {
            RestError::InvalidConfig(msg) => UFMError::InvalidConfig(msg),
            RestError::HttpConnectionError(msg) => UFMError::HttpConnectionError(msg),
            RestError::HttpError {
                status_code,
                body,
                headers,
            } => UFMError::HttpError {
                status_code,
                body,
                headers,
            },
            RestError::MalformedResponse {
                status_code,
                body,
                headers,
            } => UFMError::MalformedResponse {
                status_code,
                body,
                headers,
            },
            RestError::NotFound {
                path,
                status_code,
                body,
                headers,
            } => UFMError::NotFound {
                path,
                status_code,
                body,
                headers,
            },
        }
    }
}

#[derive(Clone, Debug)]
pub(super) struct UFMCert {
    pub(super) ca_crt: String,
    pub(super) tls_key: String,
    pub(super) tls_crt: String,
}

pub(super) struct UFMConfig {
    pub(super) address: String,
    pub(super) username: Option<String>,
    pub(super) password: Option<String>,
    pub(super) token: Option<String>,
    pub(super) cert: Option<UFMCert>,
}

pub(super) fn new_client(conf: UFMConfig) -> Result<Ufm, UFMError> {
    let addr = Url::parse(&conf.address)
        .map_err(|_| UFMError::InvalidConfig(format!("invalid UFM url: {}", conf.address)))?;
    let address = addr.host_str().ok_or(UFMError::InvalidConfig(format!(
        "invalid UFM host; url: {addr}"
    )))?;

    let (base_path, auth_info) = match &conf.token {
        None if conf.cert.is_some() => {
            let auth_cert = conf.cert.unwrap();
            (
                "/ufmRest".to_string(),
                format!(
                    "{}\n{}\n{}",
                    auth_cert.ca_crt, auth_cert.tls_key, auth_cert.tls_crt
                ),
            )
        }
        None => {
            let password = conf
                .password
                .clone()
                .ok_or(UFMError::InvalidConfig("password is empty".to_string()))?;
            let username = conf
                .username
                .clone()
                .ok_or(UFMError::InvalidConfig("username is empty".to_string()))?;

            (
                "/ufmRest".to_string(),
                BASE64_STANDARD.encode(format!("{username}:{password}")),
            )
        }
        Some(t) => ("/ufmRestV3".to_string(), t.to_string()),
    };

    let c = RestClient::new(&RestClientConfig {
        address: address.to_string(),
        port: addr.port(),
        auth_info,
        base_path,
        scheme: RestScheme::from(addr.scheme().to_string()),
    })?;

    Ok(Ufm { client: c })
}

impl Ufm {
    pub(super) async fn get_sm_config(&self) -> Result<SmConfig, UFMError> {
        let path = String::from("/app/smconf");
        let sm_config: SmConfig = self.client.get(&path).await?.0;

        Ok(sm_config)
    }

    pub(super) async fn update_partition_qos(
        &self,
        pkey: PartitionKey,
        qos: PartitionQoS,
    ) -> Result<(), UFMError> {
        let path = String::from("/resources/pkeys/qos_conf");

        #[derive(Serialize, Deserialize, Debug)]
        struct PkeyQoS {
            pkey: String,
            mtu_limit: u16,
            service_level: u8,
            rate_limit: f32,
        }

        let data = serde_json::to_string(&PkeyQoS {
            pkey: pkey.to_string(),
            mtu_limit: qos.mtu_limit,
            rate_limit: qos.rate_limit,
            service_level: qos.service_level,
        })
        .map_err(|_| UFMError::InvalidConfig("invalid partition qos".to_string()))?;

        self.client.put(&path, data).await?;

        Ok(())
    }

    pub(super) async fn bind_ports(
        &self,
        p: Partition,
        ports: Vec<PortConfig>,
    ) -> Result<(), UFMError> {
        let path = String::from("/resources/pkeys");

        let mut membership = PortMembership::Full;
        let mut index0 = true;

        let mut guids = Vec::with_capacity(ports.len());
        for pb in ports {
            membership = pb.membership;
            index0 = pb.index0;
            guids.push(pb.guid.to_string());
        }

        #[derive(Serialize, Deserialize, Debug)]
        struct Pkey {
            pkey: String,
            ip_over_ib: bool,
            membership: PortMembership,
            index0: bool,
            guids: Vec<String>,
        }

        let pkey = Pkey {
            pkey: p.pkey.to_string(),
            ip_over_ib: p.ipoib,
            membership,
            index0,
            guids,
        };

        let data = serde_json::to_string(&pkey)
            .map_err(|_| UFMError::InvalidConfig("invalid partition".to_string()))?;

        self.client.post(&path, data).await?;

        Ok(())
    }

    pub(super) async fn unbind_ports(
        &self,
        pkey: PartitionKey,
        guids: Vec<String>,
    ) -> Result<(), UFMError> {
        let path = String::from("/actions/remove_guids_from_pkey");

        #[derive(Serialize, Deserialize, Debug)]
        struct Pkey {
            pkey: String,
            guids: Vec<String>,
        }

        let pkey = Pkey {
            pkey: pkey.to_string(),
            guids,
        };

        let data = serde_json::to_string(&pkey)
            .map_err(|_| UFMError::InvalidConfig("invalid partition".to_string()))?;

        self.client.post(&path, data).await?;

        Ok(())
    }

    pub(super) async fn list_partitions(
        &self,
        options: GetPartitionOptions,
    ) -> Result<HashMap<PartitionKey, Partition>, UFMError> {
        let path = match (options.include_guids_data, options.include_qos_conf) {
            (true, true) => {
                // This API is not supported in current UFM version: https://github.com/NVIDIA/infra-controller/issues/5558
                // Instead of returning unexpected results, don't even try to talk to UFM
                // and make developers aware of the issue.
                // That at least allows the application developer to implement a workaround
                return Err(UFMError::InvalidArgument(
                    "returning qos_conf and guids_data together is not supported by UFM"
                        .to_string(),
                ));
            }
            (true, false) => "/resources/pkeys?guids_data=true",
            (false, true) => "/resources/pkeys?qos_conf=true",
            (false, false) => "/resources/pkeys?qos_conf=true", // Without any query argument, UFM return structure is different
        };

        let partitions: HashMap<String, PartitionData> = self.client.get(path).await?.0;

        let mut results = HashMap::with_capacity(partitions.len());
        for (pkey, partition) in partitions.into_iter() {
            let pkey = PartitionKey::try_from(pkey)?;
            let partition = Partition {
                name: partition.partition,
                pkey,
                ipoib: partition.ip_over_ib,
                qos: partition.qos_conf,
                guids: match options.include_guids_data {
                    true => Some(partition.guids.into_iter().map(|p| p.guid).collect()),
                    false => None,
                },
                membership: partition.membership,
            };
            results.insert(pkey, partition);
        }

        Ok(results)
    }

    pub(super) async fn get_partition(
        &self,
        pkey: PartitionKey,
        options: GetPartitionOptions,
    ) -> Result<Partition, UFMError> {
        let mut path = format!("/resources/pkeys/{pkey}");
        let mut has_query_args = false;
        if options.include_guids_data {
            path.push(if has_query_args { '&' } else { '?' });
            has_query_args = true;
            path += "guids_data=true";
        }
        if options.include_qos_conf {
            path.push(if has_query_args { '&' } else { '?' });
            path += "qos_conf=true";
        }

        let partition: PartitionData = self.client.get(&path).await?.0;

        Ok(Partition {
            name: partition.partition,
            pkey,
            ipoib: partition.ip_over_ib,
            qos: partition.qos_conf,
            guids: match options.include_guids_data {
                true => Some(partition.guids.into_iter().map(|p| p.guid).collect()),
                false => None,
            },
            membership: partition.membership,
        })
    }

    pub(super) async fn list_port(&self, filter: Option<Filter>) -> Result<Vec<Port>, UFMError> {
        let path = String::from("/resources/ports?sys_type=Computer");
        let ports: Vec<Port> = self.client.list(&path).await?.0;

        let f = filter.unwrap_or_default();
        let pkey_guids = match f.pkey {
            Some(pkey) => Some(
                self.get_partition(
                    pkey,
                    GetPartitionOptions {
                        include_guids_data: true,
                        include_qos_conf: false,
                    },
                )
                .await?
                .guids,
            ),
            None => None,
        }
        .flatten();

        Ok(Self::filter_ports(
            ports,
            pkey_guids,
            f.guids,
            f.logical_state,
        ))
    }

    fn filter_ports(
        ports: Vec<Port>,
        pkey_guids: Option<HashSet<String>>,
        guids: Option<HashSet<String>>,
        logical_state: Option<String>,
    ) -> Vec<Port> {
        let guid_filter = match (pkey_guids, guids) {
            // If both are None, means no filter, return all ports.
            (None, None) => None,
            // If just one is None, filter ports by the other guids set.
            (Some(pkey_guids), None) => Some(pkey_guids),
            (None, Some(guids)) => Some(guids),
            // If both are Some, filter ports by the intersection.
            (Some(pkey_guids), Some(guids)) => {
                Some(pkey_guids.intersection(&guids).cloned().collect())
            }
        };

        let ports = match guid_filter {
            // If no filter, return all ports;
            None => ports,
            // otherwise, filter ports accordingly.
            Some(filter) => ports
                .into_iter()
                .filter(|p: &Port| filter.contains(&p.guid))
                .collect(),
        };

        match logical_state {
            None => ports,
            Some(logical_state) => {
                let logical_state = logical_state.to_lowercase();
                let logical_state = logical_state.as_str().trim();
                ports
                    .into_iter()
                    .filter(|p| p.logical_state.to_lowercase().as_str().trim() == logical_state)
                    .collect()
            }
        }
    }

    pub(super) async fn version(&self) -> Result<String, UFMError> {
        #[derive(Serialize, Deserialize, Debug)]
        struct Version {
            ufm_release_version: String,
        }

        let path = String::from("/app/ufm_version");
        let v: Version = self.client.get(&path).await?.0;

        Ok(v.ufm_release_version)
    }

    pub(super) async fn raw_get(&self, path: &str) -> Result<(String, ResponseDetails), UFMError> {
        let (body, details) = self.client.get_raw(path).await?;

        Ok((body, details))
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;

    use super::*;

    #[derive(Clone, Copy, Debug)]
    enum FilterCase {
        NoFilter,
        PkeyGuids,
        Guids,
        GuidAndPkeyIntersection,
        GuidAndPkeyIntersectionSecondPort,
        PortState,
        PortStateIsCaseInsensitive,
        GuidAndMissingPortState,
    }

    fn port(guid: &str, logical_state: &str) -> Port {
        Port {
            guid: guid.to_string(),
            logical_state: logical_state.to_string(),
            ..Port::default()
        }
    }

    fn guid_set(guids: &[&str]) -> Option<HashSet<String>> {
        Some(guids.iter().map(|guid| (*guid).to_string()).collect())
    }

    fn filter_port_guids(case: FilterCase) -> Vec<String> {
        let ports = vec![
            port("p1", "Active"),
            port("p2", "Down"),
            port("p3", "Initialize"),
        ];
        let (pkey_guids, guids, port_state) = match case {
            FilterCase::NoFilter => (None, None, None),
            FilterCase::PkeyGuids => (guid_set(&["p1"]), None, None),
            FilterCase::Guids => (None, guid_set(&["p1"]), None),
            FilterCase::GuidAndPkeyIntersection => {
                (guid_set(&["p1", "p2"]), guid_set(&["p1"]), None)
            }
            FilterCase::GuidAndPkeyIntersectionSecondPort => {
                (guid_set(&["p1", "p2"]), guid_set(&["p2", "p3"]), None)
            }
            FilterCase::PortState => (None, None, Some("Active".to_string())),
            FilterCase::PortStateIsCaseInsensitive => (None, None, Some(" active ".to_string())),
            FilterCase::GuidAndMissingPortState => {
                (None, guid_set(&["p1"]), Some("Disabled".to_string()))
            }
        };

        Ufm::filter_ports(ports, pkey_guids, guids, port_state)
            .into_iter()
            .map(|port| port.guid)
            .collect()
    }

    #[test]
    fn filters_ports() {
        value_scenarios!(filter_port_guids:
            "unfiltered" {
                FilterCase::NoFilter => vec!["p1".to_string(), "p2".to_string(), "p3".to_string()],
            }

            "guid filters" {
                FilterCase::PkeyGuids => vec!["p1".to_string()],
                FilterCase::Guids => vec!["p1".to_string()],
                FilterCase::GuidAndPkeyIntersection => vec!["p1".to_string()],
                FilterCase::GuidAndPkeyIntersectionSecondPort => vec!["p2".to_string()],
            }

            "state filters" {
                FilterCase::PortState => vec!["p1".to_string()],
                FilterCase::PortStateIsCaseInsensitive => vec!["p1".to_string()],
                FilterCase::GuidAndMissingPortState => Vec::<String>::new(),
            }
        );
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_partition_key() {
        assert_eq!("0x67", PartitionKey(103).to_string());
        assert_eq!("0x67", PartitionKey::try_from("103").unwrap().to_string());
    }

    #[test]
    fn test_deserialize_partition_data() {
        let single_part_data = r#"
            {
                "ip_over_ib": true,
                "partition": "api_pkey_0x31a"
            }"#;

        let partition: PartitionData = serde_json::from_str(single_part_data).unwrap();
        assert_eq!(partition.partition, "api_pkey_0x31a");
        assert!(partition.qos_conf.is_none());
        assert!(partition.guids.is_empty());

        let single_part_data_with_guids_and_qos = r#"
            {
                "guids": [
                    {
                        "guid": "946dae03005985c8",
                        "index0": true,
                        "membership": "full"
                    },
                    {
                        "guid": "946dae03005985d0",
                        "index0": true,
                        "membership": "full"
                    },
                    {
                        "guid": "946dae03005985cc",
                        "index0": true,
                        "membership": "full"
                    },
                    {
                        "guid": "946dae03005985c4",
                        "index0": true,
                        "membership": "full"
                    }
                ],
                "ip_over_ib": true,
                "partition": "api_pkey_0x31a",
                "qos_conf": {
                    "mtu_limit": 4,
                    "rate_limit": 200,
                    "service_level": 0
                }
            }"#;

        let partition: PartitionData =
            serde_json::from_str(single_part_data_with_guids_and_qos).unwrap();
        assert_eq!(partition.partition, "api_pkey_0x31a");
        assert_eq!(
            partition.qos_conf,
            Some(PartitionQoS {
                mtu_limit: 4,
                rate_limit: 200.0,
                service_level: 0,
            })
        );
        assert_eq!(partition.guids.len(), 4);

        let data_with_qos = r#"
            {
                "0x2fb": {
                    "ip_over_ib": true,
                    "partition": "api_pkey_0x2fb",
                    "qos_conf": {
                        "mtu_limit": 4,
                        "rate_limit": 200,
                        "service_level": 0
                    }
                },
                "0x7fff": {
                    "ip_over_ib": true,
                    "partition": "management",
                    "qos_conf": {
                        "mtu_limit": 2,
                        "rate_limit": 2.5,
                        "service_level": 0
                    }
                }
            }"#;

        let parts: HashMap<String, PartitionData> = serde_json::from_str(data_with_qos).unwrap();
        let p1 = parts.get("0x2fb").unwrap();
        assert_eq!(
            p1.qos_conf,
            Some(PartitionQoS {
                mtu_limit: 4,
                rate_limit: 200.0,
                service_level: 0,
            })
        );
        assert_eq!(p1.partition, "api_pkey_0x2fb");

        let data_with_guids = r#"
            {
                "0x2fb": {
                    "guids": [
                        {
                            "guid": "946dae03005975c8",
                            "index0": true,
                            "membership": "full"
                        },
                        {
                            "guid": "946dae03005975d0",
                            "index0": true,
                            "membership": "full"
                        },
                        {
                            "guid": "946dae03005975cc",
                            "index0": true,
                            "membership": "full"
                        },
                        {
                            "guid": "946dae03005975c4",
                            "index0": true,
                            "membership": "full"
                        }
                    ],
                    "ip_over_ib": true,
                    "partition": "api_pkey_0x2fb"
                },
                "0x7fff": {
                    "guids": [],
                    "index0": false,
                    "ip_over_ib": true,
                    "membership": "limited",
                    "partition": "management"
                }
            }"#;

        let parts: HashMap<String, PartitionData> = serde_json::from_str(data_with_guids).unwrap();
        let p1 = parts.get("0x2fb").unwrap();
        assert!(p1.qos_conf.is_none());
        assert_eq!(p1.partition, "api_pkey_0x2fb");
        let p2 = parts.get("0x7fff").unwrap();
        assert_eq!(p2.membership.unwrap(), PortMembership::Limited);
    }
}

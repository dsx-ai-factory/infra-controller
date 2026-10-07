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

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;
use std::str::FromStr;
use std::sync::{Arc, RwLock, RwLockReadGuard};

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

use crate::config::FabricConfig;
use crate::inventory::{
    EpochId, Generation, Guid, InfinibandPortState, InventoryId, InventoryLease, MachineId, MatId,
};

pub(super) const DEFAULT_PARTITION_KEY: PartitionKey = PartitionKey(0x7fff);

#[derive(Clone, Debug)]
pub(crate) struct Fabric {
    pub(super) config: Arc<FabricConfig>,
    pub(super) inner: Arc<RwLock<FabricState>>,
}

#[derive(Debug)]
pub(super) struct FabricState {
    pub(super) candidates: HashMap<Guid, BTreeMap<InventoryId, PortCandidate>>,
    pub(super) ports: HashMap<Guid, PortRecord>,
    pub(super) sources: HashMap<InventoryId, SourceState>,
    pub(super) partitions: HashMap<PartitionKey, Partition>,
    pub(super) next_lid: i32,
}

#[derive(Clone, Debug)]
pub(super) struct PortCandidate {
    pub(super) mat_id: MatId,
    pub(super) machine_id: Option<MachineId>,
    pub(super) state: InfinibandPortState,
}

#[derive(Clone, Debug)]
pub(super) struct PortRecord {
    pub(super) owner: InventoryId,
    pub(super) candidate: PortCandidate,
    pub(super) lid: i32,
}

#[derive(Debug)]
pub(super) struct SourceState {
    pub(super) epoch_id: EpochId,
    pub(super) generation: Generation,
    pub(super) guids: BTreeSet<Guid>,
    pub(super) failed: bool,
    pub(super) lease: InventoryLease,
}

#[derive(Clone, Debug)]
pub(super) struct Partition {
    pub(super) name: PartitionName,
    pub(super) ip_over_ib: bool,
    pub(super) qos: PartitionQos,
    pub(super) members: HashMap<Guid, PortConfig>,
}

#[derive(Clone, Debug)]
pub(super) struct PartitionName(String);

impl AsRef<str> for PartitionName {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl From<String> for PartitionName {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for PartitionName {
    fn from(value: &str) -> Self {
        Self(value.to_string())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub(crate) struct PartitionQos {
    pub mtu_limit: u16,
    pub service_level: u8,
    pub rate_limit: f32,
}

impl Default for PartitionQos {
    fn default() -> Self {
        Self {
            mtu_limit: 2,
            service_level: 0,
            rate_limit: 2.5,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum PortMembership {
    Limited,
    Full,
}

#[derive(Clone, Debug)]
pub(super) struct PortConfig {
    pub(super) membership: PortMembership,
    pub(super) index0: bool,
}

#[derive(Debug, Serialize)]
pub(crate) struct PortData<'a> {
    pub(super) guid: &'a Guid,
    pub(super) name: PortName<'a>,
    #[serde(rename = "systemID")]
    pub(super) system_id: &'a Guid,
    pub(super) lid: &'a i32,
    pub(super) dname: &'a str,
    pub(super) system_name: &'a str,
    pub(super) physical_state: &'static str,
    pub(super) logical_state: &'static str,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct PortName<'a>(pub(super) &'a Guid);

impl fmt::Display for PortName<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}_1", self.0)
    }
}

impl Serialize for PortName<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_str(self)
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct PartitionKey(u16);

impl fmt::Display for PartitionKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "0x{:x}", self.0)
    }
}

impl FromStr for PartitionKey {
    type Err = InvalidPartitionKey;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let parsed = if let Some(value) = value.strip_prefix("0x") {
            u16::from_str_radix(value, 16)
        } else {
            value.parse()
        }
        .map_err(|_| InvalidPartitionKey(value.to_string()))?;
        if parsed > DEFAULT_PARTITION_KEY.0 {
            return Err(InvalidPartitionKey(value.to_string()));
        }
        Ok(Self(parsed))
    }
}

impl Serialize for PartitionKey {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for PartitionKey {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
#[error("invalid partition key {0}")]
pub(crate) struct InvalidPartitionKey(String);

#[derive(Debug, Serialize)]
pub(super) struct PortConfigData<'a> {
    pub(super) guid: &'a Guid,
    pub(super) membership: &'a PortMembership,
    pub(super) index0: &'a bool,
}

#[derive(Debug, Serialize)]
pub(crate) struct PartitionData<'a> {
    pub(super) partition: &'a str,
    pub(super) ip_over_ib: &'a bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) qos_conf: Option<&'a PartitionQos>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) guids: Option<Vec<PortConfigData<'a>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) membership: Option<&'static PortMembership>,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct BindRequest {
    pub pkey: PartitionKey,
    pub ip_over_ib: bool,
    pub membership: PortMembership,
    pub index0: bool,
    pub guids: Vec<Guid>,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct UnbindRequest {
    pub pkey: PartitionKey,
    pub guids: Vec<Guid>,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct QosRequest {
    pub pkey: PartitionKey,
    pub mtu_limit: u16,
    pub service_level: u8,
    pub rate_limit: f32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ReconcileOutcome {
    Applied,
    Duplicate,
    Stale,
}

impl ReconcileOutcome {
    pub(crate) fn metric_label(self) -> &'static str {
        match self {
            Self::Applied => "applied",
            Self::Duplicate => "duplicate",
            Self::Stale => "stale",
        }
    }
}

#[derive(Debug, Error)]
pub(crate) enum FabricError {
    #[error("partition {0} does not exist")]
    PartitionNotFound(PartitionKey),
    #[error("InfiniBand port {0} does not exist")]
    PortNotFound(Guid),
    #[error("inventory_id and epoch_id must not be empty")]
    InvalidInventoryIdentity,
}

pub(crate) struct FabricRead<'a> {
    pub(super) state: RwLockReadGuard<'a, FabricState>,
}

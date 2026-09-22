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

//! Describes the Forge site controller internal data model
//!
//! The model described here is used in both internal decision logic and might
//! be stored in database fields.
//! Data inside this module therefore needs to be backward compatible with previous
//! versions of Forge that are deployed.
//!
//! The module should only contain data definitions and associated helper functions,
//! but no actual business logic.

use std::fmt;
use std::ops::{Deref, DerefMut};

use carbide_uuid::network::NetworkSegmentId;
use instance::config::network::InterfaceFunctionId;
use mac_address::MacAddress;
use serde::{Deserialize, Serialize};

pub mod address_selection_strategy;
pub mod allocation_type;
pub mod attestation;
pub mod bmc_info;
pub mod bmc_redfish_session;
pub mod bmc_suppression;
pub mod component_manager;
pub mod compute_allocation;
pub mod controller_outcome;
pub mod dhcp_record;
pub mod dns;
pub mod dpa_interface;
pub mod dpu_machine_update;
pub mod dpu_remediation;
pub mod errors;
pub mod expected_entity;
pub mod expected_machine;
pub mod expected_power_shelf;
pub mod expected_rack;
pub mod expected_switch;
pub mod extension_service;
pub mod firmware;
pub mod hardware_info;
pub mod health;
pub mod host_machine_update;
pub mod ib;
pub mod ib_partition;
pub mod instance;
pub mod instance_address;
pub mod instance_type;
pub mod machine;
pub mod machine_boot_interface;
pub mod machine_boot_override;
pub mod machine_interface;
pub mod machine_interface_address;
pub mod machine_pending_action;
pub mod machine_update_module;
pub mod machine_validation;
pub mod metadata;
pub mod network_devices;
pub mod network_prefix;
pub mod network_security_group;
pub mod network_segment;
pub mod nmxc;
pub mod nvl_logical_partition;
pub mod nvl_partition;
pub mod operating_system_definition;
pub mod os;
pub mod pci;
pub mod power_manager;
pub mod power_shelf;
pub mod predicted_machine_interface;
pub mod rack;
pub mod rack_type;
pub mod redfish;
pub mod resource_pool;
pub mod route_server;
pub mod secrets;
pub mod site_explorer;
pub mod site_prefix;
pub mod sku;
pub mod spx_partition;
pub mod state_history;
pub mod storage;
pub mod switch;
pub mod tenant;
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
pub mod trim_table;
pub mod vpc;
pub mod vpc_prefix;

// Lets the database round-trip tests use `#[crate::sqlx_test]` to get a per-test
// Postgres pool from the shared harness (DATABASE_URL via .envrc).
#[cfg(test)]
pub(crate) use carbide_macros::sqlx_test;

/// Error that is returned when we validate various configurations that are obtained
/// from Forge users.
#[derive(Debug, thiserror::Error, Clone)]
pub enum ConfigValidationError {
    /// A configuration value is invalid
    #[error("invalid value: {0}")]
    InvalidValue(String),

    #[error(
        "initial VPC `{name}` cannot define `routing_profile_overrides`; inline routing-profile \
         overrides are only supported by VPC creation requests"
    )]
    InitialVpcRoutingProfileOverridesUnsupported { name: String },

    #[error("found unknown segments")]
    UnknownSegments,

    #[error("segment is still not updated for {0:?}")]
    MissingSegment(InterfaceFunctionId),

    #[error("no vpc is attached to segment {0}")]
    VpcNotAttachedToSegment(NetworkSegmentId),

    #[error("found segments attached to multiple VPCs")]
    MultipleVpcFound,

    #[error("IP addresses / IP networks not configured for the same prefixes")]
    NetworkPrefixAllocationMismatch,

    #[error("segment {0} is not yet ready. current state: {1}")]
    NetworkSegmentNotReady(NetworkSegmentId, String),

    #[error("segment {0} is requested to be deleted")]
    NetworkSegmentToBeDeleted(NetworkSegmentId),

    #[error("configuration value cannot be modified: {0}")]
    ConfigCanNotBeModified(String),

    #[error("duplicate tenant KeySet ID found: {0}")]
    DuplicateTenantKeysetId(String),

    #[error("more than {0} tenant KeySet IDs are not allowed")]
    TenantKeysetIdsOverMax(usize),

    #[error("storage volumes defined {0} > 8")]
    StorageVolumeCountExceeded(usize),

    #[error("instance cannot connect to multiple storage clusters")]
    StorageClusterInvalid,

    #[error("specified network is not available on the requested host")]
    NetworkSegmentUnavailableOnHost,

    #[error("another instance network config update is already in progress")]
    InstanceNetworkConfigUpdateAlreadyInProgress,

    #[error("instance deletion request is already received")]
    InstanceDeletionIsRequested,

    #[error(
        "instance release is blocked: aggregate health includes a PreventInstanceDeletion alert. remove the alert or the health override that carries it, then retry"
    )]
    InstanceReleaseBlockedByPreventInstanceDeletion,

    #[error("instance is not ready yet. can't apply the config")]
    InvalidState,
}

impl ConfigValidationError {
    /// Creates a [ConfigValidationError::InvalidValue] variant
    pub fn invalid_value<T: Into<String>>(value: T) -> Self {
        Self::InvalidValue(value.into())
    }
}

// Error that is returned when we validate various status that are obtained
/// from Forge system components
#[derive(Debug, thiserror::Error, Clone)]
pub enum StatusValidationError {
    /// A configuration value is invalid
    #[error("invalid value: {0}")]
    InvalidValue(String),
}

/// Filter for controlling whether deleted resources are included in search results.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum DeletedFilter {
    /// Exclude deleted resources (default)
    #[default]
    Exclude,
    /// Return only deleted resources
    Only,
    /// Include both deleted and non-deleted resources
    Include,
}

impl From<i32> for DeletedFilter {
    fn from(value: i32) -> Self {
        match value {
            1 => DeletedFilter::Only,
            2 => DeletedFilter::Include,
            _ => DeletedFilter::Exclude,
        }
    }
}

/// A transparent wrapper around [`MacAddress`] that enables serde serialization
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct SerializableMacAddress(MacAddress);

impl Deref for SerializableMacAddress {
    type Target = MacAddress;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for SerializableMacAddress {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl From<MacAddress> for SerializableMacAddress {
    fn from(mac: MacAddress) -> Self {
        SerializableMacAddress(mac)
    }
}

impl From<SerializableMacAddress> for MacAddress {
    fn from(mac: SerializableMacAddress) -> Self {
        mac.0
    }
}

#[cfg(test)]
impl SerializableMacAddress {
    /// Converts the wrapper into a plain `MacAddress`
    pub fn into_inner(self) -> MacAddress {
        self.0
    }
}

impl Serialize for SerializableMacAddress {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.0.to_string())
    }
}

impl<'de> Deserialize<'de> for SerializableMacAddress {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::Error;

        let str_value = String::deserialize(deserializer)?;
        let mac: MacAddress = str_value
            .parse()
            .map_err(|_| Error::custom(format!("Invalid MAC address: {str_value}")))?;
        Ok(SerializableMacAddress(mac))
    }
}

/// Specifies the SLA for a resource in a specific state
#[derive(Default, Debug, Clone)]
pub struct StateSla {
    /// The SLA for the current state
    /// This field will be absent if there is no SLA defined for a field
    /// A value of 0 (instead of absent) will indicate the `time_in_state` will always
    /// be above SLA. This can happen for certain states that should never be entered
    /// for correct operation.
    pub sla: ::core::option::Option<std::time::Duration>,
    /// Whether the object has been in the state for a longer time than permitted
    /// by the SLA.
    pub time_in_state_above_sla: bool,
}

impl StateSla {
    /// Creates a `StateSla` object which indicates that no SLA applies for the state
    pub fn no_sla() -> Self {
        Self {
            sla: None,
            time_in_state_above_sla: false,
        }
    }

    /// Creates a new StateSla object with the SLA that immediately evaluates
    /// if a certain time is above the SLA
    pub fn with_sla(sla: std::time::Duration, time_in_state: std::time::Duration) -> Self {
        Self {
            time_in_state_above_sla: time_in_state > sla,
            sla: Some(sla),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialize_mac_address() {
        let mac = MacAddress::new([1, 2, 3, 4, 5, 6]);
        let serialized = serde_json::to_string(&SerializableMacAddress::from(mac)).unwrap();
        assert_eq!(serialized, "\"01:02:03:04:05:06\"");
        assert_eq!(
            serde_json::from_str::<SerializableMacAddress>(&serialized)
                .unwrap()
                .into_inner(),
            mac
        );
    }
}

/// DPU related config.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum DpuModel {
    BlueField2,
    BlueField3,
    Unknown,
}

impl<T> From<T> for DpuModel
where
    T: AsRef<str>,
{
    fn from(model: T) -> Self {
        match model.as_ref().to_lowercase().replace("-", " ") {
            value if value.contains("bluefield 2") => DpuModel::BlueField2,
            value if value.contains("bluefield 3") => DpuModel::BlueField3,
            _ => DpuModel::Unknown,
        }
    }
}

impl fmt::Display for DpuModel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", format!("{self:?}").to_lowercase())
    }
}

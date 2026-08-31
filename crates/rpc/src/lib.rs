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
//! Carbide gRPC and protobuf module
//!
//! This module contains the gRPC and protocol buffer definitions to generate a client or server to
//! interact with the API Service

extern crate core;

use std::cmp::Ordering;
use std::fmt::Display;
use std::ops::{Deref, DerefMut};
use std::pin::Pin;
use std::str::FromStr;

use chrono::{DateTime, Utc};
#[doc(hidden)]
pub use const_format;
use dns_record::DnsResourceRecordReply;
use errors::RpcDataConversionError;
use mac_address::{MacAddress, MacParseError};
use prost::{Message, UnknownEnumValue};
use serde::ser::Error;
use serde_json::{Value, json};
use tokio_stream::Stream;

use crate::forge_agent_control_response::LegacyAction;

/// Returns the compile-time gRPC path for a Forge service method.
#[macro_export]
macro_rules! service_path {
    ($method:literal) => {
        $crate::const_format::concatcp!(
            "/",
            $crate::forge::forge_server::SERVICE_NAME,
            "/",
            $method
        )
    };
}

pub use crate::protos::common::{self, Uuid};
pub use crate::protos::dns::{self};
pub use crate::protos::forge::machine_credentials_update_request::CredentialPurpose;
pub use crate::protos::forge::machine_discovery_info::DiscoveryData;
pub use crate::protos::forge::{
    self, CredentialType, Domain, DomainList, ForgeScoutErrorReport, ForgeScoutErrorReportResult,
    IbPartition, IbPartitionCreationRequest, IbPartitionDeletionRequest, IbPartitionDeletionResult,
    IbPartitionList, Instance, InstanceAllocationRequest, InstanceConfig,
    InstanceIbInterfaceConfig, InstanceIbInterfaceStatus, InstanceInfinibandConfig,
    InstanceInfinibandStatus, InstanceInterfaceConfig, InstanceInterfaceStatus,
    InstanceInterfaceStatusObservation, InstanceList, InstanceNetworkConfig, InstanceNetworkStatus,
    InstanceNvLinkStatus, InstanceReleaseRequest, InstanceStatus, InstanceTenantStatus,
    InterfaceFunctionType, Machine, MachineCleanupInfo, MachineDiscoveryInfo,
    MachineDiscoveryReporter, MachineEvent, MachineInterface, MachineList, Metadata,
    NetworkSegment, NetworkSegmentList, NvLinkPartition, NvLinkPartitionList, NvLinkPartitionQuery,
    ResourcePoolType, SyncState, TenantConfig, TenantState, forge_agent_control_response,
};
pub use crate::protos::machine_discovery::{
    self, BlockDevice, Cpu, DiscoveryInfo, DmiData, NetworkInterface, NvmeDevice,
    PciDeviceProperties,
};
pub use crate::protos::{agent_local, fmds, health, scout_firmware_upgrade, site_explorer};

pub mod errors;
pub mod forge_tls_client;
pub mod libmlx;
pub mod measured_boot;
pub mod network;
pub mod node_jwt;
pub mod node_token_socket;
pub mod protos;
pub mod secrets;
mod site_explorer_report;
pub mod utils;

#[cfg(feature = "model")]
pub mod model;

#[cfg(feature = "cli")]
pub mod admin_cli;

pub mod forge_api_client;
pub mod forge_resolver;
pub mod nmx_c_client;

pub const REFLECTION_API_SERVICE_DESCRIPTOR: &[u8] = tonic::include_file_descriptor_set!("forge");
pub const MAX_ERR_MSG_SIZE: i32 = 1500;

impl forge::BootInterfaceSelectionSource {
    /// Returns the concise name shown in views for operators.
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Unspecified => "Unspecified",
            Self::ExpectedMachine => "ExpectedMachine",
            Self::Operator => "Operator",
            Self::RedfishUefiPci => "RedfishUefiPci",
            Self::RedfishChassisId => "RedfishChassisId",
            Self::RedfishSerialNumber => "RedfishSerialNumber",
            Self::ScoutReportPci => "ScoutReportPci",
            Self::LegacyUnknown => "LegacyUnknown",
        }
    }
}

// DynForge exists because, now that we have >= streaming interface,
// simply passing around `dyn Forge` doesn't work anymore. As any additional
// streaming interfaces are added, we just toss in type defs here, and
// any users of DynForge don't need to worry about it.
pub type DynForge = dyn forge::forge_server::Forge<
        ScoutStreamStream = Pin<
            Box<
                dyn Stream<Item = Result<forge::ScoutStreamScoutBoundMessage, tonic::Status>>
                    + Send,
            >,
        >,
    >;

pub fn get_encoded_reflection_service_fd() -> Vec<u8> {
    let mut expected = Vec::new();
    prost_types::FileDescriptorSet::decode(REFLECTION_API_SERVICE_DESCRIPTOR)
        .expect("decode reflection service file descriptor set")
        .file[0]
        .encode(&mut expected)
        .expect("encode reflection service file descriptor");
    expected
}

impl Ord for Timestamp {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0
            .seconds
            .cmp(&other.0.seconds)
            .then_with(|| self.0.nanos.cmp(&other.0.nanos))
    }
}

/// A wrapper around the prost timestamp which allows for serde serialization
/// and has helper methods to convert from and into std::time::SystemTime and DateTime
#[derive(Clone, PartialEq, Copy, Eq, Default, Debug, Hash)]
pub struct Timestamp(prost_types::Timestamp);

impl PartialOrd for Timestamp {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl std::fmt::Display for Timestamp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl Deref for Timestamp {
    type Target = prost_types::Timestamp;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for Timestamp {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl From<prost_types::Timestamp> for Timestamp {
    fn from(ts: prost_types::Timestamp) -> Self {
        Self(ts)
    }
}

impl From<Timestamp> for prost_types::Timestamp {
    fn from(ts: Timestamp) -> prost_types::Timestamp {
        ts.0
    }
}

impl From<chrono::DateTime<chrono::Utc>> for Timestamp {
    fn from(time: chrono::DateTime<chrono::Utc>) -> Self {
        Self::from(std::time::SystemTime::from(time))
    }
}

impl From<std::time::SystemTime> for Timestamp {
    fn from(time: std::time::SystemTime) -> Self {
        Self(prost_types::Timestamp::from(time))
    }
}

impl TryFrom<Timestamp> for std::time::SystemTime {
    type Error = prost_types::TimestampError;

    fn try_from(ts: Timestamp) -> Result<Self, Self::Error> {
        std::time::SystemTime::try_from(ts.0)
    }
}

impl TryFrom<Timestamp> for chrono::DateTime<chrono::Utc> {
    type Error = prost_types::TimestampError;

    fn try_from(ts: Timestamp) -> Result<Self, Self::Error> {
        std::time::SystemTime::try_from(ts.0).map(|t| t.into())
    }
}

impl serde::Serialize for Timestamp {
    fn serialize<S>(&self, s: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        // We serialize the timestamp as chrono string
        match chrono::DateTime::<chrono::Utc>::try_from(*self) {
            Ok(ts) => ts.serialize(s),
            Err(_) => chrono::DateTime::<chrono::Utc>::default().serialize(s),
        }
    }
}

impl<'a> serde::Deserialize<'a> for Timestamp {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'a>,
    {
        let s = String::deserialize(deserializer)?;
        let dt =
            DateTime::<Utc>::from_str(&s).unwrap_or_else(|_e| DateTime::<chrono::Utc>::default());
        Ok(dt.into())
    }
}

impl prost::Message for Timestamp {
    fn encode_raw(&self, buf: &mut impl prost::bytes::BufMut)
    where
        Self: Sized,
    {
        self.0.encode_raw(buf)
    }

    fn merge_field(
        &mut self,
        tag: u32,
        wire_type: prost::encoding::WireType,
        buf: &mut impl prost::bytes::Buf,
        ctx: prost::encoding::DecodeContext,
    ) -> Result<(), prost::DecodeError>
    where
        Self: Sized,
    {
        self.0.merge_field(tag, wire_type, buf, ctx)
    }

    fn encoded_len(&self) -> usize {
        self.0.encoded_len()
    }

    fn clear(&mut self) {
        self.0.clear()
    }
}

/// A wrapper around the prost Duration which allows for serde serialization
/// and has helper methods to convert from and into std::time::Duration
#[derive(Clone, PartialEq, Copy, Default, Debug, Eq, Hash)]
pub struct Duration(prost_types::Duration);

impl std::fmt::Display for Duration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl Deref for Duration {
    type Target = prost_types::Duration;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for Duration {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl From<prost_types::Duration> for Duration {
    fn from(ts: prost_types::Duration) -> Self {
        Self(ts)
    }
}

impl From<Duration> for prost_types::Duration {
    fn from(ts: Duration) -> prost_types::Duration {
        ts.0
    }
}

impl TryFrom<Duration> for std::time::Duration {
    type Error = prost_types::DurationError;

    fn try_from(duration: Duration) -> Result<Self, Self::Error> {
        std::time::Duration::try_from(duration.0)
    }
}

impl TryFrom<Duration> for chrono::TimeDelta {
    type Error = prost_types::DurationError;

    fn try_from(duration: Duration) -> Result<Self, Self::Error> {
        let std_duration = std::time::Duration::try_from(duration.0)?;
        chrono::TimeDelta::from_std(std_duration)
            .map_err(|_| prost_types::DurationError::OutOfRange)
    }
}

impl From<std::time::Duration> for Duration {
    fn from(duration: std::time::Duration) -> Self {
        // Realistically we will never deal with a `time::Duration` that can't be
        // natively converted. But if we do - clamp it to the maximum allowed value
        prost_types::Duration::try_from(duration)
            .unwrap_or(prost_types::Duration {
                seconds: i64::MAX,
                nanos: 999_999_999,
            })
            .into()
    }
}

impl From<chrono::TimeDelta> for Duration {
    fn from(duration: chrono::TimeDelta) -> Self {
        duration.to_std().unwrap_or(std::time::Duration::MAX).into()
    }
}

impl serde::Serialize for Duration {
    fn serialize<S>(&self, s: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        // We serialize the duration as std::time::Duration
        match std::time::Duration::try_from(*self) {
            Ok(duration) => duration.serialize(s),
            Err(_) => std::time::Duration::ZERO.serialize(s),
        }
    }
}

impl<'de> serde::Deserialize<'de> for Duration {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let duration: std::time::Duration = serde::Deserialize::deserialize(deserializer)?;
        Ok(Self::from(duration))
    }
}

impl prost::Message for Duration {
    fn encode_raw(&self, buf: &mut impl prost::bytes::BufMut)
    where
        Self: Sized,
    {
        self.0.encode_raw(buf)
    }

    fn merge_field(
        &mut self,
        tag: u32,
        wire_type: prost::encoding::WireType,
        buf: &mut impl prost::bytes::Buf,
        ctx: prost::encoding::DecodeContext,
    ) -> Result<(), prost::DecodeError>
    where
        Self: Sized,
    {
        self.0.merge_field(tag, wire_type, buf, ctx)
    }

    fn encoded_len(&self) -> usize {
        self.0.encoded_len()
    }

    fn clear(&mut self) {
        self.0.clear()
    }
}

impl TryFrom<common::Uuid> for ::uuid::Uuid {
    type Error = ::uuid::Error;
    fn try_from(uuid: Uuid) -> Result<Self, Self::Error> {
        ::uuid::Uuid::parse_str(&uuid.value)
    }
}

impl TryFrom<&common::Uuid> for ::uuid::Uuid {
    type Error = ::uuid::Error;
    fn try_from(uuid: &Uuid) -> Result<Self, Self::Error> {
        ::uuid::Uuid::parse_str(&uuid.value)
    }
}

impl Display for common::Uuid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match ::uuid::Uuid::try_from(self) {
            Ok(uuid) => write!(f, "{uuid}"),
            Err(err) => write!(f, "<uuid error: {err}>"),
        }
    }
}

impl MachineInterface {
    pub fn parsed_mac_address(&self) -> Result<Option<MacAddress>, MacParseError> {
        Ok(Some(MacAddress::from_str(&self.mac_address)?))
    }
}

impl From<health_report::HealthProbeSuccess> for health::HealthProbeSuccess {
    fn from(success: health_report::HealthProbeSuccess) -> Self {
        Self {
            id: success.id.to_string(),
            target: success.target,
        }
    }
}

impl TryFrom<health::HealthProbeSuccess> for health_report::HealthProbeSuccess {
    type Error = health_report::HealthReportConversionError;
    fn try_from(success: health::HealthProbeSuccess) -> Result<Self, Self::Error> {
        Ok(Self {
            id: success.id.parse()?,
            target: success.target.clone(),
        })
    }
}

impl From<health_report::HealthProbeAlert> for health::HealthProbeAlert {
    fn from(alert: health_report::HealthProbeAlert) -> Self {
        Self {
            id: alert.id.to_string(),
            target: alert.target.clone(),
            in_alert_since: alert.in_alert_since.map(Timestamp::from),
            message: alert.message,
            tenant_message: alert.tenant_message,
            classifications: alert
                .classifications
                .into_iter()
                .map(|c| c.to_string())
                .collect(),
        }
    }
}

impl TryFrom<health::HealthProbeAlert> for health_report::HealthProbeAlert {
    type Error = health_report::HealthReportConversionError;
    fn try_from(alert: health::HealthProbeAlert) -> Result<Self, Self::Error> {
        let classifications = alert
            .classifications
            .into_iter()
            .map(|c| c.parse())
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Self {
            id: alert.id.parse()?,
            target: match alert.target {
                Some(target) if !target.is_empty() => Some(target),
                _ => None,
            },
            in_alert_since: alert
                .in_alert_since
                .map(TryInto::try_into)
                .transpose()
                .map_err(|_| health_report::HealthReportConversionError::TimestampParseError)?,
            message: alert.message,
            tenant_message: alert.tenant_message,
            classifications,
        })
    }
}

impl From<health_report::HealthReport> for health::HealthReport {
    fn from(report: health_report::HealthReport) -> Self {
        Self {
            source: report.source,
            triggered_by: report.triggered_by,
            observed_at: report.observed_at.map(Timestamp::from),
            successes: report.successes.into_iter().map(Into::into).collect(),
            alerts: report.alerts.into_iter().map(Into::into).collect(),
        }
    }
}

impl TryFrom<health::HealthReport> for health_report::HealthReport {
    type Error = health_report::HealthReportConversionError;
    fn try_from(report: health::HealthReport) -> Result<Self, Self::Error> {
        if report.source.is_empty() {
            return Err(health_report::HealthReportConversionError::MissingSource);
        }
        let mut successes = Vec::new();
        let mut alerts = Vec::new();
        for success in report.successes {
            successes.push(success.try_into()?);
        }
        for alert in report.alerts {
            alerts.push(alert.try_into()?);
        }

        Ok(Self {
            source: report.source,
            observed_at: report
                .observed_at
                .map(DateTime::<Utc>::try_from)
                .transpose()
                .map_err(|_| health_report::HealthReportConversionError::TimestampParseError)?,
            successes,
            alerts,
            triggered_by: report.triggered_by,
        })
    }
}

impl From<forge::HealthReportApplyMode> for health_report::HealthReportApplyMode {
    fn from(value: forge::HealthReportApplyMode) -> Self {
        match value {
            forge::HealthReportApplyMode::Merge => health_report::HealthReportApplyMode::Merge,
            forge::HealthReportApplyMode::Replace => health_report::HealthReportApplyMode::Replace,
        }
    }
}

impl From<DnsResourceRecordReply> for crate::protos::dns::DnsResourceRecord {
    fn from(value: DnsResourceRecordReply) -> Self {
        Self {
            qtype: value.qtype,
            qname: value.qname,
            ttl: value.ttl,
            content: value.content,
            auth: value.auth,
            domain_id: value.domain_id,
            scope_mask: value.scope_mask,
        }
    }
}

pub struct JsonDnsResourceRecord(pub crate::protos::dns::DnsResourceRecord);

impl From<JsonDnsResourceRecord> for Value {
    fn from(wrapper: JsonDnsResourceRecord) -> Self {
        let json_value = json!({
            "qtype": wrapper.0.qtype,
            "qname": wrapper.0.qname,
            "ttl": wrapper.0.ttl,
            "content": wrapper.0.content,
            "domain_id": wrapper.0.domain_id,
            "scope_mask": wrapper.0.scope_mask,
            "auth": wrapper.0.auth,
        });
        json_value
    }
}

impl FromStr for forge::InstanceOperatingSystemConfig {
    type Err = RpcDataConversionError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        serde_json::from_str(s)
            .map_err(|e| RpcDataConversionError::JsonConversionFailure(e.to_string()))
    }
}

impl FromStr for forge::InstanceInfinibandConfig {
    type Err = RpcDataConversionError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        serde_json::from_str(s)
            .map_err(|e| RpcDataConversionError::JsonConversionFailure(e.to_string()))
    }
}

impl FromStr for forge::InstanceNvLinkConfig {
    type Err = RpcDataConversionError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        serde_json::from_str(s)
            .map_err(|e| RpcDataConversionError::JsonConversionFailure(e.to_string()))
    }
}

impl FromStr for forge::InstanceSpxConfig {
    type Err = RpcDataConversionError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        serde_json::from_str(s)
            .map_err(|e| RpcDataConversionError::JsonConversionFailure(e.to_string()))
    }
}

/// JSON input accepted for prost's optional integer-backed interface role.
///
/// Admin clients may use the short enum name while protobuf-compatible clients
/// may continue to send its integer value.
#[derive(serde::Deserialize)]
#[serde(untagged)]
enum ExpectedInterfaceRoleInput {
    Name(String),
    Number(i32),
}

impl forge::ExpectedInterfaceRole {
    /// Deserialize an optional role from its short name or protobuf integer.
    ///
    /// JSON `null` remains `None`. `unspecified` remains an explicit protobuf
    /// value here and becomes the legacy Host role at the model boundary.
    pub fn deserialize_optional<'de, D>(deserializer: D) -> Result<Option<i32>, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value: Option<ExpectedInterfaceRoleInput> =
            serde::Deserialize::deserialize(deserializer)?;
        let Some(value) = value else {
            return Ok(None);
        };

        let role = match value {
            ExpectedInterfaceRoleInput::Number(value) => {
                Self::try_from(value).map_err(serde::de::Error::custom)?
            }
            ExpectedInterfaceRoleInput::Name(name) => {
                match name.trim().to_ascii_lowercase().replace('-', "_").as_str() {
                    "unspecified" | "expected_interface_role_unspecified" => Self::Unspecified,
                    "host" | "expected_interface_role_host" => Self::Host,
                    "dpu_os" | "dpuos" | "expected_interface_role_dpu_os" => Self::DpuOs,
                    "dpu_bmc" | "dpubmc" | "expected_interface_role_dpu_bmc" => Self::DpuBmc,
                    "host_bmc" | "hostbmc" | "expected_interface_role_host_bmc" => Self::HostBmc,
                    _ => {
                        return Err(serde::de::Error::custom(format!(
                            "unknown expected interface role {name:?}"
                        )));
                    }
                }
            }
        };
        Ok(Some(role as i32))
    }

    /// Serialize an optional prost role as its short JSON name.
    ///
    /// `None` stays absent so model conversion can preserve the representation
    /// used by clients that predate interface roles.
    pub fn serialize_optional<S>(value: &Option<i32>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let Some(value) = value else {
            return serializer.serialize_none();
        };
        let role = Self::try_from(*value).map_err(Error::custom)?;
        serializer.serialize_str(match role {
            Self::Unspecified => "unspecified",
            Self::Host => "host",
            Self::DpuOs => "dpu_os",
            Self::DpuBmc => "dpu_bmc",
            Self::HostBmc => "host_bmc",
        })
    }
}

/// JSON input accepted for prost's optional integer-backed IP allocation
/// policy.
///
/// Admin clients may use the short enum name while protobuf-compatible clients
/// may continue to send its integer value.
#[derive(serde::Deserialize)]
#[serde(untagged)]
enum ExpectedInterfaceIpAllocationInput {
    Name(String),
    Number(i32),
}

impl forge::ExpectedInterfaceIpAllocation {
    /// Deserialize an optional allocation policy from its short name or
    /// protobuf integer.
    ///
    /// JSON `null` remains `None`. `unspecified` remains an explicit protobuf
    /// value here and becomes an omitted policy at the model boundary, where
    /// `fixed_ip` determines the legacy policy.
    pub fn deserialize_optional<'de, D>(deserializer: D) -> Result<Option<i32>, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value: Option<ExpectedInterfaceIpAllocationInput> =
            serde::Deserialize::deserialize(deserializer)?;
        let Some(value) = value else {
            return Ok(None);
        };

        let policy = match value {
            ExpectedInterfaceIpAllocationInput::Number(value) => {
                Self::try_from(value).map_err(serde::de::Error::custom)?
            }
            ExpectedInterfaceIpAllocationInput::Name(name) => {
                match name.trim().to_ascii_lowercase().replace('-', "_").as_str() {
                    "unspecified" | "expected_interface_ip_allocation_unspecified" => {
                        Self::Unspecified
                    }
                    "dynamic" | "expected_interface_ip_allocation_dynamic" => Self::Dynamic,
                    "fixed" | "expected_interface_ip_allocation_fixed" => Self::Fixed,
                    "retained" | "expected_interface_ip_allocation_retained" => Self::Retained,
                    _ => {
                        return Err(serde::de::Error::custom(format!(
                            "unknown expected interface IP allocation {name:?}"
                        )));
                    }
                }
            }
        };
        Ok(Some(policy as i32))
    }

    /// Serialize an optional prost allocation policy as its short JSON name.
    ///
    /// `None` stays absent; it must not become explicit `Dynamic`
    /// because an omitted policy plus `fixed_ip` resolves to `Fixed`.
    pub fn serialize_optional<S>(value: &Option<i32>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let Some(value) = value else {
            return serializer.serialize_none();
        };
        let policy = Self::try_from(*value).map_err(Error::custom)?;
        serializer.serialize_str(match policy {
            Self::Unspecified => "unspecified",
            Self::Dynamic => "dynamic",
            Self::Fixed => "fixed",
            Self::Retained => "retained",
        })
    }
}

/*  ****************************************************** */
// Serialization/deserialization helpers for network
// security group enums to let admin CLI callers describe
// rules with friendly property values intead of i32.
/* ******************************************************* */

impl forge::NetworkSecurityGroupRuleDirection {
    pub fn from_string<'de, D>(deserializer: D) -> Result<i32, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s: &str = serde::Deserialize::deserialize(deserializer)?;

        match s.to_uppercase().as_str() {
            "INGRESS" => Ok(Self::NsgRuleDirectionIngress as i32),
            "EGRESS" => Ok(Self::NsgRuleDirectionEgress as i32),
            _ => Ok(0),
        }
    }

    pub fn serialize_from_enum_i32<S>(v: &i32, s: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        s.serialize_str(
            &forge::NetworkSecurityGroupRuleDirection::to_string_from_enum_i32(*v)
                .map_err(Error::custom)?,
        )
    }

    pub fn to_string_from_enum_i32(v: i32) -> Result<String, UnknownEnumValue> {
        let t: forge::NetworkSecurityGroupRuleDirection = (v).try_into()?;

        Ok(match t {
            forge::NetworkSecurityGroupRuleDirection::NsgRuleDirectionIngress => {
                "INGRESS".to_string()
            }
            forge::NetworkSecurityGroupRuleDirection::NsgRuleDirectionEgress => {
                "EGRESS".to_string()
            }
            forge::NetworkSecurityGroupRuleDirection::NsgRuleDirectionInvalid => {
                "INVALID".to_string()
            }
        })
    }
}

impl forge::NetworkSecurityGroupRuleProtocol {
    pub fn from_string<'de, D>(deserializer: D) -> Result<i32, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s: &str = serde::Deserialize::deserialize(deserializer)?;

        match s.to_uppercase().as_str() {
            "ANY" => Ok(Self::NsgRuleProtoAny as i32),
            "ICMP" => Ok(Self::NsgRuleProtoIcmp as i32),
            "ICMP6" => Ok(Self::NsgRuleProtoIcmp6 as i32),
            "UDP" => Ok(Self::NsgRuleProtoUdp as i32),
            "TCP" => Ok(Self::NsgRuleProtoTcp as i32),
            _ => Ok(0),
        }
    }

    pub fn serialize_from_enum_i32<S>(v: &i32, s: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        s.serialize_str(
            &forge::NetworkSecurityGroupRuleProtocol::to_string_from_enum_i32(*v)
                .map_err(Error::custom)?,
        )
    }

    pub fn to_string_from_enum_i32(v: i32) -> Result<String, UnknownEnumValue> {
        let t: forge::NetworkSecurityGroupRuleProtocol = (v).try_into()?;

        Ok(match t {
            forge::NetworkSecurityGroupRuleProtocol::NsgRuleProtoAny => "ANY".to_string(),
            forge::NetworkSecurityGroupRuleProtocol::NsgRuleProtoIcmp => "ICMP".to_string(),
            forge::NetworkSecurityGroupRuleProtocol::NsgRuleProtoIcmp6 => "ICMP6".to_string(),
            forge::NetworkSecurityGroupRuleProtocol::NsgRuleProtoUdp => "UDP".to_string(),
            forge::NetworkSecurityGroupRuleProtocol::NsgRuleProtoTcp => "TCP".to_string(),
            forge::NetworkSecurityGroupRuleProtocol::NsgRuleProtoInvalid => "INVALID".to_string(),
        })
    }
}

impl forge::NetworkSecurityGroupRuleAction {
    pub fn from_string<'de, D>(deserializer: D) -> Result<i32, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s: &str = serde::Deserialize::deserialize(deserializer)?;

        match s.to_uppercase().as_str() {
            "PERMIT" => Ok(Self::NsgRuleActionPermit as i32),
            "DENY" => Ok(Self::NsgRuleActionDeny as i32),
            _ => Ok(0),
        }
    }

    pub fn serialize_from_enum_i32<S>(v: &i32, s: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        s.serialize_str(
            &forge::NetworkSecurityGroupRuleAction::to_string_from_enum_i32(*v)
                .map_err(Error::custom)?,
        )
    }

    pub fn to_string_from_enum_i32(v: i32) -> Result<String, UnknownEnumValue> {
        let t: forge::NetworkSecurityGroupRuleAction = (v).try_into()?;

        Ok(match t {
            forge::NetworkSecurityGroupRuleAction::NsgRuleActionPermit => "PERMIT".to_string(),
            forge::NetworkSecurityGroupRuleAction::NsgRuleActionDeny => "DENY".to_string(),
            forge::NetworkSecurityGroupRuleAction::NsgRuleActionInvalid => "INVALID".to_string(),
        })
    }
}

impl forge::MachineCapabilityType {
    pub fn from_string<'de, D>(deserializer: D) -> Result<i32, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s: &str = serde::Deserialize::deserialize(deserializer)?;

        match s.to_uppercase().as_str() {
            "CPU" => Ok(forge::MachineCapabilityType::CapTypeCpu as i32),
            "GPU" => Ok(forge::MachineCapabilityType::CapTypeGpu as i32),
            "MEMORY" => Ok(forge::MachineCapabilityType::CapTypeMemory as i32),
            "STORAGE" => Ok(forge::MachineCapabilityType::CapTypeStorage as i32),
            "NETWORK" => Ok(forge::MachineCapabilityType::CapTypeNetwork as i32),
            "INFINIBAND" => Ok(forge::MachineCapabilityType::CapTypeInfiniband as i32),
            "DPU" => Ok(forge::MachineCapabilityType::CapTypeDpu as i32),
            _ => Ok(0),
        }
    }

    pub fn serialize_from_enum_i32<S>(v: &i32, s: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        s.serialize_str(
            &forge::MachineCapabilityType::to_string_from_enum_i32(*v).map_err(Error::custom)?,
        )
    }

    pub fn to_string_from_enum_i32(v: i32) -> Result<String, UnknownEnumValue> {
        let t: forge::MachineCapabilityType = (v).try_into()?;

        Ok(match t {
            forge::MachineCapabilityType::CapTypeCpu => "CPU".to_string(),
            forge::MachineCapabilityType::CapTypeGpu => "GPU".to_string(),
            forge::MachineCapabilityType::CapTypeMemory => "MEMORY".to_string(),
            forge::MachineCapabilityType::CapTypeStorage => "STORAGE".to_string(),
            forge::MachineCapabilityType::CapTypeNetwork => "NETWORK".to_string(),
            forge::MachineCapabilityType::CapTypeInfiniband => "INFINIBAND".to_string(),
            forge::MachineCapabilityType::CapTypeDpu => "DPU".to_string(),
            forge::MachineCapabilityType::CapTypeInvalid => "INVALID".to_string(),
        })
    }
}

impl forge::MachineCapabilityDeviceType {
    pub fn from_string<'de, D>(deserializer: D) -> Result<Option<i32>, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s: &str = serde::Deserialize::deserialize(deserializer)?;

        Ok(Some(match s.to_uppercase().as_str() {
            "DPU" => Self::Dpu as i32,
            "SPECTRUMX" => Self::SpectrumX as i32,
            "UNKNOWN" => Self::Unknown as i32,
            _ => 0,
        }))
    }

    pub fn serialize_from_enum_i32<S>(v: &Option<i32>, s: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match v {
            Some(val) => s.serialize_str(
                &forge::MachineCapabilityDeviceType::to_string_from_enum_i32(*val)
                    .map_err(Error::custom)?,
            ),
            None => s.serialize_none(),
        }
    }

    pub fn to_string_from_enum_i32(v: i32) -> Result<String, UnknownEnumValue> {
        let t: forge::MachineCapabilityDeviceType = (v).try_into()?;

        Ok(match t {
            forge::MachineCapabilityDeviceType::Dpu => "DPU".to_string(),
            forge::MachineCapabilityDeviceType::Unknown => "UNKNOWN".to_string(),
            forge::MachineCapabilityDeviceType::Nvlink => "NVLINK".to_string(),
            forge::MachineCapabilityDeviceType::SpectrumX => "SpectrumX".to_string(),
        })
    }
}

impl forge::ScoutStreamScoutBoundMessage {
    pub fn new_flow(msg: forge::scout_stream_scout_bound_message::Payload) -> Self {
        Self {
            flow_uuid: Some(uuid::Uuid::new_v4().into()),
            payload: Some(msg),
        }
    }
}

impl forge::ScoutStreamApiBoundMessage {
    pub fn from_flow(
        flow_uuid: uuid::Uuid,
        msg: forge::scout_stream_api_bound_message::Payload,
    ) -> Self {
        Self {
            flow_uuid: Some(flow_uuid.into()),
            payload: Some(msg),
        }
    }
}

impl forge::ForgeAgentControlResponse {
    pub fn noop() -> Self {
        Self {
            action: Some(forge_agent_control_response::Action::noop()),
            legacy_action: LegacyAction::Noop as i32,
            data: None,
        }
    }

    pub fn reset() -> Self {
        Self {
            action: Some(forge_agent_control_response::Action::reset()),
            legacy_action: LegacyAction::Reset as i32,
            data: None,
        }
    }

    pub fn retry() -> Self {
        Self {
            action: Some(forge_agent_control_response::Action::retry()),
            legacy_action: LegacyAction::Retry as i32,
            data: None,
        }
    }

    pub fn measure() -> Self {
        Self {
            action: Some(forge_agent_control_response::Action::measure()),
            legacy_action: LegacyAction::Measure as i32,
            data: None,
        }
    }

    pub fn discovery() -> Self {
        Self {
            action: Some(forge_agent_control_response::Action::discovery()),
            legacy_action: LegacyAction::Discovery as i32,
            data: None,
        }
    }

    pub fn log_error() -> Self {
        Self {
            action: Some(forge_agent_control_response::Action::log_error()),
            legacy_action: LegacyAction::Logerror as i32,
            data: None,
        }
    }
}

impl forge_agent_control_response::Action {
    pub fn noop() -> Self {
        Self::Noop(forge_agent_control_response::Noop {})
    }

    pub fn reset() -> Self {
        Self::Reset(forge_agent_control_response::Reset {})
    }

    pub fn retry() -> Self {
        Self::Retry(forge_agent_control_response::Retry {})
    }

    pub fn measure() -> Self {
        Self::Measure(forge_agent_control_response::Measure {})
    }

    pub fn discovery() -> Self {
        Self::Discovery(forge_agent_control_response::Discovery {})
    }

    pub fn log_error() -> Self {
        Self::LogError(forge_agent_control_response::LogError {})
    }

    pub fn as_str_name(&self) -> &'static str {
        match self {
            Self::Noop(_) => "NOOP",
            Self::Reset(_) => "RESET",
            Self::Discovery(_) => "DISCOVERY",
            Self::Rebuild(_) => "REBUILD",
            Self::Retry(_) => "RETRY",
            Self::Measure(_) => "MEASURE",
            Self::LogError(_) => "LOGERROR",
            Self::MachineValidation(_) => "MACHINE_VALIDATION",
            Self::MlxAction(_) => "MLX_ACTION",
            Self::FirmwareUpgrade(_) => "FIRMWARE_UPGRADE",
        }
    }
}

impl From<MacAddress> for forge::find_bmc_ips_request::LookupBy {
    fn from(addr: MacAddress) -> Self {
        Self::MacAddress(addr.to_string())
    }
}

#[cfg(feature = "cli")]
// This impl allows us to use the RPC RouteServerSourceType type
// as a first class enum with clap, for the purpose of allowing
// users to set --source-type with the forge-admin-cli.
impl clap::ValueEnum for forge::RouteServerSourceType {
    fn value_variants<'a>() -> &'a [Self] {
        &[Self::AdminApi, Self::ConfigFile]
    }

    fn to_possible_value(&self) -> Option<clap::builder::PossibleValue> {
        Some(match self {
            Self::AdminApi => clap::builder::PossibleValue::new("admin_api"),
            Self::ConfigFile => clap::builder::PossibleValue::new("config_file"),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use carbide_uuid::device::DeviceId;
    use carbide_uuid::machine::MachineId;
    use carbide_uuid::switch::SwitchId;

    use self::forge::instance_operating_system_config::Variant;
    use self::forge::{InlineIpxe, InstanceOperatingSystemConfig};
    use super::*;
    use crate::protos::dns::{Domain, Metadata};

    fn assert_serialize<T: serde::Serialize>() {}

    fn assert_deserialize<T: for<'de> serde::Deserialize<'de>>() {}

    #[test]
    fn protobuf_codegen_annotations_apply_to_generated_type_kinds() {
        assert_serialize::<forge::ClientSecretBasic>();
        assert_deserialize::<forge::ClientSecretBasic>();
        assert_serialize::<forge::DpuMode>();
        assert_deserialize::<forge::DpuMode>();
        assert_serialize::<forge::instance_interface_config::NetworkDetails>();
        assert_serialize::<forge::get_machine_boot_interfaces_response::Reconciliation>();
    }

    #[test]
    fn reflection_descriptor_contains_all_rpc_services() {
        let descriptor_set =
            prost_types::FileDescriptorSet::decode(REFLECTION_API_SERVICE_DESCRIPTOR)
                .expect("reflection descriptor set decodes");
        let mut service_names = descriptor_set
            .file
            .iter()
            .flat_map(|file| &file.service)
            .filter_map(|service| service.name.as_deref())
            .collect::<Vec<_>>();
        service_names.sort_unstable();

        assert_eq!(
            service_names,
            ["FmdsConfigService", "Forge", "NMX_Controller"]
        );
    }

    #[test]
    fn test_serialize_timestamp() {
        let ts = std::time::SystemTime::now();

        let proto_ts = Timestamp::from(ts);
        let encoded = proto_ts.encode_to_vec();

        let decoded = Timestamp::decode(&encoded[..]).unwrap();
        let decoded_system_time: std::time::SystemTime = decoded.try_into().unwrap();
        assert_eq!(ts, decoded_system_time);
    }

    #[test]
    fn test_serialize_timestamp_as_json() {
        let ts = std::time::SystemTime::UNIX_EPOCH;
        let proto_ts = Timestamp::from(ts);
        assert_eq!(
            "\"1970-01-01T00:00:00Z\"",
            serde_json::to_string(&proto_ts).unwrap()
        );
    }

    #[test]
    fn test_serialize_machine_id_as_json() {
        let id = MachineId::from_str("fm100htjsaledfasinabqqer70e2ua5ksqj4kfjii0v0a90vulps48c1h7g")
            .unwrap();
        assert_eq!(
            "\"fm100htjsaledfasinabqqer70e2ua5ksqj4kfjii0v0a90vulps48c1h7g\"",
            serde_json::to_string(&id).unwrap()
        );
    }

    #[test]
    fn test_serialize_os() {
        let os = InstanceOperatingSystemConfig {
            phone_home_enabled: true,
            run_provisioning_instructions_on_every_boot: true,
            user_data: Some("def".to_string()),
            variant: Some(Variant::Ipxe(InlineIpxe {
                ipxe_script: "abc".to_string(),
            })),
        };

        assert_eq!(
            "{\"phone_home_enabled\":true,\"run_provisioning_instructions_on_every_boot\":true,\"user_data\":\"def\",\"variant\":{\"Ipxe\":{\"ipxe_script\":\"abc\"}}}",
            serde_json::to_string(&os).unwrap()
        );
    }

    /// Test to check that serializing a type with a custom Timestamp implementation works
    #[test]
    fn test_serialize_domain() {
        let uuid = carbide_uuid::domain::DomainId::from(::uuid::uuid!(
            "91609f10-c91d-470d-a260-1234560c0000"
        ));
        let ts = std::time::SystemTime::now();
        let ts2 = ts.checked_add(Duration::from_millis(1500)).unwrap();

        let domain_metadata = Metadata {
            allow_axfr_from: vec![],
        };
        let domain = Domain {
            id: Some(uuid),
            name: "MyDomain".to_string(),
            created: Some(ts.into()),
            updated: Some(ts2.into()),
            deleted: None,
            soa: None,
            metadata: Some(domain_metadata),
        };

        let encoded = domain.encode_to_vec();
        let decoded = Domain::decode(&encoded[..]).unwrap();

        let deserialized_uuid: carbide_uuid::domain::DomainId = decoded.id.unwrap();
        let created_system_time: std::time::SystemTime =
            decoded.created.unwrap().try_into().unwrap();
        let updated_system_time: std::time::SystemTime =
            decoded.updated.unwrap().try_into().unwrap();
        assert_eq!(uuid, deserialized_uuid);
        assert_eq!(ts, created_system_time);
        assert_eq!(ts2, updated_system_time);
    }

    /// Verifies the additive DHCP discovery IPv6 fields survive protobuf encoding.
    #[test]
    fn dhcp_discovery_round_trips_with_ipv6_fields() {
        let discovery = forge::DhcpDiscovery {
            mac_address: "00:11:22:33:44:55".to_string(),
            relay_address: "2001:db8::1".to_string(),
            vendor_string: Some("vendor".to_string()),
            link_address: Some("2001:db8::2".to_string()),
            circuit_id: Some("circuit".to_string()),
            remote_id: Some("remote".to_string()),
            desired_address: Some("2001:db8::10".to_string()),
            address_family: Some(2),
            message_kind: Some(2),
            duid: Some(vec![0, 1, 0, 1, 0xaa, 0xbb]),
        };

        // Encode then decode so prost field numbering is exercised directly.
        let encoded = discovery.encode_to_vec();
        let decoded = forge::DhcpDiscovery::decode(&encoded[..]).unwrap();

        // Verify the newly-added IPv6 fields survive the protobuf round trip.
        assert_eq!(decoded.address_family, Some(2));
        assert_eq!(decoded.message_kind, Some(2));
        assert_eq!(decoded.duid, Some(vec![0, 1, 0, 1, 0xaa, 0xbb]));
    }

    #[test]
    fn bmc_rotation_request_round_trips_the_shared_device_id() {
        let switch_id =
            SwitchId::from_str("sw100nt038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg")
                .unwrap();
        let request = forge::BmcCredentialRotationRequest {
            mode: forge::bmc_credential_rotation_request::Mode::Clear as i32,
            bmc_mac: None,
            device_id: Some(DeviceId::Switch(switch_id)),
        };

        let decoded =
            forge::BmcCredentialRotationRequest::decode(request.encode_to_vec().as_slice())
                .unwrap();
        assert_eq!(decoded.device_id, Some(DeviceId::Switch(switch_id)));
    }
}

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
#![allow(clippy::enum_variant_names)]
use serde_derive::{Deserialize, Serialize};

/// Use Option<type> to avoid breaking serde deserialize ops on receiving json responses with
/// some struct fields missing.
/// only id (_id) and uuid fields are safe to keep non-optional.

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AsyncResponse {
    #[serde(rename = "operationId")]
    pub operation_id: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RawResponse {
    pub body: String,
    pub code: u16,
    pub headers: http::HeaderMap,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CountResponse {
    /// Number of objects
    #[serde(rename = "total", skip_serializing_if = "Option::is_none")]
    pub total: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CreatePartitionRequest {
    /// Name of the partition
    #[serde(rename = "Name")]
    pub name: String,
    #[serde(rename = "Members")]
    pub members: Box<PartitionMembers>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UpdatePartitionRequest {
    #[serde(rename = "Members")]
    pub members: Box<PartitionMembers>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, serde(deny_unknown_fields))]
pub struct Chassis {
    /// System unique identifier.
    #[serde(rename = "ID", skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(rename = "Name", skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(rename = "Description", skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(rename = "CreatedAt", skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(rename = "UpdatedAt", skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    /// Universally Unique Identifier.
    #[serde(rename = "DomainUUID", skip_serializing_if = "Option::is_none")]
    pub domain_uuid: Option<uuid::Uuid>,
    /// The unique identifier of the chassis
    #[serde(rename = "InternalID", skip_serializing_if = "Option::is_none")]
    pub internal_id: Option<i32>,
    /// The unique serial number of the chassis
    #[serde(rename = "SerialNumber", skip_serializing_if = "Option::is_none")]
    pub serial_number: Option<String>,
    #[serde(rename = "ComputeNodeIDList", skip_serializing_if = "Option::is_none")]
    pub compute_node_id_list: Option<Vec<String>>,
    #[serde(rename = "SwitchNodeIDList", skip_serializing_if = "Option::is_none")]
    pub switch_node_id_list: Option<Vec<String>>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[cfg_attr(test, serde(deny_unknown_fields))]
#[serde(rename_all = "camelCase")]
pub struct ComputeNode {
    /// System unique identifier.
    #[serde(rename = "ID", skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Helpful name
    #[serde(rename = "Name", skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Brief description
    #[serde(rename = "Description", skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(rename = "CreatedAt", skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(rename = "UpdatedAt", skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    /// Universally Unique Identifier.
    #[serde(rename = "DomainUUID", skip_serializing_if = "Option::is_none")]
    pub domain_uuid: Option<uuid::Uuid>,
    #[serde(rename = "LocationInfo", skip_serializing_if = "Option::is_none")]
    pub location_info: Option<Box<LocationInfo>>,
    #[serde(rename = "Health", skip_serializing_if = "Option::is_none")]
    pub health: Option<ComputeNodeHealth>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
pub enum ComputeNodeHealth {
    #[serde(rename = "UNKNOWN")]
    ComputeNodeHealthUnknown,
    #[serde(rename = "HEALTHY")]
    ComputeNodeHealthHealthy,
    #[serde(rename = "DEGRADED")]
    ComputeNodeHealthDegraded,
    #[serde(rename = "UNHEALTHY")]
    ComputeNodeHealthUnhealthy,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Gpu {
    /// System unique identifier.
    #[serde(rename = "ID", skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(rename = "Name", skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// User specified notes & description
    #[serde(rename = "Description", skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Device internal description
    #[serde(
        rename = "InternalDescription",
        skip_serializing_if = "Option::is_none"
    )]
    pub internal_description: Option<String>,
    #[serde(rename = "CreatedAt", skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(rename = "UpdatedAt", skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    /// Universally Unique Identifier.
    #[serde(rename = "DomainUUID", skip_serializing_if = "Option::is_none")]
    pub domain_uuid: Option<uuid::Uuid>,
    #[serde(rename = "LocationInfo", skip_serializing_if = "Option::is_none")]
    pub location_info: Option<Box<LocationInfo>>,
    #[serde(rename = "DeviceUID")]
    pub device_uid: u64,
    #[serde(rename = "DeviceID")]
    pub device_id: i32,
    #[serde(rename = "DevicePcieID")]
    pub device_pcie_id: i32,
    #[serde(rename = "SystemUID")]
    pub system_uid: u64,
    #[serde(rename = "VendorID")]
    pub vendor_id: i32,
    /// List of device labels for internal routing
    #[serde(rename = "ALIDList")]
    pub alid_list: Vec<i32>,
    #[serde(rename = "PartitionID", skip_serializing_if = "Option::is_none")]
    pub partition_id: Option<i32>,
    /// List of device ports
    #[serde(rename = "PortIDList", skip_serializing_if = "Option::is_none")]
    pub port_id_list: Option<Vec<String>>,
    #[serde(rename = "Health", skip_serializing_if = "Option::is_none")]
    pub health: Option<GpuHealth>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
pub enum GpuHealth {
    #[serde(rename = "UNKNOWN")]
    GPUHealthUnknown,
    #[serde(rename = "HEALTHY")]
    GPUHealthHealthy,
    #[serde(rename = "DEGRADED")]
    GPUHealthDegraded,
    #[serde(rename = "NO_NVLINK")]
    GPUHealthNoNVL,
    #[serde(rename = "DEGRADED_BW")]
    GPUHealthDegradedBW,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, serde(deny_unknown_fields))]
#[serde(rename_all = "camelCase")]
pub struct LocationInfo {
    /// The unique identifier of the chassis
    #[serde(rename = "ChassisID", skip_serializing_if = "Option::is_none")]
    pub chassis_id: Option<i32>,
    /// The unique serial number of the chassis
    #[serde(
        rename = "ChassisSerialNumber",
        skip_serializing_if = "Option::is_none"
    )]
    pub chassis_serial_number: Option<String>,
    /// The unique identifier of the slot
    #[serde(rename = "SlotID", skip_serializing_if = "Option::is_none")]
    pub slot_id: Option<i32>,
    /// Index of the compute/switch tray within the compute/switch trays group in the chassis, a number from 0
    #[serde(rename = "TrayIndex", skip_serializing_if = "Option::is_none")]
    pub tray_index: Option<i32>,
    /// The unique identifier of the host
    #[serde(rename = "HostID", skip_serializing_if = "Option::is_none")]
    pub host_id: Option<i32>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(test, serde(deny_unknown_fields))]
pub struct NmxService {
    /// System unique identifier.
    #[serde(rename = "ID", skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Name of the service.
    #[serde(rename = "Name", skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Description of the service.
    #[serde(rename = "Description", skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(rename = "ServiceType", skip_serializing_if = "Option::is_none")]
    pub service_type: Option<NmxServiceType>,
    /// IPv4 address mask.
    #[serde(rename = "Address", skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
    #[serde(rename = "PortNumber", skip_serializing_if = "Option::is_none")]
    pub port_number: Option<i32>,
    /// Universally Unique Identifier.
    #[serde(rename = "ClusterDomainUUID", skip_serializing_if = "Option::is_none")]
    pub cluster_domain_uuid: Option<uuid::Uuid>,
    /// Universally Unique Identifier.
    #[serde(rename = "ApplicationUUID", skip_serializing_if = "Option::is_none")]
    pub application_uuid: Option<uuid::Uuid>,
    /// Version of the service.
    #[serde(rename = "Version", skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(rename = "Status", skip_serializing_if = "Option::is_none")]
    pub status: Option<NmxServiceStatus>,
    /// Additional information about the status.
    #[serde(rename = "StatusInfo", skip_serializing_if = "Option::is_none")]
    pub status_info: Option<String>,
    #[serde(rename = "RegisteredAt", skip_serializing_if = "Option::is_none")]
    pub registered_at: Option<String>,
    #[serde(rename = "UpSince", skip_serializing_if = "Option::is_none")]
    pub up_since: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
pub enum NmxServiceStatus {
    #[serde(rename = "UP")]
    NmxServiceStatusUp,
    #[serde(rename = "DRAINING")]
    NmxServiceStatusDraining,
    #[serde(rename = "REMOVED")]
    NmxServiceStatusRemoved,
    #[serde(rename = "DOWN")]
    NmxServiceStatusDown,
    #[serde(rename = "RECONNECTING")]
    NmxServiceStatusReconnecting,
    #[serde(rename = "UNREACHABLE")]
    NmxServiceStatusUnreachable,
    #[serde(rename = "IN_PROGRESS")]
    NmxServiceStatusInProgress,
    #[serde(rename = "ERROR")]
    NmxServiceStatusError,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
pub enum NmxServiceType {
    #[serde(rename = "TELEMETRY")]
    NmxServiceTypeTelemetry,
    #[serde(rename = "CONTROLLER")]
    NmxServiceTypeController,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Pagination {
    /// Start offset of the results
    #[serde(rename = "offset", skip_serializing_if = "Option::is_none")]
    pub offset: Option<i32>,
    /// Items per page
    #[serde(rename = "limit", skip_serializing_if = "Option::is_none")]
    pub limit: Option<i32>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OperationRequest {
    /// The HTTP method for the original request
    #[serde(rename = "Method")]
    pub method: OperationRequestMethod,
    /// The URI for the original request
    #[serde(rename = "URI")]
    pub uri: String,
    /// JSON request body
    #[serde(
        rename = "Body",
        default,
        with = "::serde_with::rust::double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub body: Option<Option<serde_json::Value>>,
    /// Indicates if the operation can be cancelled
    #[serde(rename = "Cancellable")]
    pub cancellable: bool,
}

/// The HTTP method for the original request
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
pub enum OperationRequestMethod {
    #[serde(rename = "get")]
    Get,
    #[serde(rename = "delete")]
    Delete,
    #[serde(rename = "post")]
    Post,
    #[serde(rename = "put")]
    Put,
    #[serde(rename = "patch")]
    Patch,
}

/// Operation : an operations represents an async request and reflects its progress of execution
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Operation {
    /// System unique identifier.
    #[serde(rename = "ID")]
    pub id: String,
    #[serde(rename = "CreatedAt")]
    pub created_at: String,
    #[serde(rename = "UpdatedAt")]
    pub updated_at: String,
    #[serde(rename = "Status")]
    pub status: OperationStatus,
    /// The percentage of completion for the operation
    #[serde(rename = "Percentage")]
    pub percentage: f32,
    /// Human-readable description of the current step of execution in the operation
    #[serde(rename = "CurrentStep")]
    pub current_step: String,
    #[serde(rename = "Request")]
    pub request: Box<OperationRequest>,
    #[serde(rename = "Result", skip_serializing_if = "Option::is_none")]
    pub result: Option<Box<OperationResult>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
pub enum OperationStatus {
    #[serde(rename = "pending")]
    Pending,
    #[serde(rename = "in-progress")]
    InProgress,
    #[serde(rename = "failed")]
    Failed,
    #[serde(rename = "completed")]
    Completed,
    #[serde(rename = "cancelled")]
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OperationResult {
    /// The result data of the operation
    #[serde(
        rename = "Data",
        default,
        with = "::serde_with::rust::double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub data: Option<Option<serde_json::Value>>,
    /// Concise textual error code
    #[serde(rename = "Error", skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Human-readable details of the operation result
    #[serde(rename = "Details", skip_serializing_if = "Option::is_none")]
    pub details: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Partition {
    /// System unique identifier.
    #[serde(rename = "ID")]
    pub id: String,
    /// Partition ID as identified by the network
    #[serde(rename = "PartitionID")]
    pub partition_id: i32,
    /// Name of the Partition.
    #[serde(rename = "Name")]
    pub name: String,
    #[serde(rename = "Type")]
    pub r#type: PartitionType,
    #[serde(rename = "Health")]
    pub health: PartitionHealth,
    #[serde(rename = "Members")]
    pub members: Box<PartitionMembers>,
    #[serde(rename = "CreatedAt")]
    pub created_at: String,
    #[serde(rename = "UpdatedAt")]
    pub updated_at: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PartitionMembersOneOfInner {
    /// Universally Unique Identifier.
    #[serde(rename = "DomainUUID")]
    pub domain_uuid: uuid::Uuid,
    /// The unique identifier of the chassis
    #[serde(rename = "ChassisID")]
    pub chassis_id: i32,
    /// The unique identifier of the slot
    #[serde(rename = "SlotID")]
    pub slot_id: i32,
    /// The unique identifier of the host
    #[serde(rename = "HostID")]
    pub host_id: i32,
    /// ID of the compute/switch tray within the compute/switch trays group in the chassis, a number from 0
    #[serde(rename = "DeviceID")]
    pub device_id: i32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PartitionMembers {
    Ids(Vec<String>),
    InnerStructs(Vec<PartitionMembersOneOfInner>),
    /// it is to handle partitions without any gpus.
    Empty(Option<Vec<String>>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
pub enum PartitionType {
    #[serde(rename = "ID_BASED")]
    PartitionTypeIDBased,
    #[serde(rename = "LOCATION_BASED")]
    PartitionTypeLocationBased,
    #[serde(rename = "UNDEFINED")]
    PartitionTypeUndefined,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
pub enum PartitionHealth {
    #[serde(rename = "HEALTHY")]
    PartitionHealthHealthy,
    #[serde(rename = "DEGRADED")]
    PartitionHealthDegraded,
    #[serde(rename = "DEGRADED_BANDWIDTH")]
    PartitionHealthDegradedBandwidth,
    #[serde(rename = "UNHEALTHY")]
    PartitionHealthUnhealthy,
    #[serde(rename = "NEW")]
    PartitionHealthIntermediate,
    #[serde(rename = "UNKNOWN")]
    PartitionHealthUnknown,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SwitchNode {
    /// System unique identifier.
    #[serde(rename = "ID", skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(rename = "Name", skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(rename = "Description", skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(rename = "CreatedAt", skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(rename = "UpdatedAt", skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    /// Universally Unique Identifier.
    #[serde(rename = "DomainUUID", skip_serializing_if = "Option::is_none")]
    pub domain_uuid: Option<uuid::Uuid>,
    #[serde(rename = "LocationInfo", skip_serializing_if = "Option::is_none")]
    pub location_info: Option<Box<LocationInfo>>,
    #[serde(rename = "Health", skip_serializing_if = "Option::is_none")]
    pub health: Option<SwitchHealth>,
    #[serde(rename = "SwitchIDList", skip_serializing_if = "Option::is_none")]
    pub switch_id_list: Option<Vec<String>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
pub enum SwitchHealth {
    #[serde(rename = "UNKNOWN")]
    SwitchHealthUnknown,
    #[serde(rename = "HEALTHY")]
    SwitchHealthHealthy,
    #[serde(rename = "MISSING_NVLINK")]
    SwitchHealthMissingNvlink,
    #[serde(rename = "UNHEALTHY")]
    SwitchHealthUnhealthy,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(test, serde(deny_unknown_fields))]
pub struct Port {
    /// System unique identifier.
    #[serde(rename = "ID", skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(rename = "CreatedAt", skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(rename = "UpdatedAt", skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    /// Universally Unique Identifier.
    #[serde(rename = "DomainUUID", skip_serializing_if = "Option::is_none")]
    pub domain_uuid: Option<uuid::Uuid>,
    #[serde(rename = "LocationInfo", skip_serializing_if = "Option::is_none")]
    pub location_info: Option<Box<LocationInfo>>,
    #[serde(rename = "Type", skip_serializing_if = "Option::is_none")]
    pub r#type: Option<PortType>,
    #[serde(rename = "PortUID")]
    pub port_uid: i32,
    #[serde(rename = "PortNum")]
    pub port_num: i32,
    #[serde(rename = "PeerPortDeviceUID")]
    pub peer_port_device_uid: i32,
    #[serde(rename = "PeerPortNum")]
    pub peer_port_num: i32,
    #[serde(rename = "Rail")]
    pub rail: i32,
    #[serde(rename = "Plane")]
    pub plane: i32,
    #[serde(rename = "PhysicalState")]
    pub physical_state: PortPhysicalState,
    #[serde(rename = "LogicalState")]
    pub logical_state: PortLogicalState,
    #[serde(rename = "SubnetPrefix")]
    pub subnet_prefix: i32,
    #[serde(rename = "IsSDNPort")]
    pub is_sdn_port: bool,
    #[serde(rename = "ContainAndDrain")]
    pub contain_and_drain: bool,
    #[serde(rename = "CageNum", skip_serializing_if = "Option::is_none")]
    pub cage_num: Option<i32>,
    #[serde(rename = "CagePortNum", skip_serializing_if = "Option::is_none")]
    pub cage_port_num: Option<i32>,
    #[serde(rename = "CageSplitPortNum", skip_serializing_if = "Option::is_none")]
    pub cage_split_port_num: Option<i32>,
    #[serde(rename = "BaseLID", skip_serializing_if = "Option::is_none")]
    pub base_lid: Option<i32>,
    #[serde(rename = "SystemPortNum", skip_serializing_if = "Option::is_none")]
    pub system_port_num: Option<i32>,
    #[serde(rename = "ComputePortNum", skip_serializing_if = "Option::is_none")]
    pub compute_port_num: Option<i32>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
pub enum PortLogicalState {
    #[serde(rename = "NO_CHANGE")]
    PortLogicalStateNoChange,
    #[serde(rename = "STATE_DOWN")]
    PortLogicalStateDown,
    #[serde(rename = "STATE_INIT")]
    PortLogicalStateInit,
    #[serde(rename = "STATE_ARMED")]
    PortLogicalStateArmed,
    #[serde(rename = "STATE_ACTIVE")]
    PortLogicalStateActive,
    #[serde(rename = "STATE_ACT_DEFER")]
    PortLogicalStateActDefer,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
pub enum PortPhysicalState {
    #[serde(rename = "NO_CHANGE")]
    PortPhysicalStateNoChange,
    #[serde(rename = "SLEEP")]
    PortPhysicalStateSleep,
    #[serde(rename = "POLLING")]
    PortPhysicalStatePolling,
    #[serde(rename = "DISABLED")]
    PortPhysicalStateDisabled,
    #[serde(rename = "PORTCONFTRAIN")]
    PortPhysicalStatePortConfTrain,
    #[serde(rename = "LINKUP")]
    PortPhysicalStateLinkUp,
    #[serde(rename = "LINKERRRECOVER")]
    PortPhysicalStateLinkErrRecover,
    #[serde(rename = "PHYTEST")]
    PortPhysicalStatePhyTest,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
pub enum PortType {
    #[serde(rename = "UNDEFINED")]
    PortTypeUndefined,
    #[serde(rename = "GPU")]
    PortTypeGPU,
    #[serde(rename = "SWITCH_ACCESS")]
    PortTypeSwitchAccess,
    #[serde(rename = "SWITCH_TRUNK")]
    PortTypeSwitchTrunk,
    #[serde(rename = "FNM")]
    PortTypeFNM,
    #[serde(rename = "HCA")]
    PortTypeHCA,
}

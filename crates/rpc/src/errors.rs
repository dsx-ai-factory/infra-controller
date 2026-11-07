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
use std::backtrace::{Backtrace, BacktraceStatus};

use tonic::Status;

/// RpcDataConversionError enumerates errors that can occur when
/// converting from the RPC data format into the internal data model.
#[derive(Debug, thiserror::Error)]
pub enum RpcDataConversionError {
    #[error("field {0} is not valid base64")]
    InvalidBase64Data(&'static str),
    #[error("virtual function ID of value {0} is not in the expected range 0-15")]
    InvalidVirtualFunctionId(usize),
    #[error("IP address {0} is not valid")]
    InvalidIpAddress(String),
    #[error("MAC address {0} is not valid")]
    InvalidMacAddress(String),
    #[error("version string {0} is not valid")]
    InvalidConfigVersion(String),
    #[error("machine ID {0} is not valid")]
    InvalidMachineId(String),
    #[error("network security group ID {0} is not valid")]
    InvalidNetworkSecurityGroupId(String),
    #[error("instance type ID {0} is not valid")]
    InvalidInstanceTypeId(String),
    #[error("compute allocation ID {0} is not valid")]
    InvalidComputeAllocationId(String),
    #[error("timestamp {0} is not valid")]
    InvalidTimestamp(String),
    #[error("tenant org {0} is not valid")]
    InvalidTenantOrg(String),
    #[error("interface function type {0} is not valid")]
    InvalidInterfaceFunctionType(i32),
    #[error("invalid UUID for field of type {0}: {1}")]
    InvalidUuid(&'static str, String),
    #[error("invalid value {1} for {0}")]
    InvalidValue(String, String),
    #[error("argument is invalid: {0}")]
    InvalidArgument(String),
    #[error("argument {0} is missing")]
    MissingArgument(&'static str),
    #[error(
        "A unique identifier was specified for a new object.  when creating a new object of type {0}, do not specify an identifier"
    )]
    IdentifierSpecifiedForNewObject(String),
    #[error("machine state {0} is invalid")]
    InvalidMachineState(String),
    #[error("invalid NetworkSegmentType {0} is received")]
    InvalidNetworkSegmentType(i32),
    #[error("pci device info {0} is invalid")]
    InvalidPciDeviceInfo(String),
    #[error("VpcVirtualizationType {0} is invalid")]
    InvalidVpcVirtualizationType(i32),
    #[error("invalid enum value received for critical error type: {0}")]
    InvalidCriticalErrorType(i32),
    #[error("PowerState {0} is not valid")]
    InvalidPowerState(i32),
    #[error("instance ID {0} is not valid")]
    InvalidInstanceId(String),
    #[error("remediation ID {0} is not valid")]
    InvalidRemediationId(String),
    #[error("VPC ID {0} is not valid")]
    InvalidVpcId(String),
    #[error("VPC peering ID {0} is not valid")]
    InvalidVpcPeeringId(String),
    #[error("IB partition ID {0} is not valid")]
    InvalidIbPartitionId(String),
    #[error("PowerShelf ID {0} is not valid")]
    InvalidPowerShelfId(String),
    #[error("switch ID {0} is not valid")]
    InvalidSwitchId(String),
    #[error("network segment ID {0} is not valid")]
    InvalidNetworkSegmentId(String),
    #[error("CIDR {0} is not valid")]
    InvalidCidr(String),
    #[error("label is not valid: {0}")]
    InvalidLabel(String),
    #[error("invalid DnsResourceRecordType: {0}")]
    InvalidDnsResourceRecordType(String),
    #[error("invalid soa record: {0}")]
    InvalidSoaRecord(String),
    #[error("could not obtain object from json: {0}")]
    JsonConversionFailure(String),
    #[error("JSON parse failure - {0}")]
    JsonParseError(#[from] serde_json::Error),
    #[error("unable to parse string into IP network: {0}")]
    NetworkParseError(#[from] ipnetwork::IpNetworkError),
    #[error("tenant routing profile type {0} is not valid")]
    InvalidRoutingProfileType(String),
    #[error("NVL partition ID {0} is not valid")]
    InvalidNvlPartitionId(String),
    #[error("logical partition ID {0} is not valid")]
    InvalidLogicalPartitionId(String),
    #[error("total memory device count {0} exceeds maximum of {1}")]
    MemoryDeviceCountExceeded(u64, u32),
}

impl From<RpcDataConversionError> for tonic::Status {
    fn from(from: RpcDataConversionError) -> Self {
        // If env RUST_BACKTRACE is set extract handler and err location
        // If it's not set `Backtrace::capture()` is very cheap to call
        let b = Backtrace::capture();
        let printed = if b.status() == BacktraceStatus::Captured {
            let b_str = b.to_string();
            let f = b_str
                .lines()
                .skip(1)
                .skip_while(|l| !l.contains("carbide"))
                .take(2)
                .collect::<Vec<&str>>();
            if f.len() == 2 {
                let handler = f[0].trim();
                let location = f[1].trim().replace("at ", "");
                tracing::error!(
                    error = %from,
                    error_location = %location,
                    handler,
                    "RPC error conversion",
                );
                true
            } else {
                false
            }
        } else {
            false
        };

        if !printed {
            tracing::error!(error = %from, "RPC error conversion");
        }

        Status::invalid_argument(from.to_string())
    }
}

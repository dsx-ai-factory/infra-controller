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

use carbide_utils::none_if_empty::NoneIfEmpty;
use model::instance_type::{InstanceType, InstanceTypeMachineCapabilityFilter};

use crate::errors::RpcDataConversionError;
use crate::{common as rpc_common, forge as rpc};

impl TryFrom<rpc::InstanceTypeMachineCapabilityFilterAttributes>
    for InstanceTypeMachineCapabilityFilter
{
    type Error = RpcDataConversionError;

    fn try_from(
        cap: rpc::InstanceTypeMachineCapabilityFilterAttributes,
    ) -> Result<Self, Self::Error> {
        Ok(InstanceTypeMachineCapabilityFilter {
            capability_type: cap.capability_type().try_into()?,
            name: cap.name,
            frequency: cap.frequency,
            capacity: cap.capacity,
            vendor: cap.vendor,
            count: cap.count,
            hardware_revision: cap.hardware_revision,
            cores: cap.cores,
            threads: cap.threads,
            inactive_devices: cap.inactive_devices.map(|l| l.items),
            device_type: cap
                .device_type
                .map(|dt| {
                    rpc::MachineCapabilityDeviceType::try_from(dt)
                        .map_err(|_| {
                            RpcDataConversionError::InvalidValue(
                                "MachineCapabilityDeviceType".to_string(),
                                dt.to_string(),
                            )
                        })
                        .and_then(|rpc_dt| rpc_dt.try_into())
                })
                .transpose()?,
        })
    }
}

impl TryFrom<InstanceTypeMachineCapabilityFilter>
    for rpc::InstanceTypeMachineCapabilityFilterAttributes
{
    type Error = RpcDataConversionError;

    fn try_from(cap: InstanceTypeMachineCapabilityFilter) -> Result<Self, Self::Error> {
        Ok(rpc::InstanceTypeMachineCapabilityFilterAttributes {
            capability_type: rpc::MachineCapabilityType::from(cap.capability_type).into(),
            name: cap.name,
            frequency: cap.frequency,
            capacity: cap.capacity,
            vendor: cap.vendor,
            count: cap.count,
            hardware_revision: cap.hardware_revision,
            cores: cap.cores,
            threads: cap.threads,
            inactive_devices: cap
                .inactive_devices
                .map(|l| rpc_common::Uint32List { items: l }),
            device_type: cap
                .device_type
                .map(|dt| rpc::MachineCapabilityDeviceType::from(dt).into()),
        })
    }
}

impl TryFrom<InstanceType> for rpc::InstanceType {
    type Error = RpcDataConversionError;

    fn try_from(inst_type: InstanceType) -> Result<Self, Self::Error> {
        let mut desired_capabilities =
            Vec::<rpc::InstanceTypeMachineCapabilityFilterAttributes>::new();

        for cap_attrs in inst_type.desired_capabilities {
            desired_capabilities.push(cap_attrs.try_into()?);
        }

        let attributes = rpc::InstanceTypeAttributes {
            desired_capabilities,
        };

        Ok(rpc::InstanceType {
            id: inst_type.id.to_string(),
            version: inst_type.version.to_string(),
            attributes: Some(attributes),
            created_at: Some(inst_type.created.to_string()),
            metadata: Some(rpc::Metadata {
                name: inst_type.metadata.name,
                description: inst_type.metadata.description,
                labels: inst_type
                    .metadata
                    .labels
                    .iter()
                    .map(|(key, value)| rpc::Label {
                        key: key.to_owned(),
                        value: value.to_owned().none_if_empty(),
                    })
                    .collect(),
            }),
            allocation_stats: None,
        })
    }
}

/* ********************************** */
/*              Tests                 */
/* ********************************** */

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use carbide_test_support::Case;
    use carbide_test_support::Outcome::*;
    use config_version::ConfigVersion;
    use model::machine::capabilities;
    use model::metadata::Metadata;

    use super::*;
    use crate::forge as rpc;

    // Verify that an internal InstanceType converts into the protobuf
    // InstanceType message. The error (RpcDataConversionError) is not the
    // contract here, so the row only asserts the converted value.
    #[test]
    fn model_instance_type_converts_to_rpc() {
        let version = ConfigVersion::initial();

        let req_type = rpc::InstanceType {
            id: "test_id".to_string(),
            version: version.to_string(),
            metadata: Some(rpc::Metadata {
                name: "fancy name".to_string(),
                description: "".to_string(),
                labels: vec![],
            }),
            allocation_stats: None,
            attributes: Some(rpc::InstanceTypeAttributes {
                desired_capabilities: vec![rpc::InstanceTypeMachineCapabilityFilterAttributes {
                    capability_type: rpc::MachineCapabilityType::CapTypeCpu.into(),
                    name: Some("pentium 4 HT".to_string()),
                    frequency: Some("1.3 GHz".to_string()),
                    capacity: Some("9001 GB".to_string()),
                    vendor: Some("intel".to_string()),
                    count: Some(1),
                    hardware_revision: Some("rev 9001".to_string()),
                    cores: Some(1),
                    threads: Some(2),
                    inactive_devices: Some(rpc_common::Uint32List { items: vec![2, 4] }),
                    device_type: Some(rpc::MachineCapabilityDeviceType::Unknown as i32),
                }],
            }),
            created_at: Some("2023-01-01 00:00:00 UTC".to_string()),
        };

        let inst_type = InstanceType {
            id: "test_id".parse().unwrap(),
            deleted: None,
            created: "2023-01-01 00:00:00 UTC".parse().unwrap(),
            version,
            metadata: Metadata {
                name: "fancy name".to_string(),
                description: "".to_string(),
                labels: HashMap::new(),
            },
            desired_capabilities: vec![InstanceTypeMachineCapabilityFilter {
                capability_type: rpc::MachineCapabilityType::CapTypeCpu.try_into().unwrap(),
                name: Some("pentium 4 HT".to_string()),
                frequency: Some("1.3 GHz".to_string()),
                capacity: Some("9001 GB".to_string()),
                vendor: Some("intel".to_string()),
                count: Some(1),
                hardware_revision: Some("rev 9001".to_string()),
                cores: Some(1),
                threads: Some(2),
                inactive_devices: Some(vec![2, 4]),
                device_type: Some(capabilities::MachineCapabilityDeviceType::Unknown),
            }],
        };

        Case {
            scenario: "internal instance type to protobuf message",
            input: inst_type,
            expect: Yields(req_type),
        }
        .check(|it| rpc::InstanceType::try_from(it).map_err(drop));
    }
}

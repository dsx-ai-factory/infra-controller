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
use std::net::IpAddr;

use mac_address::MacAddress;
use model::expected_machine::{
    BmcIpAllocationType, ExpectedInterface, ExpectedInterfaceIpAllocation, ExpectedInterfaceRole,
    ExpectedMachine, ExpectedMachineData, ExpectedMachineRequest, HostDpuPolicy,
    HostLifecycleProfile, LegacyHostBmcOverrides, LinkedExpectedMachine, UnexpectedMachine,
};
use model::metadata::Metadata;
use model::network_segment::NetworkSegmentType;
use uuid::Uuid;

use crate as rpc;
use crate::errors::RpcDataConversionError;
use crate::model::RpcTryFrom;

impl From<HostDpuPolicy> for rpc::forge::DpuMode {
    fn from(policy: HostDpuPolicy) -> Self {
        match policy {
            HostDpuPolicy::Manage => rpc::forge::DpuMode::DpuMode,
            HostDpuPolicy::Nic => rpc::forge::DpuMode::NicMode,
            HostDpuPolicy::Ignore => rpc::forge::DpuMode::NoDpu,
        }
    }
}

impl From<rpc::forge::DpuMode> for HostDpuPolicy {
    fn from(mode: rpc::forge::DpuMode) -> Self {
        match mode {
            rpc::forge::DpuMode::DpuMode => HostDpuPolicy::Manage,
            rpc::forge::DpuMode::NicMode => HostDpuPolicy::Nic,
            rpc::forge::DpuMode::NoDpu => HostDpuPolicy::Ignore,
            // Unspecified means "use the default", which preserves behavior
            // for clients that omit the compatibility field.
            rpc::forge::DpuMode::Unspecified => HostDpuPolicy::default(),
        }
    }
}

fn host_dpu_policy_from_rpc(dpu_mode: Option<i32>) -> HostDpuPolicy {
    dpu_mode
        .and_then(|value| rpc::forge::DpuMode::try_from(value).ok())
        .map(HostDpuPolicy::from)
        .unwrap_or_default()
}

fn host_dpu_policy_to_rpc(policy: HostDpuPolicy) -> Option<i32> {
    match policy {
        HostDpuPolicy::Manage => None,
        policy => Some(rpc::forge::DpuMode::from(policy) as i32),
    }
}

/// Convert the model role to its concrete protobuf value.
///
/// Optional-field omission is handled separately so this conversion never
/// needs to treat Host as an absent role.
impl From<ExpectedInterfaceRole> for rpc::forge::ExpectedInterfaceRole {
    fn from(role: ExpectedInterfaceRole) -> Self {
        match role {
            ExpectedInterfaceRole::Host => Self::Host,
            ExpectedInterfaceRole::DpuOs => Self::DpuOs,
            ExpectedInterfaceRole::DpuBmc => Self::DpuBmc,
            ExpectedInterfaceRole::HostBmc => Self::HostBmc,
        }
    }
}

/// Convert a concrete protobuf role to its model role.
///
/// `Unspecified` maps to Host for clients that predate the role field.
impl From<rpc::forge::ExpectedInterfaceRole> for ExpectedInterfaceRole {
    fn from(role: rpc::forge::ExpectedInterfaceRole) -> Self {
        match role {
            rpc::forge::ExpectedInterfaceRole::Unspecified
            | rpc::forge::ExpectedInterfaceRole::Host => Self::Host,
            rpc::forge::ExpectedInterfaceRole::DpuOs => Self::DpuOs,
            rpc::forge::ExpectedInterfaceRole::DpuBmc => Self::DpuBmc,
            rpc::forge::ExpectedInterfaceRole::HostBmc => Self::HostBmc,
        }
    }
}

/// Decode the optional role field while preserving its compatibility default.
///
/// Both a missing field and `Unspecified` become Host. Unknown protobuf values
/// remain errors so a newer role is not silently treated as a Host interface.
fn expected_interface_role_from_rpc(
    role: Option<i32>,
) -> Result<ExpectedInterfaceRole, RpcDataConversionError> {
    let Some(role) = role else {
        return Ok(ExpectedInterfaceRole::default());
    };
    rpc::forge::ExpectedInterfaceRole::try_from(role)
        .map(Into::into)
        .map_err(|_| {
            RpcDataConversionError::InvalidArgument(format!(
                "invalid expected interface role: {role}"
            ))
        })
}

/// Encode a role without adding the default Host field to legacy responses.
///
/// Non-Host roles must remain explicit because they select different endpoint
/// behavior.
fn expected_interface_role_to_rpc(role: ExpectedInterfaceRole) -> Option<i32> {
    match role {
        ExpectedInterfaceRole::Host => None,
        role => Some(rpc::forge::ExpectedInterfaceRole::from(role) as i32),
    }
}

/// Convert a resolved model allocation policy to its protobuf value.
impl From<ExpectedInterfaceIpAllocation> for rpc::forge::ExpectedInterfaceIpAllocation {
    fn from(policy: ExpectedInterfaceIpAllocation) -> Self {
        match policy {
            ExpectedInterfaceIpAllocation::Dynamic => Self::Dynamic,
            ExpectedInterfaceIpAllocation::Fixed => Self::Fixed,
            ExpectedInterfaceIpAllocation::Retained => Self::Retained,
        }
    }
}

/// Convert a concrete protobuf allocation policy to its resolved model value.
///
/// `Unspecified` has no resolved value on its own. The containing interface
/// uses its role and `fixed_ip` to infer the compatibility policy.
impl TryFrom<rpc::forge::ExpectedInterfaceIpAllocation> for ExpectedInterfaceIpAllocation {
    type Error = RpcDataConversionError;

    fn try_from(policy: rpc::forge::ExpectedInterfaceIpAllocation) -> Result<Self, Self::Error> {
        match policy {
            rpc::forge::ExpectedInterfaceIpAllocation::Unspecified => {
                Err(RpcDataConversionError::InvalidArgument(
                    "expected interface IP allocation is unspecified".into(),
                ))
            }
            rpc::forge::ExpectedInterfaceIpAllocation::Dynamic => Ok(Self::Dynamic),
            rpc::forge::ExpectedInterfaceIpAllocation::Fixed => Ok(Self::Fixed),
            rpc::forge::ExpectedInterfaceIpAllocation::Retained => Ok(Self::Retained),
        }
    }
}

/// Decode the optional allocation field without resolving an omitted policy.
///
/// Missing and `Unspecified` both remain `None`. Resolution then uses the
/// interface role and `fixed_ip`; an explicit Dynamic policy still rejects a
/// configured address.
fn expected_interface_ip_allocation_from_rpc(
    policy: Option<i32>,
) -> Result<Option<ExpectedInterfaceIpAllocation>, RpcDataConversionError> {
    let Some(policy) = policy else {
        return Ok(None);
    };
    let policy = rpc::forge::ExpectedInterfaceIpAllocation::try_from(policy).map_err(|_| {
        RpcDataConversionError::InvalidArgument(format!(
            "invalid expected interface IP allocation: {policy}"
        ))
    })?;
    match policy {
        rpc::forge::ExpectedInterfaceIpAllocation::Unspecified => Ok(None),
        policy => ExpectedInterfaceIpAllocation::try_from(policy).map(Some),
    }
}

/// Encode only explicitly configured allocation policies.
///
/// An omitted model policy stays absent so old `fixed_ip` declarations keep
/// their Fixed inference instead of being rewritten as explicit Dynamic.
fn expected_interface_ip_allocation_to_rpc(
    policy: Option<ExpectedInterfaceIpAllocation>,
) -> Option<i32> {
    policy.map(|policy| rpc::forge::ExpectedInterfaceIpAllocation::from(policy) as i32)
}

impl From<BmcIpAllocationType> for rpc::forge::BmcIpAllocationType {
    fn from(mode: BmcIpAllocationType) -> Self {
        match mode {
            BmcIpAllocationType::Auto => rpc::forge::BmcIpAllocationType::Auto,
            BmcIpAllocationType::Dynamic => rpc::forge::BmcIpAllocationType::Dynamic,
            BmcIpAllocationType::Fixed => rpc::forge::BmcIpAllocationType::Fixed,
            BmcIpAllocationType::Retained => rpc::forge::BmcIpAllocationType::Retained,
        }
    }
}

impl From<rpc::forge::BmcIpAllocationType> for BmcIpAllocationType {
    fn from(mode: rpc::forge::BmcIpAllocationType) -> Self {
        match mode {
            rpc::forge::BmcIpAllocationType::Auto => BmcIpAllocationType::Auto,
            rpc::forge::BmcIpAllocationType::Dynamic => BmcIpAllocationType::Dynamic,
            rpc::forge::BmcIpAllocationType::Fixed => BmcIpAllocationType::Fixed,
            rpc::forge::BmcIpAllocationType::Retained => BmcIpAllocationType::Retained,
            // Unspecified (0) or any unknown value means "use the default",
            // which preserves behavior for old clients that don't send the
            // field at all.
            rpc::forge::BmcIpAllocationType::Unspecified => BmcIpAllocationType::default(),
        }
    }
}

impl TryFrom<rpc::forge::ExpectedMachineRequest> for ExpectedMachineRequest {
    type Error = RpcDataConversionError;

    fn try_from(rpc: rpc::forge::ExpectedMachineRequest) -> Result<Self, Self::Error> {
        let id = rpc
            .id
            .map(|u| {
                Uuid::parse_str(&u.value)
                    .map_err(|_| RpcDataConversionError::InvalidArgument(u.value))
            })
            .transpose()?;
        let bmc_mac_address = if rpc.bmc_mac_address.is_empty() {
            None
        } else {
            Some(
                MacAddress::try_from(rpc.bmc_mac_address.as_str())
                    .map_err(|_| RpcDataConversionError::InvalidMacAddress(rpc.bmc_mac_address))?,
            )
        };

        Ok(ExpectedMachineRequest {
            id,
            bmc_mac_address,
        })
    }
}

impl From<ExpectedInterface> for rpc::forge::ExpectedInterface {
    fn from(expected_interface: ExpectedInterface) -> Self {
        rpc::forge::ExpectedInterface {
            mac_address: expected_interface.mac_address.to_string(),
            nic_type: expected_interface.nic_type,
            fixed_ip: expected_interface.fixed_ip.map(|ip| ip.to_string()),
            fixed_mask: expected_interface.fixed_mask,
            fixed_gateway: expected_interface.fixed_gateway.map(|ip| ip.to_string()),
            primary: expected_interface.primary,
            network_segment_type: expected_interface
                .network_segment_type
                .map(|segment_type| segment_type as i32),
            role: expected_interface_role_to_rpc(expected_interface.role),
            ip_allocation: expected_interface_ip_allocation_to_rpc(
                expected_interface.ip_allocation,
            ),
        }
    }
}

impl TryFrom<rpc::forge::ExpectedInterface> for ExpectedInterface {
    type Error = RpcDataConversionError;

    fn try_from(expected_interface: rpc::forge::ExpectedInterface) -> Result<Self, Self::Error> {
        let mac_address = expected_interface.mac_address.parse().map_err(|_| {
            RpcDataConversionError::InvalidMacAddress(expected_interface.mac_address.clone())
        })?;

        Ok(ExpectedInterface {
            mac_address,
            nic_type: expected_interface.nic_type,
            fixed_ip: match expected_interface.fixed_ip.as_deref() {
                None | Some("") => None,
                Some(ip) => Some(ip.parse::<IpAddr>().map_err(|_| {
                    RpcDataConversionError::InvalidArgument(format!("Invalid fixed IP: {ip}"))
                })?),
            },
            fixed_mask: expected_interface.fixed_mask,
            fixed_gateway: match expected_interface.fixed_gateway.as_deref() {
                None | Some("") => None,
                Some(ip) => Some(ip.parse::<IpAddr>().map_err(|_| {
                    RpcDataConversionError::InvalidArgument(format!("Invalid fixed gateway: {ip}"))
                })?),
            },
            primary: expected_interface.primary,
            network_segment_type: expected_interface
                .network_segment_type
                .map(NetworkSegmentType::rpc_try_from)
                .transpose()?,
            role: expected_interface_role_from_rpc(expected_interface.role)?,
            ip_allocation: expected_interface_ip_allocation_from_rpc(
                expected_interface.ip_allocation,
            )?,
        })
    }
}

impl From<ExpectedMachine> for rpc::forge::ExpectedMachine {
    fn from(expected_machine: ExpectedMachine) -> Self {
        let has_stored_host_bmc = expected_machine
            .data
            .interfaces
            .iter()
            .any(|interface| interface.role.is_host_bmc());
        let bmc_ip_allocation = expected_machine
            .compatibility_bmc_ip_allocation()
            .map(rpc::forge::BmcIpAllocationType::from)
            .map(|allocation| allocation as i32);
        let effective_host_bmc = has_stored_host_bmc
            .then(|| rpc::forge::ExpectedInterface::from(expected_machine.effective_host_bmc()));
        let interfaces = expected_machine
            .data
            .interfaces
            .into_iter()
            .filter(|interface| !interface.role.is_host_bmc())
            .map(rpc::forge::ExpectedInterface::from)
            .chain(effective_host_bmc)
            .collect::<Vec<_>>();
        rpc::forge::ExpectedMachine {
            id: expected_machine.id.map(|u| crate::common::Uuid {
                value: u.to_string(),
            }),
            bmc_mac_address: expected_machine.bmc_mac_address.to_string(),
            bmc_username: expected_machine.data.bmc_username,
            bmc_password: expected_machine.data.bmc_password,
            chassis_serial_number: expected_machine.data.serial_number,
            fallback_dpu_serial_numbers: expected_machine.data.fallback_dpu_serial_numbers,
            metadata: Some(expected_machine.data.metadata.into()),
            sku_id: expected_machine.data.sku_id,
            rack_id: expected_machine.data.rack_id,
            host_nics: interfaces,
            default_pause_ingestion_and_poweron: expected_machine
                .data
                .default_pause_ingestion_and_poweron,
            // This should be removed after few releases.
            #[allow(deprecated)]
            dpf_enabled: expected_machine.data.dpf_enabled.unwrap_or(true),
            is_dpf_enabled: expected_machine.data.dpf_enabled,
            // Optional configured BMC IP (proto optional string).
            bmc_ip_address: expected_machine
                .data
                .bmc_ip_address
                .map(|ip| ip.to_string()),
            bmc_retain_credentials: expected_machine.data.bmc_retain_credentials.filter(|&v| v),
            // Forge retains `dpu_mode` as its stable compatibility field. The
            // default policy remains represented by absence on the wire.
            dpu_mode: host_dpu_policy_to_rpc(expected_machine.data.dpu_policy),
            // An inferred Host BMC policy remains Auto on the compatibility
            // wire field, while an explicitly supplied legacy policy retains
            // its presence. This keeps both older read-modify-write clients
            // and explicit legacy inputs stable.
            bmc_ip_allocation,
            replace_host_nics: false,
            host_lifecycle_profile: (!expected_machine.data.host_lifecycle_profile.is_empty())
                .then_some(rpc::forge::HostLifecycleProfile {
                    disable_lockdown: expected_machine
                        .data
                        .host_lifecycle_profile
                        .disable_lockdown,
                }),
        }
    }
}

impl From<LinkedExpectedMachine> for rpc::forge::LinkedExpectedMachine {
    fn from(m: LinkedExpectedMachine) -> rpc::forge::LinkedExpectedMachine {
        rpc::forge::LinkedExpectedMachine {
            chassis_serial_number: m.serial_number,
            bmc_mac_address: m.bmc_mac_address.to_string(),
            interface_id: m.interface_id.map(|u| u.to_string()),
            explored_endpoint_address: m.address.map(|addr| addr.to_string()),
            machine_id: m.machine_id,
            expected_machine_id: m.expected_machine_id.map(|id| crate::common::Uuid {
                value: id.to_string(),
            }),
        }
    }
}

impl From<UnexpectedMachine> for rpc::forge::UnexpectedMachine {
    fn from(m: UnexpectedMachine) -> rpc::forge::UnexpectedMachine {
        rpc::forge::UnexpectedMachine {
            address: m.address.to_string(),
            bmc_mac_address: m.bmc_mac_address.to_string(),
            machine_id: m.machine_id,
        }
    }
}

/// Capture which top-level BMC compatibility fields were present on an RPC
/// request before model conversion applies their defaults.
///
/// A present empty address is an explicit clear. Missing fields remain absent
/// so a nested Host BMC or the previous stored configuration can supply the
/// baseline. An explicit Unspecified nested policy becomes an Auto override so
/// it remains distinguishable from a policy omitted by an older client.
impl TryFrom<&rpc::forge::ExpectedMachine> for LegacyHostBmcOverrides {
    type Error = RpcDataConversionError;

    fn try_from(machine: &rpc::forge::ExpectedMachine) -> Result<Self, Self::Error> {
        let ip_address = machine
            .bmc_ip_address
            .as_deref()
            .map(|address| {
                if address.is_empty() {
                    Ok(None)
                } else {
                    address.parse::<IpAddr>().map(Some).map_err(|_| {
                        RpcDataConversionError::InvalidArgument(format!(
                            "Invalid BMC IP address: {address}"
                        ))
                    })
                }
            })
            .transpose()?;
        let nested_allocation_reset = machine.interfaces().iter().any(|interface| {
            interface.role == Some(rpc::forge::ExpectedInterfaceRole::HostBmc as i32)
                && interface.ip_allocation
                    == Some(rpc::forge::ExpectedInterfaceIpAllocation::Unspecified as i32)
        });
        let ip_allocation = machine
            .bmc_ip_allocation
            .map(|value| {
                rpc::forge::BmcIpAllocationType::try_from(value)
                    .map(BmcIpAllocationType::from)
                    .map_err(|_| {
                        RpcDataConversionError::InvalidArgument(format!(
                            "Invalid bmc_ip_allocation: {value}"
                        ))
                    })
            })
            .transpose()?
            .or_else(|| nested_allocation_reset.then_some(BmcIpAllocationType::Auto));

        Ok(Self {
            ip_address,
            ip_allocation,
            replace_interfaces: machine.replace_host_nics,
        })
    }
}

/// Parses gRPC `ExpectedMachine` into persisted model data, including optional `bmc_ip_address`
/// (empty or unset proto field becomes `None`; invalid strings fail conversion).
impl TryFrom<rpc::forge::ExpectedMachine> for ExpectedMachineData {
    type Error = RpcDataConversionError;

    fn try_from(em: rpc::forge::ExpectedMachine) -> Result<Self, Self::Error> {
        Ok(Self {
            bmc_username: em.bmc_username,
            bmc_password: em.bmc_password,
            serial_number: em.chassis_serial_number,
            fallback_dpu_serial_numbers: em.fallback_dpu_serial_numbers,
            sku_id: em.sku_id,
            metadata: metadata_from_request(em.metadata)?,
            interfaces: em
                .host_nics
                .into_iter()
                .map(ExpectedInterface::try_from)
                .collect::<Result<Vec<_>, _>>()?,
            rack_id: em.rack_id,
            default_pause_ingestion_and_poweron: em.default_pause_ingestion_and_poweron,
            dpf_enabled: em.is_dpf_enabled,
            bmc_ip_address: match em.bmc_ip_address.as_deref() {
                None | Some("") => None,
                Some(s) => Some(s.parse::<IpAddr>().map_err(|_| {
                    RpcDataConversionError::InvalidArgument(format!("Invalid BMC IP address: {s}"))
                })?),
            },
            bmc_retain_credentials: em.bmc_retain_credentials,
            // Translate the stable Forge compatibility field immediately into
            // the internal policy model. Missing, Unspecified, and unknown raw
            // values retain the historical default behavior.
            dpu_policy: host_dpu_policy_from_rpc(em.dpu_mode),
            // `bmc_ip_allocation` is optional on the wire; an unset field (and the
            // ::Unspecified discriminant) falls back to `BmcIpAllocationType::default()`
            // (::Auto), so old clients continue to behave as before. An unknown
            // discriminant is rejected rather than silently coerced to the default.
            bmc_ip_allocation: match em.bmc_ip_allocation {
                None => BmcIpAllocationType::default(),
                Some(i) => BmcIpAllocationType::from(
                    rpc::forge::BmcIpAllocationType::try_from(i).map_err(|_| {
                        RpcDataConversionError::InvalidArgument(format!(
                            "Invalid bmc_ip_allocation: {i}"
                        ))
                    })?,
                ),
            },
            host_lifecycle_profile: em
                .host_lifecycle_profile
                .map(|hlp| HostLifecycleProfile {
                    disable_lockdown: hlp.disable_lockdown,
                })
                .unwrap_or_default(),
        })
    }
}

/// If Metadata is retrieved as part of the ExpectedMachine creation, validate and use the Metadata
/// Otherwise assume empty Metadata
fn metadata_from_request(
    opt_metadata: Option<crate::forge::Metadata>,
) -> Result<Metadata, RpcDataConversionError> {
    Ok(match opt_metadata {
        None => Metadata {
            name: "".to_string(),
            description: "".to_string(),
            labels: Default::default(),
        },
        Some(m) => {
            // Note that this is unvalidated Metadata. It can contain non-ASCII names
            // and
            let m: Metadata = m.try_into()?;
            m.validate(false)
                .map_err(|e| RpcDataConversionError::InvalidArgument(e.to_string()))?;
            m
        }
    })
}

// default_uuid removed; ids are optional to support legacy rows with NULL ids

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::*;
    use carbide_test_support::{Check, check_values, scenarios, value_scenarios};
    use prost::Message;

    use super::*;

    /// The stable protobuf boundary maps directly onto the internal policy.
    #[test]
    fn rpc_dpu_mode_maps_to_model() {
        value_scenarios!(
            run = HostDpuPolicy::from;
            "unspecified maps to default" {
                rpc::forge::DpuMode::Unspecified => HostDpuPolicy::default(),
            }

            "DPU mode maps to manage" {
                rpc::forge::DpuMode::DpuMode => HostDpuPolicy::Manage,
            }

            "NIC mode maps to NIC policy" {
                rpc::forge::DpuMode::NicMode => HostDpuPolicy::Nic,
            }

            "no DPU maps to ignore" {
                rpc::forge::DpuMode::NoDpu => HostDpuPolicy::Ignore,
            }
        );
    }

    /// The host DPU policy default is Manage, which is what the Unspecified mapping
    /// above relies on.
    #[test]
    fn host_dpu_policy_default_is_manage() {
        assert_eq!(HostDpuPolicy::default(), HostDpuPolicy::Manage);
    }

    /// The policy refactor must not change the protobuf bytes consumed by
    /// existing clients: field 16 remains a varint and values 0 through 3 retain
    /// their legacy meanings.
    #[test]
    fn host_dpu_policy_preserves_legacy_wire_encoding() {
        check_values(
            [
                Check {
                    scenario: "unspecified remains 0",
                    input: rpc::forge::DpuMode::Unspecified,
                    expect: vec![0x80, 0x01, 0x00],
                },
                Check {
                    scenario: "manage remains DPU_MODE 1",
                    input: rpc::forge::DpuMode::DpuMode,
                    expect: vec![0x80, 0x01, 0x01],
                },
                Check {
                    scenario: "NIC policy remains NIC_MODE 2",
                    input: rpc::forge::DpuMode::NicMode,
                    expect: vec![0x80, 0x01, 0x02],
                },
                Check {
                    scenario: "ignore remains NO_DPU 3",
                    input: rpc::forge::DpuMode::NoDpu,
                    expect: vec![0x80, 0x01, 0x03],
                },
            ],
            |policy| {
                rpc::forge::ExpectedMachine {
                    dpu_mode: Some(policy as i32),
                    ..Default::default()
                }
                .encode_to_vec()
            },
        );
    }

    /// Reflection-backed clients continue to see only the pre-existing field,
    /// enum type, and value names. `HostDpuPolicy` remains an internal model.
    #[test]
    fn host_dpu_policy_descriptor_retains_compatibility_surface() {
        let descriptor_set =
            prost_types::FileDescriptorSet::decode(rpc::REFLECTION_API_SERVICE_DESCRIPTOR).unwrap();
        let forge = descriptor_set
            .file
            .iter()
            .find(|file| file.package.as_deref() == Some("forge"))
            .unwrap();
        let expected_machine = forge
            .message_type
            .iter()
            .find(|message| message.name.as_deref() == Some("ExpectedMachine"))
            .unwrap();
        let policy_field = expected_machine
            .field
            .iter()
            .find(|field| field.number == Some(16))
            .unwrap();

        assert_eq!(policy_field.name.as_deref(), Some("dpu_mode"));
        assert_eq!(policy_field.json_name.as_deref(), Some("dpuMode"));
        assert_eq!(policy_field.type_name.as_deref(), Some(".forge.DpuMode"));
        assert_eq!(
            policy_field
                .options
                .as_ref()
                .and_then(|options| options.deprecated),
            None
        );
        assert!(
            expected_machine
                .field
                .iter()
                .all(|field| field.name.as_deref() != Some("dpu_policy"))
        );

        let policy_enum = forge
            .enum_type
            .iter()
            .find(|enumeration| enumeration.name.as_deref() == Some("DpuMode"))
            .unwrap();
        let names_and_numbers = policy_enum
            .value
            .iter()
            .map(|value| (value.name.as_deref().unwrap(), value.number.unwrap()))
            .collect::<Vec<_>>();
        for legacy_value in [
            ("DPU_MODE_UNSPECIFIED", 0),
            ("DPU_MODE", 1),
            ("NIC_MODE", 2),
            ("NO_DPU", 3),
        ] {
            assert!(names_and_numbers.contains(&legacy_value));
        }

        assert!(
            forge
                .enum_type
                .iter()
                .all(|enumeration| enumeration.name.as_deref() != Some("HostDpuPolicy"))
        );
        assert_eq!(rpc::forge::DpuMode::DpuMode.as_str_name(), "DPU_MODE");
    }

    #[test]
    fn expected_machine_translates_rpc_dpu_mode_to_policy() {
        value_scenarios!(
            run = host_dpu_policy_from_rpc;
            "missing field defaults to manage" {
                None => HostDpuPolicy::Manage,
            }
            "unspecified defaults to manage" {
                Some(rpc::forge::DpuMode::Unspecified as i32) => HostDpuPolicy::Manage,
            }
            "DPU mode maps to manage" {
                Some(rpc::forge::DpuMode::DpuMode as i32) => HostDpuPolicy::Manage,
            }
            "NIC mode maps to NIC policy" {
                Some(rpc::forge::DpuMode::NicMode as i32) => HostDpuPolicy::Nic,
            }
            "no DPU maps to ignore" {
                Some(rpc::forge::DpuMode::NoDpu as i32) => HostDpuPolicy::Ignore,
            }
            "unknown value preserves the historical default" {
                Some(i32::MAX) => HostDpuPolicy::Manage,
            }
        );
    }

    #[test]
    fn expected_machine_emits_policy_through_compatibility_field() {
        scenarios!(
            run = |policy| {
                let expected_machine = ExpectedMachine {
                    id: None,
                    bmc_mac_address: "AA:BB:CC:DD:EE:FF".parse().map_err(drop)?,
                    data: ExpectedMachineData {
                        dpu_policy: policy,
                        ..Default::default()
                    },
                };
                let rpc_machine = rpc::forge::ExpectedMachine::from(expected_machine);
                Ok::<_, ()>(rpc_machine.dpu_mode)
            };
            "default manage remains absent" {
                HostDpuPolicy::Manage => Yields(None),
            }

            "NIC policy uses NIC_MODE" {
                HostDpuPolicy::Nic =>
                    Yields(Some(rpc::forge::DpuMode::NicMode as i32)),
            }

            "ignore uses NO_DPU" {
                HostDpuPolicy::Ignore =>
                    Yields(Some(rpc::forge::DpuMode::NoDpu as i32)),
            }
        );
    }

    #[test]
    fn rpc_dpu_mode_serializes_and_round_trips_legacy_values() {
        scenarios!(
            run = |mode| {
                let json = serde_json::to_string(&mode).map_err(drop)?;
                let recovered =
                    serde_json::from_str::<rpc::forge::DpuMode>(&json).map_err(drop)?;
                Ok::<_, ()>((json, recovered))
            };
            "unspecified" {
                rpc::forge::DpuMode::Unspecified => Yields((
                    r#""Unspecified""#.to_string(),
                    rpc::forge::DpuMode::Unspecified,
                )),
            }
            "DPU mode" {
                rpc::forge::DpuMode::DpuMode => Yields((
                    r#""DpuMode""#.to_string(),
                    rpc::forge::DpuMode::DpuMode,
                )),
            }
            "NIC mode" {
                rpc::forge::DpuMode::NicMode => Yields((
                    r#""NicMode""#.to_string(),
                    rpc::forge::DpuMode::NicMode,
                )),
            }
            "no DPU" {
                rpc::forge::DpuMode::NoDpu => Yields((
                    r#""NoDpu""#.to_string(),
                    rpc::forge::DpuMode::NoDpu,
                )),
            }
        );
    }

    /// `BmcIpAllocationType::from(rpc::forge::BmcIpAllocationType)` maps each
    /// named variant onto its model twin, and Unspecified (what old clients send)
    /// onto the default — keeping existing deployments behaving as before. The
    /// named rows also stand in for the model -> rpc -> model round trip, since
    /// the rpc input is exactly what `rpc::forge::BmcIpAllocationType::from(model)`
    /// produces.
    #[test]
    fn rpc_bmc_ip_allocation_maps_to_model() {
        value_scenarios!(
            run = BmcIpAllocationType::from;
            "unspecified maps to default" {
                rpc::forge::BmcIpAllocationType::Unspecified => BmcIpAllocationType::default(),
            }

            "auto round trips" {
                rpc::forge::BmcIpAllocationType::Auto => BmcIpAllocationType::Auto,
            }

            "dynamic round trips" {
                rpc::forge::BmcIpAllocationType::Dynamic => BmcIpAllocationType::Dynamic,
            }

            "fixed round trips" {
                rpc::forge::BmcIpAllocationType::Fixed => BmcIpAllocationType::Fixed,
            }

            "retained round trips" {
                rpc::forge::BmcIpAllocationType::Retained => BmcIpAllocationType::Retained,
            }
        );
    }

    struct ExpectedInterfaceInput {
        mac_address: &'static str,
        fixed_ip: Option<&'static str>,
        fixed_gateway: Option<&'static str>,
    }

    #[derive(Debug, PartialEq)]
    enum ExpectedInterfaceConversion {
        Converted {
            fixed_ip: Option<IpAddr>,
            fixed_gateway: Option<IpAddr>,
        },
        InvalidMac(String),
        InvalidArgument(String),
    }

    #[test]
    fn expected_interface_converts_wire_addresses() {
        check_values(
            [
                Check {
                    scenario: "invalid MAC address is rejected",
                    input: ExpectedInterfaceInput {
                        mac_address: "not-a-mac",
                        fixed_ip: None,
                        fixed_gateway: None,
                    },
                    expect: ExpectedInterfaceConversion::InvalidMac("not-a-mac".to_string()),
                },
                Check {
                    scenario: "IPv6 fixed IP is parsed",
                    input: ExpectedInterfaceInput {
                        mac_address: "5A:5B:5C:5D:5E:66",
                        fixed_ip: Some("2001:db8::66"),
                        fixed_gateway: None,
                    },
                    expect: ExpectedInterfaceConversion::Converted {
                        fixed_ip: Some("2001:db8::66".parse().unwrap()),
                        fixed_gateway: None,
                    },
                },
                Check {
                    scenario: "invalid fixed IP is rejected",
                    input: ExpectedInterfaceInput {
                        mac_address: "5A:5B:5C:5D:5E:66",
                        fixed_ip: Some("not-a-valid-ip"),
                        fixed_gateway: None,
                    },
                    expect: ExpectedInterfaceConversion::InvalidArgument(
                        "Invalid fixed IP: not-a-valid-ip".to_string(),
                    ),
                },
                Check {
                    scenario: "IPv6 fixed gateway is parsed",
                    input: ExpectedInterfaceInput {
                        mac_address: "5A:5B:5C:5D:5E:66",
                        fixed_ip: None,
                        fixed_gateway: Some("2001:db8::1"),
                    },
                    expect: ExpectedInterfaceConversion::Converted {
                        fixed_ip: None,
                        fixed_gateway: Some("2001:db8::1".parse().unwrap()),
                    },
                },
                Check {
                    scenario: "invalid fixed gateway is rejected",
                    input: ExpectedInterfaceInput {
                        mac_address: "5A:5B:5C:5D:5E:66",
                        fixed_ip: None,
                        fixed_gateway: Some("not-a-valid-ip"),
                    },
                    expect: ExpectedInterfaceConversion::InvalidArgument(
                        "Invalid fixed gateway: not-a-valid-ip".to_string(),
                    ),
                },
                Check {
                    scenario: "empty fixed addresses are treated as absent",
                    input: ExpectedInterfaceInput {
                        mac_address: "5A:5B:5C:5D:5E:66",
                        fixed_ip: Some(""),
                        fixed_gateway: Some(""),
                    },
                    expect: ExpectedInterfaceConversion::Converted {
                        fixed_ip: None,
                        fixed_gateway: None,
                    },
                },
            ],
            |input| match ExpectedInterface::try_from(rpc::forge::ExpectedInterface {
                mac_address: input.mac_address.to_string(),
                fixed_ip: input.fixed_ip.map(str::to_string),
                fixed_gateway: input.fixed_gateway.map(str::to_string),
                ..Default::default()
            }) {
                Ok(nic) => ExpectedInterfaceConversion::Converted {
                    fixed_ip: nic.fixed_ip,
                    fixed_gateway: nic.fixed_gateway,
                },
                Err(RpcDataConversionError::InvalidMacAddress(mac)) => {
                    ExpectedInterfaceConversion::InvalidMac(mac)
                }
                Err(RpcDataConversionError::InvalidArgument(argument)) => {
                    ExpectedInterfaceConversion::InvalidArgument(argument)
                }
                Err(error) => panic!("unexpected conversion error: {error}"),
            },
        );
    }

    #[test]
    fn expected_interface_ip_allocation_uses_optional_wire_field() {
        struct WireDeclaration {
            policy: Option<i32>,
            fixed_ip: Option<String>,
        }

        check_values(
            [
                Check {
                    scenario: "missing policy without fixed IP infers dynamic and stays absent",
                    input: WireDeclaration {
                        policy: None,
                        fixed_ip: None,
                    },
                    expect: (None, ExpectedInterfaceIpAllocation::Dynamic, None),
                },
                Check {
                    scenario: "missing policy with fixed IP infers fixed and stays absent",
                    input: WireDeclaration {
                        policy: None,
                        fixed_ip: Some("192.0.2.10".into()),
                    },
                    expect: (None, ExpectedInterfaceIpAllocation::Fixed, None),
                },
                Check {
                    scenario: "unspecified policy becomes absent",
                    input: WireDeclaration {
                        policy: Some(rpc::forge::ExpectedInterfaceIpAllocation::Unspecified as i32),
                        fixed_ip: None,
                    },
                    expect: (None, ExpectedInterfaceIpAllocation::Dynamic, None),
                },
                Check {
                    scenario: "dynamic round trips",
                    input: WireDeclaration {
                        policy: Some(rpc::forge::ExpectedInterfaceIpAllocation::Dynamic as i32),
                        fixed_ip: None,
                    },
                    expect: (
                        Some(ExpectedInterfaceIpAllocation::Dynamic),
                        ExpectedInterfaceIpAllocation::Dynamic,
                        Some(rpc::forge::ExpectedInterfaceIpAllocation::Dynamic as i32),
                    ),
                },
                Check {
                    scenario: "fixed round trips",
                    input: WireDeclaration {
                        policy: Some(rpc::forge::ExpectedInterfaceIpAllocation::Fixed as i32),
                        fixed_ip: Some("192.0.2.10".into()),
                    },
                    expect: (
                        Some(ExpectedInterfaceIpAllocation::Fixed),
                        ExpectedInterfaceIpAllocation::Fixed,
                        Some(rpc::forge::ExpectedInterfaceIpAllocation::Fixed as i32),
                    ),
                },
                Check {
                    scenario: "retained round trips",
                    input: WireDeclaration {
                        policy: Some(rpc::forge::ExpectedInterfaceIpAllocation::Retained as i32),
                        fixed_ip: None,
                    },
                    expect: (
                        Some(ExpectedInterfaceIpAllocation::Retained),
                        ExpectedInterfaceIpAllocation::Retained,
                        Some(rpc::forge::ExpectedInterfaceIpAllocation::Retained as i32),
                    ),
                },
            ],
            |declaration| {
                let model = ExpectedInterface::try_from(rpc::forge::ExpectedInterface {
                    mac_address: "AA:BB:CC:DD:EE:FF".into(),
                    fixed_ip: declaration.fixed_ip,
                    ip_allocation: declaration.policy,
                    ..Default::default()
                })
                .unwrap();
                let resolved = model.resolved_ip_allocation();
                let emitted = rpc::forge::ExpectedInterface::from(model.clone());
                (model.ip_allocation, resolved, emitted.ip_allocation)
            },
        );
    }

    #[test]
    fn expected_interface_ip_allocation_json_accepts_names_and_numbers() {
        scenarios!(
            run = |value: serde_json::Value| {
                serde_json::from_value::<rpc::forge::ExpectedInterface>(serde_json::json!({
                    "mac_address": "AA:BB:CC:DD:EE:FF",
                    "ip_allocation": value,
                }))
                .map(|interface| {
                    interface.ip_allocation.map(|value| {
                        rpc::forge::ExpectedInterfaceIpAllocation::try_from(value).unwrap()
                    })
                })
                .map_err(|error| error.to_string())
            };
            "short names" {
                serde_json::json!("unspecified") =>
                    Yields(Some(rpc::forge::ExpectedInterfaceIpAllocation::Unspecified)),
                serde_json::json!("dynamic") =>
                    Yields(Some(rpc::forge::ExpectedInterfaceIpAllocation::Dynamic)),
                serde_json::json!("fixed") =>
                    Yields(Some(rpc::forge::ExpectedInterfaceIpAllocation::Fixed)),
                serde_json::json!("retained") =>
                    Yields(Some(rpc::forge::ExpectedInterfaceIpAllocation::Retained)),
            }
            "fully-qualified aliases" {
                serde_json::json!("expected_interface_ip_allocation_unspecified") =>
                    Yields(Some(rpc::forge::ExpectedInterfaceIpAllocation::Unspecified)),
                serde_json::json!("expected_interface_ip_allocation_dynamic") =>
                    Yields(Some(rpc::forge::ExpectedInterfaceIpAllocation::Dynamic)),
                serde_json::json!("expected_interface_ip_allocation_fixed") =>
                    Yields(Some(rpc::forge::ExpectedInterfaceIpAllocation::Fixed)),
                serde_json::json!("expected_interface_ip_allocation_retained") =>
                    Yields(Some(rpc::forge::ExpectedInterfaceIpAllocation::Retained)),
            }
            "normalized and numeric values" {
                serde_json::json!(" EXPECTED-INTERFACE-IP-ALLOCATION-RETAINED ") =>
                    Yields(Some(rpc::forge::ExpectedInterfaceIpAllocation::Retained)),
                serde_json::json!(2) =>
                    Yields(Some(rpc::forge::ExpectedInterfaceIpAllocation::Fixed)),
            }
            "optional and invalid values" {
                serde_json::Value::Null => Yields(None),
                serde_json::json!("automatic") => FailsWith(
                    "unknown expected interface IP allocation \"automatic\"".to_string()
                ),
            }
        );

        let interface = rpc::forge::ExpectedInterface {
            mac_address: "AA:BB:CC:DD:EE:FF".into(),
            ip_allocation: Some(rpc::forge::ExpectedInterfaceIpAllocation::Retained as i32),
            ..Default::default()
        };
        let json = serde_json::to_string(&interface).unwrap();
        assert!(json.contains(r#""ip_allocation":"retained""#), "{json}");
    }

    #[test]
    fn expected_interface_ip_allocation_rejects_unknown_rpc_value() {
        let err = ExpectedInterface::try_from(rpc::forge::ExpectedInterface {
            mac_address: "AA:BB:CC:DD:EE:FF".into(),
            ip_allocation: Some(i32::MAX),
            ..Default::default()
        })
        .unwrap_err();

        assert!(matches!(err, RpcDataConversionError::InvalidArgument(_)));
    }

    #[test]
    fn expected_interface_role_uses_optional_wire_field() {
        scenarios!(
            run = |role| {
                let model = ExpectedInterface::try_from(rpc::forge::ExpectedInterface {
                    mac_address: "AA:BB:CC:DD:EE:FF".into(),
                    role,
                    ..Default::default()
                })
                .map_err(drop)?;
                let emitted = rpc::forge::ExpectedInterface::from(model.clone());
                Ok::<_, ()>((model.role, emitted.role))
            };
            "missing field is legacy Host and remains omitted" {
                None => Yields((ExpectedInterfaceRole::Host, None)),
            }
            "Unspecified is legacy Host and remains omitted" {
                Some(rpc::forge::ExpectedInterfaceRole::Unspecified as i32) =>
                    Yields((ExpectedInterfaceRole::Host, None)),
            }
            "explicit Host remains wire-compatible" {
                Some(rpc::forge::ExpectedInterfaceRole::Host as i32) =>
                    Yields((ExpectedInterfaceRole::Host, None)),
            }
            "DPU OS round trips" {
                Some(rpc::forge::ExpectedInterfaceRole::DpuOs as i32) => Yields((
                    ExpectedInterfaceRole::DpuOs,
                    Some(rpc::forge::ExpectedInterfaceRole::DpuOs as i32),
                )),
            }
            "DPU BMC round trips" {
                Some(rpc::forge::ExpectedInterfaceRole::DpuBmc as i32) => Yields((
                    ExpectedInterfaceRole::DpuBmc,
                    Some(rpc::forge::ExpectedInterfaceRole::DpuBmc as i32),
                )),
            }
            "host BMC round trips" {
                Some(rpc::forge::ExpectedInterfaceRole::HostBmc as i32) => Yields((
                    ExpectedInterfaceRole::HostBmc,
                    Some(rpc::forge::ExpectedInterfaceRole::HostBmc as i32),
                )),
            }
        );
    }

    #[test]
    fn expected_interface_role_json_accepts_names_and_numbers() {
        scenarios!(
            run = |value: serde_json::Value| {
                serde_json::from_value::<rpc::forge::ExpectedInterface>(serde_json::json!({
                    "mac_address": "AA:BB:CC:DD:EE:FF",
                    "role": value,
                }))
                .map(|interface| {
                    interface
                        .role
                        .map(|value| rpc::forge::ExpectedInterfaceRole::try_from(value).unwrap())
                })
                .map_err(|error| error.to_string())
            };
            "short names" {
                serde_json::json!("unspecified") =>
                    Yields(Some(rpc::forge::ExpectedInterfaceRole::Unspecified)),
                serde_json::json!("host") =>
                    Yields(Some(rpc::forge::ExpectedInterfaceRole::Host)),
                serde_json::json!("dpu_os") =>
                    Yields(Some(rpc::forge::ExpectedInterfaceRole::DpuOs)),
                serde_json::json!("dpu_bmc") =>
                    Yields(Some(rpc::forge::ExpectedInterfaceRole::DpuBmc)),
                serde_json::json!("host_bmc") =>
                    Yields(Some(rpc::forge::ExpectedInterfaceRole::HostBmc)),
            }
            "fully-qualified aliases" {
                serde_json::json!("expected_interface_role_unspecified") =>
                    Yields(Some(rpc::forge::ExpectedInterfaceRole::Unspecified)),
                serde_json::json!("expected_interface_role_host") =>
                    Yields(Some(rpc::forge::ExpectedInterfaceRole::Host)),
                serde_json::json!("expected_interface_role_dpu_os") =>
                    Yields(Some(rpc::forge::ExpectedInterfaceRole::DpuOs)),
                serde_json::json!("expected_interface_role_dpu_bmc") =>
                    Yields(Some(rpc::forge::ExpectedInterfaceRole::DpuBmc)),
                serde_json::json!("expected_interface_role_host_bmc") =>
                    Yields(Some(rpc::forge::ExpectedInterfaceRole::HostBmc)),
            }
            "normalized and numeric values" {
                serde_json::json!(" DPU-OS ") =>
                    Yields(Some(rpc::forge::ExpectedInterfaceRole::DpuOs)),
                serde_json::json!("dpubmc") =>
                    Yields(Some(rpc::forge::ExpectedInterfaceRole::DpuBmc)),
                serde_json::json!("hostbmc") =>
                    Yields(Some(rpc::forge::ExpectedInterfaceRole::HostBmc)),
                serde_json::json!(" EXPECTED-INTERFACE-ROLE-HOST-BMC ") =>
                    Yields(Some(rpc::forge::ExpectedInterfaceRole::HostBmc)),
                serde_json::json!(1) =>
                    Yields(Some(rpc::forge::ExpectedInterfaceRole::Host)),
                serde_json::json!(4) =>
                    Yields(Some(rpc::forge::ExpectedInterfaceRole::HostBmc)),
            }
            "optional and invalid values" {
                serde_json::Value::Null => Yields(None),
                serde_json::json!("sidecar") =>
                    FailsWith("unknown expected interface role \"sidecar\"".to_string()),
            }
        );

        let interface = rpc::forge::ExpectedInterface {
            mac_address: "AA:BB:CC:DD:EE:FF".into(),
            role: Some(rpc::forge::ExpectedInterfaceRole::DpuBmc as i32),
            ..Default::default()
        };
        let json = serde_json::to_string(&interface).unwrap();
        assert!(json.contains(r#""role":"dpu_bmc""#), "{json}");

        let host_bmc_interface = rpc::forge::ExpectedInterface {
            mac_address: "AA:BB:CC:DD:EE:FF".into(),
            role: Some(rpc::forge::ExpectedInterfaceRole::HostBmc as i32),
            ..Default::default()
        };
        let host_bmc_json = serde_json::to_string(&host_bmc_interface).unwrap();
        assert!(
            host_bmc_json.contains(r#""role":"host_bmc""#),
            "{host_bmc_json}"
        );

        let legacy_interface = rpc::forge::ExpectedInterface {
            mac_address: "AA:BB:CC:DD:EE:FF".into(),
            role: None,
            ..Default::default()
        };
        let legacy_json = serde_json::to_string(&legacy_interface).unwrap();
        assert!(!legacy_json.contains(r#""role""#), "{legacy_json}");
        assert!(!legacy_json.contains(r#""ip_allocation""#), "{legacy_json}");
    }

    #[test]
    fn expected_interface_role_rejects_unknown_rpc_value() {
        let err = ExpectedInterface::try_from(rpc::forge::ExpectedInterface {
            mac_address: "AA:BB:CC:DD:EE:FF".into(),
            role: Some(i32::MAX),
            ..Default::default()
        })
        .unwrap_err();

        assert!(matches!(err, RpcDataConversionError::InvalidArgument(_)));
    }

    #[test]
    fn expected_interface_keeps_legacy_protobuf_descriptor_names() {
        let descriptor_set =
            prost_types::FileDescriptorSet::decode(rpc::REFLECTION_API_SERVICE_DESCRIPTOR).unwrap();
        let forge = descriptor_set
            .file
            .iter()
            .find(|file| file.package.as_deref() == Some("forge"))
            .unwrap();
        let expected_interface_message = forge
            .message_type
            .iter()
            .find(|message| message.name.as_deref() == Some("ExpectedHostNic"))
            .unwrap();
        let field_numbers = expected_interface_message
            .field
            .iter()
            .map(|field| (field.name.as_deref().unwrap(), field.number.unwrap()))
            .collect::<Vec<_>>();

        for expected in [
            ("mac_address", 1),
            ("nic_type", 2),
            ("fixed_ip", 3),
            ("fixed_mask", 4),
            ("fixed_gateway", 5),
            ("primary", 6),
            ("network_segment_type", 7),
            ("role", 8),
            ("ip_allocation", 9),
        ] {
            assert!(field_numbers.contains(&expected));
        }

        let role_field = expected_interface_message
            .field
            .iter()
            .find(|field| field.name.as_deref() == Some("role"))
            .unwrap();
        assert_eq!(
            role_field.type_name.as_deref(),
            Some(".forge.ExpectedInterfaceRole")
        );

        let allocation_field = expected_interface_message
            .field
            .iter()
            .find(|field| field.name.as_deref() == Some("ip_allocation"))
            .unwrap();
        assert_eq!(
            allocation_field.type_name.as_deref(),
            Some(".forge.ExpectedInterfaceIpAllocation")
        );

        let role_enum = forge
            .enum_type
            .iter()
            .find(|enumeration| enumeration.name.as_deref() == Some("ExpectedInterfaceRole"))
            .unwrap();
        let host_bmc = role_enum
            .value
            .iter()
            .find(|value| value.name.as_deref() == Some("EXPECTED_INTERFACE_ROLE_HOST_BMC"))
            .unwrap();
        assert_eq!(host_bmc.number, Some(4));

        let expected_machine = forge
            .message_type
            .iter()
            .find(|message| message.name.as_deref() == Some("ExpectedMachine"))
            .unwrap();
        let host_nics = expected_machine
            .field
            .iter()
            .find(|field| field.name.as_deref() == Some("host_nics"))
            .unwrap();
        assert_eq!(host_nics.number, Some(9));
        assert_eq!(
            host_nics.type_name.as_deref(),
            Some(".forge.ExpectedHostNic"),
        );
        assert_eq!(host_nics.json_name.as_deref(), Some("hostNics"));
        let replace_host_nics = expected_machine
            .field
            .iter()
            .find(|field| field.name.as_deref() == Some("replace_host_nics"))
            .unwrap();
        assert_eq!(replace_host_nics.number, Some(19));
        assert_eq!(
            replace_host_nics.json_name.as_deref(),
            Some("replaceHostNics"),
        );
    }

    #[test]
    fn expected_machine_data_rejects_invalid_interface_mac_address() {
        let mut rpc_machine = make_rpc_expected_machine(None);
        rpc_machine.host_nics.push(rpc::forge::ExpectedInterface {
            mac_address: "not-a-mac".into(),
            ..Default::default()
        });

        let Err(err) = ExpectedMachineData::try_from(rpc_machine) else {
            panic!("expected invalid interface MAC address");
        };

        assert!(
            matches!(err, RpcDataConversionError::InvalidMacAddress(mac) if mac == "not-a-mac")
        );
    }

    #[test]
    fn expected_machine_serde_uses_canonical_interface_name() {
        let machine = rpc::forge::ExpectedMachine {
            host_nics: vec![rpc::forge::ExpectedInterface {
                mac_address: "AA:BB:CC:DD:EE:FF".into(),
                ..Default::default()
            }],
            replace_host_nics: true,
            ..Default::default()
        };

        let json = serde_json::to_value(machine).unwrap();
        assert_eq!(json["interfaces"][0]["mac_address"], "AA:BB:CC:DD:EE:FF");
        assert_eq!(json.get("host_nics"), None);
        assert_eq!(json.get("replace_host_nics"), None);
    }

    #[test]
    fn expected_interface_alias_keeps_protobuf_bytes() {
        let machine = rpc::forge::ExpectedMachine {
            host_nics: vec![rpc::forge::ExpectedInterface {
                mac_address: "a".into(),
                ..Default::default()
            }],
            replace_host_nics: true,
            ..Default::default()
        };

        let bytes = machine.encode_to_vec();
        assert_eq!(bytes, [0x4a, 0x03, 0x0a, 0x01, b'a', 0x98, 0x01, 0x01],);

        let decoded = rpc::forge::ExpectedMachine::decode(bytes.as_slice()).unwrap();
        assert_eq!(decoded.interfaces()[0].mac_address, "a");
        assert!(decoded.replace_host_nics);
    }

    /// RPC conversion retains presence for both legacy compatibility fields.
    #[test]
    fn legacy_host_bmc_overrides_preserve_rpc_field_presence() {
        let fixed_address: IpAddr = "192.0.2.44".parse().unwrap();
        scenarios!(
            run = |machine: rpc::forge::ExpectedMachine| {
                LegacyHostBmcOverrides::try_from(&machine).map_err(|error| error.to_string())
            };
            "omitted compatibility fields remain absent" {
                rpc::forge::ExpectedMachine::default() =>
                    Yields(LegacyHostBmcOverrides::default()),
            }
            "an empty address is an explicit clear" {
                rpc::forge::ExpectedMachine {
                    bmc_ip_address: Some(String::new()),
                    ..Default::default()
                } => Yields(LegacyHostBmcOverrides {
                    ip_address: Some(None),
                    ip_allocation: None,
                    ..Default::default()
                }),
            }
            "a configured address retains its value" {
                rpc::forge::ExpectedMachine {
                    bmc_ip_address: Some(fixed_address.to_string()),
                    ..Default::default()
                } => Yields(LegacyHostBmcOverrides {
                    ip_address: Some(Some(fixed_address)),
                    ip_allocation: None,
                    ..Default::default()
                }),
            }
            "a configured policy retains its presence" {
                rpc::forge::ExpectedMachine {
                    bmc_ip_allocation: Some(
                        rpc::forge::BmcIpAllocationType::Retained as i32
                    ),
                    ..Default::default()
                } => Yields(LegacyHostBmcOverrides {
                    ip_address: None,
                    ip_allocation: Some(BmcIpAllocationType::Retained),
                    ..Default::default()
                }),
            }
            "explicit unspecified retains the compatibility default" {
                rpc::forge::ExpectedMachine {
                    bmc_ip_allocation: Some(
                        rpc::forge::BmcIpAllocationType::Unspecified as i32
                    ),
                    ..Default::default()
                } => Yields(LegacyHostBmcOverrides {
                    ip_address: None,
                    ip_allocation: Some(BmcIpAllocationType::Auto),
                    ..Default::default()
                }),
            }
            "nested unspecified resets compatibility allocation" {
                rpc::forge::ExpectedMachine {
                    host_nics: vec![rpc::forge::ExpectedInterface {
                        role: Some(rpc::forge::ExpectedInterfaceRole::HostBmc as i32),
                        ip_allocation: Some(
                            rpc::forge::ExpectedInterfaceIpAllocation::Unspecified as i32
                        ),
                        ..Default::default()
                    }],
                    ..Default::default()
                } => Yields(LegacyHostBmcOverrides {
                    ip_address: None,
                    ip_allocation: Some(BmcIpAllocationType::Auto),
                    ..Default::default()
                }),
            }
            "an authoritative interface replacement retains its signal" {
                rpc::forge::ExpectedMachine {
                    replace_host_nics: true,
                    ..Default::default()
                } => Yields(LegacyHostBmcOverrides {
                    replace_interfaces: true,
                    ..Default::default()
                }),
            }
        );
    }

    /// Legacy rows keep their earlier wire shape so clients whose role enum
    /// predates HostBmc can still serialize the response.
    #[test]
    fn expected_machine_output_keeps_legacy_host_bmc_fields_top_level() {
        let bmc_mac_address = "AA:BB:CC:DD:EE:FF".parse().unwrap();
        let fixed_ip = "192.0.2.44".parse().unwrap();
        let rpc_machine = rpc::forge::ExpectedMachine::from(ExpectedMachine {
            id: None,
            bmc_mac_address,
            data: ExpectedMachineData {
                bmc_ip_address: Some(fixed_ip),
                bmc_ip_allocation: BmcIpAllocationType::Auto,
                ..Default::default()
            },
        });

        assert!(rpc_machine.host_nics.is_empty());
        assert_eq!(rpc_machine.bmc_mac_address, bmc_mac_address.to_string());
        assert_eq!(rpc_machine.bmc_ip_address.as_deref(), Some("192.0.2.44"));
        assert_eq!(rpc_machine.bmc_ip_allocation, None);
    }

    /// An explicit nested policy is mirrored on the compatibility wire field.
    #[test]
    fn expected_machine_output_projects_explicit_host_bmc_policy() {
        let bmc_mac_address = "AA:BB:CC:DD:EE:FE".parse().unwrap();
        let fixed_ip = "192.0.2.45".parse().unwrap();
        let rpc_machine = rpc::forge::ExpectedMachine::from(ExpectedMachine {
            id: None,
            bmc_mac_address,
            data: ExpectedMachineData {
                interfaces: vec![ExpectedInterface {
                    mac_address: bmc_mac_address,
                    role: ExpectedInterfaceRole::HostBmc,
                    ip_allocation: Some(ExpectedInterfaceIpAllocation::Fixed),
                    fixed_ip: Some(fixed_ip),
                    ..Default::default()
                }],
                bmc_ip_address: Some(fixed_ip),
                bmc_ip_allocation: BmcIpAllocationType::Fixed,
                ..Default::default()
            },
        });

        let host_bmc = rpc_machine
            .host_nics
            .iter()
            .find(|interface| {
                interface.role == Some(rpc::forge::ExpectedInterfaceRole::HostBmc as i32)
            })
            .expect("output should contain one HostBmc");
        assert_eq!(
            host_bmc.ip_allocation,
            Some(rpc::forge::ExpectedInterfaceIpAllocation::Fixed as i32),
        );
        assert_eq!(
            rpc_machine.bmc_ip_allocation,
            Some(rpc::forge::BmcIpAllocationType::Fixed as i32),
        );
        assert_eq!(rpc_machine.bmc_ip_address.as_deref(), Some("192.0.2.45"));
    }

    /// An explicit legacy Fixed policy remains present even though its nested
    /// representation omits the policy to retain external-address fallback.
    #[test]
    fn expected_machine_output_preserves_explicit_legacy_fixed_policy() {
        let bmc_mac_address = "AA:BB:CC:DD:EE:FD".parse().unwrap();
        let fixed_ip = "192.0.2.46".parse().unwrap();
        let rpc_machine = rpc::forge::ExpectedMachine::from(ExpectedMachine {
            id: None,
            bmc_mac_address,
            data: ExpectedMachineData {
                interfaces: vec![ExpectedInterface {
                    mac_address: bmc_mac_address,
                    role: ExpectedInterfaceRole::HostBmc,
                    fixed_ip: Some(fixed_ip),
                    ..Default::default()
                }],
                bmc_ip_address: Some(fixed_ip),
                bmc_ip_allocation: BmcIpAllocationType::Fixed,
                ..Default::default()
            },
        });

        let host_bmc = rpc_machine
            .host_nics
            .iter()
            .find(|interface| {
                interface.role == Some(rpc::forge::ExpectedInterfaceRole::HostBmc as i32)
            })
            .expect("output should contain one HostBmc");
        assert_eq!(host_bmc.ip_allocation, None);
        assert_eq!(
            rpc_machine.bmc_ip_allocation,
            Some(rpc::forge::BmcIpAllocationType::Fixed as i32),
        );
        assert_eq!(rpc_machine.bmc_ip_address.as_deref(), Some("192.0.2.46"));
    }

    fn make_rpc_expected_machine(disable_lockdown: Option<bool>) -> rpc::forge::ExpectedMachine {
        rpc::forge::ExpectedMachine {
            bmc_mac_address: "AA:BB:CC:DD:EE:FF".into(),
            bmc_username: "root".into(),
            bmc_password: "pass".into(),
            chassis_serial_number: "SN-1".into(),
            host_lifecycle_profile: disable_lockdown.map(|dl| rpc::forge::HostLifecycleProfile {
                disable_lockdown: Some(dl),
            }),
            ..Default::default()
        }
    }

    /// `disable_lockdown` survives the rpc -> data -> rpc round trip: each input
    /// is projected to (data-side disable_lockdown, back-side host_lifecycle_profile
    /// mapped to its disable_lockdown). A `None` input yields no profile on the way
    /// back, so the back-side projection is `None` rather than `Some(None)`.
    #[test]
    fn disable_lockdown_round_trips_through_proto() {
        scenarios!(
            run = |disable_lockdown| {
                let data =
                    ExpectedMachineData::try_from(make_rpc_expected_machine(disable_lockdown))
                        .map_err(drop)?;
                let data_side = data.host_lifecycle_profile.disable_lockdown;

                let em = ExpectedMachine {
                    id: None,
                    bmc_mac_address: "AA:BB:CC:DD:EE:FF".parse().map_err(drop)?,
                    data,
                };
                let back: rpc::forge::ExpectedMachine = em.into();
                let back_side = back.host_lifecycle_profile.map(|p| p.disable_lockdown);

                Ok::<_, ()>((data_side, back_side))
            };
            "true" {
                Some(true) => Yields((Some(true), Some(Some(true)))),
            }

            "false" {
                Some(false) => Yields((Some(false), Some(Some(false)))),
            }

            "none" {
                None => Yields((None, None)),
            }
        );
    }
}

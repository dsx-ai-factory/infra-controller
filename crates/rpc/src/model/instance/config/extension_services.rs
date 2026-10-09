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

use std::collections::BTreeMap;

use carbide_uuid::extension_service::ExtensionServiceId;
use config_version::ConfigVersion;
use model::instance::config::extension_services::{
    InstanceExtensionServicesConfig, RequestedInstanceExtensionServiceConfig,
    RequestedInstanceExtensionServicesConfig,
};
use model::instance::config::network::InstanceServiceInterfaceConfig;

use crate::errors::RpcDataConversionError;
use crate::forge as rpc;

impl TryFrom<rpc::InstanceDpuExtensionServiceConfig> for RequestedInstanceExtensionServiceConfig {
    type Error = RpcDataConversionError;

    fn try_from(config: rpc::InstanceDpuExtensionServiceConfig) -> Result<Self, Self::Error> {
        let service_id = config
            .service_id
            .parse::<ExtensionServiceId>()
            .map_err(|e| {
                RpcDataConversionError::InvalidUuid("ExtensionServiceId", e.to_string())
            })?;

        let version = config.version.parse::<ConfigVersion>().map_err(|e| {
            RpcDataConversionError::InvalidConfigVersion(format!(
                "Failed to parse version as ConfigVersion: {}",
                e
            ))
        })?;

        // The MVP maps at most one VPC to the service's one supported interface.
        if config.service_vpc_ids.len() > 1 {
            return Err(RpcDataConversionError::InvalidArgument(
                "at most one service VPC may be selected for an extension-service attachment"
                    .to_string(),
            ));
        }
        Ok(RequestedInstanceExtensionServiceConfig {
            service_id,
            version,
            service_vpc_ids: config.service_vpc_ids,
        })
    }
}

impl TryFrom<rpc::InstanceDpuExtensionServicesConfig> for RequestedInstanceExtensionServicesConfig {
    type Error = RpcDataConversionError;

    fn try_from(config: rpc::InstanceDpuExtensionServicesConfig) -> Result<Self, Self::Error> {
        let service_configs = config
            .service_configs
            .into_iter()
            .map(RequestedInstanceExtensionServiceConfig::try_from)
            .collect::<Result<Vec<_>, _>>()?;

        Ok(RequestedInstanceExtensionServicesConfig { service_configs })
    }
}

/// Converts active attachments to caller-visible RPC form and restores each
/// VPC selection from stored service-interface records.
///
/// The caller must remove terminating attachments before calling this function.
pub(super) fn to_rpc_config(
    config: InstanceExtensionServicesConfig,
    service_interfaces: &[InstanceServiceInterfaceConfig],
) -> Result<rpc::InstanceDpuExtensionServicesConfig, RpcDataConversionError> {
    let mut service_configs = Vec::with_capacity(config.service_configs.len());
    for attachment in config.service_configs {
        let mut vpcs_by_ordinal = BTreeMap::new();
        for service_interface in service_interfaces
            .iter()
            .filter(|service_interface| Some(service_interface.attachment_id) == attachment.id)
        {
            // Every DPU copy of one service interface must select the same VPC.
            if let Some(existing) = vpcs_by_ordinal.insert(
                service_interface.interface_ordinal,
                service_interface.vpc_id,
            ) && existing != service_interface.vpc_id
            {
                return Err(RpcDataConversionError::InvalidValue(
                    "service_vpc_ids".to_string(),
                    format!(
                        "extension service {} version {} has inconsistent VPCs for interface ordinal {}",
                        attachment.service_id,
                        attachment.version,
                        service_interface.interface_ordinal
                    ),
                ));
            }
        }
        // The MVP supports only the first registered service interface.
        if vpcs_by_ordinal.len() > 1
            || vpcs_by_ordinal
                .first_key_value()
                .is_some_and(|(&ordinal, _)| ordinal != 0)
        {
            return Err(RpcDataConversionError::InvalidValue(
                "service_vpc_ids".to_string(),
                format!(
                    "extension service {} version {} has unsupported interface ordinals",
                    attachment.service_id, attachment.version
                ),
            ));
        }
        service_configs.push(rpc::InstanceDpuExtensionServiceConfig {
            service_id: attachment.service_id.into(),
            version: attachment.version.to_string(),
            service_vpc_ids: vpcs_by_ordinal.into_values().collect(),
        });
    }
    Ok(rpc::InstanceDpuExtensionServicesConfig { service_configs })
}

#[cfg(test)]
mod tests {
    use carbide_uuid::network::{NetworkPrefixId, NetworkSegmentId};
    use carbide_uuid::vpc::{VpcId, VpcPrefixId};
    use mac_address::MacAddress;
    use prost::Message;

    use super::*;

    /// Builds a valid service-interface record on one DPU so each readback test
    /// changes only whether the selected VPCs agree.
    fn service_interface(
        attachment_id: uuid::Uuid,
        dpu_id: &str,
        vpc_id: VpcId,
    ) -> InstanceServiceInterfaceConfig {
        InstanceServiceInterfaceConfig {
            attachment_id,
            interface_ordinal: 0,
            dpu_id: dpu_id.parse().expect("valid DPU machine ID"),
            slot_index: 0,
            vpc_id,
            vpc_prefix_id: VpcPrefixId::new(),
            network_segment_id: NetworkSegmentId::new(),
            network_prefix_id: NetworkPrefixId::new(),
            link_prefix: "192.0.2.0/31".parse().expect("valid service prefix"),
            mac_address: MacAddress::new([0x02, 0, 0, 0, 0, 1]),
            internal_uuid: uuid::Uuid::new_v4(),
        }
    }

    /// Verifies public readback returns a VPC only when every DPU-local
    /// service-interface record agrees, so corrupt stored selections are not
    /// hidden; ID-less legacy attachments remain empty.
    #[test]
    fn service_vpc_readback_requires_dpu_consensus() {
        // Build matching DPU-local records for one identified attachment and an
        // ID-less legacy attachment that no service-interface record can reference.
        let attachment_id = uuid::Uuid::new_v4();
        let service_id = ExtensionServiceId::new();
        let vpc_id = VpcId::new();
        let config = InstanceExtensionServicesConfig {
            service_configs: vec![
                model::instance::config::extension_services::InstanceExtensionServiceConfig {
                    id: Some(attachment_id),
                    dpu_target: None,
                    service_id,
                    version: ConfigVersion::initial(),
                    removed: None,
                },
                model::instance::config::extension_services::InstanceExtensionServiceConfig {
                    id: None,
                    dpu_target: None,
                    service_id: ExtensionServiceId::new(),
                    version: ConfigVersion::initial(),
                    removed: None,
                },
            ],
        };
        let first_dpu = "fm100ds27v4uuq7sgs4gsjummskt0b3tedugtpevjrbfh6su081n9jufcq0";
        let second_dpu = "fm100dskla0ihp0pn4tv7v1js2k2mo37sl0jjr8141okqg8pjpdpfihaa80";
        let mut service_interfaces = vec![
            service_interface(attachment_id, first_dpu, vpc_id),
            service_interface(attachment_id, second_dpu, vpc_id),
        ];

        // When every DPU agrees, return one caller-visible VPC selection.
        let projected =
            to_rpc_config(config.clone(), &service_interfaces).expect("consistent readback");
        assert_eq!(projected.service_configs[0].service_vpc_ids, vec![vpc_id]);
        assert!(projected.service_configs[1].service_vpc_ids.is_empty());

        // Divergent DPU state is corruption and must not be hidden by choosing one copy.
        service_interfaces[1].vpc_id = VpcId::new();
        assert!(to_rpc_config(config, &service_interfaces).is_err());
    }

    /// Verifies the one-interface API rejects extra VPC selections so caller
    /// intent cannot be silently discarded during conversion.
    #[test]
    fn service_vpc_request_rejects_multiple_selections() {
        // The current API supports one service interface, so two selections must fail.
        let request = rpc::InstanceDpuExtensionServicesConfig {
            service_configs: vec![rpc::InstanceDpuExtensionServiceConfig {
                service_id: ExtensionServiceId::new().to_string(),
                version: ConfigVersion::initial().to_string(),
                service_vpc_ids: vec![VpcId::new(), VpcId::new()],
            }],
        };

        // Conversion must reject the request before it becomes a domain config.
        let error = RequestedInstanceExtensionServicesConfig::try_from(request)
            .expect_err("multiple selections must be rejected");
        assert!(matches!(
            error,
            RpcDataConversionError::InvalidArgument(message)
                if message
                    == "at most one service VPC may be selected for an extension-service attachment"
        ));
    }

    /// Verifies protobuf serialization preserves the distinction between an
    /// unavailable inventory contract and a known zero-slot deployment, because
    /// consumers must not interpret missing information as zero capacity.
    #[test]
    fn service_vpc_slot_inventory_preserves_message_presence() {
        // An omitted wrapper must remain absent after crossing the wire.
        let absent = rpc::ManagedHostNetworkConfigResponse::default();
        let decoded_absent =
            rpc::ManagedHostNetworkConfigResponse::decode(absent.encode_to_vec().as_slice())
                .expect("response without inventory decodes");
        assert_eq!(decoded_absent, absent);

        // An explicit empty wrapper must remain present after crossing the wire.
        let explicit_empty = rpc::ManagedHostNetworkConfigResponse {
            service_vpc_slot_inventory: Some(rpc::ServiceVpcSlotInventory { slots: vec![] }),
            ..Default::default()
        };
        let decoded_explicit = rpc::ManagedHostNetworkConfigResponse::decode(
            explicit_empty.encode_to_vec().as_slice(),
        )
        .expect("response with explicit empty inventory decodes");
        assert_eq!(decoded_explicit, explicit_empty);
    }
}

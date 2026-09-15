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

use carbide_uuid::vpc::VpcPrefixId;
use ipnetwork::IpNetwork;
use model::metadata::Metadata;
use model::vpc_prefix::{
    DeleteVpcPrefix, NewVpcPrefix, UpdateVpcPrefix, VpcPrefix, VpcPrefixConfig,
    VpcPrefixControllerState, state_sla,
};

use crate as rpc;
use crate::TenantState;
use crate::errors::RpcDataConversionError;

impl TryFrom<rpc::forge::VpcPrefixCreationRequest> for NewVpcPrefix {
    type Error = RpcDataConversionError;

    fn try_from(value: rpc::forge::VpcPrefixCreationRequest) -> Result<Self, Self::Error> {
        let rpc::forge::VpcPrefixCreationRequest {
            id,
            prefix,
            vpc_id,
            config,
            metadata,
            site_prefix_id,
        } = value;

        let id = id.unwrap_or_else(VpcPrefixId::new);
        let vpc_id = vpc_id.ok_or(RpcDataConversionError::MissingArgument("vpc_id"))?;

        let metadata = match metadata {
            Some(metadata) => metadata.try_into()?,
            None => Metadata::new_with_default_name(),
        };

        metadata.validate(true).map_err(|e| {
            RpcDataConversionError::InvalidArgument(format!(
                "VPCPrefix metadata is not valid: {}",
                e
            ))
        })?;

        let config = match config {
            Some(config) => VpcPrefixConfig::try_from(config)?,
            None => VpcPrefixConfig {
                prefix: IpNetwork::try_from(prefix.as_str())?,
            },
        };

        Ok(Self {
            id,
            site_prefix_id,
            config,
            metadata,
            vpc_id,
        })
    }
}

impl TryFrom<rpc::forge::VpcPrefixConfig> for VpcPrefixConfig {
    type Error = RpcDataConversionError;

    fn try_from(rpc_config: rpc::forge::VpcPrefixConfig) -> Result<Self, Self::Error> {
        let rpc::forge::VpcPrefixConfig { prefix } = rpc_config;

        Ok(Self {
            prefix: IpNetwork::try_from(prefix.as_str())?,
        })
    }
}

impl TryFrom<rpc::forge::VpcPrefixUpdateRequest> for UpdateVpcPrefix {
    type Error = RpcDataConversionError;

    fn try_from(
        rpc_update_prefix: rpc::forge::VpcPrefixUpdateRequest,
    ) -> Result<Self, Self::Error> {
        let rpc::forge::VpcPrefixUpdateRequest {
            id,
            prefix,
            config,
            metadata,
        } = rpc_update_prefix;

        if prefix.is_some()
            || config
                .as_ref()
                .map(|c| !c.prefix.is_empty())
                .unwrap_or(false)
        {
            return Err(RpcDataConversionError::InvalidArgument(
                "Resizing VPC prefixes is currently unsupported".to_owned(),
            ));
        }
        let id = id.ok_or(RpcDataConversionError::MissingArgument("id"))?;

        let metadata = match metadata {
            Some(metadata) => metadata.try_into()?,
            None => Metadata::new_with_default_name(),
        };

        metadata.validate(true).map_err(|e| {
            RpcDataConversionError::InvalidArgument(format!(
                "VPC prefix metadata is not valid: {}",
                e
            ))
        })?;

        Ok(Self { id, metadata })
    }
}

impl TryFrom<rpc::forge::VpcPrefixDeletionRequest> for DeleteVpcPrefix {
    type Error = RpcDataConversionError;

    fn try_from(
        rpc_delete_prefix: rpc::forge::VpcPrefixDeletionRequest,
    ) -> Result<Self, Self::Error> {
        let id = rpc_delete_prefix
            .id
            .ok_or(RpcDataConversionError::MissingArgument("id"))?;
        Ok(Self { id })
    }
}

impl From<VpcPrefix> for rpc::forge::VpcPrefix {
    fn from(db_vpc_prefix: VpcPrefix) -> Self {
        // Derive the coarse tenant-facing state from the internal controller state.
        let tenant_state = match &db_vpc_prefix.status.controller_state.value {
            VpcPrefixControllerState::Provisioning => TenantState::Provisioning,
            VpcPrefixControllerState::Ready => TenantState::Ready,
            VpcPrefixControllerState::Deleting { .. } => TenantState::Terminating,
        };
        // Surface soft-deleted prefixes as terminating before the controller catches up.
        let tenant_state = if db_vpc_prefix.is_marked_as_deleted() {
            TenantState::Terminating
        } else {
            tenant_state
        };

        let VpcPrefix {
            id,
            site_prefix_id,
            config,
            metadata,
            status,
            vpc_id,
            ..
        } = db_vpc_prefix;

        let id = Some(id);
        let prefix = config.prefix.to_string();
        let vpc_id = Some(vpc_id);

        // Lifecycle state remains the JSON serialization of the internal controller state.
        let lifecycle_state =
            serde_json::to_string(&status.controller_state.value).unwrap_or_default();
        let lifecycle_sla = state_sla(
            &status.controller_state.value,
            &status.controller_state.version,
        );

        Self {
            id,
            prefix: prefix.clone(), // Deprecated
            vpc_id,
            total_31_segments: status.total_31_segments, // Deprecated
            available_31_segments: status.available_31_segments, // Deprecated
            status: Some(rpc::forge::VpcPrefixStatus {
                total_31_segments: status.total_31_segments,
                available_31_segments: status.available_31_segments,
                total_linknet_segments: status.total_linknet_segments,
                available_linknet_segments: status.available_linknet_segments,
                lifecycle: Some(rpc::forge::LifecycleStatus {
                    state: lifecycle_state,
                    version: status.controller_state.version.version_string(),
                    state_reason: status.controller_state_outcome.map(Into::into),
                    sla: Some(lifecycle_sla.into()),
                }),
                tenant_state: tenant_state as i32,
            }),
            metadata: Some(metadata.into()),
            config: Some(rpc::forge::VpcPrefixConfig { prefix }),
            site_prefix_id,
        }
    }
}

#[cfg(test)]
mod tests {
    use carbide_uuid::site_prefix::SitePrefixId;
    use carbide_uuid::vpc::VpcId;
    use chrono::{DateTime, Utc};
    use config_version::{ConfigVersion, Versioned};
    use model::vpc_prefix::{VpcPrefixDeletionState, VpcPrefixStatus};
    use prost::Message;

    use super::*;

    /// Builds a minimal VPC prefix for status conversion tests.
    fn test_vpc_prefix(
        controller_state: VpcPrefixControllerState,
        deleted: Option<DateTime<Utc>>,
    ) -> VpcPrefix {
        VpcPrefix {
            id: VpcPrefixId::new(),
            site_prefix_id: Some(SitePrefixId::new()),
            vpc_id: VpcId::new(),
            config: VpcPrefixConfig {
                prefix: "10.0.0.0/24".parse().unwrap(),
            },
            metadata: Metadata::default(),
            status: VpcPrefixStatus {
                controller_state: Versioned::new(controller_state, ConfigVersion::initial()),
                controller_state_outcome: None,
                last_used_prefix: None,
                total_31_segments: 0,
                available_31_segments: 0,
                total_linknet_segments: 0,
                available_linknet_segments: 0,
            },
            deleted,
        }
    }

    #[test]
    fn vpc_prefix_status_derives_tenant_state_from_controller_state() {
        let cases = [
            (
                VpcPrefixControllerState::Provisioning,
                TenantState::Provisioning,
            ),
            (VpcPrefixControllerState::Ready, TenantState::Ready),
            (
                VpcPrefixControllerState::Deleting {
                    deletion_state: VpcPrefixDeletionState::DBDelete,
                },
                TenantState::Terminating,
            ),
        ];

        for (controller_state, expected_tenant_state) in cases {
            // Convert each controller state without any soft-delete marker.
            let status = rpc::forge::VpcPrefix::from(test_vpc_prefix(controller_state, None))
                .status
                .expect("VPC prefix status should be populated");

            // Report the coarse tenant-facing enum independently from lifecycle JSON.
            assert_eq!(status.tenant_state, expected_tenant_state as i32);
        }
    }

    #[test]
    fn vpc_prefix_status_reports_soft_deleted_ready_prefix_as_terminating() {
        // Convert a ready prefix with the durable soft-delete marker set.
        let status = rpc::forge::VpcPrefix::from(test_vpc_prefix(
            VpcPrefixControllerState::Ready,
            Some(Utc::now()),
        ))
        .status
        .expect("VPC prefix status should be populated");

        // Keep lifecycle state as controller JSON while overriding tenant_state.
        let lifecycle = status
            .lifecycle
            .expect("VPC prefix lifecycle should be populated");
        assert_eq!(lifecycle.state, r#"{"state":"ready"}"#);
        assert_eq!(status.tenant_state, TenantState::Terminating as i32);
    }

    #[test]
    fn vpc_prefix_creation_preserves_site_prefix_id() {
        let site_prefix_id = SitePrefixId::new();
        let request = rpc::forge::VpcPrefixCreationRequest {
            id: None,
            prefix: "10.0.0.0/24".to_owned(),
            vpc_id: Some(VpcId::new()),
            config: None,
            metadata: None,
            site_prefix_id: Some(site_prefix_id),
        };

        let new_vpc_prefix = NewVpcPrefix::try_from(request).unwrap();

        assert_eq!(new_vpc_prefix.site_prefix_id, Some(site_prefix_id));
    }

    #[test]
    fn vpc_prefix_projection_preserves_site_prefix_id() {
        let vpc_prefix = test_vpc_prefix(VpcPrefixControllerState::Ready, None);
        let expected_site_prefix_id = vpc_prefix.site_prefix_id;

        let rpc_vpc_prefix = rpc::forge::VpcPrefix::from(vpc_prefix);

        assert_eq!(rpc_vpc_prefix.site_prefix_id, expected_site_prefix_id);
    }

    #[test]
    fn site_prefix_fields_use_fresh_protobuf_tags() {
        let descriptor_set =
            prost_types::FileDescriptorSet::decode(crate::REFLECTION_API_SERVICE_DESCRIPTOR)
                .unwrap();
        let forge = descriptor_set
            .file
            .iter()
            .find(|file| file.package.as_deref() == Some("forge"))
            .unwrap();

        for (message_name, field_number) in [
            ("VpcPrefix", 11),
            ("VpcPrefixCreationRequest", 8),
            ("VpcPrefixSearchQuery", 7),
        ] {
            let message = forge
                .message_type
                .iter()
                .find(|message| message.name.as_deref() == Some(message_name))
                .unwrap();
            let site_prefix_id = message
                .field
                .iter()
                .find(|field| field.name.as_deref() == Some("site_prefix_id"))
                .unwrap();
            assert_eq!(site_prefix_id.number, Some(field_number), "{message_name}");
        }

        for message_name in ["VpcPrefix", "VpcPrefixCreationRequest"] {
            let message = forge
                .message_type
                .iter()
                .find(|message| message.name.as_deref() == Some(message_name))
                .unwrap();
            assert!(
                message
                    .reserved_range
                    .iter()
                    .any(|range| range.start == Some(5) && range.end == Some(6)),
                "{message_name} must retain reserved field 5",
            );
        }

        let search = forge
            .message_type
            .iter()
            .find(|message| message.name.as_deref() == Some("VpcPrefixSearchQuery"))
            .unwrap();
        let legacy_tenant_prefix_id = search
            .field
            .iter()
            .find(|field| field.name.as_deref() == Some("tenant_prefix_id"))
            .unwrap();
        assert_eq!(legacy_tenant_prefix_id.number, Some(2));
    }
}

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

use carbide_uuid::nvlink::NvLinkLogicalPartitionId;
use model::nvl_logical_partition::{
    LogicalPartition, LogicalPartitionConfig, LogicalPartitionState, NewLogicalPartition,
    NvLinkLogicalPartitionSearchFilter, is_marked_as_deleted,
};
use model::tenant::TenantOrganizationId;

use crate as rpc;
use crate::errors::RpcDataConversionError;
use crate::forge as rpc_forge;

impl From<rpc_forge::NvLinkLogicalPartitionSearchFilter> for NvLinkLogicalPartitionSearchFilter {
    fn from(filter: rpc_forge::NvLinkLogicalPartitionSearchFilter) -> Self {
        NvLinkLogicalPartitionSearchFilter { name: filter.name }
    }
}

impl TryFrom<rpc_forge::NvLinkLogicalPartitionCreationRequest> for NewLogicalPartition {
    type Error = RpcDataConversionError;
    fn try_from(
        value: rpc_forge::NvLinkLogicalPartitionCreationRequest,
    ) -> Result<Self, Self::Error> {
        let id: NvLinkLogicalPartitionId = value.id.unwrap_or_else(|| uuid::Uuid::new_v4().into());

        let conf = value.config.ok_or_else(|| {
            RpcDataConversionError::InvalidArgument(
                "NvLinkLogicalPartition config is empty".to_string(),
            )
        })?;

        Ok(NewLogicalPartition {
            id,
            config: LogicalPartitionConfig::try_from(conf)?,
        })
    }
}

impl TryFrom<rpc_forge::NvLinkLogicalPartitionConfig> for LogicalPartitionConfig {
    type Error = RpcDataConversionError;

    fn try_from(conf: rpc_forge::NvLinkLogicalPartitionConfig) -> Result<Self, Self::Error> {
        if conf.tenant_organization_id.is_empty() {
            return Err(RpcDataConversionError::InvalidArgument(
                "NvLinkLogicalPartition organization_id is empty".to_string(),
            ));
        }

        let tenant_organization_id =
            TenantOrganizationId::try_from(conf.tenant_organization_id.clone()).map_err(|_| {
                RpcDataConversionError::InvalidArgument(conf.tenant_organization_id)
            })?;

        Ok(LogicalPartitionConfig {
            metadata: conf.metadata.unwrap_or_default().try_into()?,
            tenant_organization_id,
        })
    }
}

impl TryFrom<LogicalPartitionConfig> for rpc_forge::NvLinkLogicalPartitionConfig {
    type Error = RpcDataConversionError;
    fn try_from(src: LogicalPartitionConfig) -> Result<Self, Self::Error> {
        Ok(rpc_forge::NvLinkLogicalPartitionConfig {
            metadata: Some(src.metadata.into()),
            tenant_organization_id: src.tenant_organization_id.to_string(),
        })
    }
}

impl TryFrom<LogicalPartition> for rpc_forge::NvLinkLogicalPartition {
    type Error = RpcDataConversionError;
    fn try_from(src: LogicalPartition) -> Result<Self, Self::Error> {
        let mut state = match &src.partition_state {
            LogicalPartitionState::Provisioning => rpc_forge::TenantState::Provisioning,
            LogicalPartitionState::Ready => rpc_forge::TenantState::Ready,
            LogicalPartitionState::Error => rpc_forge::TenantState::Failed,
            LogicalPartitionState::Deleting => rpc_forge::TenantState::Terminating,
            LogicalPartitionState::Updating => rpc_forge::TenantState::Updating,
        };

        if is_marked_as_deleted(&src) {
            state = rpc_forge::TenantState::Terminating;
        }
        let status = Some(rpc_forge::NvLinkLogicalPartitionStatus {
            state: state as i32,
        });

        let config = rpc_forge::NvLinkLogicalPartitionConfig {
            metadata: Some(rpc::Metadata {
                name: src.name,
                description: src.description,
                ..Default::default()
            }),
            tenant_organization_id: src.tenant_organization_id.to_string(),
        };

        Ok(rpc_forge::NvLinkLogicalPartition {
            id: Some(src.id),
            config_version: src.config_version.version_string(),
            status,
            config: Some(config),
            created: Some(src.created.into()),
        })
    }
}

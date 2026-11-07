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

use model::ib::{IBMtu, IBRateLimit, IBServiceLevel};
use model::ib_partition::{
    IBPartition, IBPartitionConfig, IBPartitionControllerState, IbPartitionSearchFilter,
    NewIBPartition, state_sla,
};
use model::metadata::Metadata;
use model::tenant::TenantOrganizationId;

use crate as rpc;
use crate::forge as rpc_forge;

impl From<rpc::forge::IbPartitionSearchFilter> for IbPartitionSearchFilter {
    fn from(filter: rpc::forge::IbPartitionSearchFilter) -> Self {
        IbPartitionSearchFilter {
            tenant_org_id: filter.tenant_org_id,
            name: filter.name,
        }
    }
}

impl TryFrom<rpc_forge::IbPartitionCreationRequest> for NewIBPartition {
    type Error = rpc::errors::RpcDataConversionError;
    fn try_from(value: rpc_forge::IbPartitionCreationRequest) -> Result<Self, Self::Error> {
        let conf = value.config.ok_or_else(|| {
            rpc::errors::RpcDataConversionError::InvalidArgument(
                "IBPartition configuration is empty".to_string(),
            )
        })?;

        let id = value.id.unwrap_or(uuid::Uuid::new_v4().into());
        let name = conf.name.clone();

        Ok(NewIBPartition {
            id,
            config: IBPartitionConfig::try_from(conf)?,
            metadata: match value.metadata {
                Some(m) => Metadata::try_from(m)?,
                // Deprecated field handling
                None => Metadata {
                    name,
                    ..Default::default()
                },
            },
        })
    }
}

impl From<IBPartitionConfig> for rpc_forge::IbPartitionConfig {
    fn from(conf: IBPartitionConfig) -> Self {
        rpc_forge::IbPartitionConfig {
            name: conf.name, // Deprecated field
            tenant_organization_id: conf.tenant_organization_id.to_string(),
            pkey: conf.pkey.map(|k| k.to_string()),
        }
    }
}

impl TryFrom<rpc_forge::IbPartitionConfig> for IBPartitionConfig {
    type Error = rpc::errors::RpcDataConversionError;

    fn try_from(conf: rpc_forge::IbPartitionConfig) -> Result<Self, Self::Error> {
        if conf.tenant_organization_id.is_empty() {
            return Err(rpc::errors::RpcDataConversionError::InvalidArgument(
                "IBPartition organization_id is empty".to_string(),
            ));
        }

        let tenant_organization_id =
            TenantOrganizationId::try_from(conf.tenant_organization_id.clone()).map_err(|_| {
                rpc::errors::RpcDataConversionError::InvalidArgument(conf.tenant_organization_id)
            })?;

        Ok(IBPartitionConfig {
            name: conf.name,
            pkey: None,
            tenant_organization_id,
            mtu: None,
            rate_limit: None,
            service_level: None,
        })
    }
}

impl TryFrom<IBPartition> for rpc_forge::IbPartition {
    type Error = rpc::errors::RpcDataConversionError;
    fn try_from(src: IBPartition) -> Result<Self, Self::Error> {
        let mut state = match &src.controller_state.value {
            IBPartitionControllerState::Provisioning => rpc_forge::TenantState::Provisioning,
            IBPartitionControllerState::Ready => rpc_forge::TenantState::Ready,
            IBPartitionControllerState::Error { cause: _cause } => rpc_forge::TenantState::Failed,
            IBPartitionControllerState::Deleting => rpc_forge::TenantState::Terminating,
        };

        if src.is_marked_as_deleted() {
            state = rpc_forge::TenantState::Terminating;
        }

        let pkey = src
            .status
            .as_ref()
            .and_then(|s| s.pkey.map(|k| k.to_string()));

        let (partition, rate_limit, mtu, service_level) = match src.status {
            Some(s) => (
                s.partition,
                s.rate_limit.map(IBRateLimit::into),
                s.mtu.map(IBMtu::into),
                s.service_level.map(IBServiceLevel::into),
            ),
            None => (None, None, None, None),
        };

        let status = Some(rpc_forge::IbPartitionStatus {
            state: state as i32,
            state_reason: src.controller_state_outcome.map(|r| r.into()),
            state_sla: Some(
                state_sla(&src.controller_state.value, &src.controller_state.version).into(),
            ),
            enable_sharp: Some(false),
            partition,
            pkey,
            rate_limit,
            mtu,
            service_level,
        });

        let metadata = src.metadata.into();

        Ok(rpc_forge::IbPartition {
            id: Some(src.id),
            config_version: src.version.version_string(),
            config: Some(src.config.into()),
            status,
            metadata: Some(metadata),
        })
    }
}

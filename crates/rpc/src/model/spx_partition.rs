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

use model::metadata::Metadata;
use model::spx_partition::{NewSpxPartition, SpxPartition, SpxPartitionSearchFilter};

use crate::errors::RpcDataConversionError;
use crate::forge as rpc;

impl From<rpc::SpxPartitionSearchFilter> for SpxPartitionSearchFilter {
    fn from(filter: rpc::SpxPartitionSearchFilter) -> Self {
        SpxPartitionSearchFilter {
            name: filter.name,
            tenant_org_id: filter.tenant_org_id,
        }
    }
}

impl TryFrom<rpc::SpxPartitionCreationRequest> for NewSpxPartition {
    type Error = RpcDataConversionError;
    fn try_from(req: rpc::SpxPartitionCreationRequest) -> Result<Self, Self::Error> {
        if req.tenant_organization_id.is_empty() {
            return Err(RpcDataConversionError::InvalidArgument(
                "tenant_organization_id is required".to_string(),
            ));
        }

        let id = req.id.unwrap_or_else(|| uuid::Uuid::new_v4().into());

        let metadata = req
            .metadata
            .map(Metadata::try_from)
            .transpose()?
            .unwrap_or_default();

        metadata.validate(true).map_err(|error| {
            RpcDataConversionError::InvalidArgument(format!(
                "SPX partition metadata is not valid: {error}"
            ))
        })?;

        Ok(NewSpxPartition {
            id,
            name: metadata.name,
            description: metadata.description,
            labels: metadata.labels,
            tenant_organization_id: req.tenant_organization_id,
            vni: req.vni.map(|v| v.try_into()).transpose().map_err(
                |e: std::num::TryFromIntError| {
                    RpcDataConversionError::InvalidValue(
                        format!(
                            "`{}` cannot be converted to VNI",
                            req.vni.unwrap_or_default()
                        ),
                        e.to_string(),
                    )
                },
            )?,
        })
    }
}

impl TryFrom<SpxPartition> for rpc::SpxPartition {
    type Error = RpcDataConversionError;
    fn try_from(src: SpxPartition) -> Result<Self, Self::Error> {
        if src.vni.is_none() {
            return Err(RpcDataConversionError::InvalidValue(
                "VNI is required".to_string(),
                "VNI is required".to_string(),
            ));
        }
        let vni = src.vni.unwrap();
        Ok(rpc::SpxPartition {
            id: Some(src.id),
            metadata: Some(
                Metadata {
                    name: src.name,
                    description: src.description,
                    labels: src.labels,
                }
                .into(),
            ),
            tenant_organization_id: src.tenant_organization_id.to_string(),
            vni: vni as u32,
        })
    }
}

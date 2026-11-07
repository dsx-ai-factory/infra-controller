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

use model::nvl_partition::{NvLinkPartitionSearchFilter, NvlPartition};

use crate::errors::RpcDataConversionError;
use crate::forge as rpc_forge;

impl From<rpc_forge::NvLinkPartitionSearchFilter> for NvLinkPartitionSearchFilter {
    fn from(filter: rpc_forge::NvLinkPartitionSearchFilter) -> Self {
        NvLinkPartitionSearchFilter {
            tenant_organization_id: filter.tenant_organization_id,
            name: filter.name,
        }
    }
}

impl TryFrom<NvlPartition> for rpc_forge::NvLinkPartition {
    type Error = RpcDataConversionError;
    fn try_from(src: NvlPartition) -> Result<Self, Self::Error> {
        Ok(rpc_forge::NvLinkPartition {
            id: Some(src.id),
            name: src.name.clone().into(),
            // Keep the existing proto field for wire compatibility; it now carries the NMX-C partition ID.
            nmx_m_id: src.nmx_c_partition_id.to_string(),
            domain_uuid: Some(src.domain_uuid),
            logical_partition_id: src.logical_partition_id,
        })
    }
}

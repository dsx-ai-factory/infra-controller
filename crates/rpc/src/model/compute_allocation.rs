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
use model::compute_allocation::ComputeAllocation;

use crate::errors::RpcDataConversionError;
use crate::forge as rpc;

impl TryFrom<ComputeAllocation> for rpc::ComputeAllocation {
    type Error = RpcDataConversionError;

    fn try_from(compute_alloc: ComputeAllocation) -> Result<Self, Self::Error> {
        let attributes = rpc::ComputeAllocationAttributes {
            instance_type_id: compute_alloc.instance_type_id.to_string(),
            count: compute_alloc.count,
        };

        Ok(rpc::ComputeAllocation {
            id: Some(compute_alloc.id),
            tenant_organization_id: compute_alloc.tenant_organization_id.to_string(),
            version: compute_alloc.version.to_string(),
            attributes: Some(attributes),
            created_at: Some(compute_alloc.created.to_string()),
            created_by: compute_alloc.created_by,
            updated_by: compute_alloc.updated_by,
            metadata: Some(rpc::Metadata {
                name: compute_alloc.metadata.name,
                description: compute_alloc.metadata.description,
                labels: compute_alloc
                    .metadata
                    .labels
                    .iter()
                    .map(|(key, value)| rpc::Label {
                        key: key.to_owned(),
                        value: value.to_owned().none_if_empty(),
                    })
                    .collect(),
            }),
        })
    }
}

/* ********************************** */
/*              Tests                 */
/* ********************************** */

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use config_version::ConfigVersion;
    use model::metadata::Metadata;

    use super::*;
    use crate::forge as rpc;

    #[test]
    fn test_model_compute_allocation_to_rpc_conversion() {
        let version = ConfigVersion::initial();

        let req_type = rpc::ComputeAllocation {
            id: Some("dbe71f32-1bdc-11f1-8101-3b10d91c938c".parse().unwrap()),
            version: version.to_string(),
            metadata: Some(rpc::Metadata {
                name: "fancy name".to_string(),
                description: "".to_string(),
                labels: vec![],
            }),
            tenant_organization_id: "theorg".to_string(),
            attributes: Some(rpc::ComputeAllocationAttributes {
                instance_type_id: "12345".to_string(),
                count: 10,
            }),
            created_at: Some("2023-01-01 00:00:00 UTC".to_string()),
            created_by: Some("user1".to_string()),
            updated_by: Some("user2".to_string()),
        };

        let compute_alloc = ComputeAllocation {
            id: "dbe71f32-1bdc-11f1-8101-3b10d91c938c".parse().unwrap(),
            deleted: None,
            created: "2023-01-01 00:00:00 UTC".parse().unwrap(),
            version,
            metadata: Metadata {
                name: "fancy name".to_string(),
                description: "".to_string(),
                labels: HashMap::new(),
            },
            tenant_organization_id: "theorg".parse().unwrap(),

            instance_type_id: "12345".parse().unwrap(),
            count: 10,
            created_by: Some("user1".to_string()),
            updated_by: Some("user2".to_string()),
        };

        // Verify that we can go from an internal compute allocation to the
        // protobuf ComputeAllocation message
        assert_eq!(
            req_type,
            rpc::ComputeAllocation::try_from(compute_alloc).unwrap()
        );
    }
}

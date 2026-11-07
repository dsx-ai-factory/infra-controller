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

//! Model for operating system definitions (CRUD resource, table operating_systems).
//!
//! Conversions follow db <-> model <-> RPC: database rows are converted to
//! this model (in api-db), then this model is converted to RPC types (here).
//! The model type name matches the RPC message name (OperatingSystem).

use carbide_ipxe_renderer::IpxeTemplateArtifactCacheStrategy;
use carbide_uuid::ipxe_template::IpxeTemplateId;
use carbide_uuid::operating_system::OperatingSystemId;
use model::operating_system_definition::{OS_TYPE_IPXE, OS_TYPE_TEMPLATED_IPXE, OperatingSystem};

use crate::forge as forgerpc;

impl From<OperatingSystem> for forgerpc::OperatingSystem {
    fn from(m: OperatingSystem) -> Self {
        let os_type = match m.type_.as_str() {
            OS_TYPE_IPXE => forgerpc::OperatingSystemType::OsTypeIpxe,
            OS_TYPE_TEMPLATED_IPXE => forgerpc::OperatingSystemType::OsTypeTemplatedIpxe,
            _ => forgerpc::OperatingSystemType::OsTypeUnspecified,
        };
        Self {
            id: Some(
                m.id.parse::<OperatingSystemId>()
                    .expect("operating system id from model must be a valid UUID"),
            ),
            name: m.name,
            description: m.description,
            tenant_organization_id: m.tenant_organization_id,
            r#type: os_type as i32,
            status: forgerpc::TenantState::from_str_name(&m.status.to_uppercase())
                .unwrap_or_default() as i32,
            is_active: m.is_active,
            allow_override: m.allow_override,
            phone_home_enabled: m.phone_home_enabled,
            user_data: m.user_data,
            created: m.created,
            updated: m.updated,
            ipxe_script: m.ipxe_script,
            ipxe_template_id: m.ipxe_template_id.map(|id| {
                id.parse::<IpxeTemplateId>()
                    .expect("ipxe_template_id from model must be a valid UUID")
            }),
            ipxe_template_parameters: m
                .ipxe_template_parameters
                .into_iter()
                .map(|p| forgerpc::IpxeTemplateParameter {
                    name: p.name,
                    value: p.value,
                })
                .collect(),
            ipxe_template_artifacts: m
                .ipxe_template_artifacts
                .into_iter()
                .map(|a| forgerpc::IpxeTemplateArtifact {
                    name: a.name,
                    url: a.url,
                    sha: a.sha,
                    auth_type: a.auth_type,
                    auth_token: a.auth_token,
                    cache_strategy: match a.cache_strategy {
                        IpxeTemplateArtifactCacheStrategy::CacheAsNeeded => 0,
                        IpxeTemplateArtifactCacheStrategy::LocalOnly => 1,
                        IpxeTemplateArtifactCacheStrategy::CachedOnly => 2,
                        IpxeTemplateArtifactCacheStrategy::RemoteOnly => 3,
                    },
                    cached_url: a.cached_url,
                })
                .collect(),
            ipxe_template_definition_hash: m.ipxe_template_definition_hash,
        }
    }
}

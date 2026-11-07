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

use ::rpc::forge::{
    self as forgerpc, IpxeTemplateArtifact, IpxeTemplateParameter, OperatingSystem,
};
use serde::Serialize;

use crate::errors::{CarbideCliError, CarbideCliResult};

pub(super) fn str_to_os_id(
    id: &str,
) -> CarbideCliResult<::carbide_uuid::operating_system::OperatingSystemId> {
    let id = uuid::Uuid::parse_str(id)
        .map_err(|e| CarbideCliError::GenericError(e.to_string()))?
        .into();
    Ok(id)
}

pub(super) fn str_to_ipxe_template_id(
    id: &str,
) -> CarbideCliResult<::carbide_uuid::ipxe_template::IpxeTemplateId> {
    let id = uuid::Uuid::parse_str(id)
        .map_err(|e| CarbideCliError::GenericError(e.to_string()))?
        .into();
    Ok(id)
}

/// Parse a "key=value" string into an `IpxeTemplateParameter`.
pub(super) fn parse_param(s: &str) -> Result<IpxeTemplateParameter, String> {
    let (name, value) = s
        .split_once('=')
        .ok_or_else(|| format!("expected KEY=VALUE, got '{s}'"))?;
    Ok(IpxeTemplateParameter {
        name: name.to_string(),
        value: value.to_string(),
    })
}

/// Local serializable mirror of `OperatingSystem` for JSON output.
#[derive(Serialize)]
pub(super) struct SerializableOs {
    id: String,
    name: String,
    description: Option<String>,
    org: String,
    #[serde(rename = "type")]
    os_type: String,
    status: String,
    is_active: bool,
    allow_override: bool,
    phone_home_enabled: bool,
    user_data: Option<String>,
    created: String,
    updated: String,
    ipxe_script: Option<String>,
    ipxe_template_id: Option<String>,
    ipxe_template_parameters: Vec<SerializableParam>,
    ipxe_template_artifacts: Vec<SerializableArtifact>,
    ipxe_template_definition_hash: Option<String>,
}

#[derive(Serialize)]
struct SerializableParam {
    name: String,
    value: String,
}

#[derive(Serialize)]
pub(super) struct SerializableArtifact {
    name: String,
    url: String,
    sha: Option<String>,
    auth_type: Option<String>,
    cache_strategy: String,
    cached_url: Option<String>,
}

impl From<IpxeTemplateParameter> for SerializableParam {
    fn from(p: IpxeTemplateParameter) -> Self {
        Self {
            name: p.name,
            value: p.value,
        }
    }
}

impl From<IpxeTemplateArtifact> for SerializableArtifact {
    fn from(a: IpxeTemplateArtifact) -> Self {
        use ::rpc::forge::IpxeTemplateArtifactCacheStrategy;
        let cache_strategy = match IpxeTemplateArtifactCacheStrategy::try_from(a.cache_strategy) {
            Ok(IpxeTemplateArtifactCacheStrategy::CacheAsNeeded) => "cache_as_needed",
            Ok(IpxeTemplateArtifactCacheStrategy::LocalOnly) => "local_only",
            Ok(IpxeTemplateArtifactCacheStrategy::CachedOnly) => "cached_only",
            Ok(IpxeTemplateArtifactCacheStrategy::RemoteOnly) => "remote_only",
            _ => "cache_as_needed",
        };
        Self {
            name: a.name,
            url: a.url,
            sha: a.sha,
            auth_type: a.auth_type,
            cache_strategy: cache_strategy.to_string(),
            cached_url: a.cached_url,
        }
    }
}

impl From<OperatingSystem> for SerializableOs {
    fn from(os: OperatingSystem) -> Self {
        Self {
            id: os.id.map(|u| u.to_string()).unwrap_or_default(),
            name: os.name,
            description: os.description,
            org: os.tenant_organization_id.unwrap_or_default(),
            os_type: forgerpc::OperatingSystemType::try_from(os.r#type)
                .map(|t| t.as_str_name().to_string())
                .unwrap_or_else(|_| os.r#type.to_string()),
            status: forgerpc::TenantState::try_from(os.status)
                .map(|s| s.as_str_name().to_string())
                .unwrap_or_else(|_| os.status.to_string()),
            is_active: os.is_active,
            allow_override: os.allow_override,
            phone_home_enabled: os.phone_home_enabled,
            user_data: os.user_data,
            created: os.created,
            updated: os.updated,
            ipxe_script: os.ipxe_script,
            ipxe_template_id: os.ipxe_template_id.map(|id| id.to_string()),
            ipxe_template_parameters: os
                .ipxe_template_parameters
                .into_iter()
                .map(Into::into)
                .collect(),
            ipxe_template_artifacts: os
                .ipxe_template_artifacts
                .into_iter()
                .map(Into::into)
                .collect(),
            ipxe_template_definition_hash: os.ipxe_template_definition_hash,
        }
    }
}

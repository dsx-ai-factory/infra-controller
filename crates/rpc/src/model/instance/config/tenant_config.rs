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

use model::instance::config::tenant_config::TenantConfig;
use model::tenant::TenantOrganizationId;

use crate as rpc;
use crate::errors::RpcDataConversionError;

impl TryFrom<rpc::forge::TenantConfig> for TenantConfig {
    type Error = RpcDataConversionError;

    fn try_from(config: rpc::forge::TenantConfig) -> Result<Self, Self::Error> {
        let truncated_hostname = config.hostname.map(|mut name| {
            if name.len() > 63 {
                name.truncate(63);
                tracing::warn!("Hostname has been truncated to 63 characters.")
            }
            name
        });

        Ok(Self {
            tenant_organization_id: TenantOrganizationId::try_from(
                config.tenant_organization_id.clone(),
            )
            .map_err(|_| RpcDataConversionError::InvalidTenantOrg(config.tenant_organization_id))?,
            tenant_keyset_ids: config.tenant_keyset_ids,
            hostname: truncated_hostname,
        })
    }
}

impl TryFrom<TenantConfig> for rpc::forge::TenantConfig {
    type Error = RpcDataConversionError;

    fn try_from(config: TenantConfig) -> Result<rpc::forge::TenantConfig, Self::Error> {
        Ok(Self {
            tenant_organization_id: config.tenant_organization_id.to_string(),
            tenant_keyset_ids: config.tenant_keyset_ids,
            hostname: config.hostname,
        })
    }
}

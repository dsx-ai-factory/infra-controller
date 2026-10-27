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
use std::collections::HashMap;
use std::net::{Ipv4Addr, Ipv6Addr};

use carbide_uuid::dhcp_option_set::DhcpOptionSetId;
use chrono::prelude::*;
use config_version::ConfigVersion;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use sqlx::postgres::PgRow;

use crate::ConfigValidationError;
use crate::metadata::Metadata;
use crate::tenant::TenantOrganizationId;

/// DhcpOptionSet is a tenant-owned set of values that replace
/// the values NICo sends in its DHCP replies.
#[derive(Clone, Debug, PartialEq)]
pub struct DhcpOptionSet {
    pub id: DhcpOptionSetId,
    pub tenant_organization_id: TenantOrganizationId,
    pub metadata: Metadata,
    pub options: DhcpOptions,
    pub version: ConfigVersion,
    pub created: DateTime<Utc>,
}

impl<'r> sqlx::FromRow<'r, PgRow> for DhcpOptionSet {
    fn from_row(row: &'r PgRow) -> Result<Self, sqlx::Error> {
        let labels: sqlx::types::Json<HashMap<String, String>> = row.try_get("labels")?;
        let options: sqlx::types::Json<DhcpOptions> = row.try_get("options")?;
        let tenant_organization_id: String = row.try_get("tenant_organization_id")?;

        Ok(DhcpOptionSet {
            id: row.try_get("id")?,
            tenant_organization_id: tenant_organization_id
                .parse::<TenantOrganizationId>()
                .map_err(|e| sqlx::Error::Decode(Box::new(e)))?,
            metadata: Metadata {
                name: row.try_get("name")?,
                description: row.try_get("description")?,
                labels: labels.0,
            },
            options: options.0,
            version: row.try_get("version")?,
            created: row.try_get("created")?,
        })
    }
}

/// Values left unset or empty keep NICo's default.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DhcpOptions {
    pub v4: Option<DhcpOptionsV4>,
    pub v6: Option<DhcpOptionsV6>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DhcpOptionsV4 {
    pub boot_url: Option<String>,
    pub dns_servers: Vec<Ipv4Addr>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DhcpOptionsV6 {
    pub boot_url: Option<String>,
    pub dns_servers: Vec<Ipv6Addr>,
}

impl DhcpOptions {
    pub fn validate(&self) -> Result<(), ConfigValidationError> {
        let mut is_set = false;
        if let Some(v4) = &self.v4 {
            validate_boot_url(&v4.boot_url)?;
            is_set |= v4.boot_url.is_some() || !v4.dns_servers.is_empty();
        }
        if let Some(v6) = &self.v6 {
            validate_boot_url(&v6.boot_url)?;
            is_set |= v6.boot_url.is_some() || !v6.dns_servers.is_empty();
        }

        if !is_set {
            return Err(ConfigValidationError::invalid_value(
                "at least one DHCP option must be set",
            ));
        }
        Ok(())
    }
}

fn validate_boot_url(boot_url: &Option<String>) -> Result<(), ConfigValidationError> {
    let Some(boot_url) = boot_url else {
        return Ok(());
    };
    match url::Url::parse(boot_url) {
        Ok(url) if matches!(url.scheme(), "http" | "https") => Ok(()),
        _ => Err(ConfigValidationError::invalid_value(format!(
            "boot URL `{boot_url}` must be a valid http or https URL"
        ))),
    }
}

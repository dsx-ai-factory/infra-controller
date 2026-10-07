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

use std::collections::HashSet;

use once_cell::sync::Lazy;
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::ConfigValidationError;
use crate::tenant::TenantOrganizationId;

const MAX_KEYSET_IDS: usize = 10;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TenantConfig {
    /// Identifies the tenant that uses this instance
    pub tenant_organization_id: TenantOrganizationId,

    pub tenant_keyset_ids: Vec<String>,

    pub hostname: Option<String>,
}

/// A single DNS label: lowercase alphanumeric and dashes, 1-63 octets per RFC 1035.
/// This is also within the Linux `HOST_NAME_MAX` of 64 bytes.
pub static HOSTNAME_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?$").unwrap());

impl TenantConfig {
    /// Validates the tenant configuration
    pub fn validate(&self) -> Result<(), ConfigValidationError> {
        // Perform a check for duplicate keysets
        // and throw back an error to the caller if found.
        let mut unique_keyset_ids: HashSet<&String> = HashSet::new();
        for keyset_id in self.tenant_keyset_ids.iter() {
            if !unique_keyset_ids.insert(keyset_id) {
                return Err(ConfigValidationError::DuplicateTenantKeysetId(
                    keyset_id.into(),
                ));
            }
        }
        if let Some(hostname) = &self.hostname
            && !HOSTNAME_RE.is_match(hostname)
        {
            return Err(ConfigValidationError::InvalidValue(
                    "Hostname does not meet DNS requirements (lowercase alphanumeric characters and dashes). Valid examples: test, test-hostname, host-1".to_string()
                ));
        }

        // check to see if we are over the max IDs or not
        if self.tenant_keyset_ids.len() > MAX_KEYSET_IDS {
            return Err(ConfigValidationError::TenantKeysetIdsOverMax(
                MAX_KEYSET_IDS,
            ));
        }

        Ok(())
    }

    pub fn verify_update_allowed_to(&self, new_config: &Self) -> Result<(), ConfigValidationError> {
        if self.tenant_organization_id != new_config.tenant_organization_id {
            return Err(ConfigValidationError::ConfigCanNotBeModified(
                "TenantConfig::tenant_organization_id".to_string(),
            ));
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::mem::discriminant;

    use carbide_test_support::Outcome::*;
    use carbide_test_support::scenarios;

    use super::*;

    #[test]
    fn serialize_tenant_config() {
        let config = TenantConfig {
            tenant_organization_id: TenantOrganizationId::try_from("TenantA".to_string()).unwrap(),
            tenant_keyset_ids: vec![],
            hostname: Some("test-instance".to_string()),
        };

        let serialized = serde_json::to_string(&config).unwrap();
        assert_eq!(
            serialized,
            "{\"tenant_organization_id\":\"TenantA\",\"tenant_keyset_ids\":[],\"hostname\":\"test-instance\"}"
        );
        assert_eq!(
            serde_json::from_str::<TenantConfig>(&serialized).unwrap(),
            config
        );
    }

    #[test]
    fn validate_tenant_config() {
        // `TenantConfig::validate`: duplicate keyset IDs are rejected, unique ones
        // pass. The error type (ConfigValidationError) is not PartialEq, so failing
        // rows assert the rejected *variant* via its discriminant rather than the
        // exact error value.
        scenarios!(
            run = |config: TenantConfig| config.validate().map_err(|e| discriminant(&e));
            "duplicate keyset ids are rejected" {
                TenantConfig {
                    tenant_organization_id: TenantOrganizationId::try_from(
                        "TenantA".to_string(),
                    )
                    .unwrap(),
                    tenant_keyset_ids: vec![
                        "a".to_string(),
                        "b".to_string(),
                        "c".to_string(),
                        "a".to_string(),
                    ],
                    hostname: Some("test-instance".to_string()),
                } => FailsWith(discriminant(
                    &ConfigValidationError::DuplicateTenantKeysetId(String::new()),
                )),
            }

            "unique keyset ids validate" {
                TenantConfig {
                    tenant_organization_id: TenantOrganizationId::try_from(
                        "TenantA".to_string(),
                    )
                    .unwrap(),
                    tenant_keyset_ids: vec!["a".to_string(), "b".to_string(), "c".to_string()],
                    hostname: Some("test-instance".to_string()),
                } => Yields(()),
            }

            "63-octet hostname (DNS label max) validates" {
                TenantConfig {
                    tenant_organization_id: TenantOrganizationId::try_from(
                        "TenantA".to_string(),
                    )
                    .unwrap(),
                    tenant_keyset_ids: vec![],
                    hostname: Some("a".repeat(63)),
                } => Yields(()),
            }

            "64-octet hostname exceeds the DNS label max and is rejected" {
                TenantConfig {
                    tenant_organization_id: TenantOrganizationId::try_from(
                        "TenantA".to_string(),
                    )
                    .unwrap(),
                    tenant_keyset_ids: vec![],
                    hostname: Some("a".repeat(64)),
                } => FailsWith(discriminant(
                    &ConfigValidationError::InvalidValue(String::new()),
                )),
            }
        );
    }
}

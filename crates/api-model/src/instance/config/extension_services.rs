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

use carbide_uuid::extension_service::ExtensionServiceId;
use chrono::{DateTime, Utc};
use config_version::ConfigVersion;
use serde::{Deserialize, Serialize};

use crate::ConfigValidationError;

/// Extension service configuration for a single service
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstanceExtensionServiceConfig {
    pub service_id: ExtensionServiceId,
    pub version: ConfigVersion,
    /// Immutable registration policy, populated by the API during admission.
    #[serde(default)]
    pub dpu_target: Option<crate::extension_service::DpuTarget>,
    pub removed: Option<DateTime<Utc>>, // We need to track terminating services
}

/// Extension services configuration for an instance
///
/// Note: the actual extension services config is the set of active services and services being terminated.
/// This is different from the extension services config obtained from RPC call since user only
/// considers active services when configuring extension services. However, inside the DB, we need
/// to track both active services and services being terminated.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct InstanceExtensionServicesConfig {
    pub service_configs: Vec<InstanceExtensionServiceConfig>,
}

impl InstanceExtensionServicesConfig {
    pub fn verify_update_allowed_to(
        &self,
        _new_config: &Self,
    ) -> Result<(), ConfigValidationError> {
        Ok(())
    }

    /// Determines if the new config is different from the current config
    /// We expect the new_config to come from RPC call issued by the user and hence only
    /// contain active services.
    /// This function compares the current active services with the new config to detect any changes
    /// in the active services configured.
    ///
    /// Returns:
    /// - `true` if an update is requested (configs are different)
    /// - `false` if no update needed (configs are the same)
    pub fn is_extension_services_config_update_requested(&self, new_config: &Self) -> bool {
        let current_active: HashSet<_> = self
            .active_services() // Only active services are considered for updates
            .iter()
            .map(|s| (s.service_id, s.version.to_string()))
            .collect();

        // Get new services (should already only contain active services from RPC, but we still do some cleaning up)
        let new_services: HashSet<_> = new_config
            .service_configs
            .iter()
            .filter(|s| s.removed.is_none()) // Only active services are considered for updates
            .map(|s| (s.service_id, s.version.to_string()))
            .collect();

        current_active != new_services
    }

    /// Returns whether `new_config` introduces an active service or service version that is not
    /// already active in the current config.
    pub fn has_new_active_services(&self, new_config: &Self) -> bool {
        let current_active: HashSet<_> = self
            .active_services()
            .iter()
            .map(|s| (s.service_id, s.version.to_string()))
            .collect();

        new_config
            .active_services()
            .iter()
            .any(|s| !current_active.contains(&(s.service_id, s.version.to_string())))
    }

    /// Calculates the new actual extension services config based on the current config and the new config.
    /// For any current active service that is not in the new config, it will be marked as deleted.
    /// For any new service that is not in the current config, it will be added to the new config.
    ///
    /// Param:
    /// - new_config: The new extension services config
    ///
    /// Returns:
    /// - The new extension services config
    pub fn calculate_new_extension_services_config(&self, new_config: &Self) -> Self {
        let now: DateTime<Utc> = Utc::now();

        // New services config = new active services + new terminating services
        // We first set the result to be the new active services, which is the new config's active services
        let mut result: Vec<InstanceExtensionServiceConfig> = Vec::new();

        // Add new active services to the result, which is the new config's active services
        result.extend(
            new_config
                .service_configs
                .iter()
                .filter(|s| s.removed.is_none())
                .cloned()
                .collect::<Vec<_>>(),
        );

        // Now we add the new terminating services to the result, which is the old config's services that's not in the new config's active services
        let want_active: HashSet<(ExtensionServiceId, String)> = new_config
            .service_configs
            .iter()
            .filter(|s| s.removed.is_none())
            .map(|s| (s.service_id, s.version.to_string()))
            .collect();
        let current = self.service_configs.clone();
        for service in current {
            if !want_active.contains(&(service.service_id, service.version.to_string())) {
                if service.removed.is_some() {
                    // The service is already being terminated, so we just need to add it back to the new config
                    result.push(service.clone());
                } else {
                    // The service is not being terminated, so we need to mark it as terminated
                    result.push(InstanceExtensionServiceConfig {
                        dpu_target: service.dpu_target,
                        service_id: service.service_id,
                        version: service.version,
                        removed: Some(now),
                    });
                }
            }
        }

        InstanceExtensionServicesConfig {
            service_configs: result,
        }
    }

    /// Get all active (non-removed) services
    pub fn active_services(&self) -> Vec<&InstanceExtensionServiceConfig> {
        self.service_configs
            .iter()
            .filter(|s| s.removed.is_none())
            .collect()
    }

    /// Get all terminating (removed) services
    pub fn terminating_services(&self) -> Vec<&InstanceExtensionServiceConfig> {
        self.service_configs
            .iter()
            .filter(|s| s.removed.is_some())
            .collect()
    }

    /// Removes extension service entries that match a fully-terminated `(service_id, version)`.
    pub fn remove_terminated_services(
        &self,
        keys_to_remove: &[(ExtensionServiceId, ConfigVersion)],
    ) -> Self {
        let mut config = self.clone();
        config.service_configs.retain(|s| {
            !keys_to_remove
                .iter()
                .any(|&(id, ver)| id == s.service_id && ver == s.version)
        });
        config
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use carbide_uuid::extension_service::ExtensionServiceId;
    use chrono::Utc;
    use config_version::ConfigVersion;

    use super::{InstanceExtensionServiceConfig, InstanceExtensionServicesConfig};

    #[test]
    fn extension_service_remove_terminated_services() {
        let sid = ExtensionServiceId::from_str("00000000-0000-0000-0000-000000000001").unwrap();
        let init_version = ConfigVersion::initial();
        let second_version = init_version.increment();

        let config = InstanceExtensionServicesConfig {
            service_configs: vec![
                InstanceExtensionServiceConfig {
                    dpu_target: None,
                    service_id: sid,
                    version: second_version,
                    removed: None,
                },
                InstanceExtensionServiceConfig {
                    dpu_target: None,
                    service_id: sid,
                    version: init_version,
                    removed: Some(Utc::now()),
                },
            ],
        };

        let cleaned = config.remove_terminated_services(&[(sid, init_version)]);

        assert_eq!(cleaned.service_configs.len(), 1);
        assert_eq!(cleaned.service_configs[0].service_id, sid);
        assert_eq!(cleaned.service_configs[0].version, second_version);
        assert!(cleaned.service_configs[0].removed.is_none());
    }

    #[test]
    fn extension_service_detects_only_new_active_services() {
        let existing_id =
            ExtensionServiceId::from_str("00000000-0000-0000-0000-000000000001").unwrap();
        let new_id = ExtensionServiceId::from_str("00000000-0000-0000-0000-000000000002").unwrap();
        let initial_version = ConfigVersion::initial();
        let current = InstanceExtensionServicesConfig {
            service_configs: vec![InstanceExtensionServiceConfig {
                dpu_target: None,
                service_id: existing_id,
                version: initial_version,
                removed: None,
            }],
        };

        let detached = InstanceExtensionServicesConfig::default();
        assert!(!current.has_new_active_services(&detached));

        let unchanged = current.clone();
        assert!(!current.has_new_active_services(&unchanged));

        let added = InstanceExtensionServicesConfig {
            service_configs: vec![
                unchanged.service_configs[0].clone(),
                InstanceExtensionServiceConfig {
                    dpu_target: None,
                    service_id: new_id,
                    version: initial_version,
                    removed: None,
                },
            ],
        };
        assert!(current.has_new_active_services(&added));

        let upgraded = InstanceExtensionServicesConfig {
            service_configs: vec![InstanceExtensionServiceConfig {
                dpu_target: None,
                service_id: existing_id,
                version: initial_version.increment(),
                removed: None,
            }],
        };
        assert!(current.has_new_active_services(&upgraded));
    }
}

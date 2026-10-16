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

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::SecretsError;
use crate::credentials::{
    BmcCredentialType, CredentialKey, CredentialReader, CredentialType, Credentials,
    MqttCredentialType,
};

mod env;
mod file;

pub use env::{EnvCredentials, EnvCredentialsConfig};
pub use file::{FileCredentialsConfig, FileCredentialsWatcher};

/// Flat username/password struct for serde compatibility with env vars and
/// config files, where the externally-tagged `Credentials` enum layout
/// (`{"UsernamePassword": {...}}`) is not ergonomic.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct UsernamePassword {
    pub username: String,
    pub password: String,
}

impl From<UsernamePassword> for Credentials {
    fn from(up: UsernamePassword) -> Self {
        Credentials::UsernamePassword {
            username: up.username,
            password: up.password,
        }
    }
}

impl From<Credentials> for UsernamePassword {
    fn from(creds: Credentials) -> Self {
        match creds {
            Credentials::UsernamePassword { username, password } => {
                UsernamePassword { username, password }
            }
        }
    }
}

/// Machine identity credentials (encryption keys for signing keys and token delegation).
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct MachineIdentityConfig {
    /// Map of encryption key id (e.g. `kv1`) to base64-encoded 32-byte AES key material (`openssl rand -base64 32`).
    pub encryption_keys: HashMap<String, String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct CredentialSnapshot {
    /// Legacy single DPU BMC factory default (catch-all when no per-model entry exists).
    /// Kept for backward compatibility; prefer `dpu_redfish_factory_default_by_model`.
    pub dpu_redfish_factory_default: Option<UsernamePassword>,
    /// Per-model DPU BMC factory defaults, keyed by [`bmc_vendor::DpuModel`].
    /// Takes precedence over `dpu_redfish_factory_default` when present.
    pub dpu_redfish_factory_default_by_model: HashMap<bmc_vendor::DpuModel, UsernamePassword>,
    pub dpu_redfish_site_default: Option<UsernamePassword>,
    pub host_redfish_factory_default_by_vendor: HashMap<bmc_vendor::BMCVendor, UsernamePassword>,
    pub host_redfish_site_default: Option<UsernamePassword>,
    pub ufm_auth_by_fabric: HashMap<String, UsernamePassword>,
    pub dpu_uefi_factory_default: Option<UsernamePassword>,
    pub dpu_uefi_site_default: Option<UsernamePassword>,
    pub host_uefi_site_default: Option<UsernamePassword>,
    pub nmxm_auth_by_id: HashMap<String, UsernamePassword>,
    pub mqtt_auth_by_credential_type: HashMap<MqttCredentialType, UsernamePassword>,
    pub machine_identity: Option<MachineIdentityConfig>,
    pub bmc_site_wide_root: Option<UsernamePassword>,
}

impl CredentialSnapshot {
    pub fn get_credentials(&self, key: &CredentialKey) -> Option<Credentials> {
        match key {
            CredentialKey::DpuRedfish { credential_type } => match credential_type {
                CredentialType::DpuHardwareDefault { model } => self
                    .dpu_redfish_factory_default_by_model
                    .get(model)
                    .cloned()
                    .or_else(|| self.dpu_redfish_factory_default.clone())
                    .map(Into::into),
                CredentialType::SiteDefault => {
                    self.dpu_redfish_site_default.clone().map(Into::into)
                }
                CredentialType::HostHardwareDefault { .. } => None,
            },
            CredentialKey::HostRedfish { credential_type } => match credential_type {
                CredentialType::HostHardwareDefault { vendor } => self
                    .host_redfish_factory_default_by_vendor
                    .get(vendor)
                    .cloned()
                    .map(Into::into),
                CredentialType::SiteDefault => {
                    self.host_redfish_site_default.clone().map(Into::into)
                }
                CredentialType::DpuHardwareDefault { .. } => None,
            },
            CredentialKey::UfmAuth { fabric } => {
                self.ufm_auth_by_fabric.get(fabric).cloned().map(Into::into)
            }
            CredentialKey::DpuUefi { credential_type } => match credential_type {
                CredentialType::DpuHardwareDefault { .. } => {
                    self.dpu_uefi_factory_default.clone().map(Into::into)
                }
                CredentialType::SiteDefault => self.dpu_uefi_site_default.clone().map(Into::into),
                CredentialType::HostHardwareDefault { .. } => None,
            },
            CredentialKey::HostUefi {
                credential_type: CredentialType::SiteDefault,
            } => self.host_uefi_site_default.clone().map(Into::into),
            CredentialKey::NmxM { nmxm_id } => {
                self.nmxm_auth_by_id.get(nmxm_id).cloned().map(Into::into)
            }
            CredentialKey::MqttAuth { credential_type } => self
                .mqtt_auth_by_credential_type
                .get(credential_type)
                .cloned()
                .map(Into::into),
            CredentialKey::MachineIdentityEncryptionKey { key_id } => self
                .machine_identity
                .as_ref()
                .and_then(|mi| mi.encryption_keys.get(key_id).cloned())
                .map(|secret| Credentials::UsernamePassword {
                    username: key_id.clone(),
                    password: secret,
                }),
            CredentialKey::BmcCredentials {
                credential_type: BmcCredentialType::SiteWideRoot,
            } => self.bmc_site_wide_root.clone().map(Into::into),
            _ => None,
        }
    }
}

#[async_trait]
impl CredentialReader for CredentialSnapshot {
    async fn get_credentials(
        &self,
        key: &CredentialKey,
    ) -> Result<Option<Credentials>, SecretsError> {
        Ok(CredentialSnapshot::get_credentials(self, key))
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;

    use super::*;

    fn up(user: &str, pass: &str) -> UsernamePassword {
        UsernamePassword {
            username: user.to_string(),
            password: pass.to_string(),
        }
    }

    fn cred(user: &str, pass: &str) -> Credentials {
        Credentials::UsernamePassword {
            username: user.to_string(),
            password: pass.to_string(),
        }
    }

    fn populated_snapshot() -> CredentialSnapshot {
        let mut host_vendors = HashMap::new();
        host_vendors.insert(bmc_vendor::BMCVendor::Dell, up("dell-u", "dell-p"));

        let mut dpu_models = HashMap::new();
        dpu_models.insert(bmc_vendor::DpuModel::BlueField3, up("bf3-u", "bf3-p"));

        let mut ufm = HashMap::new();
        ufm.insert("fabric-1".to_string(), up("ufm-u", "ufm-p"));

        let mut nmxm = HashMap::new();
        nmxm.insert("nmxm-1".to_string(), up("nmxm-u", "nmxm-p"));

        CredentialSnapshot {
            dpu_redfish_factory_default: Some(up("drf-u", "drf-p")),
            dpu_redfish_factory_default_by_model: dpu_models,
            dpu_redfish_site_default: Some(up("drs-u", "drs-p")),
            host_redfish_factory_default_by_vendor: host_vendors,
            host_redfish_site_default: Some(up("hrs-u", "hrs-p")),
            ufm_auth_by_fabric: ufm,
            dpu_uefi_factory_default: Some(up("duf-u", "duf-p")),
            dpu_uefi_site_default: Some(up("dus-u", "dus-p")),
            host_uefi_site_default: Some(up("hus-u", "hus-p")),
            nmxm_auth_by_id: nmxm,
            mqtt_auth_by_credential_type: HashMap::from([(
                MqttCredentialType::Dpa,
                up("mqtt-u", "mqtt-p"),
            )]),
            machine_identity: None,
            bmc_site_wide_root: None,
        }
    }

    // One table over a single `populated_snapshot()`: each row is a
    // `CredentialKey` and the credentials that snapshot should return for it,
    // covering the per-variant hits plus the misses (unknown vendor / fabric /
    // credential type, unsupported keys, and invalid type combinations).
    #[test]
    fn snapshot_lookups_return_expected_credentials() {
        let snap = populated_snapshot();
        value_scenarios!(run = |key| snap.get_credentials(&key);
            "dpu redfish" {
                // per-model entry takes precedence
                CredentialKey::DpuRedfish {
                    credential_type: CredentialType::DpuHardwareDefault {
                        model: bmc_vendor::DpuModel::BlueField3,
                    },
                } => Some(cred("bf3-u", "bf3-p")),
                // unknown model falls back to legacy dpu_redfish_factory_default
                CredentialKey::DpuRedfish {
                    credential_type: CredentialType::DpuHardwareDefault {
                        model: bmc_vendor::DpuModel::Unknown,
                    },
                } => Some(cred("drf-u", "drf-p")),
                // unconfigured model also falls back to legacy field
                CredentialKey::DpuRedfish {
                    credential_type: CredentialType::DpuHardwareDefault {
                        model: bmc_vendor::DpuModel::BlueField4,
                    },
                } => Some(cred("drf-u", "drf-p")),
                CredentialKey::DpuRedfish {
                    credential_type: CredentialType::SiteDefault,
                } => Some(cred("drs-u", "drs-p")),
            }

            "host redfish" {
                CredentialKey::HostRedfish {
                    credential_type: CredentialType::HostHardwareDefault {
                        vendor: bmc_vendor::BMCVendor::Dell,
                    },
                } => Some(cred("dell-u", "dell-p")),
                CredentialKey::HostRedfish {
                    credential_type: CredentialType::HostHardwareDefault {
                        vendor: bmc_vendor::BMCVendor::Lenovo,
                    },
                } => None,
                CredentialKey::HostRedfish {
                    credential_type: CredentialType::SiteDefault,
                } => Some(cred("hrs-u", "hrs-p")),
            }

            "ufm auth" {
                CredentialKey::UfmAuth {
                    fabric: "fabric-1".to_string(),
                } => Some(cred("ufm-u", "ufm-p")),
                CredentialKey::UfmAuth {
                    fabric: "no-such-fabric".to_string(),
                } => None,
            }

            "dpu uefi" {
                CredentialKey::DpuUefi {
                    credential_type: CredentialType::DpuHardwareDefault {
                        model: bmc_vendor::DpuModel::Unknown,
                    },
                } => Some(cred("duf-u", "duf-p")),
                CredentialKey::DpuUefi {
                    credential_type: CredentialType::SiteDefault,
                } => Some(cred("dus-u", "dus-p")),
            }

            "host uefi" {
                CredentialKey::HostUefi {
                    credential_type: CredentialType::SiteDefault,
                } => Some(cred("hus-u", "hus-p")),
            }

            "nmxm" {
                CredentialKey::NmxM {
                    nmxm_id: "nmxm-1".to_string(),
                } => Some(cred("nmxm-u", "nmxm-p")),
            }

            "mqtt auth" {
                CredentialKey::MqttAuth {
                    credential_type: MqttCredentialType::Dpa,
                } => Some(cred("mqtt-u", "mqtt-p")),
                CredentialKey::MqttAuth {
                    credential_type: MqttCredentialType::DsxExchangeConsumer,
                } => None,
            }

            "unsupported key" {
                CredentialKey::ExtensionService {
                    service_id: "svc".to_string(),
                    version: "1".to_string(),
                } => None,
            }

            "invalid type combo" {
                CredentialKey::DpuRedfish {
                    credential_type: CredentialType::HostHardwareDefault {
                        vendor: bmc_vendor::BMCVendor::Dell,
                    },
                } => None,
                CredentialKey::HostRedfish {
                    credential_type: CredentialType::DpuHardwareDefault {
                        model: bmc_vendor::DpuModel::Unknown,
                    },
                } => None,
            }
        );
    }

    #[test]
    fn snapshot_bmc_site_wide_root() {
        let snap = CredentialSnapshot {
            bmc_site_wide_root: Some(up("bmc-u", "bmc-p")),
            ..Default::default()
        };
        let key = CredentialKey::BmcCredentials {
            credential_type: BmcCredentialType::SiteWideRoot,
        };
        assert_eq!(snap.get_credentials(&key), Some(cred("bmc-u", "bmc-p")));
    }

    #[test]
    fn default_snapshot_returns_none_for_all() {
        let snap = CredentialSnapshot::default();
        let key = CredentialKey::DpuUefi {
            credential_type: CredentialType::SiteDefault,
        };
        assert_eq!(snap.get_credentials(&key), None);
    }

    #[test]
    fn snapshot_machine_identity_encryption_key() {
        let mut encryption_keys = HashMap::new();
        encryption_keys.insert("v1".to_string(), "secret-1".to_string());
        encryption_keys.insert("v2".to_string(), "secret-2".to_string());
        let snap = CredentialSnapshot {
            machine_identity: Some(MachineIdentityConfig { encryption_keys }),
            ..Default::default()
        };

        let v1 = CredentialKey::MachineIdentityEncryptionKey {
            key_id: "v1".to_string(),
        };
        let v2 = CredentialKey::MachineIdentityEncryptionKey {
            key_id: "v2".to_string(),
        };
        let missing = CredentialKey::MachineIdentityEncryptionKey {
            key_id: "v3".to_string(),
        };

        assert_eq!(snap.get_credentials(&v1), Some(cred("v1", "secret-1")));
        assert_eq!(snap.get_credentials(&v2), Some(cred("v2", "secret-2")));
        assert_eq!(snap.get_credentials(&missing), None);
    }
}

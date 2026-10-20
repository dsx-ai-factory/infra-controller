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
use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use carbide_secrets::SecretsError;
use carbide_secrets::credentials::{
    BmcCredentialType, CredentialKey, CredentialReader, Credentials,
};
use carbide_secrets::local_credentials::StructuredCredentialReader;
use mac_address::MacAddress;
use serde::{Deserialize, Serialize};

/// Partial credentials; omitted or null fields inherit the next fallback.
/// An explicit empty string is a value and does not inherit.
#[derive(Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct CredentialOverrides {
    /// Username override, or absence to inherit.
    pub(crate) username: Option<String>,
    /// Password override, or absence to inherit.
    pub(crate) password: Option<String>,
}

impl fmt::Debug for CredentialOverrides {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CredentialOverrides")
            .field("username", &self.username)
            .field("password", &self.password.as_ref().map(|_| "REDACTED"))
            .finish()
    }
}

/// A target's profile selection and inline credential fields.
/// Resolution uses per-MAC overrides, then inline fields, then the profile.
#[derive(Clone, Debug, Default)]
pub(crate) struct BmcCredentialConfig {
    /// Explicit profile selection; unknown profiles fail lookup.
    pub(crate) profile: Option<String>,
    /// Target-specific inline fields, taking precedence over profile fields.
    pub(crate) inline: CredentialOverrides,
}

/// One structured BMC credentials file, independent of target inventory.
/// Both maps may be omitted or empty. MAC letter case does not affect matching.
/// Unknown fields or malformed MAC keys reject the entire snapshot.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct BmcCredentialSnapshot {
    /// Named credential sets selected explicitly by targets; no implicit default.
    profiles: HashMap<String, CredentialOverrides>,
    /// Per-MAC fields, taking precedence over inline values and selected profiles.
    overrides: HashMap<MacAddress, CredentialOverrides>,
}

#[derive(Debug, thiserror::Error)]
enum ResolutionError {
    #[error("unknown credential profile {0}")]
    UnknownProfile(String),
    #[error("no username configured for BMC {0}")]
    MissingUsername(MacAddress),
    #[error("no password configured for BMC {0}")]
    MissingPassword(MacAddress),
}

impl BmcCredentialSnapshot {
    fn resolve(
        &self,
        mac: MacAddress,
        config: &BmcCredentialConfig,
    ) -> Result<Credentials, ResolutionError> {
        let profile = config
            .profile
            .as_ref()
            .map(|name| {
                self.profiles
                    .get(name)
                    .ok_or_else(|| ResolutionError::UnknownProfile(name.clone()))
            })
            .transpose()?;
        let entry = self.overrides.get(&mac);
        let username = entry
            .and_then(|v| v.username.clone())
            .or_else(|| config.inline.username.clone())
            .or_else(|| profile.and_then(|v| v.username.clone()))
            .ok_or(ResolutionError::MissingUsername(mac))?;
        let password = entry
            .and_then(|v| v.password.clone())
            .or_else(|| config.inline.password.clone())
            .or_else(|| profile.and_then(|v| v.password.clone()))
            .ok_or(ResolutionError::MissingPassword(mac))?;
        Ok(Credentials::UsernamePassword { username, password })
    }
}

/// Binds one target's configuration to a shared cached credential file.
/// Only `CredentialKey::BmcCredentials` with `BmcCredentialType::BmcRoot` for
/// the bound MAC is supported; other keys return `None`. Successful lookups
/// return a complete username/password pair. Unknown profiles or missing resolved
/// fields return an error. Each lookup uses one complete cached snapshot.
pub(super) struct BmcFileCredentialReader {
    reader: Arc<StructuredCredentialReader<BmcCredentialSnapshot>>,
    mac: MacAddress,
    config: BmcCredentialConfig,
}

impl BmcFileCredentialReader {
    /// Binds a target without creating additional file watches or reload tasks.
    pub(super) fn new(
        reader: Arc<StructuredCredentialReader<BmcCredentialSnapshot>>,
        mac: MacAddress,
        config: BmcCredentialConfig,
    ) -> Self {
        Self {
            reader,
            mac,
            config,
        }
    }
}

#[async_trait]
impl CredentialReader for BmcFileCredentialReader {
    async fn get_credentials(
        &self,
        key: &CredentialKey,
    ) -> Result<Option<Credentials>, SecretsError> {
        match key {
            CredentialKey::BmcCredentials {
                credential_type: BmcCredentialType::BmcRoot { bmc_mac_address },
            } if *bmc_mac_address == self.mac => self
                .reader
                .snapshot()
                .resolve(self.mac, &self.config)
                .map(Some)
                .map_err(|error| SecretsError::GenericError(error.into())),
            _ => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use carbide_secrets::local_credentials::{FileCredentialsConfig, StructuredCredentialReader};

    use super::*;

    fn key(mac: &str) -> CredentialKey {
        CredentialKey::BmcCredentials {
            credential_type: BmcCredentialType::BmcRoot {
                bmc_mac_address: mac.parse().unwrap(),
            },
        }
    }

    #[test]
    fn profile_selection_and_field_precedence() {
        use carbide_test_support::Outcome::*;
        use carbide_test_support::{Case, check_cases};
        let snapshot: BmcCredentialSnapshot = serde_json::from_str(
            r#"{
            "profiles": {
                "bmc": {"username":"admin", "password":"shared"},
                "switch": {"username":"cumulus", "password":"switch-password"}
            },
            "overrides": {"aa:bb:cc:dd:ee:01": {"password":"individual"}}
        }"#,
        )
        .unwrap();
        let config = |profile: Option<&str>, username: Option<&str>, password: Option<&str>| {
            BmcCredentialConfig {
                profile: profile.map(str::to_owned),
                inline: CredentialOverrides {
                    username: username.map(str::to_owned),
                    password: password.map(str::to_owned),
                },
            }
        };
        check_cases(
            [
                Case {
                    scenario: "selected switch profile",
                    input: ("AA:BB:CC:DD:EE:00", config(Some("switch"), None, None)),
                    expect: Yields(Credentials::new("cumulus", "switch-password")),
                },
                Case {
                    scenario: "override beats inline; inline beats profile",
                    input: (
                        "AA:BB:CC:DD:EE:01",
                        config(Some("bmc"), Some("cumulus"), Some("inline")),
                    ),
                    expect: Yields(Credentials::new("cumulus", "individual")),
                },
                Case {
                    scenario: "explicit empty password",
                    input: ("AA:BB:CC:DD:EE:00", config(Some("bmc"), None, Some(""))),
                    expect: Yields(Credentials::new("admin", "")),
                },
                Case {
                    scenario: "complete inline credentials without profile",
                    input: (
                        "AA:BB:CC:DD:EE:00",
                        config(None, Some("inline"), Some("inline-password")),
                    ),
                    expect: Yields(Credentials::new("inline", "inline-password")),
                },
                Case {
                    scenario: "no implicit profile or password",
                    input: ("AA:BB:CC:DD:EE:00", config(None, Some("inline"), None)),
                    expect: Fails,
                },
                Case {
                    scenario: "missing username",
                    input: ("AA:BB:CC:DD:EE:01", config(None, None, None)),
                    expect: Fails,
                },
                Case {
                    scenario: "unknown selected profile",
                    input: ("AA:BB:CC:DD:EE:01", config(Some("missing"), None, None)),
                    expect: Fails,
                },
            ],
            |(mac, config)| {
                snapshot
                    .resolve(mac.parse().unwrap(), &config)
                    .map_err(|_| ())
            },
        );
    }

    #[tokio::test]
    async fn rejects_invalid_snapshot_without_exposing_password() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.json");
        for (input, field) in [
            (
                r#"{"overrides":{"invalid-mac":{"password":"sensitive"}}}"#,
                "overrides",
            ),
            (
                r#"{"profiles":{"bmc":{"pasword":"sensitive"}}}"#,
                "profiles.bmc",
            ),
            (
                r#"{"profiles":{"bmc":{"password":["sensitive"]}}}"#,
                "profiles.bmc.password",
            ),
            (r#"{"profiles":{"bmc":{"password":"sensitive"}}"#, "JSON"),
            ("default: {password: sensitive}", "JSON"),
            (
                r#"{"profiles":{"bmc":{"password":"sensitive"}}} {}"#,
                "document",
            ),
        ] {
            std::fs::write(&path, input).unwrap();
            let error = StructuredCredentialReader::<BmcCredentialSnapshot>::new_json(
                FileCredentialsConfig {
                    path: Some(path.clone()),
                    ..Default::default()
                },
            )
            .await
            .err()
            .expect("invalid file must fail initial load");
            assert!(!format!("{error:?}").contains("sensitive"));
            assert!(
                error.to_string().contains(field),
                "expected {field} in {error}"
            );
        }
        let fields = CredentialOverrides {
            username: None,
            password: Some("sensitive".into()),
        };
        assert!(!format!("{fields:?}").contains("sensitive"));
    }

    #[cfg(unix)]
    async fn password(reader: &dyn CredentialReader, key: &CredentialKey) -> String {
        let Some(Credentials::UsernamePassword { password, .. }) =
            reader.get_credentials(key).await.unwrap()
        else {
            panic!("expected username/password credentials");
        };
        password
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn projected_file_reloads_additions_removals_and_keeps_last_valid_snapshot() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        for (revision, content) in [
            (
                "v1",
                r#"{"profiles":{"bmc":{"username":"admin","password":"first"}}}"#,
            ),
            (
                "v2",
                r#"{"profiles":{"bmc":{"username":"admin","password":"rotated"}},"overrides":{"00:11:22:33:44:55":{"password":"individual"}}}"#,
            ),
            (
                "v3",
                r#"{"profiles":{"bmc":{"username":"admin","password":"final"}}}"#,
            ),
        ] {
            let path = dir.path().join(revision);
            std::fs::create_dir(&path).unwrap();
            std::fs::write(path.join("credentials.json"), content).unwrap();
        }
        symlink("v1", dir.path().join("..data")).unwrap();
        let path = dir.path().join("credentials.json");
        symlink("..data/credentials.json", &path).unwrap();
        let reader =
            StructuredCredentialReader::<BmcCredentialSnapshot>::new_json(FileCredentialsConfig {
                path: Some(path.clone()),
                poll_interval: Some(Duration::from_millis(25)),
                ..Default::default()
            })
            .await
            .unwrap();
        let mac = key("00:11:22:33:44:55");
        let shared = Arc::new(reader);
        let reader = BmcFileCredentialReader::new(
            shared.clone(),
            "00:11:22:33:44:55".parse().unwrap(),
            BmcCredentialConfig {
                profile: Some("bmc".into()),
                ..Default::default()
            },
        );
        let peer = BmcFileCredentialReader::new(
            shared,
            "00:11:22:33:44:66".parse().unwrap(),
            BmcCredentialConfig {
                profile: Some("bmc".into()),
                ..Default::default()
            },
        );
        assert!(Arc::ptr_eq(&reader.reader, &peer.reader));
        assert_eq!(
            reader
                .get_credentials(&key("00:11:22:33:44:66"))
                .await
                .unwrap(),
            None
        );
        assert_eq!(
            reader
                .get_credentials(&CredentialKey::UfmAuth {
                    fabric: "test".into()
                })
                .await
                .unwrap(),
            None
        );
        for (revision, expected) in [("v2", "individual"), ("v3", "final")] {
            symlink(revision, dir.path().join("..next")).unwrap();
            std::fs::rename(dir.path().join("..next"), dir.path().join("..data")).unwrap();
            tokio::time::timeout(Duration::from_secs(3), async {
                loop {
                    if password(&reader, &mac).await.as_str() == expected {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("projected snapshot reload");
        }
        // Exercise both parse failure and an unavailable mount after a valid load.
        for content in [
            Some(r#"{"profiles":{"bmc":{"password":"incomplete"}}"#),
            None,
        ] {
            if let Some(content) = content {
                std::fs::write(&path, content).unwrap();
            } else {
                std::fs::remove_file(&path).unwrap();
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
            assert_eq!(password(&reader, &mac).await.as_str(), "final");
        }
        std::fs::write(
            &path,
            r#"{"profiles":{"bmc":{"username":"admin","password":"recovered"}}}"#,
        )
        .unwrap();
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if password(&reader, &mac).await.as_str() == "recovered" {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("recover after invalid update");
    }
}

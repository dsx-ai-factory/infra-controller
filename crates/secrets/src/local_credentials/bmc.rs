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

use async_trait::async_trait;
use mac_address::MacAddress;
use serde::{Deserialize, Serialize};

use crate::SecretsError;
use crate::credentials::CredentialReader;

/// Partial credentials; omitted or null fields inherit the next fallback.
/// An explicit empty string is a value and does not inherit.
#[derive(Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct CredentialOverrides {
    /// Username override, or absence to inherit.
    pub username: Option<String>,
    /// Password override, or absence to inherit.
    pub password: Option<String>,
}

impl fmt::Debug for CredentialOverrides {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CredentialOverrides")
            .field("username", &self.username)
            .field("password", &self.password.as_ref().map(|_| "REDACTED"))
            .finish()
    }
}

/// A target lookup: its MAC, optional selected profile, and inline fields.
/// Each field resolves as per-MAC override, then inline value, then profile.
#[derive(Clone, Debug)]
pub struct BmcCredentialKey {
    /// MAC address identifying per-target overrides.
    pub mac: MacAddress,
    /// Explicit profile selection; unknown profiles fail lookup.
    pub profile: Option<String>,
    /// Target-specific inline fields, taking precedence over profile fields.
    pub inline: CredentialOverrides,
}

/// One structured BMC credentials file, independent of target inventory.
/// Both maps may be omitted or empty. MAC letter case does not affect matching.
/// Unknown fields or malformed MAC keys reject the entire snapshot.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct BmcCredentialSnapshot {
    /// Named credential sets selected explicitly by targets; no implicit default.
    pub profiles: HashMap<String, CredentialOverrides>,
    /// Per-MAC fields, taking precedence over inline values and selected profiles.
    pub overrides: HashMap<MacAddress, CredentialOverrides>,
}

#[async_trait]
impl CredentialReader<BmcCredentialKey, CredentialOverrides> for BmcCredentialSnapshot {
    async fn get_credentials(
        &self,
        key: &BmcCredentialKey,
    ) -> Result<Option<CredentialOverrides>, SecretsError> {
        let profile = key
            .profile
            .as_ref()
            .map(|name| {
                self.profiles.get(name).ok_or_else(|| {
                    SecretsError::GenericError(eyre::eyre!("unknown credential profile {name}"))
                })
            })
            .transpose()?;
        let entry = self.overrides.get(&key.mac);
        Ok(Some(CredentialOverrides {
            username: entry
                .and_then(|v| v.username.clone())
                .or_else(|| key.inline.username.clone())
                .or_else(|| profile.and_then(|v| v.username.clone())),
            password: entry
                .and_then(|v| v.password.clone())
                .or_else(|| key.inline.password.clone())
                .or_else(|| profile.and_then(|v| v.password.clone())),
        }))
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::local_credentials::{FileCredentialsConfig, StructuredCredentialReader};

    fn key(mac: &str) -> BmcCredentialKey {
        BmcCredentialKey {
            mac: mac.parse().unwrap(),
            profile: Some("bmc".into()),
            inline: CredentialOverrides::default(),
        }
    }

    #[tokio::test]
    async fn profile_selection_and_field_precedence() {
        use carbide_test_support::Outcome::*;
        use carbide_test_support::{Case, check_cases_async};
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
        let key = |mac: &str,
                   profile: Option<&str>,
                   username: Option<&str>,
                   password: Option<&str>| BmcCredentialKey {
            mac: mac.parse().unwrap(),
            profile: profile.map(str::to_owned),
            inline: CredentialOverrides {
                username: username.map(str::to_owned),
                password: password.map(str::to_owned),
            },
        };
        let fields = |username: Option<&str>, password: Option<&str>| {
            Some(CredentialOverrides {
                username: username.map(str::to_owned),
                password: password.map(str::to_owned),
            })
        };
        check_cases_async(
            [
                Case {
                    scenario: "selected switch profile",
                    input: key("AA:BB:CC:DD:EE:00", Some("switch"), None, None),
                    expect: Yields(fields(Some("cumulus"), Some("switch-password"))),
                },
                Case {
                    scenario: "override beats inline; inline beats profile",
                    input: key(
                        "AA:BB:CC:DD:EE:01",
                        Some("bmc"),
                        Some("cumulus"),
                        Some("inline"),
                    ),
                    expect: Yields(fields(Some("cumulus"), Some("individual"))),
                },
                Case {
                    scenario: "empty password beats profile",
                    input: key("AA:BB:CC:DD:EE:00", Some("bmc"), None, Some("")),
                    expect: Yields(fields(Some("admin"), Some(""))),
                },
                Case {
                    scenario: "no implicit profile",
                    input: key("AA:BB:CC:DD:EE:00", None, Some("inline"), None),
                    expect: Yields(fields(Some("inline"), None)),
                },
                Case {
                    scenario: "unknown selected profile",
                    input: key("AA:BB:CC:DD:EE:01", Some("missing"), None, None),
                    expect: Fails,
                },
            ],
            |key| {
                let snapshot = &snapshot;
                async move { snapshot.get_credentials(&key).await.map_err(|_| ()) }
            },
        )
        .await;
        let empty: BmcCredentialSnapshot = serde_json::from_str("{}").unwrap();
        assert_eq!(
            empty
                .get_credentials(&key("AA:BB:CC:DD:EE:00", None, None, None))
                .await
                .unwrap(),
            fields(None, None)
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
    #[tokio::test]
    #[serial_test::serial(credential_watcher_metrics)]
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
        for (revision, expected) in [("v2", "individual"), ("v3", "final")] {
            symlink(revision, dir.path().join("..next")).unwrap();
            std::fs::rename(dir.path().join("..next"), dir.path().join("..data")).unwrap();
            tokio::time::timeout(Duration::from_secs(3), async {
                loop {
                    if reader
                        .get_credentials(&mac)
                        .await
                        .unwrap()
                        .unwrap()
                        .password
                        .as_deref()
                        == Some(expected)
                    {
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
            assert_eq!(
                reader
                    .get_credentials(&mac)
                    .await
                    .unwrap()
                    .unwrap()
                    .password
                    .as_deref(),
                Some("final")
            );
        }
        std::fs::write(
            &path,
            r#"{"profiles":{"bmc":{"username":"admin","password":"recovered"}}}"#,
        )
        .unwrap();
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if reader
                    .get_credentials(&mac)
                    .await
                    .unwrap()
                    .unwrap()
                    .password
                    .as_deref()
                    == Some("recovered")
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("recover after invalid update");
    }
}

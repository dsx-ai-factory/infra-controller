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

use mac_address::MacAddress;
use serde::{Deserialize, Serialize};

use crate::endpoint::BmcCredentials;

/// Partial credentials; omitted or null fields inherit the next fallback.
/// An explicit empty string is a value and does not inherit.
#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
struct CredentialOverrides {
    /// Username override, or absence to inherit.
    username: Option<String>,
    /// Password override, or absence to inherit.
    password: Option<String>,
}

impl fmt::Debug for CredentialOverrides {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CredentialOverrides")
            .field("username", &self.username)
            .field("password", &self.password.as_ref().map(|_| "REDACTED"))
            .finish()
    }
}

/// One structured BMC credentials file, independent of target inventory.
/// Both maps may be omitted or empty. MAC letter case does not affect matching.
/// Unknown fields or malformed MAC keys reject the entire snapshot.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct BmcCredentialSnapshot {
    /// Named credential sets selected explicitly by targets; no implicit default.
    profiles: HashMap<String, CredentialOverrides>,
    /// Per-MAC fields, taking precedence over selected profiles.
    overrides: HashMap<MacAddress, CredentialOverrides>,
}

#[derive(Debug, thiserror::Error)]
pub(super) enum ResolutionError {
    #[error("unknown credential profile {0}")]
    UnknownProfile(String),
    #[error("no username configured for BMC {0}")]
    MissingUsername(MacAddress),
}

impl BmcCredentialSnapshot {
    pub(super) fn resolve(
        &self,
        mac: MacAddress,
        profile_name: Option<&str>,
    ) -> Result<BmcCredentials, ResolutionError> {
        let profile = profile_name
            .map(|name| {
                self.profiles
                    .get(name)
                    .ok_or_else(|| ResolutionError::UnknownProfile(name.to_owned()))
            })
            .transpose()?;
        let entry = self.overrides.get(&mac);
        let username = entry
            .and_then(|v| v.username.clone())
            .or_else(|| profile.and_then(|v| v.username.clone()))
            .ok_or(ResolutionError::MissingUsername(mac))?;
        let password = entry
            .and_then(|v| v.password.clone())
            .or_else(|| profile.and_then(|v| v.password.clone()));
        Ok(BmcCredentials::UsernamePassword { username, password })
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use carbide_secrets::local_credentials::{FileCredentialsConfig, StructuredCredentialReader};

    use super::*;

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
            "overrides": {
                "aa:bb:cc:dd:ee:01": {"password":"individual"},
                "aa:bb:cc:dd:ee:02": {"username":"standalone"},
                "aa:bb:cc:dd:ee:03": {"password":""}
            }
        }"#,
        )
        .unwrap();
        check_cases(
            [
                Case {
                    scenario: "selected switch profile",
                    input: ("AA:BB:CC:DD:EE:00", Some("switch")),
                    expect: Yields(("cumulus".into(), Some("switch-password".into()))),
                },
                Case {
                    scenario: "override beats profile",
                    input: ("AA:BB:CC:DD:EE:01", Some("bmc")),
                    expect: Yields(("admin".into(), Some("individual".into()))),
                },
                Case {
                    scenario: "explicit empty password overrides profile",
                    input: ("AA:BB:CC:DD:EE:03", Some("bmc")),
                    expect: Yields(("admin".into(), Some("".into()))),
                },
                Case {
                    scenario: "passwordless override without profile",
                    input: ("AA:BB:CC:DD:EE:02", None),
                    expect: Yields(("standalone".into(), None)),
                },
                Case {
                    scenario: "no implicit profile",
                    input: ("AA:BB:CC:DD:EE:00", None),
                    expect: Fails,
                },
                Case {
                    scenario: "unknown selected profile",
                    input: ("AA:BB:CC:DD:EE:01", Some("missing")),
                    expect: Fails,
                },
            ],
            |(mac, profile)| {
                snapshot
                    .resolve(mac.parse().unwrap(), profile)
                    .map(|credentials| match credentials {
                        BmcCredentials::UsernamePassword { username, password } => {
                            (username, password)
                        }
                        BmcCredentials::SessionToken { .. } => {
                            panic!("file credentials must use username/password")
                        }
                    })
                    .map_err(|_| ())
            },
        );
    }

    #[tokio::test]
    async fn rejects_invalid_snapshot_without_exposing_password() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.json");
        for input in [
            r#"{"overrides":{"invalid-mac":{"password":"sensitive"}}}"#,
            r#"{"profiles":{"bmc":{"pasword":"sensitive"}}}"#,
            r#"{"profiles":{"bmc":{"password":["sensitive"]}}}"#,
            r#"{"profiles":{"bmc":{"password":"sensitive"}}"#,
            "default: {password: sensitive}",
            r#"{"profiles":{"bmc":{"password":"sensitive"}}} {}"#,
        ] {
            std::fs::write(&path, input).unwrap();
            let error =
                StructuredCredentialReader::<BmcCredentialSnapshot>::new(FileCredentialsConfig {
                    path: Some(path.clone()),
                    ..Default::default()
                })
                .await
                .err()
                .expect("invalid file must fail initial load");
            assert!(!format!("{error:?}").contains("sensitive"));
        }
        let fields = CredentialOverrides {
            username: None,
            password: Some("sensitive".into()),
        };
        assert!(!format!("{fields:?}").contains("sensitive"));
    }

    #[cfg(unix)]
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
            StructuredCredentialReader::<BmcCredentialSnapshot>::new(FileCredentialsConfig {
                path: Some(path.clone()),
                poll_interval: Some(Duration::from_millis(25)),
                ..Default::default()
            })
            .await
            .unwrap();
        let mac = "00:11:22:33:44:55".parse().unwrap();
        let password = || {
            let BmcCredentials::UsernamePassword { password, .. } =
                reader.snapshot().resolve(mac, Some("bmc")).unwrap()
            else {
                panic!("file credentials must use username/password");
            };
            password.unwrap()
        };
        for (revision, expected) in [("v2", "individual"), ("v3", "final")] {
            symlink(revision, dir.path().join("..next")).unwrap();
            std::fs::rename(dir.path().join("..next"), dir.path().join("..data")).unwrap();
            tokio::time::timeout(Duration::from_secs(3), async {
                loop {
                    if password().as_str() == expected {
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
            assert_eq!(password().as_str(), "final");
        }
        std::fs::write(
            &path,
            r#"{"profiles":{"bmc":{"username":"admin","password":"recovered"}}}"#,
        )
        .unwrap();
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if password().as_str() == "recovered" {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("recover after invalid update");
    }
}

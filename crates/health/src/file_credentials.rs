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

use std::path::{Path, PathBuf};

use crate::HealthError;
use crate::bmc::{BoxFuture, CredentialProvider};
use crate::config::StaticBmcEndpoint;
use crate::endpoint::{BmcAddr, BmcCredentials};

pub(crate) struct FileCredentialProvider {
    username: Option<String>,
    password: Option<String>,
    username_file: Option<PathBuf>,
    password_file: Option<PathBuf>,
}

impl FileCredentialProvider {
    pub(crate) fn from_config(config: &StaticBmcEndpoint) -> Self {
        Self {
            username: config.username.clone(),
            password: config.password.clone(),
            username_file: config.username_file.clone(),
            password_file: config.password_file.clone(),
        }
    }

    async fn read(path: &Path) -> Result<String, HealthError> {
        // Open the configured path on every fetch, rather than retaining a file
        // descriptor, so atomic replacements and Kubernetes Secret symlinks reload.
        let value = tokio::fs::read_to_string(path).await.map_err(|source| {
            HealthError::CredentialFileRead {
                path: path.to_path_buf(),
                source,
            }
        })?;
        // Remove a text-file line ending without stripping password whitespace.
        Ok(value
            .strip_suffix("\r\n")
            .or_else(|| value.strip_suffix('\n'))
            .unwrap_or(&value)
            .to_string())
    }
}

impl CredentialProvider for FileCredentialProvider {
    fn reload_before_use(&self) -> bool {
        true
    }

    fn fetch_credentials<'a>(
        &'a self,
        _endpoint: &'a BmcAddr,
    ) -> BoxFuture<'a, Result<BmcCredentials, HealthError>> {
        Box::pin(async move {
            let username = match &self.username_file {
                Some(path) => Self::read(path).await?,
                None => self.username.clone().ok_or_else(|| {
                    HealthError::GenericError("missing static endpoint username".to_string())
                })?,
            };
            let password = match &self.password_file {
                Some(path) => Some(Self::read(path).await?),
                None => self.password.clone(),
            };
            Ok(BmcCredentials::UsernamePassword { username, password })
        })
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::*;
    use carbide_test_support::{Case, check_cases_async};

    use super::*;

    #[tokio::test]
    async fn credential_file_contents() {
        check_cases_async(
            [
                Case {
                    scenario: "plain text",
                    input: b"secret".as_slice(),
                    expect: Yields("secret".to_string()),
                },
                Case {
                    scenario: "LF ending preserves spaces",
                    input: b" secret \n".as_slice(),
                    expect: Yields(" secret ".to_string()),
                },
                Case {
                    scenario: "CRLF ending",
                    input: b"secret\r\n".as_slice(),
                    expect: Yields("secret".to_string()),
                },
                Case {
                    scenario: "empty password",
                    input: b"".as_slice(),
                    expect: Yields(String::new()),
                },
            ],
            |contents| async move {
                let file = tempfile::NamedTempFile::new().unwrap();
                std::fs::write(file.path(), contents).unwrap();
                FileCredentialProvider::read(file.path())
                    .await
                    .map_err(|_| ())
            },
        )
        .await;
    }

    #[tokio::test]
    async fn credential_file_failure_is_redacted_and_recoverable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("password");
        for contents in [None, Some(b"do-not-log-this\xff".as_slice())] {
            if let Some(contents) = contents {
                std::fs::write(&path, contents).unwrap();
            }
            let error = FileCredentialProvider::read(&path).await.unwrap_err();
            assert!(matches!(error, HealthError::CredentialFileRead { .. }));
            assert!(!format!("{error:?}").contains("do-not-log-this"));
        }
        std::fs::write(&path, "repaired").unwrap();
        assert_eq!(
            FileCredentialProvider::read(&path).await.unwrap(),
            "repaired"
        );
    }

    #[tokio::test]
    async fn file_provider_combines_inline_and_file_values() {
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), "from-file").unwrap();
        let addr = BmcAddr {
            ip: "127.0.0.1".parse().unwrap(),
            port: None,
            mac: None,
        };
        check_cases_async(
            [
                Case {
                    scenario: "inline username and password file",
                    input: FileCredentialProvider {
                        username: Some("inline".to_string()),
                        password: None,
                        username_file: None,
                        password_file: Some(file.path().to_path_buf()),
                    },
                    expect: Yields(("inline".to_string(), Some("from-file".to_string()))),
                },
                Case {
                    scenario: "username file and inline password",
                    input: FileCredentialProvider {
                        username: None,
                        password: Some("inline".to_string()),
                        username_file: Some(file.path().to_path_buf()),
                        password_file: None,
                    },
                    expect: Yields(("from-file".to_string(), Some("inline".to_string()))),
                },
                Case {
                    scenario: "username file without password",
                    input: FileCredentialProvider {
                        username: None,
                        password: None,
                        username_file: Some(file.path().to_path_buf()),
                        password_file: None,
                    },
                    expect: Yields(("from-file".to_string(), None)),
                },
            ],
            |provider| {
                let addr = addr.clone();
                async move {
                    match provider.fetch_credentials(&addr).await.map_err(|_| ())? {
                        BmcCredentials::UsernamePassword { username, password } => {
                            Ok((username, password))
                        }
                        BmcCredentials::SessionToken { .. } => Err(()),
                    }
                }
            },
        )
        .await;
    }
}

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

//! Machine Validation plugin result contract and safe result-file handling.

use std::io::Read;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use serde::Deserialize;

const MAX_RESULT_SIZE: u64 = 64 * 1024;

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct PluginResult {
    #[serde(rename = "contractVersion")]
    pub(crate) contract_version: String,
    pub(crate) kind: String,
    pub(crate) outcome: String,
    pub(crate) summary: String,
    #[serde(default)]
    pub(crate) severity: Option<String>,
    #[serde(default)]
    pub(crate) findings: Vec<PluginFinding>,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct PluginFinding {
    pub(crate) name: String,
    pub(crate) message: String,
}

pub(crate) fn read_plugin_result(path: &Path) -> Result<PluginResult, String> {
    #[cfg(unix)]
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|error| format!("plugin did not write result.json: {error}"))?;
    #[cfg(not(unix))]
    let file = std::fs::OpenOptions::new()
        .read(true)
        .open(path)
        .map_err(|error| format!("plugin did not write result.json: {error}"))?;

    let metadata = file
        .metadata()
        .map_err(|error| format!("failed to inspect plugin result: {error}"))?;
    if !metadata.is_file() {
        return Err("plugin result must be a regular file".to_owned());
    }

    let mut contents = Vec::with_capacity(MAX_RESULT_SIZE as usize);
    file.take(MAX_RESULT_SIZE + 1)
        .read_to_end(&mut contents)
        .map_err(|error| format!("failed to read plugin result: {error}"))?;
    if contents.len() > MAX_RESULT_SIZE as usize {
        return Err("plugin result exceeds the 64 KiB limit".to_owned());
    }
    parse_plugin_result(&contents)
}

fn parse_plugin_result(contents: &[u8]) -> Result<PluginResult, String> {
    let result: PluginResult = serde_json::from_slice(contents)
        .map_err(|error| format!("invalid plugin result: {error}"))?;
    if result.contract_version != "v1" {
        return Err("plugin result has an unsupported contractVersion".to_owned());
    }
    if result.kind != "MachineValidationPluginResult" {
        return Err("plugin result has an unsupported kind".to_owned());
    }
    if !matches!(result.outcome.as_str(), "pass" | "fail" | "error") {
        return Err("plugin result has an unsupported outcome".to_owned());
    }
    if result.summary.len() > 4096 {
        return Err("plugin result summary exceeds the 4 KiB limit".to_owned());
    }
    if result
        .severity
        .as_deref()
        .is_some_and(|severity| !matches!(severity, "info" | "warning" | "critical" | "unknown"))
    {
        return Err("plugin result has an unsupported severity".to_owned());
    }
    if result.findings.len() > 100 {
        return Err("plugin result has more than 100 findings".to_owned());
    }
    if result
        .findings
        .iter()
        .any(|finding| finding.name.is_empty() || finding.message.len() > 4096)
    {
        return Err("plugin result contains an invalid finding".to_owned());
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::*;
    use carbide_test_support::{Case, check_cases};
    use uuid::Uuid;

    use super::*;

    #[test]
    fn plugin_result_contract_validation() {
        check_cases(
            [
                Case {
                    scenario: "valid result",
                    input: r#"{"contractVersion":"v1","kind":"MachineValidationPluginResult","outcome":"pass","summary":"check completed","severity":"info","findings":[{"name":"gpu","message":"available"}]}"#,
                    expect: Yields(()),
                },
                Case {
                    scenario: "unknown outcome",
                    input: r#"{"contractVersion":"v1","kind":"MachineValidationPluginResult","outcome":"skipped","summary":"check completed"}"#,
                    expect: Fails,
                },
                Case {
                    scenario: "unknown field",
                    input: r#"{"contractVersion":"v1","kind":"MachineValidationPluginResult","outcome":"pass","summary":"check completed","extra":"not allowed"}"#,
                    expect: Fails,
                },
            ],
            |contents| {
                parse_plugin_result(contents.as_bytes())
                    .map(|_| ())
                    .map_err(drop)
            },
        );
    }

    #[test]
    fn plugin_result_file_must_be_regular_and_bounded() {
        let directory =
            std::env::temp_dir().join(format!("plugin-contract-test-{}", Uuid::new_v4()));
        std::fs::create_dir(&directory).expect("create test directory");
        let result_path = directory.join("result.json");

        std::fs::write(&result_path, vec![b'x'; MAX_RESULT_SIZE as usize + 1])
            .expect("write oversized result");
        assert!(read_plugin_result(&result_path).is_err());

        #[cfg(unix)]
        {
            std::fs::remove_file(&result_path).expect("remove oversized result");
            std::os::unix::fs::symlink("/etc/passwd", &result_path).expect("create result symlink");
            assert!(read_plugin_result(&result_path).is_err());
        }

        std::fs::remove_dir_all(directory).expect("remove test directory");
    }
}

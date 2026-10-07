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

use std::fmt::{self, Display};
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// Selects how a booting DPU obtains the CA used to authenticate Carbide.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BootstrapCaSource {
    /// Download the CA from carbide-pxe at boot, preserving the historical behavior.
    #[default]
    LegacyDownload,
    /// Use the CA embedded in the boot artifact.
    Embedded,
    /// Use a CA mounted into the boot environment by the operator.
    Mounted,
}

impl Display for BootstrapCaSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::LegacyDownload => "legacy_download",
            Self::Embedded => "embedded",
            Self::Mounted => "mounted",
        })
    }
}

#[derive(Debug, Eq, PartialEq, thiserror::Error)]
#[error("unknown bootstrap CA source `{0}`")]
pub struct ParseBootstrapCaSourceError(String);

impl FromStr for BootstrapCaSource {
    type Err = ParseBootstrapCaSourceError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "legacy_download" => Ok(Self::LegacyDownload),
            "embedded" => Ok(Self::Embedded),
            "mounted" => Ok(Self::Mounted),
            _ => Err(ParseBootstrapCaSourceError(value.to_string())),
        }
    }
}

impl From<BootstrapCaSource> for rpc::forge::BootstrapCaSource {
    fn from(value: BootstrapCaSource) -> Self {
        match value {
            BootstrapCaSource::LegacyDownload => Self::LegacyDownload,
            BootstrapCaSource::Embedded => Self::Embedded,
            BootstrapCaSource::Mounted => Self::Mounted,
        }
    }
}

impl From<rpc::forge::BootstrapCaSource> for BootstrapCaSource {
    fn from(value: rpc::forge::BootstrapCaSource) -> Self {
        match value {
            rpc::forge::BootstrapCaSource::LegacyDownload => Self::LegacyDownload,
            rpc::forge::BootstrapCaSource::Embedded => Self::Embedded,
            rpc::forge::BootstrapCaSource::Mounted => Self::Mounted,
        }
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::{Check, check_values, value_scenarios};

    use super::*;

    #[test]
    fn string_forms_round_trip() {
        value_scenarios!(
            run = |source: BootstrapCaSource| source.to_string().parse().unwrap();
            "legacy download" {
                BootstrapCaSource::LegacyDownload => BootstrapCaSource::LegacyDownload,
            }
            "embedded" {
                BootstrapCaSource::Embedded => BootstrapCaSource::Embedded,
            }
            "mounted" {
                BootstrapCaSource::Mounted => BootstrapCaSource::Mounted,
            }
        );
    }

    #[test]
    fn invalid_string_is_rejected() {
        check_values(
            [
                Check {
                    scenario: "empty",
                    input: "",
                    expect: Err(ParseBootstrapCaSourceError(String::new())),
                },
                Check {
                    scenario: "unknown",
                    input: "download",
                    expect: Err(ParseBootstrapCaSourceError("download".to_string())),
                },
            ],
            str::parse::<BootstrapCaSource>,
        );
    }

    #[test]
    fn protobuf_forms_round_trip() {
        value_scenarios!(
            run = |source: BootstrapCaSource| {
                BootstrapCaSource::from(rpc::forge::BootstrapCaSource::from(source))
            };
            "legacy download" {
                BootstrapCaSource::LegacyDownload => BootstrapCaSource::LegacyDownload,
            }
            "embedded" {
                BootstrapCaSource::Embedded => BootstrapCaSource::Embedded,
            }
            "mounted" {
                BootstrapCaSource::Mounted => BootstrapCaSource::Mounted,
            }
        );
    }

    #[test]
    fn default_preserves_legacy_download() {
        assert_eq!(
            BootstrapCaSource::default(),
            BootstrapCaSource::LegacyDownload
        );
    }
}

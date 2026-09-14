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

use serde::{Deserialize, Serialize};

/// Mock configuration. Every field has a default so that a host can mount
/// the mock without any configuration at all.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct RmsMockConfig {
    /// Reported by `GetVersion`. `librms` issues `GetVersion` as its
    /// connection liveness probe, so this is the first call any client makes.
    pub version_string: String,

    /// How quickly asynchronous jobs reach a terminal state.
    pub job_pacing: JobPacing,

    /// How to answer a poll for a job this process did not issue.
    pub unknown_job_policy: UnknownJobPolicy,

    /// Prefix for generated job ids. Only cosmetic, but it makes a job id in
    /// a log recognisable as the mock's.
    pub job_id_prefix: String,

    /// Topology reported for the scale-up fabric.
    pub fabric_topology_type: String,
}

/// How many polls a job takes to advance.
///
/// Jobs advance on observation rather than on elapsed time so that behaviour
/// is reproducible and tests never sleep. The defaults let a job be observed
/// once as running before completing, which exercises the caller's polling
/// loop without stalling ingestion.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct JobPacing {
    pub running_after_observations: u32,
    pub terminal_after_observations: u32,
}

impl Default for JobPacing {
    fn default() -> Self {
        Self {
            running_after_observations: 1,
            terminal_after_observations: 2,
        }
    }
}

/// What to report when asked about a job the mock has no record of.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UnknownJobPolicy {
    /// Report it complete. NICo persists job ids across its own restarts and
    /// the mock's, and failing those polls would strand every component that
    /// had work in flight.
    #[default]
    Complete,
    /// Report it failed.
    Fail,
    /// Return gRPC `NOT_FOUND`.
    NotFound,
}

impl Default for RmsMockConfig {
    fn default() -> Self {
        Self {
            version_string: concat!("machine-a-tron-rms-mock/", env!("CARGO_PKG_VERSION"))
                .to_string(),
            job_pacing: JobPacing::default(),
            unknown_job_policy: UnknownJobPolicy::default(),
            job_id_prefix: "rms-mock".to_string(),
            fabric_topology_type: "nvl72".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::*;
    use carbide_test_support::scenarios;
    use serde::Deserialize;

    use super::{JobPacing, RmsMockConfig, UnknownJobPolicy};

    /// The table as a host embeds it.
    #[derive(Deserialize)]
    struct Host {
        rms_mock: RmsMockConfig,
    }

    fn parse(toml: &str) -> Result<RmsMockConfig, ()> {
        toml::from_str::<Host>(toml)
            .map(|host| host.rms_mock)
            .map_err(drop)
    }

    #[test]
    fn the_rms_mock_table_deserializes_with_defaults_for_omitted_keys() {
        scenarios!(parse:
            "an empty table is the defaults" {
                "[rms_mock]\n" => Yields(RmsMockConfig::default()),
            }

            "keys override individually" {
                "[rms_mock]\n\
                 unknown_job_policy = \"not_found\"\n\
                 job_id_prefix = \"sim\"\n\
                 [rms_mock.job_pacing]\n\
                 terminal_after_observations = 3\n" => Yields(RmsMockConfig {
                    unknown_job_policy: UnknownJobPolicy::NotFound,
                    job_id_prefix: "sim".to_string(),
                    job_pacing: JobPacing {
                        running_after_observations: 1,
                        terminal_after_observations: 3,
                    },
                    ..RmsMockConfig::default()
                }),
            }

            "a policy outside the vocabulary is rejected" {
                "[rms_mock]\nunknown_job_policy = \"shrug\"\n" => Fails,
            }
        );
    }
}

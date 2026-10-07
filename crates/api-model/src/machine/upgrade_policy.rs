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

use std::cmp::Ordering;
use std::fmt;

use crate::firmware::AgentUpgradePolicyChoice;

/// How we decide whether a DPU should upgrade its forge-dpu-agent
#[derive(Debug, Copy, Clone, PartialEq)]
pub enum AgentUpgradePolicy {
    /// Never upgrade it
    Off,
    /// Upgrade but never downgrade. Allows us to test new versions manually.
    UpOnly,
    /// Upgrade or downgrade as necessary to make the versions match
    UpDown,
}

impl fmt::Display for AgentUpgradePolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // enums are a special case where their debug impl is their name ("Off")
        fmt::Debug::fmt(self, f)
    }
}

impl AgentUpgradePolicy {
    // The versions are strings like this: v2023.09-rc1-27-gc3ce4d5d
    pub fn should_upgrade(&self, agent_version: &str, carbide_api_version: &str) -> bool {
        use AgentUpgradePolicy::*;
        match self {
            Off => false,
            UpOnly => {
                let agent = match BuildVersion::try_from(agent_version) {
                    Ok(bv) => bv,
                    Err(err) => {
                        tracing::error!(
                            invalid_version = agent_version,
                            error = format!("{err:#}"),
                            "Invalid agent build version. Forcing upgrade."
                        );
                        // If the agent has an invalid build version we need to fix it,
                        // otherwise upgrades would be broken forever.
                        return true;
                    }
                };
                let carbide = match BuildVersion::try_from(carbide_api_version) {
                    Ok(bv) => bv,
                    Err(err) => {
                        tracing::error!(
                            invalid_version = carbide_api_version,
                            error = format!("{err:#}"),
                            "Invalid carbide-api build version"
                        );
                        // If carbide has an invalid version we wait until a fixed
                        // carbide is deployed.
                        return false;
                    }
                };
                agent.cmp(&carbide).is_lt()
            }
            UpDown => agent_version != carbide_api_version,
        }
    }
}

// From the database
impl From<&str> for AgentUpgradePolicy {
    fn from(str_policy: &str) -> Self {
        match str_policy {
            "Off" | "off" => AgentUpgradePolicy::Off,
            "UpOnly" | "uponly" | "up_only" => AgentUpgradePolicy::UpOnly,
            "UpDown" | "updown" | "up_down" => AgentUpgradePolicy::UpDown,
            _ => {
                tracing::error!(
                    invalid_policy = str_policy,
                    "Invalid dpu agent upgrade policy name in database. Disabling upgrades."
                );
                AgentUpgradePolicy::Off
            }
        }
    }
}

// From the config file
impl From<AgentUpgradePolicyChoice> for AgentUpgradePolicy {
    fn from(c: AgentUpgradePolicyChoice) -> Self {
        use crate::firmware::AgentUpgradePolicyChoice::*;
        match c {
            Off => AgentUpgradePolicy::Off,
            UpOnly => AgentUpgradePolicy::UpOnly,
            UpDown => AgentUpgradePolicy::UpDown,
        }
    }
}

/// Represents a build version, supporting both date-based (v2023.08) and semver (v0.0.4) formats.
#[derive(Debug, PartialEq, Eq)]
pub struct BuildVersion<'a> {
    /// The main version part. Either a date (2023.08) or semver (0.0.4)
    version: &'a str,
    rc: &'a str,
    hotfix: usize,
    commits: usize,
    git_hash: &'a str,
    /// True if (git) appended a "-dirty" suffix.
    is_dirty: bool,
}

impl BuildVersion<'_> {
    /// Returns true if this is a date-based version (starts with "20")
    fn is_date_version(&self) -> bool {
        self.version.starts_with("20")
    }

    /// Parse semver "X.Y.Z" into (major, minor, patch) for comparison
    fn parse_semver(&self) -> Option<(u32, u32, u32)> {
        let parts: Vec<&str> = self.version.split('.').collect();
        if parts.len() >= 3
            && let (Ok(major), Ok(minor), Ok(patch)) = (
                parts[0].parse::<u32>(),
                parts[1].parse::<u32>(),
                parts[2].parse::<u32>(),
            )
        {
            return Some((major, minor, patch));
        }
        None
    }
}

impl fmt::Display for BuildVersion<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> Result<(), fmt::Error> {
        write!(f, "v{}", self.version)?;
        if !self.rc.is_empty() {
            // rc and hotfix must either both appear, or neither
            write!(f, "-{}-{}", self.rc, self.hotfix)?;
        }
        if self.commits != 0 {
            write!(f, "-{}", self.commits)?;
        }
        if !self.git_hash.is_empty() {
            write!(f, "-{}", self.git_hash)?;
        }
        if self.is_dirty {
            write!(f, "-dirty")?;
        }
        Ok(())
    }
}

impl<'a> TryFrom<&'a str> for BuildVersion<'a> {
    type Error = eyre::Report;

    fn try_from(s: &'_ str) -> Result<BuildVersion<'_>, Self::Error> {
        let (s, is_dirty) = match s.strip_suffix("-dirty") {
            Some(stripped) => (stripped, true),
            None => (s, false),
        };

        if s.is_empty() {
            eyre::bail!("build version is empty");
        }
        if !s.starts_with('v') {
            eyre::bail!("build version should start with a 'v'");
        }
        let parts = s[1..].split('-').collect::<Vec<&str>>();
        if parts.is_empty() || parts[0].is_empty() {
            eyre::bail!("build version should have a version number after 'v'");
        }
        // Validate that the first part looks like a version (date or semver)
        // Date: 2023.08, 2024.05.02
        // Semver: 0.0.4, 1.2.3
        let first_char = parts[0].chars().next().unwrap();
        if !first_char.is_ascii_digit() {
            eyre::bail!("build version should start with a digit after 'v'");
        }
        match parts.len() {
            // Tag only. The tag is <year>.<month> or semver. e.g:
            // v2023.08 or v0.0.4
            1 => Ok(BuildVersion {
                version: parts[0],
                rc: "",
                hotfix: 0,
                commits: 0,
                git_hash: "",
                is_dirty,
            }),
            // Tag only with a release-candidate part
            // v2023.09-rc1 or v0.0.4-rc1
            2 => Ok(BuildVersion {
                version: parts[0],
                rc: parts[1],
                hotfix: 0,
                commits: 0,
                git_hash: "",
                is_dirty,
            }),
            // Version tag with commits OR new style version-rc-hotfix
            // v2023.08-92-g1b48e8b6 OR v2024.05.02-rc3-0
            3 => {
                if parts[1].chars().next().unwrap().is_numeric() {
                    // v2023.08-92-g1b48e8b6
                    Ok(BuildVersion {
                        version: parts[0],
                        rc: "",
                        hotfix: 0,
                        commits: parts[1].parse().unwrap(),
                        git_hash: parts[2],
                        is_dirty,
                    })
                } else {
                    // v2024.05.02-rc3-0 or v0.0.4-rc4-0
                    Ok(BuildVersion {
                        version: parts[0],
                        rc: parts[1],
                        hotfix: parts[2].parse().unwrap(),
                        commits: 0,
                        git_hash: "",
                        is_dirty,
                    })
                }
            }
            // Version-and-rc tag, commits
            // v2023.09-rc1-27-gc3ce4d5d or v0.0.4-rc4-0-g2a3c98cac
            4 => Ok(BuildVersion {
                version: parts[0],
                rc: parts[1],
                hotfix: 0,
                commits: parts[2].parse().unwrap(),
                git_hash: parts[3],
                is_dirty,
            }),
            // version, rc, hotfix, commits and hash
            // v2024.05.02-rc4-0-27-gc3ce4d5d or v0.0.4-rc4-0-27-gc3ce4d5d
            5 => Ok(BuildVersion {
                version: parts[0],
                rc: parts[1],
                hotfix: parts[2].parse().unwrap(),
                commits: parts[3].parse().unwrap(),
                git_hash: parts[4],
                is_dirty,
            }),
            n => {
                eyre::bail!("invalid build version. has {n} dash-separated parts")
            }
        }
    }
}

impl Ord for BuildVersion<'_> {
    // Ordering is by version/rc/hotfix/commits only; `git_hash` and `is_dirty`
    // are intentionally ignored (neither affects which build is "newer"). Note
    // the derived `Eq` is structural and does consider them.
    fn cmp(&self, other: &Self) -> Ordering {
        // If one is date-based and one is semver, semver is always newer
        // (represents migration from date-based to semver versioning)
        match (self.is_date_version(), other.is_date_version()) {
            (true, false) => return Ordering::Less,    // date < semver
            (false, true) => return Ordering::Greater, // semver > date
            (false, false) => {
                // Both are semver - compare numerically
                if let (Some(self_ver), Some(other_ver)) =
                    (self.parse_semver(), other.parse_semver())
                {
                    return self_ver
                        .cmp(&other_ver)
                        .then_with(|| self.rc.cmp(other.rc))
                        .then_with(|| self.hotfix.cmp(&other.hotfix))
                        .then_with(|| self.commits.cmp(&other.commits));
                }
                // Fall through to string comparison if parse fails
            }
            (true, true) => {
                // Both are date-based - use existing string comparison
            }
        }
        // Default: string comparison (works for date-based versions)
        self.version
            .cmp(other.version)
            .then_with(|| self.rc.cmp(other.rc))
            .then_with(|| self.hotfix.cmp(&other.hotfix))
            .then_with(|| self.commits.cmp(&other.commits))
    }
}

impl PartialOrd for BuildVersion<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[test]
fn test_parse_version() {
    use carbide_test_support::Outcome::*;
    use carbide_test_support::{Case, check_cases};

    check_cases(
        [
            // Date-based versions
            Case {
                scenario: "date tag with commits and hash",
                input: "v2023.08-92-g1b48e8b6",
                expect: Yields(BuildVersion {
                    version: "2023.08",
                    rc: "",
                    hotfix: 0,
                    commits: 92,
                    git_hash: "g1b48e8b6",
                    is_dirty: false,
                }),
            },
            Case {
                scenario: "date tag with rc, commits and hash",
                input: "v2023.09-rc1-27-gc3ce4d5d",
                expect: Yields(BuildVersion {
                    version: "2023.09",
                    rc: "rc1",
                    hotfix: 0,
                    commits: 27,
                    git_hash: "gc3ce4d5d",
                    is_dirty: false,
                }),
            },
            Case {
                scenario: "bare date tag",
                input: "v2023.08",
                expect: Yields(BuildVersion {
                    version: "2023.08",
                    rc: "",
                    hotfix: 0,
                    commits: 0,
                    git_hash: "",
                    is_dirty: false,
                }),
            },
            // Semver versions
            Case {
                scenario: "semver tag with rc, hotfix and hash",
                input: "v0.0.4-rc4-0-g2a3c98cac",
                expect: Yields(BuildVersion {
                    version: "0.0.4",
                    rc: "rc4",
                    hotfix: 0,
                    commits: 0,
                    git_hash: "g2a3c98cac",
                    is_dirty: false,
                }),
            },
            Case {
                scenario: "bare semver tag",
                input: "v1.2.3",
                expect: Yields(BuildVersion {
                    version: "1.2.3",
                    rc: "",
                    hotfix: 0,
                    commits: 0,
                    git_hash: "",
                    is_dirty: false,
                }),
            },
            Case {
                scenario: "semver tag with rc and hotfix",
                input: "v0.0.4-rc1-0",
                expect: Yields(BuildVersion {
                    version: "0.0.4",
                    rc: "rc1",
                    hotfix: 0,
                    commits: 0,
                    git_hash: "",
                    is_dirty: false,
                }),
            },
            Case {
                scenario: "date tag with rc and hotfix (3 parts, non-numeric second)",
                input: "v2024.05.02-rc3-0",
                expect: Yields(BuildVersion {
                    version: "2024.05.02",
                    rc: "rc3",
                    hotfix: 0,
                    commits: 0,
                    git_hash: "",
                    is_dirty: false,
                }),
            },
            Case {
                scenario: "date tag with rc, hotfix, commits and hash (5 parts)",
                input: "v2024.05.02-rc4-0-27-gc3ce4d5d",
                expect: Yields(BuildVersion {
                    version: "2024.05.02",
                    rc: "rc4",
                    hotfix: 0,
                    commits: 27,
                    git_hash: "gc3ce4d5d",
                    is_dirty: false,
                }),
            },
            Case {
                scenario: "bare semver, two parts only is treated as rc",
                input: "v2023.09-rc1",
                expect: Yields(BuildVersion {
                    version: "2023.09",
                    rc: "rc1",
                    hotfix: 0,
                    commits: 0,
                    git_hash: "",
                    is_dirty: false,
                }),
            },
            Case {
                scenario: "semver with rc, hotfix, commits and hash (5 parts)",
                input: "v0.0.4-rc4-0-27-gc3ce4d5d",
                expect: Yields(BuildVersion {
                    version: "0.0.4",
                    rc: "rc4",
                    hotfix: 0,
                    commits: 27,
                    git_hash: "gc3ce4d5d",
                    is_dirty: false,
                }),
            },
            Case {
                scenario: "semver with rc, commits, hash and -dirty suffix",
                input: "v2.0.0-rc.2-9-g0401aad9f-dirty",
                expect: Yields(BuildVersion {
                    version: "2.0.0",
                    rc: "rc.2",
                    hotfix: 0,
                    commits: 9,
                    git_hash: "g0401aad9f",
                    is_dirty: true,
                }),
            },
            Case {
                scenario: "nonzero hotfix parses",
                input: "v2024.05.02-rc3-2",
                expect: Yields(BuildVersion {
                    version: "2024.05.02",
                    rc: "rc3",
                    hotfix: 2,
                    commits: 0,
                    git_hash: "",
                    is_dirty: false,
                }),
            },
            Case {
                scenario: "too many dash-separated parts",
                input: "v2023.08-rc1-0-3-g123eff-x",
                expect: Fails,
            },
            Case {
                scenario: "no version number after the 'v'",
                input: "v-rc1",
                expect: Fails,
            },
            Case {
                scenario: "empty string",
                input: "",
                expect: Fails,
            },
            Case {
                scenario: "missing the leading 'v'",
                input: "2023.08",
                expect: Fails,
            },
            Case {
                scenario: "just the leading 'v'",
                input: "v",
                expect: Fails,
            },
            Case {
                scenario: "first part does not start with a digit",
                input: "vfoo.bar",
                expect: Fails,
            },
            Case {
                scenario: "leading 'v' followed by a dash",
                input: "v-",
                expect: Fails,
            },
        ],
        // The operation under test: parse a build-version string. The error type
        // (eyre::Report) is not PartialEq, so failing rows assert only that it
        // errors; we discard the report to keep the outcome comparable.
        |s| BuildVersion::try_from(s).map_err(|_| ()),
    );
}

#[cfg(test)]
mod tests {
    use std::cmp::Ordering;

    use carbide_test_support::Outcome::*;
    use carbide_test_support::{Check, check_values, scenarios, value_scenarios};

    use super::{AgentUpgradePolicy, BuildVersion};
    use crate::firmware::AgentUpgradePolicyChoice;

    #[test]
    fn agent_upgrade_policy_display() {
        value_scenarios!(
            run = |p| p.to_string();
            "off" {
                AgentUpgradePolicy::Off => "Off".to_string(),
            }

            "up-only" {
                AgentUpgradePolicy::UpOnly => "UpOnly".to_string(),
            }

            "up-down" {
                AgentUpgradePolicy::UpDown => "UpDown".to_string(),
            }
        );
    }

    #[test]
    fn agent_upgrade_policy_from_str() {
        value_scenarios!(
            run = AgentUpgradePolicy::from;
            "canonical Off" {
                "Off" => AgentUpgradePolicy::Off,
            }

            "lowercase off" {
                "off" => AgentUpgradePolicy::Off,
            }

            "canonical UpOnly" {
                "UpOnly" => AgentUpgradePolicy::UpOnly,
            }

            "lowercase uponly" {
                "uponly" => AgentUpgradePolicy::UpOnly,
            }

            "snake_case up_only" {
                "up_only" => AgentUpgradePolicy::UpOnly,
            }

            "canonical UpDown" {
                "UpDown" => AgentUpgradePolicy::UpDown,
            }

            "lowercase updown" {
                "updown" => AgentUpgradePolicy::UpDown,
            }

            "snake_case up_down" {
                "up_down" => AgentUpgradePolicy::UpDown,
            }

            "unknown string falls back to Off" {
                "nonsense" => AgentUpgradePolicy::Off,
            }

            "empty string falls back to Off" {
                "" => AgentUpgradePolicy::Off,
            }

            "wrong casing falls back to Off" {
                "OFF" => AgentUpgradePolicy::Off,
            }
        );
    }

    #[test]
    fn agent_upgrade_policy_from_choice() {
        value_scenarios!(
            run = AgentUpgradePolicy::from;
            "off" {
                AgentUpgradePolicyChoice::Off => AgentUpgradePolicy::Off,
            }

            "up-only" {
                AgentUpgradePolicyChoice::UpOnly => AgentUpgradePolicy::UpOnly,
            }

            "up-down" {
                AgentUpgradePolicyChoice::UpDown => AgentUpgradePolicy::UpDown,
            }
        );
    }

    #[test]
    fn should_upgrade_decides_per_policy() {
        struct Inputs {
            policy: AgentUpgradePolicy,
            agent: &'static str,
            carbide: &'static str,
        }
        check_values(
            [
                // Off never upgrades, regardless of versions.
                Check {
                    scenario: "off, agent older",
                    input: Inputs {
                        policy: AgentUpgradePolicy::Off,
                        agent: "v2023.08",
                        carbide: "v2023.09",
                    },
                    expect: false,
                },
                Check {
                    scenario: "off, agent newer",
                    input: Inputs {
                        policy: AgentUpgradePolicy::Off,
                        agent: "v2023.09",
                        carbide: "v2023.08",
                    },
                    expect: false,
                },
                Check {
                    scenario: "off, invalid agent version",
                    input: Inputs {
                        policy: AgentUpgradePolicy::Off,
                        agent: "garbage",
                        carbide: "v2023.08",
                    },
                    expect: false,
                },
                // UpOnly upgrades only when the agent is strictly older.
                Check {
                    scenario: "up-only, agent older",
                    input: Inputs {
                        policy: AgentUpgradePolicy::UpOnly,
                        agent: "v2023.08",
                        carbide: "v2023.09",
                    },
                    expect: true,
                },
                Check {
                    scenario: "up-only, agent equal",
                    input: Inputs {
                        policy: AgentUpgradePolicy::UpOnly,
                        agent: "v2023.09",
                        carbide: "v2023.09",
                    },
                    expect: false,
                },
                Check {
                    scenario: "up-only, agent newer (no downgrade)",
                    input: Inputs {
                        policy: AgentUpgradePolicy::UpOnly,
                        agent: "v2023.09",
                        carbide: "v2023.08",
                    },
                    expect: false,
                },
                Check {
                    scenario: "up-only, agent older by commit count",
                    input: Inputs {
                        policy: AgentUpgradePolicy::UpOnly,
                        agent: "v2023.08-14-gbc549a66",
                        carbide: "v2023.08-92-g1b48e8b6",
                    },
                    expect: true,
                },
                Check {
                    scenario: "up-only, semver agent newer than date carbide (no upgrade)",
                    input: Inputs {
                        policy: AgentUpgradePolicy::UpOnly,
                        agent: "v0.0.1",
                        carbide: "v2024.05.10-rc1-3",
                    },
                    expect: false,
                },
                Check {
                    scenario: "up-only, date agent older than semver carbide",
                    input: Inputs {
                        policy: AgentUpgradePolicy::UpOnly,
                        agent: "v2024.05.10-rc1-3",
                        carbide: "v0.0.1",
                    },
                    expect: true,
                },
                Check {
                    scenario: "up-only, invalid agent version forces upgrade",
                    input: Inputs {
                        policy: AgentUpgradePolicy::UpOnly,
                        agent: "not-a-version",
                        carbide: "v2023.09",
                    },
                    expect: true,
                },
                Check {
                    scenario: "up-only, invalid carbide version waits (no upgrade)",
                    input: Inputs {
                        policy: AgentUpgradePolicy::UpOnly,
                        agent: "v2023.08",
                        carbide: "not-a-version",
                    },
                    expect: false,
                },
                // UpDown upgrades on any string difference.
                Check {
                    scenario: "up-down, identical versions",
                    input: Inputs {
                        policy: AgentUpgradePolicy::UpDown,
                        agent: "v2023.09",
                        carbide: "v2023.09",
                    },
                    expect: false,
                },
                Check {
                    scenario: "up-down, agent older",
                    input: Inputs {
                        policy: AgentUpgradePolicy::UpDown,
                        agent: "v2023.08",
                        carbide: "v2023.09",
                    },
                    expect: true,
                },
                Check {
                    scenario: "up-down, agent newer (downgrade)",
                    input: Inputs {
                        policy: AgentUpgradePolicy::UpDown,
                        agent: "v2023.09",
                        carbide: "v2023.08",
                    },
                    expect: true,
                },
                Check {
                    scenario: "up-down, differing only by hash",
                    input: Inputs {
                        policy: AgentUpgradePolicy::UpDown,
                        agent: "v2023.08-92-g1b48e8b6",
                        carbide: "v2023.08-92-gdeadbeef",
                    },
                    expect: true,
                },
            ],
            |Inputs {
                 policy,
                 agent,
                 carbide,
             }| policy.should_upgrade(agent, carbide),
        );
    }

    #[test]
    fn build_version_display_round_trips() {
        scenarios!(
            run = |s| {
                BuildVersion::try_from(s)
                    .map(|bv| bv.to_string())
                    .map_err(drop)
            };
            "bare date tag" {
                "v2023.08" => Yields("v2023.08".to_string()),
            }

            "bare semver tag" {
                "v1.2.3" => Yields("v1.2.3".to_string()),
            }

            "date tag with commits and hash" {
                "v2023.08-92-g1b48e8b6" => Yields("v2023.08-92-g1b48e8b6".to_string()),
            }

            "rc and hotfix both render" {
                "v2024.05.02-rc3-0" => Yields("v2024.05.02-rc3-0".to_string()),
            }

            "full version with rc, hotfix, commits and hash" {
                "v2024.05.02-rc4-0-27-gc3ce4d5d" => Yields("v2024.05.02-rc4-0-27-gc3ce4d5d".to_string()),
            }

            "two-part input normalizes hotfix to 0" {
                "v2023.09-rc1" => Yields("v2023.09-rc1-0".to_string()),
            }
        );
    }

    // Total ordering over build versions. Each row pins the `cmp` result; the
    // runner also asserts `partial_cmp` agrees (it must return `Some(cmp)`), so
    // the PartialOrd impl is checked against Ord on every row.
    #[test]
    fn build_version_ordering() {
        struct Pair {
            left: &'static str,
            right: &'static str,
        }
        value_scenarios!(
            run = |Pair { left, right }| {
                let l = BuildVersion::try_from(left).unwrap();
                let r = BuildVersion::try_from(right).unwrap();
                let ordering = l.cmp(&r);
                assert_eq!(
                    l.partial_cmp(&r),
                    Some(ordering),
                    "partial_cmp must agree with cmp for {left} vs {right}",
                );
                ordering
            };
            "older date less than newer date" {
                Pair {
                    left: "v2023.08",
                    right: "v2023.09",
                } => Ordering::Less,
            }

            "equal date tags" {
                Pair {
                    left: "v2023.08",
                    right: "v2023.08",
                } => Ordering::Equal,
            }

            "newer date greater than older date" {
                Pair {
                    left: "v2023.09",
                    right: "v2023.08",
                } => Ordering::Greater,
            }

            "more commits is greater" {
                Pair {
                    left: "v2023.08-92-g1b48e8b6",
                    right: "v2023.08-14-gbc549a66",
                } => Ordering::Greater,
            }

            "date is always less than semver" {
                Pair {
                    left: "v2024.05.10-rc1-3",
                    right: "v0.0.1",
                } => Ordering::Less,
            }

            "semver is always greater than date" {
                Pair {
                    left: "v0.0.1",
                    right: "v2024.05.10-rc1-3",
                } => Ordering::Greater,
            }

            "lower semver less than higher semver" {
                Pair {
                    left: "v0.0.1",
                    right: "v0.0.4-rc4-0-g2a3c98cac",
                } => Ordering::Less,
            }

            "semver major dominates" {
                Pair {
                    left: "v0.0.4-rc4-0-g2a3c98cac",
                    right: "v1.0.0",
                } => Ordering::Less,
            }

            "equal semver tags" {
                Pair {
                    left: "v1.2.3",
                    right: "v1.2.3",
                } => Ordering::Equal,
            }

            "semver patch difference" {
                Pair {
                    left: "v0.0.4",
                    right: "v0.0.5",
                } => Ordering::Less,
            }
        );
    }

    #[test]
    fn test_compare_versions() -> eyre::Result<()> {
        use rand::prelude::SliceRandom;

        // In the correct order
        const VERSIONS: &[(&str, Option<&str>)] = &[
            // Left is input, Right is output if different
            // Due to Debian version numbering contraints, we changed the rules in May 2024 to
            // force a "hotfix" number to appear if and only if the rc number appears.
            //
            // Date-based versions (older)
            ("v2023.04", None),
            ("v2023.04.01", None),
            ("v2023.04.01-1-g17e5c956", None),
            (
                "v2023.06-rc2-1-gc5c05de3",
                Some("v2023.06-rc2-0-1-gc5c05de3"),
            ),
            ("v2023.08", None),
            ("v2023.08-14-gbc549a66", None),
            ("v2023.08-89-gd73315bc", None),
            ("v2023.08-92-g1b48e8b6", None),
            ("v2023.09-89-gd73315bc", None),
            ("v2023.09-rc1", Some("v2023.09-rc1-0")),
            (
                "v2023.09-rc1-1-g681e499f",
                Some("v2023.09-rc1-0-1-g681e499f"),
            ),
            (
                "v2023.09-rc1-27-gc3ce4d5d",
                Some("v2023.09-rc1-0-27-gc3ce4d5d"),
            ),
            ("v2024.05-rc3-0", None),
            ("v2024.05.02-rc3-0", None),
            ("v2024.05.02-rc4-0-27-gc3ce4d5d", None),
            ("v2024.05.10-rc1-0-3-g6497fef4d", None),
            ("v2024.05.10-rc1-3", None),
            // Semver versions (newer than all date-based versions)
            ("v0.0.1", None),
            ("v0.0.4-rc1-0", None),
            ("v0.0.4-rc4-0-g2a3c98cac", None),
            ("v0.0.5", None),
            ("v1.0.0", None),
            ("v1.2.3-rc1-0", None),
        ];
        let mut rng = rand::rng();

        // What we're testing
        let mut t: Vec<BuildVersion> = VERSIONS.iter().map(|v| (v.0).try_into().unwrap()).collect();
        t.shuffle(&mut rng);
        t.sort();

        // 't' should now be in the original order again
        for (i, (expect_normal, expect_different)) in VERSIONS.iter().enumerate() {
            let got = t[i].to_string();
            let expect = expect_different.unwrap_or(expect_normal);
            if got != expect {
                panic!("Pos {i} does not match. Got {got} expected {expect}.");
            }
        }

        Ok(())
    }
}

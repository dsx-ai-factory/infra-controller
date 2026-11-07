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

use carbide_utils::config::as_duration;
use chrono::Duration;
use duration_str::deserialize_duration_chrono;
use serde::{Deserialize, Serialize};

/// Power management configuration controlling retry
/// intervals and reboot timing.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PowerManagerOptions {
    /// Master switch to enable or disable power
    /// management.
    #[serde(default)]
    pub enabled: bool,
    /// Interval before retrying power operations after
    /// a successful attempt.
    /// Default is 5 minutes.
    #[serde(
        default = "default_next_duration_success",
        deserialize_with = "deserialize_duration_chrono",
        serialize_with = "as_duration"
    )]
    pub next_try_duration_on_success: chrono::TimeDelta,
    /// Interval before retrying power operations after
    /// a failed attempt.
    /// Default is 2 minutes.
    #[serde(
        default = "default_next_duration_failure",
        deserialize_with = "deserialize_duration_chrono",
        serialize_with = "as_duration"
    )]
    pub next_try_duration_on_failure: chrono::TimeDelta,
    /// Time to wait after power-down before powering on
    /// the host.
    /// Default is 15 minutes.
    #[serde(
        default = "default_wait_duration_next_reboot",
        deserialize_with = "deserialize_duration_chrono",
        serialize_with = "as_duration"
    )]
    pub wait_duration_until_host_reboot: chrono::TimeDelta,
}

pub fn default_power_options() -> PowerManagerOptions {
    PowerManagerOptions {
        enabled: false,
        next_try_duration_on_success: default_next_duration_success(),
        next_try_duration_on_failure: default_next_duration_failure(),
        wait_duration_until_host_reboot: default_wait_duration_next_reboot(),
    }
}

pub fn default_next_duration_success() -> Duration {
    Duration::minutes(5)
}

pub fn default_next_duration_failure() -> Duration {
    Duration::minutes(2)
}

pub fn default_wait_duration_next_reboot() -> Duration {
    Duration::minutes(15)
}

#[cfg(test)]
mod test {
    use figment::Figment;
    use figment::providers::{Format, Toml};

    use super::*;

    #[test]
    fn test_power_manager_default() {
        let toml = r#"
enabled = true
next_try_duration_on_success = "3m"
"#;

        let power_config: PowerManagerOptions =
            Figment::new().merge(Toml::string(toml)).extract().unwrap();

        println!("{power_config:?}");
        assert!(power_config.enabled);
        assert_eq!(
            Duration::minutes(3),
            power_config.next_try_duration_on_success
        );
        assert_eq!(
            Duration::minutes(2),
            power_config.next_try_duration_on_failure
        );
        assert_eq!(
            Duration::minutes(15),
            power_config.wait_duration_until_host_reboot
        );
    }

    #[test]
    fn test_power_manager_default_1() {
        let toml = r#""#;

        let power_config: PowerManagerOptions =
            Figment::new().merge(Toml::string(toml)).extract().unwrap();

        assert!(!power_config.enabled);
        assert_eq!(
            Duration::minutes(5),
            power_config.next_try_duration_on_success
        );
        assert_eq!(
            Duration::minutes(2),
            power_config.next_try_duration_on_failure
        );
        assert_eq!(
            Duration::minutes(15),
            power_config.wait_duration_until_host_reboot
        );
    }
}

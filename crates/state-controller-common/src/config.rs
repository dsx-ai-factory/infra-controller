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

use carbide_utils::config::as_std_duration;
use duration_str::deserialize_duration;
use serde::{Deserialize, Serialize};
use state_controller::config::IterationConfig;

/// Common StateController configurations
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct StateControllerConfig {
    /// Configures the desired duration for one state controller iteration
    ///
    /// Lower iteration times will make the controller react faster to state changes.
    /// However they will also increase the load on the system
    #[serde(
        default = "StateControllerConfig::iteration_time_default",
        deserialize_with = "deserialize_duration",
        serialize_with = "as_std_duration"
    )]
    pub iteration_time: std::time::Duration,

    /// Configures the shared time budget for claiming queued objects and evaluating
    /// and advancing each object's state. The deadline starts before acquiring the
    /// claim connection and does not reset at dispatch. A late claim is not dispatched;
    /// a late handler fails with `StateHandlerError::Timeout`. Abandoned reservations
    /// become eligible again after three times this duration. Defaults to 180 seconds.
    #[serde(
        default = "StateControllerConfig::max_object_handling_time_default",
        deserialize_with = "deserialize_duration",
        serialize_with = "as_std_duration"
    )]
    pub max_object_handling_time: std::time::Duration,

    /// Configures the maximum amount of concurrency for the object state controller
    ///
    /// The controller will attempt to advance the state of this amount of objects
    /// in parallel.
    #[serde(default = "StateControllerConfig::max_concurrency_default")]
    pub max_concurrency: usize,

    /// Configures the maximum time the state processor will wait when checking
    /// for and dispatching new tasks.
    /// This value needs to be lower than `iteration_time` in order to assure that
    /// tasks are executed more often than generated.
    /// If the value is set to 0, the processor will dispatch object handling tasks
    /// immediately once they are enqueued. The downside of 0 (or low) interval is
    /// however that the state controller will poll the database for new tasks
    /// with the same low interval.
    #[serde(
        default = "StateControllerConfig::processor_dispatch_interval_default",
        deserialize_with = "deserialize_duration",
        serialize_with = "as_std_duration"
    )]
    pub processor_dispatch_interval: std::time::Duration,

    /// Configures how often the state handling processor will emit log messages
    #[serde(
        default = "StateControllerConfig::processor_log_interval_default",
        deserialize_with = "deserialize_duration",
        serialize_with = "as_std_duration"
    )]
    pub processor_log_interval: std::time::Duration,

    /// Configures how often the state handling processor will reassess metrics and emit them.
    /// Calculating aggregate metrics is expensive (all object metrics need to be traversed).
    /// Therefore this should not happen much more frequently than the observabilty system
    /// will access them.
    #[serde(
        default = "StateControllerConfig::metric_emission_interval",
        deserialize_with = "deserialize_duration",
        serialize_with = "as_std_duration"
    )]
    pub metric_emission_interval: std::time::Duration,

    /// Configures for how long metrics for each object managed by the state controller
    /// will show up before they get evicted.
    /// The duration of this needs to be longer than the time between state handler
    /// invocations for the object
    #[serde(
        default = "StateControllerConfig::metric_hold_time",
        deserialize_with = "deserialize_duration",
        serialize_with = "as_std_duration"
    )]
    pub metric_hold_time: std::time::Duration,
}

impl StateControllerConfig {
    pub const fn max_object_handling_time_default() -> std::time::Duration {
        std::time::Duration::from_secs(3 * 60)
    }

    pub const fn iteration_time_default() -> std::time::Duration {
        std::time::Duration::from_secs(30)
    }

    pub const fn processor_dispatch_interval_default() -> std::time::Duration {
        std::time::Duration::from_secs(2)
    }

    pub const fn processor_log_interval_default() -> std::time::Duration {
        std::time::Duration::from_secs(60)
    }

    pub const fn metric_emission_interval() -> std::time::Duration {
        std::time::Duration::from_secs(60)
    }

    pub const fn metric_hold_time() -> std::time::Duration {
        std::time::Duration::from_secs(5 * 60)
    }

    pub const fn max_concurrency_default() -> usize {
        10
    }
}

impl Default for StateControllerConfig {
    fn default() -> Self {
        Self {
            iteration_time: Self::iteration_time_default(),
            max_object_handling_time: Self::max_object_handling_time_default(),
            processor_dispatch_interval: Self::processor_dispatch_interval_default(),
            processor_log_interval: Self::processor_log_interval_default(),
            max_concurrency: Self::max_concurrency_default(),
            metric_emission_interval: Self::metric_emission_interval(),
            metric_hold_time: Self::metric_hold_time(),
        }
    }
}

impl From<&StateControllerConfig> for IterationConfig {
    fn from(config: &StateControllerConfig) -> Self {
        IterationConfig {
            iteration_time: config.iteration_time,
            max_object_handling_time: config.max_object_handling_time,
            max_concurrency: config.max_concurrency,
            processor_dispatch_interval: config.processor_dispatch_interval,
            processor_log_interval: config.processor_log_interval,
            metric_emission_interval: config.metric_emission_interval,
            metric_hold_time: config.metric_hold_time,
        }
    }
}

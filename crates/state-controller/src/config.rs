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

use std::time::Duration;

/// General settings for state controller iterations
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct IterationConfig {
    /// Configures the desired duration for one state controller iteration
    ///
    /// Lower iteration times will make the controller react faster to state changes.
    /// However they will also increase the load on the system
    pub iteration_time: Duration,

    /// Configures the shared time budget for claiming queued objects and evaluating
    /// and advancing each object's state. The deadline starts before acquiring the
    /// claim connection and does not reset at dispatch. A late claim is not dispatched;
    /// a late handler fails with `StateHandlerError::Timeout`. Abandoned reservations
    /// become eligible again after three times this duration. Defaults to 180 seconds.
    pub max_object_handling_time: Duration,

    /// Configures the maximum amount of concurrency for the object state controller
    ///
    /// The controller will attempt to advance the state of this amount of instances
    /// in parallel.
    pub max_concurrency: usize,

    /// Configures how long the state processor will wait between dispatching new tasks
    pub processor_dispatch_interval: Duration,

    /// Configures how often the state handling processor will emit periodic log messages
    pub processor_log_interval: Duration,

    /// Configures how often the state handling processor will reassess metrics and emit them.
    /// Calculating aggregate metrics is expensive (all object metrics need to be traversed).
    /// Therefore this should not happen much more frequently than the observabilty system
    /// will access them.
    pub metric_emission_interval: std::time::Duration,

    /// Configures for how long metrics for each object managed by the state controller
    /// will show up before they get evicted.
    /// The duration of this needs to be longer than the time between state handler
    /// invocations for the object
    pub metric_hold_time: std::time::Duration,
}

impl Default for IterationConfig {
    fn default() -> Self {
        Self {
            iteration_time: Duration::from_secs(30),
            // This is by default set rather high to make sure we usually run the operations
            // in the state handlers to completion. The purpose of the timeout is just to
            // prevent an indefinitely stuck state handler - e.g. to due to networking issues
            // and missing sqlx timeouts
            max_object_handling_time: Duration::from_secs(3 * 60),
            max_concurrency: 10,
            processor_log_interval: Duration::from_secs(60),
            processor_dispatch_interval: Duration::from_secs(2),
            metric_emission_interval: Duration::from_secs(60),
            metric_hold_time: Duration::from_secs(5 * 60),
        }
    }
}

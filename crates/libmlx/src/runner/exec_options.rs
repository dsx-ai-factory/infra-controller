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

// src/exec_options.rs
// This defines execution options for the underlying mlxconfig
// command that is run, as part of the mlxconfig-runner crate,
// allowing us to set various things for how we want to interact
// (or not) with mlxconfig, including timeout and retry behavior.

use std::time::Duration;

// DESTRUCTIVE_VARIABLES are variables that may potentially require
// confirmation before modification (and will be enforced if the
// runner is configured with confirm_destructive: true).
pub const DESTRUCTIVE_VARIABLES: &[&str] = &["OH_MY_DPU"];

// ExecOptions contains all options for controlling command execution,
// including timeout, retry, and exponential backoff behavior.
#[derive(Debug, Clone)]
pub struct ExecOptions {
    // timeout is the underlying mlxconfig command timeout.
    // If None, commands will run indefinitely.
    pub timeout: Option<Duration>,

    // retries defines the number of retries to allow in
    // the event mlxconfig is having problems with the device.
    pub retries: u32,

    // retry_delay is the initial delay between retries when using
    // exponential backoff.
    pub retry_delay: Duration,

    // max_retry_delay is the maximum delay between retries, capping
    // the exponential backoff growth.
    pub max_retry_delay: Duration,

    // retry_multiplier is the exponential backoff multiplier.
    // For example, 2.0 means each retry delay doubles, 1.5 means 50% increase.
    pub retry_multiplier: f32,

    // dry_run instructs the runner to not actually execute
    // change operations on the device, usually resulting in
    // us just logging the command that would be run otherwise.
    pub dry_run: bool,

    // verbose logging/output.
    pub verbose: bool,

    // log_json_output will result in us also logging the
    // full JSON output we got back from the device.
    pub log_json_output: bool,

    // confirm_destructive will make it so the runner requires
    // confirmation for destructive variables.
    pub confirm_destructive: bool,
}

impl Default for ExecOptions {
    // Creates default ExecOptions with reasonable settings for
    // production use with mlxconfig commands.
    fn default() -> Self {
        Self {
            timeout: Some(Duration::from_secs(30)),
            retries: 3,
            retry_delay: Duration::from_millis(500),
            max_retry_delay: Duration::from_secs(60),
            retry_multiplier: 2.0,
            dry_run: false,
            verbose: false,
            log_json_output: false,
            confirm_destructive: false,
        }
    }
}

impl ExecOptions {
    // Creates new ExecOptions using the defaults.
    pub fn new() -> Self {
        Self::default()
    }

    // Sets the command timeout. Use None for no timeout.
    pub fn with_timeout(mut self, timeout: Option<Duration>) -> Self {
        self.timeout = timeout;
        self
    }

    // Sets the number of retry attempts.
    pub fn with_retries(mut self, retries: u32) -> Self {
        self.retries = retries;
        self
    }

    // Sets the initial retry delay for exponential backoff.
    pub fn with_retry_delay(mut self, delay: Duration) -> Self {
        self.retry_delay = delay;
        self
    }

    // Sets the maximum retry delay, capping exponential backoff growth.
    pub fn with_max_retry_delay(mut self, max_delay: Duration) -> Self {
        self.max_retry_delay = max_delay;
        self
    }

    // Sets the exponential backoff multiplier.
    // 2.0 = double each time, 1.5 = 50% increase, etc.
    pub fn with_retry_multiplier(mut self, multiplier: f32) -> Self {
        self.retry_multiplier = multiplier;
        self
    }

    // Enables or disables dry-run mode. In dry-run mode, commands
    // are logged but not actually executed.
    pub fn with_dry_run(mut self, dry_run: bool) -> Self {
        self.dry_run = dry_run;
        self
    }

    // Enables or disables verbose logging of command execution,
    // retry attempts, timeouts, and other operational details.
    pub fn with_verbose(mut self, verbose: bool) -> Self {
        self.verbose = verbose;
        self
    }

    // Enables or disables logging of raw JSON responses from mlxconfig.
    // Useful for debugging but can be very verbose.
    pub fn with_log_json_output(mut self, log_json_output: bool) -> Self {
        self.log_json_output = log_json_output;
        self
    }

    // Enables or disables confirmation prompts for destructive variables.
    // When enabled, the user will be prompted before modifying variables
    // listed in DESTRUCTIVE_VARIABLES.
    pub fn with_confirm_destructive(mut self, confirm_destructive: bool) -> Self {
        self.confirm_destructive = confirm_destructive;
        self
    }
}

// Checks if a given variable is considered a "destructive" variable
// that should require confirmation before modification.
pub fn is_destructive_variable(variable_name: &str) -> bool {
    DESTRUCTIVE_VARIABLES.contains(&variable_name)
}

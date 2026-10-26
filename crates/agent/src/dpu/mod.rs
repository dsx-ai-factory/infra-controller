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
use std::fmt;
use std::fmt::{Display, Formatter};
use std::time::Duration;

use carbide_host_support::agent_config::FmdsDpuNetworkingConfig;
use eyre::WrapErr;
use ipnetwork::IpNetwork;

use crate::pretty_cmd;

pub mod interface;
pub mod link;
pub mod route;

pub(crate) const ARMOS_TEST_DATA_DIR: &str =
    concat!(env!("CARGO_MANIFEST_DIR"), "/../../dev/docker-env");
pub(crate) const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);

#[cfg(test)]
tokio::task_local! {
    /// Opt one isolated networking test into kernel observations without changing
    /// the fixture behavior of other tests running in the same process.
    pub(crate) static REAL_KERNEL_NETWORKING: ();
}

/// Use fixtures for environment overrides and tests that have not explicitly
/// requested kernel I/O. Production uses the kernel unless an override is set.
fn use_networking_fixture(override_env: &str) -> bool {
    let use_fixture = std::env::var(override_env).is_ok();
    #[cfg(test)]
    let use_fixture = use_fixture || REAL_KERNEL_NETWORKING.try_with(|()| ()).is_err();
    use_fixture
}

/// Bound network commands by `COMMAND_TIMEOUT` and preserve the command, status,
/// and stderr so callers can distinguish observation failures from empty state.
async fn run_ip_command(mut command: tokio::process::Command) -> eyre::Result<String> {
    command.kill_on_drop(true);
    let command_text = pretty_cmd(command.as_std());
    tracing::trace!(command = %command_text, "Running network command");
    let output = tokio::time::timeout(COMMAND_TIMEOUT, command.output())
        .await
        .wrap_err_with(|| {
            format!("timeout after {COMMAND_TIMEOUT:?} while running command: {command_text}")
        })?
        .wrap_err_with(|| format!("could not run command: {command_text}"))?;
    if !output.status.success() {
        eyre::bail!(
            "command {command_text} failed with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    String::from_utf8(output.stdout)
        .wrap_err_with(|| format!("invalid UTF-8 output from command: {command_text}"))
}

#[derive(Debug, Clone)]
pub struct DpuNetworkInterfaces {
    pub desired: Vec<IpNetwork>,
}

#[derive(PartialOrd, PartialEq, Eq, Hash, Debug, Clone, Copy)]
pub enum Action {
    Add,
    Remove,
}

impl Display for Action {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        match self {
            Action::Add => write!(f, "Add"),
            Action::Remove => write!(f, "Remove"),
        }
    }
}

impl DpuNetworkInterfaces {
    pub fn new(fmds_interface_config: &FmdsDpuNetworkingConfig) -> Self {
        DpuNetworkInterfaces {
            desired: fmds_interface_config.config.addresses.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    /// A failed network command must retain its status and stderr so its caller
    /// can report the kernel diagnostic instead of treating failure as no data.
    #[tokio::test]
    async fn ip_command_failure_preserves_status_and_stderr() {
        // Exercise subprocess failure without changing the test host's network.
        let mut command = tokio::process::Command::new("/bin/sh");
        command.args(["-c", "printf 'route rejected\\n' >&2; exit 17"]);
        let error = super::run_ip_command(command)
            .await
            .expect_err("failed network command must propagate");
        let message = error.to_string();
        assert!(message.contains("exit status: 17"), "{message}");
        assert!(message.contains("route rejected"), "{message}");
    }
}

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
use std::fmt::Write;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::Duration;

use carbide_instrument::emit;
use carbide_uuid::machine::MachineId;
use eyre::WrapErr;
use tokio::process::Command;

use crate::instrumentation::LldpdRestart;

// FIXME: This should probably be configurable and come from the API's config
// file.
const SITE_OPERATOR: &str = "Forge-SRE (ngc-forge-sre@exchange.nvidia.com)";
const LLDP_INTERFACES_CONFIG: &str = "/etc/lldpd.d/lldp-interfaces.conf";
const DISABLED_LLDP_INTERFACES_CONFIG: &str = "/etc/lldpd.d/lldp-interfaces.conf.disabled";
const LLDPD_DEFAULT_CONFIG: &str = "/etc/default/lldpd";
const LLDPD_DAEMON_ARGS: &str = "DAEMON_ARGS=\"-M 1\"";
const MAX_LLDPD_DEFAULT_CONFIG_SIZE: u64 = 1024 * 1024;
const LLDPD_RESTART_ATTEMPTS: u8 = 3;
const LLDP_MED_CONFIGURATION_CHECK_ATTEMPTS: u8 = 3;
const LLDPCLI_TIMEOUT: Duration = Duration::from_secs(10);
const LLDPD_RESTART_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LldpdRestartFailureAction {
    Retry,
    ReturnError,
}

fn lldpd_restart_failure_action(attempt: u8) -> LldpdRestartFailureAction {
    if attempt == LLDPD_RESTART_ATTEMPTS {
        LldpdRestartFailureAction::ReturnError
    } else {
        LldpdRestartFailureAction::Retry
    }
}

pub(crate) async fn prepare_lldp() -> eyre::Result<()> {
    let interfaces_config_disabled = disable_interfaces_config()?;
    let daemon_args_updated = ensure_lldpd_daemon_args()?;

    if interfaces_config_disabled || daemon_args_updated {
        restart_lldpd().await?;
    }

    ensure_lldp_med_inventory_enabled().await
}

fn disable_interfaces_config() -> eyre::Result<bool> {
    let source_path = Path::new(LLDP_INTERFACES_CONFIG);
    if !source_path.try_exists().wrap_err_with(|| {
        format!(
            "couldn't check existence of LLDP interfaces config {path}",
            path = source_path.display()
        )
    })? {
        return Ok(false);
    }

    let destination_path = Path::new(DISABLED_LLDP_INTERFACES_CONFIG);
    fs::rename(source_path, destination_path).wrap_err_with(|| {
        format!(
            "couldn't rename LLDP interfaces config from {source} to {destination}",
            source = source_path.display(),
            destination = destination_path.display()
        )
    })?;
    tracing::info!(
        source_path = %source_path.display(),
        destination_path = %destination_path.display(),
        "Disabled LLDP interfaces config"
    );

    Ok(true)
}

fn ensure_lldpd_daemon_args() -> eyre::Result<bool> {
    let mut current_contents = String::new();
    fs::File::open(LLDPD_DEFAULT_CONFIG)
        .wrap_err("couldn't open lldpd default config")?
        .take(MAX_LLDPD_DEFAULT_CONFIG_SIZE + 1)
        .read_to_string(&mut current_contents)
        .wrap_err("couldn't read lldpd default config")?;
    eyre::ensure!(
        current_contents.len() as u64 <= MAX_LLDPD_DEFAULT_CONFIG_SIZE,
        "lldpd default config exceeds {MAX_LLDPD_DEFAULT_CONFIG_SIZE} bytes"
    );

    let desired_contents = rewrite_lldpd_daemon_args(&current_contents);

    let mut config_file =
        crate::agent_platform::ManagedFile::new(PathBuf::from(LLDPD_DEFAULT_CONFIG));
    let updated = config_file.ensure_contents(desired_contents.as_bytes())?;
    if updated {
        tracing::info!(
            path = LLDPD_DEFAULT_CONFIG,
            "Updated lldpd daemon arguments"
        );
    }

    Ok(updated)
}

fn rewrite_lldpd_daemon_args(current_contents: &str) -> String {
    let mut daemon_args_found = false;
    let mut desired_contents = String::with_capacity(current_contents.len());

    for line in current_contents.split_inclusive('\n') {
        let line_contents = line.strip_suffix('\n').unwrap_or(line);
        let line_contents = line_contents.strip_suffix('\r').unwrap_or(line_contents);
        if line_contents.trim_start().starts_with("DAEMON_ARGS=") {
            daemon_args_found = true;
            desired_contents.push_str(LLDPD_DAEMON_ARGS);
            if line.ends_with("\r\n") {
                desired_contents.push_str("\r\n");
            } else if line.ends_with('\n') {
                desired_contents.push('\n');
            }
        } else {
            desired_contents.push_str(line);
        }
    }

    if !daemon_args_found {
        if !desired_contents.is_empty() && !desired_contents.ends_with('\n') {
            desired_contents.push('\n');
        }
        desired_contents.push_str(LLDPD_DAEMON_ARGS);
        desired_contents.push('\n');
    }

    desired_contents
}

async fn lldp_med_inventory_disabled() -> eyre::Result<bool> {
    let mut command = Command::new("lldpcli");
    command.args(["show", "configuration", "-f", "json0"]);
    let output = command_output_with_timeout(
        command,
        LLDPCLI_TIMEOUT,
        "lldpcli show configuration -f json0",
    )
    .await?;
    if !output.status.success() {
        eyre::bail!(
            "lldpcli show configuration -f json0 failed with status {status}: {stderr}",
            status = output.status,
            stderr = String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    parse_lldp_med_inventory_disabled(&output.stdout)
}

fn parse_lldp_med_inventory_disabled(configuration: &[u8]) -> eyre::Result<bool> {
    let configuration: serde_json::Value = serde_json::from_slice(configuration)
        .wrap_err("lldpcli returned invalid JSON configuration")?;
    let inventory_disabled = configuration
        .pointer("/configuration/0/config/0/lldpmed-no-inventory/0/value")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| eyre::eyre!("lldpcli configuration did not report LLDP-MED inventory"))?;

    match inventory_disabled {
        "yes" => Ok(true),
        "no" => Ok(false),
        value => eyre::bail!("lldpcli returned unexpected LLDP-MED inventory value {value:?}"),
    }
}

async fn command_output_with_timeout(
    mut command: Command,
    timeout: Duration,
    command_name: &str,
) -> eyre::Result<Output> {
    // Dropping the timed-out output future drops its child and terminates it.
    command.kill_on_drop(true);
    tokio::time::timeout(timeout, command.output())
        .await
        .wrap_err_with(|| {
            format!(
                "{command_name} timed out after {timeout_seconds} seconds",
                timeout_seconds = timeout.as_secs()
            )
        })?
        .wrap_err_with(|| format!("couldn't run {command_name}"))
}

async fn ensure_lldp_med_inventory_enabled() -> eyre::Result<()> {
    let mut attempt = 1;
    loop {
        match lldp_med_inventory_disabled().await {
            Ok(false) => return Ok(()),
            Ok(true) if attempt == LLDP_MED_CONFIGURATION_CHECK_ATTEMPTS => {
                eyre::bail!(
                    "LLDP-MED inventory remained disabled after {attempt} configuration checks"
                );
            }
            Err(error) if attempt == LLDP_MED_CONFIGURATION_CHECK_ATTEMPTS => {
                return Err(error).wrap_err_with(|| {
                    format!(
                        "couldn't verify LLDP-MED inventory after {attempt} configuration checks"
                    )
                });
            }
            Ok(true) => {
                tracing::warn!(
                    attempt,
                    "LLDP-MED inventory is disabled, restarting lldpd service"
                );
            }
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    attempt,
                    "Couldn't query LLDP-MED inventory, restarting lldpd service"
                );
            }
        }

        restart_lldpd().await?;
        attempt += 1;
    }
}

async fn restart_lldpd() -> eyre::Result<()> {
    let mut attempt = 1;
    loop {
        let mut command = Command::new("systemctl");
        command.args(["restart", "lldpd.service"]);
        let restart_result = command_output_with_timeout(
            command,
            LLDPD_RESTART_TIMEOUT,
            "systemctl restart lldpd.service",
        )
        .await
        .and_then(|output| {
            if output.status.success() {
                Ok(())
            } else {
                eyre::bail!(
                    "systemctl restart lldpd.service failed with status {status}",
                    status = output.status
                )
            }
        });

        match restart_result {
            Ok(()) => {
                emit(LldpdRestart::Succeeded {
                    attempt: i64::from(attempt),
                });
                return Ok(());
            }
            Err(error) => match lldpd_restart_failure_action(attempt) {
                LldpdRestartFailureAction::Retry => {
                    emit(LldpdRestart::Retrying {
                        error: error.to_string(),
                        attempt: i64::from(attempt),
                    });
                    tokio::time::sleep(Duration::from_secs(u64::from(attempt))).await;
                    attempt += 1;
                }
                LldpdRestartFailureAction::ReturnError => {
                    emit(LldpdRestart::Failed {
                        error: error.to_string(),
                        attempt_count: i64::from(attempt),
                    });
                    return Err(error);
                }
            },
        }
    }
}

pub async fn set_lldp_system_description(machine_id: &MachineId) -> eyre::Result<()> {
    let system_description = format!("{SITE_OPERATOR}, {machine_id}");
    let lldp_config = LldpConfig {
        system_description: Some(system_description),
    };
    let writer = LldpdConfigFileWriter::default();

    let file_updated = writer.ensure_file(&lldp_config)?;

    // If the file contents were updated, we'll ask lldpcli to read it in, which
    // updates the running config in the lldpd service.
    match file_updated {
        true => writer.daemon_read().await,
        false => Ok(()),
    }
}

#[derive(Debug)]
pub struct LldpConfig {
    pub system_description: Option<String>,
}

#[derive(Debug)]
pub struct LldpdConfigFileWriter {
    pub filename: PathBuf,
    pub header_comments: Vec<String>,
}

impl LldpdConfigFileWriter {
    pub fn ensure_file(&self, config: &LldpConfig) -> eyre::Result<bool> {
        let file_contents = self.render_contents(config);
        let mut config_file = crate::agent_platform::ManagedFile::new(self.filename.to_owned());
        config_file.ensure_contents(file_contents.as_bytes())
    }

    fn render_contents(&self, config: &LldpConfig) -> String {
        let mut contents = String::new();

        for comment_line in self.header_comments.iter() {
            writeln!(&mut contents, "# {comment_line}").unwrap();
        }

        let LldpConfig { system_description } = config;
        if let Some(system_description) = system_description {
            writeln!(
                &mut contents,
                "configure system description \"{system_description}\""
            )
            .unwrap();
        }

        contents
    }

    // Ask lldpcli to read in the config file commands (which will be passed
    // to the running lldpd service).
    pub async fn daemon_read(&self) -> eyre::Result<()> {
        let mut command = Command::new("lldpcli");
        command.arg("-c");
        command.arg(self.filename.as_os_str());
        let output =
            command_output_with_timeout(command, LLDPCLI_TIMEOUT, "lldpcli config read").await?;
        match output.status {
            status if status.success() => Ok(()),
            status => Err(eyre::eyre!(
                "unsuccessful exit status from lldpcli: {status}"
            )),
        }
    }
}

impl Default for LldpdConfigFileWriter {
    fn default() -> Self {
        Self {
            filename: "/etc/lldpd.d/forge.conf".into(),
            header_comments: vec!["This file is managed by the Forge DPU agent".into()],
        }
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::*;
    use carbide_test_support::{scenarios, value_scenarios};

    use super::*;

    #[test]
    fn test_rewrite_lldpd_daemon_args() {
        value_scenarios!(rewrite_lldpd_daemon_args:
            "missing DAEMON_ARGS" {
                "" => "DAEMON_ARGS=\"-M 1\"\n".to_string(),
                "# lldpd defaults\n#DAEMON_ARGS=\"old\"\nOLD_DAEMON_ARGS=\"old\"\n" =>
                    "# lldpd defaults\n#DAEMON_ARGS=\"old\"\nOLD_DAEMON_ARGS=\"old\"\nDAEMON_ARGS=\"-M 1\"\n".to_string(),
                "# lldpd defaults\r\n#DAEMON_ARGS=\"old\"\r\n" =>
                    "# lldpd defaults\r\n#DAEMON_ARGS=\"old\"\r\nDAEMON_ARGS=\"-M 1\"\n".to_string(),
                "# lldpd defaults" =>
                    "# lldpd defaults\nDAEMON_ARGS=\"-M 1\"\n".to_string(),
            }

            "existing DAEMON_ARGS" {
                "# lldpd defaults\nDAEMON_ARGS=\"\"\nOTHER=yes\n" =>
                    "# lldpd defaults\nDAEMON_ARGS=\"-M 1\"\nOTHER=yes\n".to_string(),
                "# lldpd defaults\r\nDAEMON_ARGS=\"--foo\"\r\nOTHER=yes\r\n" =>
                    "# lldpd defaults\r\nDAEMON_ARGS=\"-M 1\"\r\nOTHER=yes\r\n".to_string(),
                "# lldpd defaults\nDAEMON_ARGS=\"--foo\"" =>
                    "# lldpd defaults\nDAEMON_ARGS=\"-M 1\"".to_string(),
            }
        );
    }

    #[test]
    fn test_parse_lldp_med_inventory_disabled() {
        scenarios!(run = |configuration: &str| {
            parse_lldp_med_inventory_disabled(configuration.as_bytes()).map_err(drop)
        };
            "reported inventory state" {
                r#"{"configuration":[{"config":[{"lldpmed-no-inventory":[{"value":"yes"}]}]}]}"# =>
                    Yields(true),
                r#"{"configuration":[{"config":[{"lldpmed-no-inventory":[{"value":"no"}]}]}]}"# =>
                    Yields(false),
            }

            "invalid inventory state" {
                r#"{"configuration":[{"config":[{}]}]}"# => Fails,
                r#"{"configuration":[{"config":[{"lldpmed-no-inventory":[{"value":"maybe"}]}]}]}"# =>
                    Fails,
            }
        );
    }

    #[test]
    fn lldpd_restart_failures_retry_until_the_final_attempt() {
        value_scenarios!(lldpd_restart_failure_action:
            "retry budget remains" {
                1 => LldpdRestartFailureAction::Retry,
                2 => LldpdRestartFailureAction::Retry,
            }

            "retry budget is exhausted" {
                3 => LldpdRestartFailureAction::ReturnError,
            }
        );
    }

    #[test]
    fn test_lldp_contents() {
        let lldp_config = LldpConfig {
            system_description: Some("deluxe toaster".into()),
        };
        let lldpd_writer = LldpdConfigFileWriter::default();
        let contents = lldpd_writer.render_contents(&lldp_config);

        let expected_contents = "# This file is managed by the Forge DPU agent\n\
            configure system description \"deluxe toaster\"\n";

        assert_eq!(contents.as_str(), expected_contents);
    }
}

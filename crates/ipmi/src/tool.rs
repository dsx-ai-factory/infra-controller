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

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use carbide_secrets::credentials::{CredentialKey, CredentialReader, Credentials};
use carbide_utils::cmd::{CmdError, CmdResult, TokioCmd};
use eyre::eyre;

use crate::metrics::{IpmiCommand, count_ipmi_command};
use crate::{IPMITool, RestartError};

pub(super) struct IPMIToolImpl {
    instrumentation: carbide_instrument::Instrumentation,
    credential_reader: Arc<dyn CredentialReader>,
    attempts: u32,
}

impl IPMIToolImpl {
    pub(super) fn new(
        credential_reader: Arc<dyn CredentialReader>,
        attempts: Option<u32>,
        instrumentation: carbide_instrument::Instrumentation,
    ) -> Self {
        IPMIToolImpl {
            instrumentation,
            credential_reader,
            attempts: attempts.unwrap_or(3),
        }
    }

    /// The `ipmitool` argument tail that runs `command`.
    fn command_args(command: IpmiCommand) -> &'static str {
        match command {
            IpmiCommand::ChassisPowerReset => "-I lanplus -C 17 chassis power reset",
            IpmiCommand::DpuLegacyPowerReset => "-I lanplus -C 17 raw 0x32 0xA1 0x01",
            IpmiCommand::BmcColdReset => "-I lanplus -C 17 bmc reset cold",
        }
    }

    fn connection_args(bmc_address: SocketAddr, username: &str) -> [String; 7] {
        [
            "-H".to_string(),
            bmc_address.ip().to_string(),
            "-p".to_string(),
            bmc_address.port().to_string(),
            "-U".to_string(),
            username.to_string(),
            "-E".to_string(),
        ]
    }
}

#[async_trait]
impl IPMITool for IPMIToolImpl {
    async fn restart_once(
        &self,
        bmc_address: SocketAddr,
        credential_key: &CredentialKey,
    ) -> Result<(), RestartError> {
        let credentials = self
            .credentials(credential_key)
            .await
            .map_err(RestartError::NotSent)?;
        self.execute_ipmitool_command(
            IpmiCommand::ChassisPowerReset,
            bmc_address,
            &credentials,
            true,
        )
        .await
        .map_err(classify_restart_error)?;
        Ok(())
    }

    async fn restart_with_credentials(
        &self,
        bmc_address: SocketAddr,
        credentials: &Credentials,
    ) -> Result<(), eyre::Report> {
        self.execute_ipmitool_command(
            IpmiCommand::ChassisPowerReset,
            bmc_address,
            credentials,
            false,
        )
        .await?;
        Ok(())
    }

    async fn bmc_cold_reset(
        &self,
        bmc_address: SocketAddr,
        credential_key: &CredentialKey,
    ) -> Result<(), eyre::Report> {
        let credentials = self.credentials(credential_key).await?;

        match self
            .execute_ipmitool_command(IpmiCommand::BmcColdReset, bmc_address, &credentials, false)
            .await
        {
            Ok(_) => Ok(()),
            Err(e) => Err(eyre::eyre!("{}", e.to_string())),
        }
    }

    async fn restart(
        &self,
        bmc_address: SocketAddr,
        legacy_boot: bool,
        credential_key: &CredentialKey,
    ) -> Result<(), eyre::Report> {
        let credentials = self.credentials(credential_key).await?;

        // A failed legacy command is counted by the runner. Fall back to standard chassis reset.
        if legacy_boot
            && self
                .execute_ipmitool_command(
                    IpmiCommand::DpuLegacyPowerReset,
                    bmc_address,
                    &credentials,
                    false,
                )
                .await
                .is_ok()
        {
            return Ok(());
        }
        self.restart_with_credentials(bmc_address, &credentials)
            .await
    }
}

impl IPMIToolImpl {
    async fn credentials(
        &self,
        credential_key: &CredentialKey,
    ) -> Result<Credentials, eyre::Report> {
        self.credential_reader
            .get_credentials(credential_key)
            .await
            .map_err(|e| {
                eyre!("secret engine getting credentials for key {credential_key:#?}: {e:#?}")
            })?
            .ok_or_else(|| eyre!("no credentials for key {credential_key:#?} found"))
    }

    async fn execute_ipmitool_command(
        &self,
        command: IpmiCommand,
        bmc_address: SocketAddr,
        credentials: &Credentials,
        single_attempt: bool,
    ) -> CmdResult<String> {
        let (username, password) = match credentials {
            Credentials::UsernamePassword { username, password } => (username, password),
        };

        // cmd line args that are filled in from the db
        let mut args = Self::connection_args(bmc_address, username).to_vec();
        args.extend(Self::command_args(command).split(' ').map(str::to_owned));
        let cmd = TokioCmd::new("/usr/bin/ipmitool")
            .args(&args)
            .attempts(self.attempts);

        tracing::info!(command = ?cmd, "Running IPMI command");
        let cmd = cmd.env("IPMITOOL_PASSWORD", password);
        let result = if single_attempt {
            // Bound this one command, not the controller's durable recovery lifetime.
            // The default controller budget is 180 seconds and can cancel it earlier.
            execute_single_attempt(cmd, Duration::from_secs(60)).await
        } else {
            cmd.output().await
        };
        count_ipmi_command(command, &result, &self.instrumentation);
        result
    }
}

/// Only a failed spawn proves that this attempt could not have sent a reset.
fn classify_restart_error(error: CmdError) -> RestartError {
    match error {
        CmdError::RunError(..) => RestartError::NotSent(error.into()),
        _ => RestartError::Uncertain(error.into()),
    }
}

/// Bound and cancel one local reset process while retaining whether it started.
async fn execute_single_attempt(command: TokioCmd, deadline: Duration) -> CmdResult<String> {
    tokio::time::timeout(deadline, command.attempts(1).kill_on_drop(true).output())
        .await
        .map_err(|error| {
            CmdError::TokioRunError("ipmitool chassis power reset".into(), error.to_string())
        })?
}

#[cfg(test)]
mod test {
    use std::net::SocketAddr;
    use std::sync::Arc;

    use carbide_secrets::credentials::Credentials;
    use carbide_secrets::test_support::credentials::TestCredentialManager;

    /// A failed result must not repeat a mutation, even when the command requests retries.
    #[cfg(unix)]
    #[tokio::test]
    async fn durable_reset_runs_one_process() {
        // Zero would otherwise run no process; three would replay a failed mutation.
        for configured_attempts in [0, 3] {
            let dir = tempfile::tempdir().unwrap();
            let calls = dir.path().join("calls");
            let command = carbide_utils::cmd::TokioCmd::new("/bin/sh")
                .args([
                    "-c",
                    "printf x >> \"$1\"; exit 1",
                    "ipmi-fixture",
                    calls.to_str().unwrap(),
                ])
                .attempts(configured_attempts);
            let error = super::execute_single_attempt(command, std::time::Duration::from_secs(5))
                .await
                .unwrap_err();
            assert!(matches!(
                super::classify_restart_error(error),
                crate::RestartError::Uncertain(_)
            ));
            assert_eq!(std::fs::read(calls).unwrap(), b"x");
        }
    }

    /// A missing executable proves non-delivery; unlike a failed child, it can clear
    /// the new attempt marker without losing an earlier attempt's restart evidence.
    #[tokio::test]
    async fn durable_reset_spawn_failure_is_not_sent() {
        let dir = tempfile::tempdir().unwrap();
        let command = carbide_utils::cmd::TokioCmd::new(dir.path().join("missing-ipmitool"));
        let error = super::execute_single_attempt(command, std::time::Duration::from_secs(5))
            .await
            .unwrap_err();
        assert!(matches!(
            super::classify_restart_error(error),
            crate::RestartError::NotSent(_)
        ));
    }

    /// Cancellation and the attempt deadline must stop/reap the actual local process.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn durable_reset_stops_the_process_when_its_attempt_ends() {
        use std::time::Duration;

        for cancel in [true, false] {
            // Eight seconds independently bounds this proof; the fixture exits naturally at 30.
            tokio::time::timeout(Duration::from_secs(8), async {
                let dir = tempfile::tempdir().unwrap();
                let pid_file = dir.path().join("pid");
                let command = carbide_utils::cmd::TokioCmd::new("/bin/sh").args([
                    "-c", "printf '%s' \"$$\" > \"$1\"; exec sleep 30",
                    "ipmi-fixture", pid_file.to_str().unwrap(),
                ]);
                let deadline = if cancel { Duration::from_secs(60) } else { Duration::from_secs(3) };
                let mut attempt = Box::pin(super::execute_single_attempt(command, deadline));
                let pid = tokio::select! {
                    // A delayed test poll can observe both the marker and deadline at once.
                    // Prefer the recorded start before judging how the command ended.
                    biased;
                    pid = async {
                        loop {
                            if let Ok(text) = std::fs::read_to_string(&pid_file)
                                && let Ok(pid) = text.parse::<u32>() {
                                break pid;
                            }
                            tokio::time::sleep(Duration::from_millis(10)).await;
                        }
                    } => pid,
                    result = &mut attempt => panic!("command ended before its start marker: {result:?}"),
                };
                // Cancel only after the real command has started. The other row exercises
                // the helper's own deadline rather than an outer test cancellation.
                if cancel {
                    drop(attempt);
                } else {
                    assert!(attempt.await.is_err());
                }
                while std::path::Path::new(&format!("/proc/{pid}")).exists() {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }).await.expect("the process must disappear before the fixture's natural exit");
        }
    }

    #[test]
    fn test_ipmitool_new() {
        let cp = Arc::new(TestCredentialManager::new(Credentials::UsernamePassword {
            username: "user".to_string(),
            password: "password".to_string(),
        }));
        let tool =
            super::IPMIToolImpl::new(cp, Some(1), carbide_instrument::Instrumentation::noop());

        assert_eq!(tool.attempts, 1);
    }

    #[test]
    fn connection_args_include_non_default_port() {
        assert_eq!(
            super::IPMIToolImpl::connection_args(
                "[2001:db8::1]:1623".parse::<SocketAddr>().unwrap(),
                "admin",
            ),
            ["-H", "2001:db8::1", "-p", "1623", "-U", "admin", "-E"]
        );
    }
}

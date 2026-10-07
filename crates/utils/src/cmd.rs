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
use std::ffi::OsStr;
use std::process::{Command, Stdio};
use std::time::Duration;

use chrono::Utc;
use tokio::process::Command as TokioCommand;
use tokio::time::timeout;

#[derive(thiserror::Error, Debug)]
pub enum CmdError {
    #[error("invalid retry value {0} for {1}")]
    InvalidRetry(u32, String),
    #[error("subprocess {0} with arguments {1:?} failed with output: {2}")]
    Subprocess(String, Vec<String>, String),
    #[error("command {0} with args {1:?} produced output that is not valid UTF8")]
    OutputParse(String, Vec<String>),
    #[error("error running '{0}': {1:#}")]
    RunError(String, String),
    #[error("error async running '{0}': {1:#}")]
    TokioRunError(String, String),
}

impl CmdError {
    pub fn subprocess_error(
        command: &std::process::Command,
        output: &std::process::Output,
    ) -> Self {
        let error_details = if output.stderr.is_empty() {
            String::from_utf8_lossy(&output.stdout).to_string()
        } else {
            String::from_utf8_lossy(&output.stderr).to_string()
        };

        Self::Subprocess(
            command.get_program().to_string_lossy().to_string(),
            command
                .get_args()
                .map(|arg| arg.to_string_lossy().to_string())
                .collect::<Vec<String>>(),
            error_details,
        )
    }
    pub fn output_parse_error(command: &Command) -> Self {
        Self::OutputParse(
            command.get_program().to_string_lossy().to_string(),
            command
                .get_args()
                .map(|arg| arg.to_string_lossy().to_string())
                .collect::<Vec<String>>(),
        )
    }
}

pub type CmdResult<T> = std::result::Result<T, CmdError>;

#[derive(Debug)]
pub struct Cmd {
    command: Command,
    attempts: u32,
    ignore_return: bool,
}

#[derive(Debug)]
pub struct CmdOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
    pub start_time: chrono::DateTime<Utc>,
    pub end_time: chrono::DateTime<Utc>,
}
impl Cmd {
    pub fn new<S: AsRef<OsStr>>(program: S) -> Self {
        Self {
            command: Command::new(program),
            attempts: 1,
            ignore_return: false,
        }
    }

    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.command.args(args);
        self
    }

    pub fn env<S>(mut self, key: S, value: S) -> Self
    where
        S: AsRef<OsStr>,
    {
        self.command.env(key, value);
        self
    }

    pub fn attempts(mut self, attempts: u32) -> Self {
        self.attempts = attempts;
        self
    }

    pub fn ignore_return(mut self, ignore: bool) -> Self {
        self.ignore_return = ignore;
        self
    }

    pub fn output(mut self) -> CmdResult<String> {
        if cfg!(test) {
            return Ok("test string".to_string());
        }

        let mut last_output = None;
        for _attempt in 0..self.attempts {
            let output = self
                .command
                .output()
                .map_err(|x| CmdError::RunError(self.pretty_cmd(), x.to_string()))?;

            last_output = Some(output.clone());

            if output.status.success() || self.ignore_return {
                return String::from_utf8(output.stdout)
                    .map_err(|_| CmdError::output_parse_error(&self.command));
            }

            // Give some breathing time.
            std::thread::sleep(Duration::from_millis(100));
        }
        if let Some(output) = last_output {
            Err(CmdError::subprocess_error(&self.command, &output))
        } else {
            Err(CmdError::InvalidRetry(self.attempts, self.pretty_cmd()))
        }
    }

    fn pretty_cmd(&self) -> String {
        format!(
            "{} {}",
            self.command.get_program().to_string_lossy(),
            self.command
                .get_args()
                .map(|x| x.to_string_lossy())
                .collect::<Vec<std::borrow::Cow<'_, str>>>()
                .join(" ")
        )
    }
}

/// Async implementation of Cmd.
#[derive(Debug)]
pub struct TokioCmd {
    command: TokioCommand,
    attempts: u32,
    timeout: u64,
}

impl TokioCmd {
    pub fn new<S: AsRef<OsStr>>(program: S) -> Self {
        Self {
            command: TokioCommand::new(program),
            attempts: 1,
            timeout: 3600,
        }
    }

    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.command.args(args);
        self
    }

    pub fn env<S>(mut self, key: S, value: S) -> Self
    where
        S: AsRef<OsStr>,
    {
        self.command.env(key, value);
        self
    }

    pub fn attempts(mut self, attempts: u32) -> Self {
        self.attempts = attempts;
        self
    }

    pub fn timeout(mut self, timeout: u64) -> Self {
        self.timeout = timeout;
        self
    }

    pub async fn output_with_timeout(mut self) -> CmdResult<CmdOutput> {
        if cfg!(test) {
            return Ok(CmdOutput {
                stdout: "test string".to_string(),
                stderr: "test string".to_string(),
                exit_code: 0,
                start_time: Utc::now(),
                end_time: Utc::now(),
            });
        }
        let mut last_output = None;
        let start_time = Utc::now();

        for _attempt in 0..self.attempts {
            let child = self
                .command
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .map_err(|e| CmdError::RunError(self.pretty_cmd(), e.to_string()))?;

            // Apply timeout and run command
            let output = timeout(Duration::from_secs(self.timeout), child.wait_with_output())
                .await
                .map_err(|x| CmdError::TokioRunError(self.pretty_cmd(), x.to_string()))?
                .map_err(|y| CmdError::TokioRunError(self.pretty_cmd(), y.to_string()))?;
            last_output = Some(output.clone());

            if output.status.success() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let end_time = Utc::now();
        // Here idea is to capture both std out and std err along with exit code
        if let Some(output) = last_output {
            Ok(CmdOutput {
                stdout: String::from_utf8(output.stdout)
                    .map_err(|_| CmdError::output_parse_error(self.command.as_std()))?,
                stderr: String::from_utf8(output.stderr)
                    .map_err(|_| CmdError::output_parse_error(self.command.as_std()))?,
                exit_code: output.status.code().unwrap_or_default(),
                start_time,
                end_time,
            })
        } else {
            Err(CmdError::InvalidRetry(self.attempts, self.pretty_cmd()))
        }
    }

    pub async fn output(mut self) -> CmdResult<String> {
        if cfg!(test) {
            return Ok("test string".to_string());
        }

        let mut last_output = None;
        for _attempt in 0..self.attempts {
            let output = self
                .command
                .output()
                .await
                .map_err(|x| CmdError::TokioRunError(self.pretty_cmd(), x.to_string()))?;

            last_output = Some(output.clone());

            if output.status.success() {
                return String::from_utf8(output.stdout)
                    .map_err(|_| CmdError::output_parse_error(self.command.as_std()));
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        if let Some(output) = last_output {
            Err(CmdError::subprocess_error(self.command.as_std(), &output))
        } else {
            Err(CmdError::InvalidRetry(self.attempts, self.pretty_cmd()))
        }
    }

    fn pretty_cmd(&self) -> String {
        let c = self.command.as_std();
        format!(
            "{} {}",
            c.get_program().to_string_lossy(),
            c.get_args()
                .map(|x| x.to_string_lossy())
                .collect::<Vec<std::borrow::Cow<'_, str>>>()
                .join(" ")
        )
    }
}

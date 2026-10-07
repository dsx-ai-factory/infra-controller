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

use eyre::WrapErr;

use crate::containerd::command::Command;
use crate::pretty_cmd;

const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(PartialEq, Eq, Clone, Debug)]
pub struct BashCommand {
    command: String,
    args: Vec<String>,
    output: Option<String>,
}

impl BashCommand {
    pub fn new(command: &str) -> Self {
        BashCommand {
            command: command.to_string(),
            args: Vec::new(),
            output: None,
        }
    }

    pub fn args(self, args: Vec<&str>) -> Self {
        BashCommand {
            command: self.command,
            args: args.iter().map(|x| x.to_string()).collect(),
            output: self.output,
        }
    }
}

#[async_trait::async_trait]
impl Command for BashCommand {
    async fn run(&mut self) -> eyre::Result<String> {
        let mut cmd = tokio::process::Command::new(&self.command);
        let fullcmd = cmd.args(&self.args);
        fullcmd.kill_on_drop(true);

        let cmd_str = pretty_cmd(fullcmd.as_std());

        let output = tokio::time::timeout(COMMAND_TIMEOUT, fullcmd.output())
            .await
            .wrap_err_with(|| format!("timeout while running command: {cmd_str:?}"))??;

        let fout = String::from_utf8_lossy(&output.stdout).to_string();
        Ok(fout)
    }
}

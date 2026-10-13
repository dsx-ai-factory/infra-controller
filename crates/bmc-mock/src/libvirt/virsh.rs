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

use std::path::Path;
use std::process::{Output, Stdio};

use eyre::WrapErr;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

use super::commands::{DomainConfiguration, VirshCommand};
use super::domain_xml::DomainXml;
use super::{VIRSH_CLEANUP_TIMEOUT, VIRSH_COMMAND_TIMEOUT};

/// Executes a prepared command against one libvirt connection and domain.
pub(super) struct Virsh<'a> {
    pub(super) path: &'a Path,
    pub(super) uri: &'a str,
    pub(super) domain: &'a str,
}

impl Virsh<'_> {
    /// Reads a domain configuration and returns its XML model.
    pub(super) async fn dumpxml(
        &self,
        configuration: DomainConfiguration,
    ) -> eyre::Result<DomainXml> {
        let output = self
            .execute(VirshCommand::DumpXml { configuration })
            .await?;
        let xml =
            String::from_utf8(output.stdout).context("virsh dumpxml returned invalid UTF-8")?;
        xml.parse()
    }

    /// Whether the domain currently has a live configuration.
    pub(super) async fn domain_active(&self) -> eyre::Result<bool> {
        let output = self.execute(VirshCommand::DomainState).await?;
        let state =
            String::from_utf8(output.stdout).context("virsh domstate returned invalid UTF-8")?;
        match state.trim() {
            "running" | "idle" | "blocked" | "paused" | "in shutdown" | "pmsuspended" => Ok(true),
            "shut off" | "crashed" => Ok(false),
            state => eyre::bail!("virsh domstate returned an unknown domain state: {state}"),
        }
    }

    pub(super) async fn execute(&self, command: VirshCommand) -> eyre::Result<Output> {
        let arguments = command.arguments(self.domain);
        let input = match command {
            VirshCommand::Define { xml } => Some(xml.into_xml()),
            VirshCommand::AttachDevice { xml, .. } | VirshCommand::UpdateDevice { xml, .. } => {
                Some(xml.into_xml()?)
            }
            _ => None,
        };
        let output = self.virsh_output(&arguments, input.as_deref()).await?;
        if output.status.success() {
            Ok(output)
        } else {
            Err(eyre::eyre!(
                "{} exited with {}: {}",
                self.path.display(),
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            ))
        }
    }

    async fn virsh_output(
        &self,
        arguments: &[std::ffi::OsString],
        input: Option<&str>,
    ) -> eyre::Result<Output> {
        let command = self.path.display().to_string();
        let mut child = Command::new(self.path)
            .env("LC_ALL", "C")
            .arg("--connect")
            .arg(self.uri)
            .args(arguments)
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("could not execute {command}"))?;
        let stdin = child.stdin.take();
        let mut stdout = child.stdout.take().expect("stdout is piped");
        let mut stderr = child.stderr.take().expect("stderr is piped");
        let mut stdout_bytes = Vec::new();
        let mut stderr_bytes = Vec::new();
        // Write stdin and drain both output pipes concurrently with waiting so
        // no full pipe can deadlock the child. The deadline covers all I/O.
        let completion = tokio::time::timeout(VIRSH_COMMAND_TIMEOUT, async {
            tokio::try_join!(
                async move {
                    if let Some(input) = input {
                        let mut stdin = stdin.expect("stdin is piped for XML commands");
                        stdin.write_all(input.as_bytes()).await?;
                        // Dropping the pipe signals EOF to virsh.
                    }
                    Ok::<_, std::io::Error>(())
                },
                child.wait(),
                stdout.read_to_end(&mut stdout_bytes),
                stderr.read_to_end(&mut stderr_bytes),
            )
        })
        .await;
        match completion {
            Ok(Ok((_, status, _, _))) => Ok(Output {
                status,
                stdout: stdout_bytes,
                stderr: stderr_bytes,
            }),
            failed => {
                let kill_result = child
                    .start_kill()
                    .with_context(|| format!("could not stop {command}"));
                // On timeout, returning drops Child and leaves reaping to Tokio's
                // best-effort cleanup so the actor can keep processing.
                // A failed kill request must not skip the bounded reap attempt.
                let cleanup_result =
                    tokio::time::timeout(VIRSH_CLEANUP_TIMEOUT, child.wait()).await;
                kill_result?;
                cleanup_result
                    .with_context(|| {
                        format!("could not reap {command} within {VIRSH_CLEANUP_TIMEOUT:?} after kill request")
                    })?
                    .with_context(|| format!("could not reap {command}"))?;
                Err(match failed {
                    Err(_) => eyre::eyre!("{command} timed out after {VIRSH_COMMAND_TIMEOUT:?}"),
                    Ok(Err(error)) => {
                        eyre::eyre!("could not complete {command} I/O: {error}")
                    }
                    Ok(Ok(_)) => unreachable!(),
                })
            }
        }
    }
}

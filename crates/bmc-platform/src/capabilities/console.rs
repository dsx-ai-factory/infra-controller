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

use std::num::NonZeroU16;

use async_trait::async_trait;
use nv_redfish::core::Bmc;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{DriverOutcome, OpCx, PlatformError};

/// A byte string guaranteed non-empty, for prompts, commands, and escape tails.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "Vec<u8>")]
pub struct NonEmptyBytes(Vec<u8>);

impl NonEmptyBytes {
    /// Validates that `value` has at least one byte.
    pub fn new(value: Vec<u8>) -> Result<Self, NonEmptyBytesError> {
        if value.is_empty() {
            return Err(NonEmptyBytesError);
        }
        Ok(Self(value))
    }

    /// Returns the bytes.
    pub fn as_slice(&self) -> &[u8] {
        &self.0
    }
}

impl TryFrom<Vec<u8>> for NonEmptyBytes {
    type Error = NonEmptyBytesError;

    fn try_from(value: Vec<u8>) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
#[error("byte sequence must not be empty")]
pub struct NonEmptyBytesError;

/// Bytes the console client must swallow so they do not reach the BMC shell.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum EscapeSeq {
    #[default]
    None,
    Single(u8),
    Pair {
        lead: u8,
        trailing: NonEmptyBytes,
    },
}

impl EscapeSeq {
    /// A two-part escape: `lead` followed by one of `trailing`; `trailing` must be non-empty.
    pub fn pair(lead: u8, trailing: Vec<u8>) -> Result<Self, NonEmptyBytesError> {
        Ok(Self::Pair {
            lead,
            trailing: trailing.try_into()?,
        })
    }
}

/// Whether the BIOS and BMC settings the console needs are in place.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConsoleState {
    /// Every checked setting has the console-enabled value.
    Enabled,
    /// Settings disagree, or some were not reported.
    Partial,
    /// Every checked setting has a console-disabled value.
    Disabled,
}

/// Console configuration state with the observed settings for diagnostics.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ConsoleStatus {
    pub state: ConsoleState,
    /// Human-readable `key=value` list of the settings that were checked.
    pub message: String,
}

/// Commands to run when the shell answers the activation command with `trigger`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "ConsoleFallbackWire")]
pub struct ConsoleFallback {
    trigger: NonEmptyBytes,
    commands: Vec<NonEmptyBytes>,
}

#[derive(Deserialize)]
struct ConsoleFallbackWire {
    trigger: Vec<u8>,
    commands: Vec<Vec<u8>>,
}

impl TryFrom<ConsoleFallbackWire> for ConsoleFallback {
    type Error = ConsoleSpecError;

    fn try_from(wire: ConsoleFallbackWire) -> Result<Self, Self::Error> {
        Self::new(wire.trigger, wire.commands)
    }
}

impl ConsoleFallback {
    /// Validates a fallback: a non-empty `trigger` and at least one non-empty command.
    pub fn new(trigger: Vec<u8>, commands: Vec<Vec<u8>>) -> Result<Self, ConsoleSpecError> {
        let trigger =
            NonEmptyBytes::new(trigger).map_err(|_| ConsoleSpecError::EmptyFallbackTrigger)?;
        if commands.is_empty() {
            return Err(ConsoleSpecError::NoFallbackCommands);
        }
        let commands = commands
            .into_iter()
            .map(|command| {
                NonEmptyBytes::new(command).map_err(|_| ConsoleSpecError::EmptyFallbackCommand)
            })
            .collect::<Result<_, _>>()?;
        Ok(Self { trigger, commands })
    }

    /// Shell output that means the activation command failed and the fallback applies.
    pub fn trigger(&self) -> &[u8] {
        self.trigger.as_slice()
    }

    /// Commands to send, in order, before retrying activation.
    pub fn commands(&self) -> impl ExactSizeIterator<Item = &[u8]> {
        self.commands.iter().map(NonEmptyBytes::as_slice)
    }
}

/// An SSH login that reaches the serial console through a BMC shell command.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SshShellSpec {
    /// SSH port of the BMC shell.
    pub port: NonZeroU16,
    /// Command typed at the shell prompt to attach to the serial console.
    pub activate: NonEmptyBytes,
    /// Recovery when `activate` fails because a stale session holds the console.
    pub fallback: Option<ConsoleFallback>,
    /// Shell prompt to wait for before sending `activate`.
    pub prompt: NonEmptyBytes,
    /// Bytes to swallow so they are not forwarded to the console.
    pub escape_filter: EscapeSeq,
}

/// How a console client reaches the host serial console.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ConsoleSpec {
    SshShell(SshShellSpec),
    /// SSH login lands on the console directly.
    SshDirect {
        port: NonZeroU16,
    },
    IpmiSol {
        port: NonZeroU16,
        escape_filter: EscapeSeq,
    },
    /// No console transport is known for this platform.
    None {
        reason: String,
    },
}

impl ConsoleSpec {
    /// Builds an [`SshShellSpec`]; `activate` and `prompt` must be non-empty.
    pub fn ssh_shell(
        port: NonZeroU16,
        activate: Vec<u8>,
        fallback: Option<ConsoleFallback>,
        prompt: Vec<u8>,
        escape_filter: EscapeSeq,
    ) -> Result<Self, ConsoleSpecError> {
        Ok(Self::SshShell(SshShellSpec {
            port,
            activate: NonEmptyBytes::new(activate)
                .map_err(|_| ConsoleSpecError::EmptyActivateCommand)?,
            fallback,
            prompt: NonEmptyBytes::new(prompt).map_err(|_| ConsoleSpecError::EmptyPrompt)?,
            escape_filter,
        }))
    }

    /// Returns the shell spec when this console is reached through a BMC shell.
    pub const fn as_ssh_shell(&self) -> Option<&SshShellSpec> {
        if let Self::SshShell(spec) = self {
            Some(spec)
        } else {
            None
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum ConsoleSpecError {
    #[error("SSH shell activation command must not be empty")]
    EmptyActivateCommand,
    #[error("SSH shell prompt must not be empty")]
    EmptyPrompt,
    #[error("fallback trigger must not be empty")]
    EmptyFallbackTrigger,
    #[error("fallback must contain at least one command")]
    NoFallbackCommands,
    #[error("fallback commands must not be empty")]
    EmptyFallbackCommand,
}

#[async_trait]
pub trait Console<B: Bmc>: Send + Sync {
    async fn setup(&self, cx: &OpCx<'_, B>) -> Result<DriverOutcome, PlatformError>;

    async fn status(&self, cx: &OpCx<'_, B>) -> Result<ConsoleStatus, PlatformError>;

    async fn spec(&self, cx: &OpCx<'_, B>) -> Result<ConsoleSpec, PlatformError>;
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU16;

    use serde_json::json;

    use super::*;

    fn port(value: u16) -> NonZeroU16 {
        NonZeroU16::new(value).expect("fixture port is nonzero")
    }

    #[test]
    fn console_constructors_reject_empty_values() {
        let fallback = ConsoleFallback::new(
            b"extraneous arguments".to_vec(),
            vec![b"console kill".to_vec(), b"console start".to_vec()],
        )
        .expect("fallback is valid");
        let spec = ConsoleSpec::ssh_shell(
            port(22),
            b"console".to_vec(),
            Some(fallback),
            b"> ".to_vec(),
            EscapeSeq::pair(0x1b, vec![b'(']).expect("escape pair is valid"),
        )
        .expect("SSH shell spec is valid");
        let shell = spec.as_ssh_shell().expect("spec is SSH shell");
        assert_eq!(shell.activate.as_slice(), b"console");
        assert_eq!(
            shell
                .fallback
                .as_ref()
                .expect("fallback is configured")
                .commands()
                .collect::<Vec<_>>(),
            vec![b"console kill".as_slice(), b"console start".as_slice()]
        );
        assert!(
            ConsoleSpec::ssh_shell(port(22), Vec::new(), None, b"> ".to_vec(), EscapeSeq::None)
                .is_err()
        );
        assert!(ConsoleFallback::new(b"error".to_vec(), Vec::new()).is_err());
        assert!(EscapeSeq::pair(b'~', Vec::new()).is_err());
    }

    #[test]
    fn console_specs_round_trip_and_reject_invalid_wire_values() {
        let specs = [
            ConsoleSpec::ssh_shell(
                port(22),
                b"console".to_vec(),
                Some(
                    ConsoleFallback::new(b"error".to_vec(), vec![b"console kill".to_vec()])
                        .expect("fallback is valid"),
                ),
                b"> ".to_vec(),
                EscapeSeq::None,
            )
            .expect("SSH shell spec is valid"),
            ConsoleSpec::SshDirect { port: port(2200) },
            ConsoleSpec::IpmiSol {
                port: port(623),
                escape_filter: EscapeSeq::pair(b'~', vec![b'.', b'B'])
                    .expect("escape pair is valid"),
            },
            ConsoleSpec::None {
                reason: "serial console is unavailable".to_string(),
            },
        ];
        for spec in specs {
            let encoded = serde_json::to_value(&spec).expect("console spec serializes");
            assert_eq!(
                serde_json::from_value::<ConsoleSpec>(encoded).expect("spec deserializes"),
                spec
            );
        }
        let invalid = [
            json!({
                "type": "ssh_shell",
                "port": 22,
                "activate": [],
                "fallback": null,
                "prompt": [62, 32],
                "escape_filter": {"type": "none"}
            }),
            json!({
                "type": "ssh_shell",
                "port": 22,
                "activate": [99],
                "fallback": {"trigger": [1], "commands": []},
                "prompt": [62],
                "escape_filter": {"type": "none"}
            }),
            json!({
                "type": "ipmi_sol",
                "port": 623,
                "escape_filter": {
                    "type": "pair",
                    "value": {"lead": 126, "trailing": []}
                }
            }),
            json!({"type": "ssh_direct", "port": 0}),
        ];
        for value in invalid {
            assert!(serde_json::from_value::<ConsoleSpec>(value).is_err());
        }
    }
}

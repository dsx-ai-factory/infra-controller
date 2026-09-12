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
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

use crate::{DriverOutcome, OpCx, PlatformError};

/// A non-empty owned byte sequence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NonEmptyBytes(Vec<u8>);

impl NonEmptyBytes {
    pub fn new(value: Vec<u8>) -> Result<Self, NonEmptyBytesError> {
        if value.is_empty() {
            return Err(NonEmptyBytesError);
        }
        Ok(Self(value))
    }

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

impl Serialize for NonEmptyBytes {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.0.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for NonEmptyBytes {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Vec::<u8>::deserialize(deserializer)?
            .try_into()
            .map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
#[error("byte sequence must not be empty")]
pub struct NonEmptyBytesError;

/// Client-input bytes reserved by a console transport.
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
    pub fn pair(lead: u8, trailing: Vec<u8>) -> Result<Self, NonEmptyBytesError> {
        Ok(Self::Pair {
            lead,
            trailing: trailing.try_into()?,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConsoleState {
    Enabled,
    Partial,
    Disabled,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ConsoleStatus {
    pub state: ConsoleState,
    pub message: String,
}

/// Ordered recovery commands for a failed SSH-shell activation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ConsoleFallback {
    trigger: NonEmptyBytes,
    commands: Vec<NonEmptyBytes>,
}

impl ConsoleFallback {
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

    pub fn trigger(&self) -> &[u8] {
        self.trigger.as_slice()
    }

    pub fn commands(&self) -> impl ExactSizeIterator<Item = &[u8]> {
        self.commands.iter().map(NonEmptyBytes::as_slice)
    }
}

#[derive(Deserialize)]
struct ConsoleFallbackWire {
    trigger: Vec<u8>,
    commands: Vec<Vec<u8>>,
}

impl<'de> Deserialize<'de> for ConsoleFallback {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = ConsoleFallbackWire::deserialize(deserializer)?;
        Self::new(wire.trigger, wire.commands).map_err(serde::de::Error::custom)
    }
}

/// Validated instructions for activating SOL from an interactive SSH shell.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SshShellSpec {
    port: NonZeroU16,
    activate: NonEmptyBytes,
    fallback: Option<ConsoleFallback>,
    prompt: NonEmptyBytes,
    escape_filter: EscapeSeq,
}

impl SshShellSpec {
    pub fn new(
        port: NonZeroU16,
        activate: Vec<u8>,
        fallback: Option<ConsoleFallback>,
        prompt: Vec<u8>,
        escape_filter: EscapeSeq,
    ) -> Result<Self, ConsoleSpecError> {
        let activate =
            NonEmptyBytes::new(activate).map_err(|_| ConsoleSpecError::EmptyActivateCommand)?;
        let prompt = NonEmptyBytes::new(prompt).map_err(|_| ConsoleSpecError::EmptyPrompt)?;
        Ok(Self {
            port,
            activate,
            fallback,
            prompt,
            escape_filter,
        })
    }

    pub const fn port(&self) -> NonZeroU16 {
        self.port
    }

    pub fn activate(&self) -> &[u8] {
        self.activate.as_slice()
    }

    pub const fn fallback(&self) -> Option<&ConsoleFallback> {
        self.fallback.as_ref()
    }

    pub fn prompt(&self) -> &[u8] {
        self.prompt.as_slice()
    }

    pub const fn escape_filter(&self) -> &EscapeSeq {
        &self.escape_filter
    }
}

/// Validated instructions for using an SSH shell as the console directly.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SshDirectSpec {
    port: NonZeroU16,
}

impl SshDirectSpec {
    pub const fn new(port: NonZeroU16) -> Self {
        Self { port }
    }

    pub const fn port(&self) -> NonZeroU16 {
        self.port
    }
}

/// Validated instructions for starting an IPMI SOL session.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct IpmiSolSpec {
    port: NonZeroU16,
    escape_filter: EscapeSeq,
}

impl IpmiSolSpec {
    pub const fn new(port: NonZeroU16, escape_filter: EscapeSeq) -> Self {
        Self {
            port,
            escape_filter,
        }
    }

    pub const fn port(&self) -> NonZeroU16 {
        self.port
    }

    pub const fn escape_filter(&self) -> &EscapeSeq {
        &self.escape_filter
    }
}

/// Validated explanation for an endpoint without a console transport.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct UnsupportedConsoleSpec {
    reason: String,
}

impl UnsupportedConsoleSpec {
    pub fn new(reason: String) -> Result<Self, ConsoleSpecError> {
        if reason.trim().is_empty() {
            return Err(ConsoleSpecError::EmptyUnsupportedReason);
        }
        Ok(Self { reason })
    }

    pub fn reason(&self) -> &str {
        &self.reason
    }
}

impl<'de> Deserialize<'de> for UnsupportedConsoleSpec {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Wire {
            reason: String,
        }

        Self::new(Wire::deserialize(deserializer)?.reason).map_err(serde::de::Error::custom)
    }
}

/// Complete, validated instructions for opening a BMC-backed console.
///
/// Ports are final connection ports after platform-specific discovery and have
/// no implicit default.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ConsoleSpec {
    SshShell(SshShellSpec),
    SshDirect(SshDirectSpec),
    IpmiSol(IpmiSolSpec),
    None(UnsupportedConsoleSpec),
}

impl ConsoleSpec {
    pub fn ssh_shell(
        port: NonZeroU16,
        activate: Vec<u8>,
        fallback: Option<ConsoleFallback>,
        prompt: Vec<u8>,
        escape_filter: EscapeSeq,
    ) -> Result<Self, ConsoleSpecError> {
        SshShellSpec::new(port, activate, fallback, prompt, escape_filter).map(Self::SshShell)
    }

    pub const fn ssh_direct(port: NonZeroU16) -> Self {
        Self::SshDirect(SshDirectSpec::new(port))
    }

    pub const fn ipmi_sol(port: NonZeroU16, escape_filter: EscapeSeq) -> Self {
        Self::IpmiSol(IpmiSolSpec::new(port, escape_filter))
    }

    pub fn unsupported(reason: String) -> Result<Self, ConsoleSpecError> {
        UnsupportedConsoleSpec::new(reason).map(Self::None)
    }

    pub const fn as_ssh_shell(&self) -> Option<&SshShellSpec> {
        if let Self::SshShell(spec) = self {
            Some(spec)
        } else {
            None
        }
    }

    pub const fn as_ssh_direct(&self) -> Option<&SshDirectSpec> {
        if let Self::SshDirect(spec) = self {
            Some(spec)
        } else {
            None
        }
    }

    pub const fn as_ipmi_sol(&self) -> Option<&IpmiSolSpec> {
        if let Self::IpmiSol(spec) = self {
            Some(spec)
        } else {
            None
        }
    }

    pub const fn as_unsupported(&self) -> Option<&UnsupportedConsoleSpec> {
        if let Self::None(spec) = self {
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
    #[error("unsupported console reason must not be empty")]
    EmptyUnsupportedReason,
}

/// Resolves the console protocol and activation sequence for a BMC.
#[async_trait]
pub trait Console<B: Bmc>: Send + Sync {
    async fn setup(&self, cx: &OpCx<'_, B>) -> Result<DriverOutcome, PlatformError>;

    async fn status(&self, cx: &OpCx<'_, B>) -> Result<ConsoleStatus, PlatformError>;

    /// Returns a validated connection specification.
    ///
    /// Drivers use [`ConsoleSpec`] constructors. Invalid persisted wire data
    /// fails deserialization before a consumer can attempt a connection.
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
    fn console_constructors_expose_validated_values() {
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

        assert_eq!(shell.port(), port(22));
        assert_eq!(shell.activate(), b"console");
        assert_eq!(shell.prompt(), b"> ");
        let fallback = shell.fallback().expect("fallback is configured");
        assert_eq!(fallback.trigger(), b"extraneous arguments");
        assert_eq!(
            fallback.commands().collect::<Vec<_>>(),
            vec![b"console kill".as_slice(), b"console start".as_slice()]
        );
        assert!(
            ConsoleSpec::ssh_shell(port(22), Vec::new(), None, b"> ".to_vec(), EscapeSeq::None)
                .is_err()
        );
        assert!(ConsoleFallback::new(b"error".to_vec(), Vec::new()).is_err());
        assert!(EscapeSeq::pair(b'~', Vec::new()).is_err());
        assert!(ConsoleSpec::unsupported(" \t".to_string()).is_err());
    }

    #[test]
    fn console_specs_exhaustively_round_trip() {
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
            ConsoleSpec::ssh_direct(port(2200)),
            ConsoleSpec::ipmi_sol(
                port(623),
                EscapeSeq::pair(b'~', vec![b'.', b'B']).expect("escape pair is valid"),
            ),
            ConsoleSpec::unsupported("serial console is unavailable".to_string())
                .expect("unsupported reason is valid"),
        ];

        for spec in specs {
            let encoded = serde_json::to_value(&spec).expect("console spec serializes");
            assert_eq!(
                serde_json::from_value::<ConsoleSpec>(encoded).expect("spec deserializes"),
                spec
            );
        }
    }

    #[test]
    fn invalid_console_json_is_rejected() {
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
            json!({"type": "none", "reason": " "}),
        ];

        for value in invalid {
            assert!(serde_json::from_value::<ConsoleSpec>(value).is_err());
        }
    }
}

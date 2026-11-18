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

use std::ffi::OsString;

use super::device_xml::DeviceXml;
use super::domain_xml::DomainXml;
use crate::{ActionError, ResourceResetType};

/// A virsh operation as data. Preparing commands never performs I/O.
/// XML-bearing variants retain their contents for the executor to pipe to stdin.
#[derive(Debug, Eq, PartialEq)]
pub(super) enum VirshCommand {
    DomainState,
    DumpXml {
        configuration: DomainConfiguration,
    },
    Define {
        xml: DomainXml,
    },
    AttachDevice {
        xml: DeviceXml,
        configuration: DeviceConfiguration,
    },
    UpdateDevice {
        xml: DeviceXml,
        configuration: DeviceConfiguration,
    },
    Start,
    Shutdown,
    Destroy,
    Reboot,
    Reset,
    Suspend,
    Resume,
    InjectNmi,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum DomainConfiguration {
    Persistent,
    Live,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum DeviceConfiguration {
    Persistent,
    Live,
    LiveAndPersistent,
}

impl DeviceConfiguration {
    fn flags(self) -> &'static [&'static str] {
        match self {
            Self::Persistent => &["--config"],
            Self::Live => &["--live"],
            Self::LiveAndPersistent => &["--live", "--config"],
        }
    }
}

impl VirshCommand {
    pub(super) fn boot_order(domain: DomainXml, devices: &[&str]) -> eyre::Result<Self> {
        Ok(Self::Define {
            xml: domain.with_boot_order(devices)?,
        })
    }

    pub(super) fn power(reset_type: ResourceResetType) -> Result<Vec<Self>, ActionError> {
        use ResourceResetType::*;
        Ok(match reset_type {
            On | ForceOn => vec![Self::Start],
            GracefulShutdown => vec![Self::Shutdown],
            ForceOff => vec![Self::Destroy],
            GracefulRestart => vec![Self::Reboot],
            ForceRestart => vec![Self::Reset],
            PowerCycle | FullPowerCycle => vec![Self::Destroy, Self::Start],
            Pause => vec![Self::Suspend],
            Resume => vec![Self::Resume],
            Nmi => vec![Self::InjectNmi],
            Sleep | Hibernate | PushPowerButton | Suspend | UnsupportedValue => {
                return Err(ActionError::BadRequest(eyre::eyre!(
                    "libvirt backend does not support {reset_type:?}"
                )));
            }
        })
    }

    pub(super) fn arguments(&self, domain: &str) -> Vec<OsString> {
        let command = match self {
            Self::DomainState => "domstate",
            Self::DumpXml { .. } => "dumpxml",
            Self::Define { .. } => "define",
            Self::AttachDevice { .. } => "attach-device",
            Self::UpdateDevice { .. } => "update-device",
            Self::Start => "start",
            Self::Shutdown => "shutdown",
            Self::Destroy => "destroy",
            Self::Reboot => "reboot",
            Self::Reset => "reset",
            Self::Suspend => "suspend",
            Self::Resume => "resume",
            Self::InjectNmi => "inject-nmi",
        };
        let mut arguments = vec![OsString::from(command)];
        match self {
            Self::Define { .. } => arguments.push("/dev/stdin".into()),
            Self::AttachDevice { configuration, .. } | Self::UpdateDevice { configuration, .. } => {
                arguments.extend([domain.into(), "/dev/stdin".into()]);
                arguments.extend(configuration.flags().iter().map(OsString::from));
            }
            Self::DumpXml { configuration } => {
                if *configuration == DomainConfiguration::Persistent {
                    arguments.push("--inactive".into());
                }
                arguments.push(domain.into());
            }
            Self::DomainState
            | Self::Start
            | Self::Shutdown
            | Self::Destroy
            | Self::Reboot
            | Self::Reset
            | Self::Suspend
            | Self::Resume
            | Self::InjectNmi => {
                arguments.push(domain.into());
            }
        }
        arguments
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selects_saved_and_live_configurations_for_xml_commands() {
        let device =
            || DeviceXml::virtual_media("Cd", "sdb", &crate::VirtualMediaContents::Empty).unwrap();
        for (command, expected) in [
            (
                VirshCommand::DumpXml {
                    configuration: DomainConfiguration::Persistent,
                },
                vec!["dumpxml", "--inactive", "test-domain"],
            ),
            (
                VirshCommand::DumpXml {
                    configuration: DomainConfiguration::Live,
                },
                vec!["dumpxml", "test-domain"],
            ),
            (
                VirshCommand::Define {
                    xml: "<domain/>".parse().unwrap(),
                },
                vec!["define", "/dev/stdin"],
            ),
            (
                VirshCommand::AttachDevice {
                    xml: device(),
                    configuration: DeviceConfiguration::Live,
                },
                vec!["attach-device", "test-domain", "/dev/stdin", "--live"],
            ),
            (
                VirshCommand::UpdateDevice {
                    xml: device(),
                    configuration: DeviceConfiguration::Persistent,
                },
                vec!["update-device", "test-domain", "/dev/stdin", "--config"],
            ),
            (
                VirshCommand::UpdateDevice {
                    xml: device(),
                    configuration: DeviceConfiguration::LiveAndPersistent,
                },
                vec![
                    "update-device",
                    "test-domain",
                    "/dev/stdin",
                    "--live",
                    "--config",
                ],
            ),
        ] {
            assert_eq!(
                command.arguments("test-domain"),
                expected.into_iter().map(OsString::from).collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn power_cycles_stop_before_starting_and_restarts_keep_the_domain_running() {
        let cases = [
            (
                "cold cycle",
                ResourceResetType::PowerCycle,
                vec![VirshCommand::Destroy, VirshCommand::Start],
            ),
            (
                "full cold cycle",
                ResourceResetType::FullPowerCycle,
                vec![VirshCommand::Destroy, VirshCommand::Start],
            ),
            (
                "graceful restart",
                ResourceResetType::GracefulRestart,
                vec![VirshCommand::Reboot],
            ),
            (
                "forced restart",
                ResourceResetType::ForceRestart,
                vec![VirshCommand::Reset],
            ),
        ];
        for (name, reset_type, expected) in cases {
            assert_eq!(VirshCommand::power(reset_type).unwrap(), expected, "{name}");
        }
    }
}

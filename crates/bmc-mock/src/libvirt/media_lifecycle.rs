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

use std::collections::BTreeMap;

use super::commands::{DeviceConfiguration, DomainConfiguration, VirshCommand};
use super::device_xml::DeviceXml;
use super::domain_xml::DomainDisk;
use super::virsh::Virsh;
use crate::{ActionError, VirtualMediaContents};

/// Creates owned CD-ROM devices at startup and changes their media in place.
/// Redfish publication remains with the actor after all commands succeed.
pub(super) struct MediaLifecycle<'a> {
    virsh: Virsh<'a>,
    targets: &'a BTreeMap<String, String>,
}

impl<'a> MediaLifecycle<'a> {
    pub(super) fn new(virsh: Virsh<'a>, targets: &'a BTreeMap<String, String>) -> Self {
        Self { virsh, targets }
    }

    fn device(&self, id: &'a str) -> Result<MediaDevice<'a>, ActionError> {
        let target = self.targets.get(id).ok_or_else(|| {
            ActionError::BadRequest(eyre::eyre!(
                "virtual media device {} has no libvirt target",
                id
            ))
        })?;
        Ok(MediaDevice { id, target })
    }

    async fn presence(&self, device: &MediaDevice<'_>) -> eyre::Result<MediaDevicePresence> {
        let persistent = self
            .virsh
            .dumpxml(DomainConfiguration::Persistent)
            .await?
            .disks()?;
        let live = if self.virsh.domain_active().await? {
            Some(
                self.virsh
                    .dumpxml(DomainConfiguration::Live)
                    .await?
                    .disks()?,
            )
        } else {
            None
        };
        device.presence(&persistent, live.as_deref())
    }

    /// Ensures both configurations have the owned device before serving requests.
    /// Existing devices keep their current media; only missing devices are created.
    pub(super) async fn ensure_device(&self, device_id: &'a str) -> eyre::Result<()> {
        let device = self.device(device_id)?;
        let presence = self.presence(&device).await?;
        if let Some(command) = device.create_command(&presence)? {
            self.virsh.execute(command).await?;
        }
        Ok(())
    }

    pub(super) async fn apply(
        &self,
        device_id: &'a str,
        contents: &VirtualMediaContents,
    ) -> Result<(), ActionError> {
        let device = self.device(device_id)?;
        let xml = DeviceXml::virtual_media(device.id, device.target, contents)
            .map_err(ActionError::Internal)?;
        let presence = self
            .presence(&device)
            .await
            .map_err(ActionError::Internal)?;
        let command = VirshCommand::UpdateDevice {
            xml,
            configuration: presence
                .update_configuration()
                .map_err(ActionError::Internal)?,
        };
        self.virsh
            .execute(command)
            .await
            .map_err(ActionError::Internal)?;
        Ok(())
    }
}

/// A configured media device. Command planning and ownership checks perform no I/O.
struct MediaDevice<'a> {
    id: &'a str,
    target: &'a str,
}

impl MediaDevice<'_> {
    fn owned_presence(&self, disks: &[DomainDisk], configuration: &str) -> eyre::Result<bool> {
        let mut matches = disks.iter().filter(|disk| {
            disk.target
                .as_ref()
                .and_then(|target| target.dev.as_deref())
                == Some(self.target)
        });
        let Some(disk) = matches.next() else {
            return Ok(false);
        };
        eyre::ensure!(
            matches.next().is_none(),
            "libvirt target {} is ambiguous in {configuration} configuration",
            self.target
        );
        let device = disk.device.as_deref();
        let bus = disk
            .target
            .as_ref()
            .and_then(|target| target.bus.as_deref());
        let alias = disk.alias.as_ref().and_then(|alias| alias.name.as_deref());
        let expected_alias = DeviceXml::media_alias(self.id);
        eyre::ensure!(
            device == Some("cdrom")
                && bus == Some("sata")
                && alias == Some(expected_alias.as_str()),
            "libvirt target {} in {configuration} configuration is used by an unowned device (device={}, bus={}, alias={})",
            self.target,
            device.unwrap_or("unknown"),
            bus.unwrap_or("unknown"),
            alias.unwrap_or("none")
        );
        Ok(true)
    }

    fn presence(
        &self,
        persistent: &[DomainDisk],
        live: Option<&[DomainDisk]>,
    ) -> eyre::Result<MediaDevicePresence> {
        Ok(MediaDevicePresence {
            persistent: self.owned_presence(persistent, "persistent")?,
            live: live
                .map(|disks| self.owned_presence(disks, "live"))
                .transpose()?,
        })
    }

    fn create_command(&self, presence: &MediaDevicePresence) -> eyre::Result<Option<VirshCommand>> {
        presence
            .attach_configuration()
            .map(|configuration| {
                Ok(VirshCommand::AttachDevice {
                    xml: DeviceXml::virtual_media(
                        self.id,
                        self.target,
                        &VirtualMediaContents::Empty,
                    )?,
                    configuration,
                })
            })
            .transpose()
    }
}

// Constructed only after ownership checks in both configurations succeed.
struct MediaDevicePresence {
    persistent: bool,
    live: Option<bool>,
}

impl MediaDevicePresence {
    fn attach_configuration(&self) -> Option<DeviceConfiguration> {
        match (self.persistent, self.live) {
            (false, Some(false)) => Some(DeviceConfiguration::LiveAndPersistent),
            (true, Some(false)) => Some(DeviceConfiguration::Live),
            (false, _) => Some(DeviceConfiguration::Persistent),
            (true, _) => None,
        }
    }

    fn update_configuration(&self) -> eyre::Result<DeviceConfiguration> {
        eyre::ensure!(
            self.persistent,
            "persistent virtual-media CD-ROM is missing"
        );
        eyre::ensure!(
            self.live != Some(false),
            "live virtual-media CD-ROM is missing"
        );
        Ok(if self.live.is_some() {
            DeviceConfiguration::LiveAndPersistent
        } else {
            DeviceConfiguration::Persistent
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::domain_xml::DomainXml;
    use super::*;

    fn disks(xml: &str) -> Vec<DomainDisk> {
        xml.parse::<DomainXml>().unwrap().disks().unwrap()
    }

    #[test]
    fn creates_only_missing_devices_and_updates_existing_configurations() {
        use DeviceConfiguration::{Live, LiveAndPersistent, Persistent};
        let device = MediaDevice {
            id: "Cd",
            target: "sdb",
        };
        let owned = disks(
            "<domain><devices><disk device='cdrom'><target dev='sdb' bus='sata'/><alias name='ua-bmc-mock-vmedia-Cd'/></disk></devices></domain>",
        );
        for (name, persistent, live, attach, update) in [
            ("create stopped", &[][..], None, Some(Persistent), None),
            (
                "create running",
                &[][..],
                Some(&[][..]),
                Some(LiveAndPersistent),
                None,
            ),
            (
                "repair live",
                owned.as_slice(),
                Some(&[][..]),
                Some(Live),
                None,
            ),
            (
                "repair persistent",
                &[][..],
                Some(owned.as_slice()),
                Some(Persistent),
                None,
            ),
            (
                "update stopped",
                owned.as_slice(),
                None,
                None,
                Some(Persistent),
            ),
            (
                "update running",
                owned.as_slice(),
                Some(owned.as_slice()),
                None,
                Some(LiveAndPersistent),
            ),
        ] {
            let presence = device.presence(persistent, live).unwrap();
            let command = device.create_command(&presence).unwrap();
            assert_eq!(
                command,
                attach.map(|configuration| VirshCommand::AttachDevice {
                    xml: DeviceXml::virtual_media("Cd", "sdb", &VirtualMediaContents::Empty)
                        .unwrap(),
                    configuration,
                }),
                "{name}"
            );
            assert_eq!(presence.update_configuration().ok(), update, "{name}");
        }
    }

    #[test]
    fn rejects_foreign_or_ambiguous_targets_before_planning_changes() {
        let device = MediaDevice {
            id: "Cd",
            target: "sdb",
        };
        for (kind, bus, alias) in [
            ("disk", "sata", "ua-bmc-mock-vmedia-Cd"),
            ("cdrom", "scsi", "ua-bmc-mock-vmedia-Cd"),
            ("cdrom", "sata", "ua-foreign"),
            ("cdrom", "sata", ""),
        ] {
            let foreign = disks(&format!(
                "<domain><devices><disk device='{kind}'><target dev='sdb' bus='{bus}'/><alias name='{alias}'/></disk></devices></domain>"
            ));
            for (persistent, live) in [
                (foreign.as_slice(), None),
                (&[][..], Some(foreign.as_slice())),
            ] {
                assert!(
                    device.presence(persistent, live).is_err(),
                    "{kind}, {bus}, {alias}"
                );
            }
        }
        let duplicate = disks(
            "<domain><devices><disk device='cdrom'><target dev='sdb' bus='sata'/><alias name='ua-bmc-mock-vmedia-Cd'/></disk><disk device='cdrom'><target dev='sdb' bus='sata'/><alias name='ua-bmc-mock-vmedia-Cd'/></disk></devices></domain>",
        );
        assert!(device.presence(&duplicate, None).is_err());
    }
}

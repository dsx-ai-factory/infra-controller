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

use super::block_list::BlockList;
use super::commands::VirshCommand;
use super::device_xml::DeviceXml;
use super::virsh::Virsh;
use crate::{ActionError, VirtualMediaContents};

/// Applies media changes to configured libvirt targets. Redfish publication
/// remains with the owning actor after all commands have succeeded.
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

    pub(super) async fn apply(
        &self,
        device_id: &'a str,
        contents: &VirtualMediaContents,
    ) -> Result<(), ActionError> {
        let device = self.device(device_id)?;
        let listing = self
            .virsh
            .block_list()
            .await
            .map_err(ActionError::Internal)?;
        if let Some(command) = device.detach_command(&listing) {
            self.virsh
                .execute(command)
                .await
                .map_err(ActionError::Internal)?;
        }
        // Preserve the existing order: detach precedes validation and attachment
        // of new media. Empty contents only detach the current device.
        if let Some(command) = device
            .attach_command(contents)
            .map_err(ActionError::Internal)?
        {
            self.virsh
                .execute(command)
                .await
                .map_err(ActionError::Internal)?;
        }
        Ok(())
    }
}

/// A configured media device. Command planning performs no I/O.
struct MediaDevice<'a> {
    id: &'a str,
    target: &'a str,
}

impl MediaDevice<'_> {
    fn detach_command(&self, listing: &BlockList) -> Option<VirshCommand> {
        listing
            .targets
            .contains(self.target)
            .then(|| VirshCommand::DetachDisk {
                target: self.target.to_owned(),
            })
    }

    fn attach_command(
        &self,
        contents: &VirtualMediaContents,
    ) -> eyre::Result<Option<VirshCommand>> {
        match contents {
            VirtualMediaContents::Empty => Ok(None),
            VirtualMediaContents::Inserted {
                image,
                write_protected,
            } => Ok(Some(VirshCommand::AttachDevice {
                xml: DeviceXml::virtual_media(self.id, self.target, image, *write_protected)?,
            })),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detach_only_plans_a_command_when_the_requested_target_is_present() {
        let listing = BlockList {
            targets: ["sdb", "vda"].map(str::to_owned).into(),
        };
        let device = |target| MediaDevice { id: "Cd", target };
        assert_eq!(
            device("sdb").detach_command(&listing),
            Some(VirshCommand::DetachDisk {
                target: "sdb".to_owned()
            })
        );
        assert_eq!(device("sdc").detach_command(&listing), None);
    }
}

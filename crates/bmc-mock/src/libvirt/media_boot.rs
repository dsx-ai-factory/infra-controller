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

//! Media identity and one-time media boot through vmctl's BMC virsh shim.
//!
//! vmctl reports an inserted medium's opaque identity as a `vm-media://ID` source in
//! `domblklist --details` and adds `boot-selection` and `boot-once-media` commands.
//! A libvirt connection that reports ordinary sources exposes no identity, so the
//! media boot action is unavailable there. Deciding what to run performs no I/O.

use crate::ActionError;

const MEDIA_SOURCE_PREFIX: &str = "vm-media://";

/// Returns the identity of the medium attached at `target`.
///
/// `Ok(None)` means the target is absent or its source is not a vmctl medium.
pub(super) fn media_identity(
    block_list: &str,
    target: &str,
) -> Result<Option<String>, ActionError> {
    let mut identity = None;
    for line in block_list.lines() {
        let fields: Vec<_> = line.split_whitespace().collect();
        let [_, _, line_target, source] = fields[..] else {
            continue;
        };
        if line_target != target {
            continue;
        }
        let Some(id) = source.strip_prefix(MEDIA_SOURCE_PREFIX) else {
            continue;
        };
        if id.is_empty() {
            return Err(ActionError::BadRequest(eyre::eyre!(
                "backend reported an empty media identity for {target}"
            )));
        }
        if identity.replace(id.to_owned()).is_some() {
            return Err(ActionError::BadRequest(eyre::eyre!(
                "multiple media identities match {target}"
            )));
        }
    }
    Ok(identity)
}

/// What to do for a media boot request, given the shim's current boot selection.
#[derive(Debug, Eq, PartialEq)]
pub(super) enum MediaBootPlan {
    /// The same medium is already reserved or its boot has started: do nothing.
    AlreadyAccepted,
    /// Select the medium and start or reset the target. Cancel a pending network
    /// selection first so it cannot run after the media boot.
    Arm { cancel_pending_network: bool },
}

/// Plans a media boot without replaying an accepted one.
///
/// A started attempt for the same medium is never started again. An attempt in an
/// uncertain state, or a pending selection of another medium, needs inspection.
pub(super) fn plan_media_boot(
    selection: &serde_json::Value,
    media_id: &str,
) -> Result<MediaBootPlan, ActionError> {
    if selection["reserved"]["media_id"].as_str() == Some(media_id) {
        return Ok(MediaBootPlan::AlreadyAccepted);
    }
    if selection["attempt"]["media_id"].as_str() == Some(media_id) {
        return match selection["attempt"]["state"].as_str() {
            Some("started") => Ok(MediaBootPlan::AlreadyAccepted),
            _ => Err(ActionError::BadRequest(eyre::eyre!(
                "previous media boot outcome is uncertain; inspect the target before rearming"
            ))),
        };
    }
    match selection["pending"]["boot_device"].as_str() {
        Some("network") => Ok(MediaBootPlan::Arm {
            cancel_pending_network: true,
        }),
        Some("media") if selection["pending"]["media_id"].as_str() != Some(media_id) => Err(
            ActionError::BadRequest(eyre::eyre!("another media boot selection is pending")),
        ),
        _ => Ok(MediaBootPlan::Arm {
            cancel_pending_network: false,
        }),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn reads_vmctl_media_identity_and_ignores_ordinary_sources() {
        let vmctl = "Type Device Target Source\nfile cdrom sdc vm-media://media-1\n";
        assert_eq!(
            media_identity(vmctl, "sdc").unwrap().as_deref(),
            Some("media-1")
        );
        assert_eq!(media_identity(vmctl, "sdb").unwrap(), None);
        let libvirt = " Type   Device   Target   Source\n\
            ------------------------------------------\n \
            file   disk     vda      /var/lib/libvirt/images/a.qcow2\n \
            file   cdrom    sdc      /tmp/config.iso\n";
        assert_eq!(media_identity(libvirt, "sdc").unwrap(), None);
        assert_eq!(media_identity("", "sdc").unwrap(), None);
    }

    #[test]
    fn rejects_empty_or_ambiguous_identities() {
        assert!(media_identity("file cdrom sdc vm-media://\n", "sdc").is_err());
        assert!(
            media_identity(
                "file cdrom sdc vm-media://a\nfile cdrom sdc vm-media://b\n",
                "sdc"
            )
            .is_err()
        );
    }

    #[test]
    fn never_replays_an_accepted_media_boot() {
        let arm = |cancel_pending_network| MediaBootPlan::Arm {
            cancel_pending_network,
        };
        for (name, selection, expected) in [
            (
                "nothing pending",
                json!({"pending": {"boot_device": "disk"}}),
                Ok(arm(false)),
            ),
            ("no selection", json!({}), Ok(arm(false))),
            (
                "network pending",
                json!({"pending": {"boot_device": "network"}}),
                Ok(arm(true)),
            ),
            (
                "same media pending",
                json!({"pending": {"boot_device": "media", "media_id": "media-1"}}),
                Ok(arm(false)),
            ),
            (
                "other media pending",
                json!({"pending": {"boot_device": "media", "media_id": "other"}}),
                Err(()),
            ),
            (
                "reserved",
                json!({"pending": {"boot_device": "network"}, "reserved": {"boot_device": "media", "media_id": "media-1"}}),
                Ok(MediaBootPlan::AlreadyAccepted),
            ),
            (
                "started",
                json!({"pending": {"boot_device": "disk"}, "attempt": {"media_id": "media-1", "state": "started"}}),
                Ok(MediaBootPlan::AlreadyAccepted),
            ),
            (
                "uncertain",
                json!({"pending": {"boot_device": "disk"}, "attempt": {"media_id": "media-1", "state": "uncertain"}}),
                Err(()),
            ),
            (
                "another medium's attempt",
                json!({"pending": {"boot_device": "disk"}, "attempt": {"media_id": "old", "state": "uncertain"}}),
                Ok(arm(false)),
            ),
        ] {
            assert_eq!(
                plan_media_boot(&selection, "media-1").map_err(drop),
                expected,
                "{name}"
            );
        }
    }
}

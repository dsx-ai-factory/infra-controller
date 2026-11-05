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
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};
    use std::sync::Weak;

    use serde_json::json;
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::libvirt::{AppliedState, BootKind, Config, LibvirtActor, LibvirtBackend};

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

    /// A backend whose virsh is the checked-in vmctl shim stand-in, controlled
    /// through `directory`.
    fn backend(directory: &Path, power: &str, selection: &str) -> LibvirtBackend {
        std::fs::write(directory.join("power"), power).unwrap();
        std::fs::write(directory.join("selection"), selection).unwrap();
        let (actor, _callbacks) = LibvirtActor::new(
            Config {
                state_file: None,
                virsh_path: PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("src/libvirt/testdata/media-boot-virsh"),
                uri: format!("test://{}", directory.display()),
                domain: "test-domain".into(),
                virtual_media_targets: BTreeMap::from([("ConfigCd".into(), "sdc".into())]),
            },
            CancellationToken::new().drop_guard(),
        );
        LibvirtBackend {
            persistence: None,
            persistence_retry: None,
            config: actor.config,
            restore_boot_after_power_on: false,
            system_state: None,
            bmc_state: Weak::new(),
            last_observed_power: None,
            boot_pending: None,
            initialized: actor.initialized,
            applied_state: AppliedState::default(),
            refresh_pending: actor.refresh_pending,
            power_state: actor.power_state,
        }
    }

    fn read(directory: &Path, file: &str) -> String {
        std::fs::read_to_string(directory.join(file)).unwrap()
    }

    #[tokio::test]
    async fn media_boot_with_unknown_power_state_arms_nothing() {
        let directory = tempfile::tempdir().unwrap();
        let network = r#"{"pending":{"boot_device":"network"}}"#;
        let mut backend = backend(directory.path(), "shut off", network);
        std::fs::write(directory.path().join("domstate-fails"), "").unwrap();

        let result = backend.boot_media_once("ConfigCd", "media-1").await;

        assert!(
            matches!(result, Err(ActionError::BadRequest(_))),
            "{result:?}"
        );
        assert_eq!(
            read(directory.path(), "log"),
            "domblklist\nboot-selection\ndomstate\n"
        );
        assert_eq!(
            read(directory.path(), "selection"),
            network,
            "neither a media nor the network selection changes"
        );
        assert!(backend.boot_pending.is_none());
    }

    #[tokio::test]
    async fn failed_media_boot_cancels_the_selection_and_keeps_the_power_error() {
        for (power, command) in [("shut off", "start"), ("running", "reset")] {
            for cancel_fails in [false, true] {
                let case = format!("{command}, cancel fails: {cancel_fails}");
                let directory = tempfile::tempdir().unwrap();
                let mut backend = backend(directory.path(), power, "{}");
                std::fs::write(directory.path().join(format!("{command}-fails")), "").unwrap();
                if cancel_fails {
                    std::fs::write(directory.path().join("boot-default-fails"), "").unwrap();
                }

                let result = backend.boot_media_once("ConfigCd", "media-1").await;

                let Err(ActionError::Internal(error)) = result else {
                    panic!("{case}: {result:?}");
                };
                assert!(
                    error.to_string().contains(&format!("{command} failed")),
                    "{case}: {error}"
                );
                assert_eq!(
                    read(directory.path(), "log"),
                    format!(
                        "domblklist\nboot-selection\ndomstate\nboot-once-media\n{command}\nboot-default\n"
                    ),
                    "{case}"
                );
                if !cancel_fails {
                    assert_eq!(
                        read(directory.path(), "selection"),
                        r#"{"pending":{"boot_device":"disk"}}"#,
                        "{case}"
                    );
                }
                assert!(backend.boot_pending.is_none(), "{case}");
            }
        }
    }

    #[tokio::test]
    async fn media_boot_cancels_pending_network_and_keeps_its_own_selection() {
        for (power, command, cold) in [("shut off", "start", true), ("running", "reset", false)] {
            let directory = tempfile::tempdir().unwrap();
            let network = r#"{"pending":{"boot_device":"network"}}"#;
            let mut backend = backend(directory.path(), power, network);

            backend
                .boot_media_once("ConfigCd", "media-1")
                .await
                .unwrap();

            assert_eq!(
                read(directory.path(), "log"),
                format!(
                    "domblklist\nboot-selection\ndomstate\nboot-default\nboot-once-media\n{command}\n"
                )
            );
            assert_eq!(
                read(directory.path(), "selection"),
                r#"{"pending":{"boot_device":"media","media_id":"media-1"}}"#
            );
            assert_eq!(
                (
                    matches!(backend.boot_pending, Some(BootKind::Cold)),
                    matches!(backend.boot_pending, Some(BootKind::Warm)),
                ),
                (cold, !cold),
                "{command}"
            );
        }
    }
}

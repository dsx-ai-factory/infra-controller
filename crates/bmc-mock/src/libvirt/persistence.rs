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

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use crate::persistence::{BmcSnapshotSource, PersistedBmcState, PersistenceError, atomic_write};
use crate::{BmcState, Callbacks};

pub(super) struct StateFile {
    path: PathBuf,
    source: BmcSnapshotSource,
    saved: Option<PersistedBmcState>,
}

impl StateFile {
    pub(super) fn load<C: Callbacks>(
        path: PathBuf,
        state: &BmcState<C>,
    ) -> Result<Self, PersistenceError> {
        let io_error = |operation, source| PersistenceError::Io {
            operation,
            path: path.clone(),
            source,
        };
        let saved = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => {
                if !metadata.file_type().is_file() {
                    return Err(PersistenceError::Invalid(
                        "state path is not a regular file",
                    ));
                }
                let contents = std::fs::read(&path).map_err(|e| io_error("read", e))?;
                let snapshot = serde_json::from_slice::<PersistedBmcState>(&contents)?;
                state.restore_persisted(&snapshot)?;
                // Only private files can skip the startup save on snapshot equality.
                // Otherwise save replaces the file with owner-only permissions before serving.
                (metadata.permissions().mode() & 0o077 == 0).then_some(snapshot)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(io_error("inspect", error)),
        };
        Ok(Self {
            path,
            source: state.snapshot_source(),
            saved,
        })
    }

    pub(super) fn save(&mut self) -> Result<(), PersistenceError> {
        let snapshot = self
            .source
            .persisted()
            .ok_or(PersistenceError::Invalid("BMC snapshot source was dropped"))?;
        if self.saved.as_ref() == Some(&snapshot) {
            return Ok(());
        }
        let bytes = serde_json::to_vec(&snapshot)?;
        match atomic_write(&self.path, &bytes) {
            Ok(()) => {}
            Err(error @ PersistenceError::Replaced { .. }) => {
                tracing::warn!(error = %error, "BMC state saved with uncertain crash durability")
            }
            Err(error) => return Err(error),
        }
        self.saved = Some(snapshot);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::body::Body;
    use axum::http::Request;
    use tokio_util::sync::CancellationToken;
    use tower::ServiceExt;

    use super::*;
    use crate::libvirt::{Config, LibvirtActor, LibvirtCallbacks};
    use crate::test_support::host_info;
    use crate::{HardwareType, MachineRouterOptions, machine_router};

    fn unstarted(
        path: PathBuf,
    ) -> (
        axum::Router,
        BmcState<LibvirtCallbacks>,
        LibvirtActor,
        Arc<LibvirtCallbacks>,
    ) {
        let stop = CancellationToken::new();
        let (actor, callbacks) = LibvirtActor::new(
            Config {
                state_file: Some(path),
                virsh_path: PathBuf::from("unused"),
                uri: "test:///default".into(),
                domain: "test-domain".into(),
                virtual_media_targets: Default::default(),
            },
            stop.drop_guard(),
        );
        let callbacks = Arc::new(callbacks);
        let (router, state) = machine_router(
            &host_info(HardwareType::GenericAmi),
            callbacks.clone(),
            "test".into(),
            false,
            MachineRouterOptions::default(),
        );
        (router, state, actor, callbacks)
    }

    #[test]
    fn missing_file_is_created_only_by_explicit_save() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let (_, state, _, _) = unstarted(path.clone());
        let mut storage = StateFile::load(path.clone(), &state).unwrap();
        assert!(!path.exists());
        storage.save().unwrap();
        let saved: PersistedBmcState =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(saved, state.persisted());
    }

    #[test]
    fn invalid_startup_state_is_preserved_and_never_replaces_profile_accounts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        for contents in [
            "{",
            r#"{"version":2,"accounts":[]}"#,
            r#"{"version":1,"accounts":[]}"#,
        ] {
            std::fs::write(&path, contents).unwrap();
            let (_, state, _, _) = unstarted(path.clone());
            let before = state.persisted();
            assert!(StateFile::load(path.clone(), &state).is_err());
            assert_eq!(state.persisted(), before);
            assert_eq!(std::fs::read_to_string(&path).unwrap(), contents);
        }
        let link = dir.path().join("link.json");
        std::os::unix::fs::symlink(dir.path().join("absent"), &link).unwrap();
        let (_, state, _, _) = unstarted(link.clone());
        assert!(StateFile::load(link.clone(), &state).is_err());
        assert!(
            std::fs::symlink_metadata(link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }
    #[tokio::test]
    async fn restart_retains_lockdown_and_boot_policy() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let (router, state, _actor, _callbacks) = unstarted(path.clone());
        let mut storage = StateFile::load(path.clone(), &state).unwrap();
        let changes = [
            (
                "/redfish/v1/Systems/Self/Bios",
                serde_json::json!({"Attributes": {"KCSACP": "Deny All", "USB000": "Disabled"}}),
            ),
            (
                "/redfish/v1/Managers/Self/HostInterfaces/Self",
                serde_json::json!({"InterfaceEnabled": false}),
            ),
            (
                "/redfish/v1/Systems/Self",
                serde_json::json!({"Boot": {
                    "BootOrder": ["Boot0001", "Boot0000"],
                    "BootSourceOverrideEnabled": "Once",
                    "BootSourceOverrideTarget": "Pxe"
                }}),
            ),
        ];
        let mut expected = Vec::new();
        for (uri, body) in changes {
            let response = router
                .clone()
                .oneshot(
                    Request::builder()
                        .method("PATCH")
                        .uri(uri)
                        .header("content-type", "application/json")
                        .body(Body::from(body.to_string()))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert!(
                response.status().is_success(),
                "{uri}: {}",
                response.status()
            );
            let response = router
                .clone()
                .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
                .await
                .unwrap();
            let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            expected.push((
                uri,
                serde_json::from_slice::<serde_json::Value>(&bytes).unwrap(),
            ));
        }
        storage.save().unwrap();
        let (restored, restored_state, _second_actor, _second_callbacks) = unstarted(path.clone());
        StateFile::load(path, &restored_state).unwrap();
        for (uri, before) in expected {
            let response = restored
                .clone()
                .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
                .await
                .unwrap();
            let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            let after: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            for key in ["Attributes", "InterfaceEnabled", "Boot"] {
                assert_eq!(after.get(key), before.get(key), "{uri}: {key}");
            }
        }
    }
}

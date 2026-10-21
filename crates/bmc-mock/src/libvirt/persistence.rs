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
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use std::sync::Arc;
    use std::time::Duration;

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tokio::task::JoinSet;
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

    async fn fixture(
        path: PathBuf,
    ) -> (
        axum::Router,
        BmcState<LibvirtCallbacks>,
        JoinSet<()>,
        Arc<LibvirtCallbacks>,
    ) {
        let virsh = path.with_extension("virsh");
        std::fs::write(
            &virsh,
            r#"#!/bin/sh
case "$3" in
    domstate) printf '%s\n' running ;;
    dumpxml) printf '%s\n' '<domain><os><type>hvm</type></os><devices/></domain>' ;;
    define) exit 0 ;;
    *) exit 2 ;;
esac
"#,
        )
        .unwrap();
        std::fs::set_permissions(&virsh, std::fs::Permissions::from_mode(0o755)).unwrap();
        let (router, state, mut actor, callbacks) = unstarted(path);
        actor.config.virsh_path = virsh;
        let mut tasks = JoinSet::new();
        actor
            .run(&state, &mut tasks, CancellationToken::new())
            .await
            .unwrap();
        (router, state, tasks, callbacks)
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

    #[tokio::test]
    async fn startup_privately_replaces_permissive_state_without_changing_accounts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let (_, seeded, _, _) = unstarted(path.clone());
        seeded
            .account_service_state
            .change_factory_default_password("seeded-password");
        let snapshot = seeded.persisted();
        std::fs::write(&path, serde_json::to_vec(&snapshot).unwrap()).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let old_inode = std::fs::metadata(&path).unwrap().ino();

        let (_, restored, running, callbacks) = fixture(path.clone()).await;
        assert_eq!(restored.persisted(), snapshot);
        let metadata = std::fs::metadata(&path).unwrap();
        assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
        assert_ne!(metadata.ino(), old_inode);
        let saved: PersistedBmcState =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved, snapshot);
        callbacks.finish().await.unwrap();
        running.join_all().await;
    }

    #[tokio::test]
    async fn invalid_startup_state_is_preserved_and_never_replaces_profile_accounts() {
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

    async fn patch(router: axum::Router, body: serde_json::Value) -> StatusCode {
        router
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri("/redfish/v1/AccountService/Accounts/2")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap()
            .status()
    }

    async fn wait_saved(path: &std::path::Path, expected: &PersistedBmcState) {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if std::fs::read(path)
                    .ok()
                    .and_then(|b| serde_json::from_slice::<PersistedBmcState>(&b).ok())
                    .as_ref()
                    == Some(expected)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn callback_saves_password_and_skips_unchanged_state() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let (router, state, running, callbacks) = fixture(path.clone()).await;
        assert_eq!(
            patch(router.clone(), serde_json::json!({"UserName":"new-user"})).await,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            patch(router, serde_json::json!({"Password":"new-password"})).await,
            StatusCode::NO_CONTENT
        );
        let saved = state.persisted();
        wait_saved(&path, &saved).await;

        let inode = std::fs::metadata(&path).unwrap().ino();
        for _ in 0..100 {
            callbacks.state_refresh_indication();
        }
        callbacks.finish().await.unwrap();
        running.join_all().await;
        assert_eq!(std::fs::metadata(&path).unwrap().ino(), inode);
        let (_, restored, second, second_callbacks) = fixture(path).await;
        assert_eq!(restored.persisted(), saved);
        second_callbacks.finish().await.unwrap();
        second.join_all().await;
    }

    #[tokio::test]
    async fn final_save_captures_internal_changes_without_periodic_polling() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let (_, state, running, callbacks) = fixture(path.clone()).await;
        let old = std::fs::read(&path).unwrap();
        state
            .account_service_state
            .change_factory_default_password("internal-change");
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(std::fs::read(&path).unwrap(), old);
        callbacks.finish().await.unwrap();
        running.join_all().await;
        assert_eq!(
            serde_json::from_slice::<PersistedBmcState>(&std::fs::read(path).unwrap()).unwrap(),
            state.persisted()
        );
    }
}

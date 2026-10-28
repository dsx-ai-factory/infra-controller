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

//! Portable BMC snapshots and atomic file replacement. The caller owns when and where to save.
use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::redfish::account_service::PersistedAccount;
use crate::redfish::computer_system::{PersistedSystemState, UefiPasswords};
use crate::redfish::manager::PersistedManagerState;
use crate::redfish::update_service::PersistedDpuUpdate;
use crate::{BmcState, Callbacks};

/// State shared by standalone BMCs and enclosing simulator snapshots.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistedBmcState {
    version: u32,
    accounts: Vec<PersistedAccount>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    systems: Option<BTreeMap<String, PersistedSystemState>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    managers: Option<BTreeMap<String, PersistedManagerState>>,
    #[serde(default)]
    factory_reset_generation: u64,
    #[serde(default)]
    uefi_passwords: BTreeMap<String, UefiPasswords>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    dpu_update: Option<PersistedDpuUpdate>,
}

impl std::fmt::Debug for PersistedBmcState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PersistedBmcState")
            .field("version", &self.version)
            .field("account_count", &self.accounts.len())
            .finish_non_exhaustive()
    }
}

impl PersistedBmcState {
    /// Number of factory resets represented by this snapshot.
    pub fn factory_reset_generation(&self) -> u64 {
        self.factory_reset_generation
    }

    /// The saved DPU disk, when this snapshot includes resumable DPU task state.
    /// The MAT actor integration uses this hook to prefer the atomic BMC disk
    /// over a separately sampled OS observation.
    pub fn dpu_os_status(&self) -> Option<crate::DpuOsStatus> {
        self.dpu_update.as_ref().map(PersistedDpuUpdate::status)
    }

    /// Validates a snapshot before any live state is changed.
    pub fn validate(&self) -> Result<(), PersistenceError> {
        if self.version != 1 {
            return Err(PersistenceError::Invalid("unsupported snapshot version"));
        }
        PersistedAccount::validate_all(&self.accounts).map_err(PersistenceError::Invalid)?;
        if let Some(saved) = &self.dpu_update {
            saved.validate().map_err(PersistenceError::Invalid)?;
        }
        Ok(())
    }
}

/// Read-only live BMC effects, without account credentials or task payloads.
/// Fields are individual observations, not an atomic transaction across resources.
#[derive(Clone, Debug, Serialize)]
pub struct BmcEffectSnapshot {
    /// Whether the complete account set equals this model's factory account.
    pub factory_accounts_restored: bool,
    /// Current effects for each system configured by this BMC.
    pub systems: HashMap<String, BmcSystemEffectSnapshot>,
    /// Actual Redfish task states, keyed by task ID, including retained finished tasks.
    pub tasks: HashMap<String, crate::BmcTaskState>,
}

/// Read-only effects for one live Redfish system.
#[derive(Clone, Debug, Serialize)]
pub struct BmcSystemEffectSnapshot {
    /// Merged BIOS Attributes as served by BIOS GET, or None when absent.
    pub bios_attributes: Option<serde_json::Value>,
    /// Whether any BIOS setup password is currently nonempty.
    pub uefi_passwords_set: bool,
    /// BootProgress as served by Systems GET, respecting the observed power state.
    pub boot_progress: Option<serde_json::Value>,
}

/// Non-owning snapshot access independent of the BMC's backend callback type.
#[derive(Clone)]
pub struct BmcSnapshotSource {
    snapshot: Arc<dyn Fn() -> Option<PersistedBmcState> + Send + Sync>,
    effects: Arc<dyn Fn() -> Option<BmcEffectSnapshot> + Send + Sync>,
}

impl std::fmt::Debug for BmcSnapshotSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BmcSnapshotSource").finish_non_exhaustive()
    }
}

impl BmcSnapshotSource {
    /// Observes existing resource state without requests or lifecycle advancement.
    /// Returns None if any required live resource has been dropped.
    /// Captures only weak references to live resources.
    pub fn effect_snapshot(&self) -> Option<BmcEffectSnapshot> {
        (self.effects)()
    }

    /// Returns None when the BMC's resource state has been dropped.
    pub fn persisted(&self) -> Option<PersistedBmcState> {
        (self.snapshot)()
    }
}

impl<C: Callbacks> BmcState<C> {
    /// Exports current BMC resource state without performing any file I/O.
    pub fn persisted(&self) -> PersistedBmcState {
        self.snapshot_source()
            .persisted()
            .expect("BMC owns its resource state")
    }
    /// Returns a weak resource handle suitable for an enclosing simulator or persistence actor.
    pub fn snapshot_source(&self) -> BmcSnapshotSource {
        let accounts = Arc::downgrade(&self.account_service_state);
        let systems = Arc::downgrade(&self.system_state);
        let managers = Arc::downgrade(&self.manager);
        let uefi_passwords = self
            .system_state
            .uefi_password_sources()
            .map(|(id, source)| (id.to_owned(), source))
            .collect::<BTreeMap<_, _>>();
        let effect_systems = systems.clone();
        let effect_accounts = accounts.clone();
        let updates = Arc::downgrade(&self.update_service_state);
        let saved_updates = updates.clone();
        BmcSnapshotSource {
            snapshot: Arc::new(move || {
                let accounts = accounts.upgrade()?;
                let uefi_passwords = uefi_passwords
                    .iter()
                    .map(|(id, passwords)| {
                        let passwords = passwords.upgrade()?;
                        let saved = passwords.lock().expect("mutex poisoned").clone();
                        Some((id.clone(), saved))
                    })
                    .collect::<Option<_>>()?;
                let (accounts, factory_reset_generation) = accounts.persisted_accounts();
                // Manager resources are captured separately from account reset. A
                // snapshot overlapping reset can contain earlier manager overrides. The
                // next save after the reset request captures the restored manager values.
                Some(PersistedBmcState {
                    version: 1,
                    accounts,
                    systems: Some(systems.upgrade()?.persisted()),
                    managers: Some(managers.upgrade()?.persisted()),
                    factory_reset_generation,
                    uefi_passwords,
                    dpu_update: saved_updates.upgrade()?.persisted_dpu_update(),
                })
            }),
            effects: Arc::new(move || {
                let systems = effect_systems.upgrade()?;
                let accounts = effect_accounts.upgrade()?;
                let updates = updates.upgrade()?;
                Some(BmcEffectSnapshot {
                    factory_accounts_restored: accounts.factory_accounts_restored(),
                    systems: systems.effect_snapshots(&updates),
                    tasks: updates.task_states(),
                })
            }),
        }
    }
    /// Restores validated state before serving requests or attaching IPMI synchronization.
    /// A validation failure leaves all live state unchanged.
    pub fn restore_persisted(&self, snapshot: &PersistedBmcState) -> Result<(), PersistenceError> {
        snapshot.validate()?;
        if snapshot
            .systems
            .as_ref()
            .is_some_and(|saved| !self.system_state.validate_persisted(saved))
            || snapshot
                .managers
                .as_ref()
                .is_some_and(|saved| !self.manager.validate_persisted(saved))
        {
            return Err(PersistenceError::Invalid(
                "snapshot resources do not match the BMC profile",
            ));
        }
        if let Some(saved) = &snapshot.dpu_update {
            self.update_service_state
                .validate_dpu_update(saved)
                .map_err(PersistenceError::Invalid)?;
        }
        // Password restoration mutates state, so validate both resource maps first.
        self.system_state
            .restore_uefi_passwords(&snapshot.uefi_passwords)
            .map_err(PersistenceError::Invalid)?;
        if let Some(saved) = &snapshot.systems {
            self.system_state.restore_persisted(saved);
        }
        if let Some(saved) = &snapshot.managers {
            self.manager.restore_persisted(saved);
        }
        self.account_service_state
            .restore_accounts(&snapshot.accounts, snapshot.factory_reset_generation);
        if let Some(saved) = &snapshot.dpu_update {
            self.update_service_state.restore_dpu_update(
                saved,
                self.callbacks.clone(),
                Arc::downgrade(&self.system_state),
            );
        }
        Ok(())
    }
}

/// Storage errors redact serde diagnostics, which can otherwise contain passwords.
#[derive(thiserror::Error)]
pub enum PersistenceError {
    #[error("invalid BMC snapshot: {0}")]
    Invalid(&'static str),
    #[error("invalid BMC snapshot JSON at line {line}, column {column}")]
    Json { line: usize, column: usize },
    #[error("could not {operation} state file {}: {source}", path.display())]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("state file {} was replaced but directory sync failed: {source}", path.display())]
    Replaced {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

impl std::fmt::Debug for PersistenceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self, f)
    }
}

impl From<serde_json::Error> for PersistenceError {
    fn from(source: serde_json::Error) -> Self {
        Self::Json {
            line: source.line(),
            column: source.column(),
        }
    }
}

/// Atomically replaces a file with owner-only bytes. Its parent directory must exist.
/// `Replaced` means rename succeeded: callers must treat the new bytes as committed,
/// although durability across a crash is uncertain. Other errors leave the old file intact.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), PersistenceError> {
    let err = |operation, source| PersistenceError::Io {
        operation,
        path: path.to_owned(),
        source,
    };
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let directory = File::open(parent).map_err(|e| err("open parent directory of", e))?;
    let mut temporary =
        tempfile::NamedTempFile::new_in(parent).map_err(|e| err("create temporary file for", e))?;
    temporary.write_all(bytes).map_err(|e| err("write", e))?;
    temporary.as_file().sync_all().map_err(|e| err("sync", e))?;
    temporary
        .persist(path)
        .map_err(|e| err("replace", e.error))?;
    directory
        .sync_all()
        .map_err(|source| PersistenceError::Replaced {
            path: path.to_owned(),
            source,
        })
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use axum::Router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use serde_json::json;
    use tower::ServiceExt;

    use super::*;

    fn uefi_bmc() -> (Router, BmcState<crate::test_support::TestCallbacks>) {
        crate::machine_router(
            &crate::test_support::host_info(crate::HardwareType::WiwynnGB200Nvl),
            Arc::new(crate::test_support::TestCallbacks::default()),
            "uefi-snapshot".into(),
            false,
            crate::MachineRouterOptions::default(),
        )
    }

    async fn change_password(router: &Router, name: &str, old: &str, new: &str) -> StatusCode {
        router
            .clone()
            .oneshot(
                Request::post("/redfish/v1/Systems/System_0/Bios/Actions/Bios.ChangePassword")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"PasswordName": name, "OldPassword": old, "NewPassword": new})
                            .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap()
            .status()
    }

    #[tokio::test]
    async fn account_snapshots_keep_the_generation_across_reset_restore_and_legacy_restore() {
        let (_, state) = uefi_bmc();
        let source = state.snapshot_source();
        let factory_accounts = state.account_service_state.accounts();
        state
            .account_service_state
            .change_factory_default_password("rotated");
        let before_reset = source.persisted().unwrap();
        assert_eq!(before_reset.factory_reset_generation(), 0);
        {
            let _update = state.account_service_state.lock_updates().await;
            state
                .account_service_state
                .reset_to_factory()
                .await
                .unwrap();
        }
        let after_reset = source.persisted().unwrap();
        assert_eq!(after_reset.factory_reset_generation(), 1);
        assert_eq!(state.factory_reset_generation(), 1);
        assert_eq!(state.account_service_state.accounts(), factory_accounts);
        assert!(before_reset.accounts != after_reset.accounts);

        for snapshot in [&before_reset, &after_reset] {
            state.restore_persisted(snapshot).unwrap();
            let observed = source.persisted().unwrap();
            assert!(observed.accounts == snapshot.accounts);
            assert_eq!(
                observed.factory_reset_generation(),
                snapshot.factory_reset_generation()
            );
            assert_eq!(
                state.factory_reset_generation(),
                snapshot.factory_reset_generation()
            );
        }

        let mut legacy = serde_json::to_value(&before_reset).unwrap();
        legacy
            .as_object_mut()
            .unwrap()
            .remove("factory_reset_generation");
        let legacy = serde_json::from_value(legacy).unwrap();
        state.restore_persisted(&legacy).unwrap();
        let observed = source.persisted().unwrap();
        assert!(observed.accounts == before_reset.accounts);
        assert_eq!(observed.factory_reset_generation(), 0);
        assert_eq!(state.factory_reset_generation(), 0);
    }

    #[tokio::test]
    async fn uefi_passwords_survive_snapshot_restore_and_clearing() {
        let (router, state) = uefi_bmc();
        let source = state.snapshot_source();
        for (name, password) in [
            ("AdminPassword", "saved-admin"),
            ("UserPassword", "saved-user"),
        ] {
            assert_eq!(
                change_password(&router, name, "", password).await,
                StatusCode::OK
            );
        }
        let snapshot = source.persisted().unwrap();
        assert_eq!(snapshot, state.persisted());
        assert!(!format!("{snapshot:?}").contains("saved-admin"));
        assert!(!format!("{source:?}").contains("saved-admin"));
        let snapshot: PersistedBmcState =
            serde_json::from_slice(&serde_json::to_vec(&snapshot).unwrap()).unwrap();
        drop(router);
        drop(state);
        assert!(source.persisted().is_none());

        let (router, state) = uefi_bmc();
        state.restore_persisted(&snapshot).unwrap();
        for (name, old, expected) in [
            ("AdminPassword", "wrong", StatusCode::BAD_REQUEST),
            ("AdminPassword", "saved-admin", StatusCode::OK),
            ("UserPassword", "saved-user", StatusCode::OK),
        ] {
            assert_eq!(change_password(&router, name, old, "").await, expected);
        }
        let cleared = state.snapshot_source().persisted().unwrap();
        let (fresh_router, fresh_state) = uefi_bmc();
        fresh_state.restore_persisted(&cleared).unwrap();
        assert_eq!(
            change_password(&fresh_router, "AdminPassword", "", "next").await,
            StatusCode::OK
        );
    }

    async fn get_json(router: &Router, path: &str) -> serde_json::Value {
        let response = router
            .clone()
            .oneshot(Request::get(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        serde_json::from_slice(
            &axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap(),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn live_effects_match_bios_and_password_changes_without_retaining_the_bmc() {
        let (router, state) = uefi_bmc();
        let callbacks = Arc::downgrade(state.callbacks.as_ref().unwrap());
        let source = state.snapshot_source();
        let system_id = "System_0";
        let bios_path = "/redfish/v1/Systems/System_0/Bios";
        let defaults = get_json(&router, bios_path).await["Attributes"].clone();
        let initial = source.effect_snapshot().unwrap();
        assert!(initial.factory_accounts_restored);
        assert!(!initial.systems[system_id].uefi_passwords_set);
        assert_eq!(
            initial.systems[system_id].bios_attributes.as_ref(),
            Some(&defaults)
        );

        let response = router
            .clone()
            .oneshot(
                Request::patch(bios_path)
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"Attributes": {"MockProbe": "changed"}}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(response.status().is_success());
        assert_eq!(
            change_password(&router, "AdminPassword", "", "setup-secret-marker").await,
            StatusCode::OK
        );
        state
            .account_service_state
            .change_factory_default_password("account-secret-marker");
        let changed = source.effect_snapshot().unwrap();
        assert!(!changed.factory_accounts_restored);
        assert!(changed.systems[system_id].uefi_passwords_set);
        assert_eq!(
            changed.systems[system_id].bios_attributes,
            Some(get_json(&router, bios_path).await["Attributes"].clone())
        );
        assert_eq!(
            changed.systems[system_id].bios_attributes.as_ref().unwrap()["MockProbe"],
            "changed"
        );
        for output in [
            serde_json::to_string(&changed).unwrap(),
            format!("{changed:?} {source:?}"),
        ] {
            for forbidden in [
                "setup-secret-marker",
                "account-secret-marker",
                "/redfish/",
                "Password",
                "Token",
            ] {
                assert!(!output.contains(forbidden), "snapshot exposed {forbidden}");
            }
        }
        // Observation leaves the current setup password intact.
        assert_eq!(
            change_password(&router, "AdminPassword", "setup-secret-marker", "").await,
            StatusCode::OK
        );
        state
            .account_service_state
            .reset_to_factory()
            .await
            .unwrap();
        let response = router
            .clone()
            .oneshot(
                Request::post("/redfish/v1/Systems/System_0/Bios/Actions/Bios.ResetBios")
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(response.status().is_success());
        let reset = source.effect_snapshot().unwrap();
        assert!(reset.factory_accounts_restored);
        assert!(!reset.systems[system_id].uefi_passwords_set);
        assert_eq!(reset.systems[system_id].bios_attributes, Some(defaults));
        let cloned = source.clone();
        drop(router);
        drop(state);
        assert!(source.effect_snapshot().is_none());
        assert!(cloned.effect_snapshot().is_none());
        assert!(callbacks.upgrade().is_none());
    }

    #[tokio::test]
    async fn live_boot_effects_match_system_get_at_observed_power_and_image_generation() {
        let info = {
            let mut pool = crate::test_support::TEST_MAC_POOL.lock().unwrap();
            crate::MachineInfo::Dpu(crate::DpuMachineInfo::new(
                crate::HardwareType::DellPowerEdgeR750,
                &mut pool,
                crate::DpuSettings::default(),
            ))
        };
        let (router, state) = crate::machine_router(
            &info,
            Arc::new(crate::test_support::TestCallbacks::default()),
            "boot-effects".into(),
            false,
            crate::MachineRouterOptions::default(),
        );
        let source = state.snapshot_source();
        let path = state.system_state.primary_system_odata_id().unwrap();
        let id = path.rsplit('/').next().unwrap();
        assert!(
            source.effect_snapshot().unwrap().systems[id]
                .boot_progress
                .is_none()
        );
        state
            .update_service_state
            .record_dpu_os_image(Some(crate::DpuOsImage::Managed));
        state.update_service_state.on_dpu_power_on();
        state.update_service_state.on_dpu_boot_completed();
        for (power, image, expected) in [
            (
                nv_redfish::schema::resource::PowerState::On,
                crate::DpuOsImage::Managed,
                "OSRunning",
            ),
            (
                nv_redfish::schema::resource::PowerState::Off,
                crate::DpuOsImage::Managed,
                "None",
            ),
            (
                nv_redfish::schema::resource::PowerState::On,
                crate::DpuOsImage::PreIngestion,
                "OSBootStarted",
            ),
        ] {
            state.system_state.set_power_state(power);
            state.update_service_state.record_dpu_os_image(Some(image));
            let observed = source.effect_snapshot().unwrap().systems[id]
                .boot_progress
                .clone()
                .unwrap();
            assert_eq!(observed, get_json(&router, &path).await["BootProgress"]);
            assert_eq!(observed["LastState"], expected);
        }
    }

    #[tokio::test]
    async fn legacy_snapshots_restore_with_unset_uefi_passwords() {
        let (router, state) = uefi_bmc();
        let mut legacy = serde_json::to_value(state.persisted()).unwrap();
        for field in [
            "uefi_passwords",
            "systems",
            "managers",
            "factory_reset_generation",
        ] {
            legacy.as_object_mut().unwrap().remove(field);
        }
        let legacy: PersistedBmcState = serde_json::from_value(legacy).unwrap();
        assert_eq!(legacy.factory_reset_generation(), 0);
        let response = router
            .clone()
            .oneshot(
                Request::patch("/redfish/v1/Systems/System_0/Bios")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"Attributes": {"MockProbe": "changed"}}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(response.status().is_success());
        let resources = state.persisted();
        assert_eq!(
            change_password(&router, "AdminPassword", "", "existing").await,
            StatusCode::OK
        );
        state.restore_persisted(&legacy).unwrap();
        let restored = state.persisted();
        assert!(restored.systems == resources.systems);
        assert!(restored.managers == resources.managers);
        for (old, expected) in [("existing", StatusCode::BAD_REQUEST), ("", StatusCode::OK)] {
            assert_eq!(
                change_password(&router, "AdminPassword", old, "next").await,
                expected
            );
        }
    }

    #[tokio::test]
    async fn invalid_resource_maps_do_not_restore_passwords_or_other_resources() {
        let (router, state) = uefi_bmc();
        assert_eq!(
            change_password(&router, "AdminPassword", "", "existing").await,
            StatusCode::OK
        );
        let original = state.persisted();
        for resource in ["systems", "managers"] {
            let mut invalid = serde_json::to_value(&original).unwrap();
            invalid["uefi_passwords"]["System_0"]["AdminPassword"] = json!("replacement");
            invalid["accounts"][0]["password"] = json!("replacement-account");
            invalid["factory_reset_generation"] = json!(17);
            let resources = invalid[resource].as_object_mut().unwrap();
            let saved = resources.values().next().unwrap().clone();
            resources.clear();
            resources.insert("unknown-resource".to_string(), saved);
            let invalid: PersistedBmcState = serde_json::from_value(invalid).unwrap();
            assert!(state.restore_persisted(&invalid).is_err(), "{resource}");
            assert_eq!(state.persisted(), original, "{resource}");
        }
        assert_eq!(
            change_password(&router, "AdminPassword", "existing", "").await,
            StatusCode::OK
        );
    }

    #[test]
    fn atomic_replacement_is_private_and_failed_replacement_preserves_old_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        atomic_write(&path, b"old").unwrap();
        atomic_write(&path, b"new").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"new");
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let blocked = dir.path().join("blocked");
        std::fs::create_dir(&blocked).unwrap();
        assert!(matches!(
            atomic_write(&blocked, b"bytes"),
            Err(PersistenceError::Io { .. })
        ));
        assert_eq!(std::fs::read(&path).unwrap(), b"new");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
    }

    #[test]
    fn additive_snapshot_fields_remain_readable_by_existing_versions() {
        let (_, state) = crate::machine_router(
            &crate::test_support::host_info(crate::HardwareType::GenericAmi),
            Arc::new(crate::test_support::TestCallbacks::default()),
            "snapshot".into(),
            true,
            crate::MachineRouterOptions::default(),
        );
        let original = state.persisted();
        let mut extended = serde_json::to_value(&original).unwrap();
        extended["future_resource"] = serde_json::json!({"enabled":true});
        extended["accounts"][0]["future_account_property"] = serde_json::json!("value");
        let restored: PersistedBmcState = serde_json::from_value(extended).unwrap();
        restored.validate().unwrap();
        assert_eq!(restored, original);
    }

    #[test]
    fn invalid_restore_is_atomic_and_diagnostics_do_not_expose_credentials() {
        let (_, state) = crate::machine_router(
            &crate::test_support::host_info(crate::HardwareType::GenericAmi),
            Arc::new(crate::test_support::TestCallbacks::default()),
            "snapshot".into(),
            true,
            crate::MachineRouterOptions::default(),
        );
        let original = state.persisted();
        let mut invalid = original.clone();
        invalid.accounts.push(invalid.accounts[0].clone());
        assert!(state.restore_persisted(&invalid).is_err());
        assert_eq!(state.persisted(), original);
        let mut invalid = original.clone();
        invalid
            .uefi_passwords
            .insert("unknown-system".into(), BTreeMap::new());
        assert!(state.restore_persisted(&invalid).is_err());
        assert_eq!(state.persisted(), original);
        for contents in [
            r#"{"version":"secret-marker"}"#,
            r#"{"version":1,"accounts":"secret-marker"}"#,
        ] {
            let raw = serde_json::from_str::<PersistedBmcState>(contents).unwrap_err();
            assert!(raw.to_string().contains("secret-marker"));
            let error = PersistenceError::from(raw);
            assert!(std::error::Error::source(&error).is_none());
            assert!(!format!("{error:?}").contains("secret-marker"));
            let report = eyre::Report::new(error).wrap_err("could not initialize BMC persistence");
            assert!(!format!("{report:?}").contains("secret-marker"));
        }
        let password = serde_json::to_value(&original).unwrap()["accounts"][0]["password"]
            .as_str()
            .unwrap()
            .to_owned();
        assert!(!format!("{original:?}").contains(&password));
    }
}

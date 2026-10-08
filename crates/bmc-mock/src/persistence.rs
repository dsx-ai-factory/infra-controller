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
use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};

use serde::{Deserialize, Serialize};

use crate::redfish::account_service::{AccountServiceState, PersistedAccount};
use crate::{BmcState, Callbacks};

/// State shared by standalone BMCs and enclosing simulator snapshots.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistedBmcState {
    version: u32,
    accounts: Vec<PersistedAccount>,
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
    /// Validates a snapshot before any live state is changed.
    pub fn validate(&self) -> Result<(), PersistenceError> {
        if self.version != 1 {
            return Err(PersistenceError::Invalid("unsupported snapshot version"));
        }
        PersistedAccount::validate_all(&self.accounts).map_err(PersistenceError::Invalid)
    }
}

/// Non-owning snapshot access independent of the BMC's backend callback type.
#[derive(Clone, Debug)]
pub struct BmcSnapshotSource {
    accounts: Weak<AccountServiceState>,
}

impl BmcSnapshotSource {
    /// Returns None when the BMC's resource state has been dropped.
    pub fn persisted(&self) -> Option<PersistedBmcState> {
        self.accounts.upgrade().map(|a| PersistedBmcState {
            version: 1,
            accounts: a.persisted_accounts(),
        })
    }
}

impl<C: Callbacks> BmcState<C> {
    /// Exports current BMC resource state without performing any file I/O.
    pub fn persisted(&self) -> PersistedBmcState {
        PersistedBmcState {
            version: 1,
            accounts: self.account_service_state.persisted_accounts(),
        }
    }
    /// Returns a weak resource handle suitable for an enclosing simulator or persistence actor.
    pub fn snapshot_source(&self) -> BmcSnapshotSource {
        BmcSnapshotSource {
            accounts: Arc::downgrade(&self.account_service_state),
        }
    }
    /// Restores validated state before serving requests or attaching IPMI synchronization.
    /// A validation failure leaves all live accounts unchanged.
    pub fn restore_persisted(&self, snapshot: &PersistedBmcState) -> Result<(), PersistenceError> {
        snapshot.validate()?;
        self.account_service_state
            .restore_accounts(&snapshot.accounts);
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

    use super::*;

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

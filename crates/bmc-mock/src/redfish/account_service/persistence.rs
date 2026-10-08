// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::Account;

#[derive(thiserror::Error)]
pub enum AccountError {
    #[error("BMC account state operation failed\n  operation: {operation}\n  state file: {}\n  system error: {source}", path.display())]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    // Do not render serde's error text: it can contain credential values.
    #[error("BMC account state is invalid\n  state file: {}\n  line: {line}\n  column: {column}\n  hint: restore a valid account state file before restarting the BMC", path.display())]
    Json {
        path: PathBuf,
        line: usize,
        column: usize,
        #[source]
        source: serde_json::Error,
    },
    #[error("BMC account state is invalid\n  state file: {}\n  condition: {condition}\n  hint: restore a valid account state file before restarting the BMC", path.display())]
    State {
        path: PathBuf,
        condition: &'static str,
    },
    #[error("BMC account state version is unsupported\n  state file: {}\n  expected version: 1\n  actual version: {actual}", path.display())]
    Version { path: PathBuf, actual: u32 },
    #[error("BMC account operation is invalid\n  condition: {0}")]
    Invalid(&'static str),
    #[error("BMC account was not found\n  account ID: {0}")]
    NotFound(String),
    #[error("BMC account synchronization failed")]
    Synchronization(#[source] SynchronizationError),
}

// The existing IPMI adapter returns strings. Keep its cause inspectable but do
// not render potentially sensitive upstream command output in diagnostics.
#[derive(thiserror::Error)]
#[error("IPMI password synchronization failed")]
pub struct SynchronizationError(pub String);

impl std::fmt::Debug for AccountError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self, f)
    }
}

impl std::fmt::Debug for SynchronizationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self, f)
    }
}

fn io_error(operation: &'static str, path: &Path, source: io::Error) -> AccountError {
    AccountError::Io {
        operation,
        path: path.to_owned(),
        source,
    }
}

fn json_error(path: &Path, source: serde_json::Error) -> AccountError {
    AccountError::Json {
        path: path.to_owned(),
        line: source.line(),
        column: source.column(),
        source,
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    version: u32,
    accounts: Vec<Account>,
}

fn validate(path: &Path, accounts: &[Account]) -> Result<(), AccountError> {
    if accounts.is_empty() {
        return Err(AccountError::State {
            path: path.to_owned(),
            condition: "saved account list is empty",
        });
    }
    let mut ids = HashSet::new();
    let mut names = HashSet::new();
    for account in accounts {
        if account.id.is_empty() || account.username.is_empty() || account.role_id.is_empty() {
            return Err(AccountError::State {
                path: path.to_owned(),
                condition: "saved account identity or role is empty",
            });
        }
        if !ids.insert(&account.id) || !names.insert(&account.username) {
            return Err(AccountError::State {
                path: path.to_owned(),
                condition: "saved accounts contain duplicate IDs or usernames",
            });
        }
    }
    Ok(())
}

pub(super) fn load_or_create(
    path: &Path,
    defaults: &[Account],
) -> Result<Vec<Account>, AccountError> {
    // lstat distinguishes an absent file from a dangling link; the latter must
    // not silently cause initialization with factory credentials.
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if !metadata.file_type().is_file() {
                return Err(AccountError::State {
                    path: path.to_owned(),
                    condition: "account state path is not a regular file",
                });
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            save(path, defaults)?;
            return Ok(defaults.to_vec());
        }
        Err(error) => return Err(io_error("inspect", path, error)),
    }
    let contents = fs::read(path).map_err(|e| io_error("read", path, e))?;
    let snapshot: Snapshot = serde_json::from_slice(&contents).map_err(|e| json_error(path, e))?;
    if snapshot.version != 1 {
        return Err(AccountError::Version {
            path: path.to_owned(),
            actual: snapshot.version,
        });
    }
    validate(path, &snapshot.accounts)?;
    Ok(snapshot.accounts)
}

pub(super) fn save(path: &Path, accounts: &[Account]) -> Result<(), AccountError> {
    validate(path, accounts)?;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let directory = File::open(parent).map_err(|e| io_error("open parent directory", path, e))?;
    let bytes = serde_json::to_vec(&Snapshot {
        version: 1,
        accounts: accounts.to_vec(),
    })
    .map_err(|e| json_error(path, e))?;
    // tempfile creates a mode-0600 file; keeping it in the destination directory
    // makes replacement atomic and avoids crossing filesystem boundaries.
    let mut temporary = tempfile::NamedTempFile::new_in(parent)
        .map_err(|e| io_error("create temporary file", path, e))?;
    temporary
        .write_all(&bytes)
        .map_err(|e| io_error("write temporary file", path, e))?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|e| io_error("sync temporary file", path, e))?;
    temporary
        .persist(path)
        .map_err(|e| io_error("replace state file", path, e.error))?;
    // Rename is the commit point. A later directory-sync failure must not leave
    // memory using the old credentials while the file already contains the new ones.
    if let Err(error) = directory.sync_all() {
        tracing::warn!(%error, path = %path.display(), "account state saved but directory sync failed; crash durability is uncertain");
    }
    Ok(())
}

pub(super) fn parse_patch(
    value: &serde_json::Value,
) -> Result<(Option<&str>, Option<&str>), AccountError> {
    let object = value
        .as_object()
        .ok_or(AccountError::Invalid("account PATCH must be an object"))?;
    if !object.contains_key("UserName") && !object.contains_key("Password") {
        return Err(AccountError::Invalid(
            "account PATCH requires UserName or Password",
        ));
    }
    let field = |key| {
        object
            .get(key)
            .map(|value| {
                value.as_str().ok_or(AccountError::Invalid(
                    "UserName and Password must be strings",
                ))
            })
            .transpose()
    };
    let username = field("UserName")?;
    if username == Some("") {
        return Err(AccountError::Invalid("UserName must not be empty"));
    }
    Ok((username, field("Password")?))
}

#[cfg(test)]
mod tests {
    use std::error::Error;
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn defaults() -> Vec<Account> {
        vec![Account::administrator("1", "root", "redfish")]
    }

    #[test]
    fn initializes_private_versioned_state_and_restores_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("accounts.json");
        assert_eq!(load_or_create(&path, &defaults()).unwrap(), defaults());
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let mut changed = defaults();
        changed[0].username = "new-admin".into();
        changed[0].password = "new-password".into();
        save(&path, &changed).unwrap();
        assert_eq!(load_or_create(&path, &defaults()).unwrap(), changed);
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn corrupt_or_unsupported_state_never_reverts_to_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("accounts.json");
        for content in [
            "{",
            r#"{"version":2,"accounts":[]}"#,
            r#"{"version":1,"accounts":[]}"#,
        ] {
            fs::write(&path, content).unwrap();
            assert!(load_or_create(&path, &defaults()).is_err());
            assert_eq!(fs::read_to_string(&path).unwrap(), content);
        }
        let duplicate = vec![defaults()[0].clone(), defaults()[0].clone()];
        assert!(save(&path, &duplicate).is_err());
    }

    #[test]
    fn dangling_symlink_and_missing_directory_fail_closed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("accounts.json");
        std::os::unix::fs::symlink(dir.path().join("absent"), &path).unwrap();
        assert!(load_or_create(&path, &defaults()).is_err());
        let error =
            load_or_create(&dir.path().join("missing/accounts.json"), &defaults()).unwrap_err();
        assert!(
            error
                .source()
                .unwrap()
                .downcast_ref::<io::Error>()
                .is_some()
        );
    }

    #[test]
    fn diagnostics_redact_invalid_credential_values_and_preserve_causes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("accounts.json");
        fs::write(&path, r#"{"version":"secret-marker","accounts":[]}"#).unwrap();
        let error = load_or_create(&path, &defaults()).unwrap_err();
        assert!(!error.to_string().contains("secret-marker"));
        assert!(!format!("{error:?}").contains("secret-marker"));
        assert!(
            error
                .source()
                .unwrap()
                .downcast_ref::<serde_json::Error>()
                .is_some()
        );
        assert_eq!(
            AccountError::Invalid("UserName must not be empty").to_string(),
            "BMC account operation is invalid\n  condition: UserName must not be empty"
        );
        assert_eq!(
            AccountError::NotFound("1".into()).to_string(),
            "BMC account was not found\n  account ID: 1"
        );
    }

    #[test]
    fn patch_accepts_partial_updates_and_rejects_invalid_fields() {
        assert_eq!(
            parse_patch(&serde_json::json!({"UserName":"admin"})).unwrap(),
            (Some("admin"), None)
        );
        assert_eq!(
            parse_patch(&serde_json::json!({"Password":""})).unwrap(),
            (None, Some(""))
        );
        for invalid in [
            serde_json::json!({}),
            serde_json::json!([]),
            serde_json::json!({"UserName":""}),
            serde_json::json!({"UserName":null}),
            serde_json::json!({"Password":42}),
            serde_json::json!({"RoleId":"Administrator"}),
        ] {
            assert!(parse_patch(&invalid).is_err());
        }
    }
}

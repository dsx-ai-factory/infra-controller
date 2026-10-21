/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

//! Preserve the server DUID independently of agent-written DHCP configuration.

use std::io::ErrorKind;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use carbide_dhcp_server::errors::DhcpError;
use carbide_rpc_utils::dhcp::{DhcpConfig, DhcpV6ServerId};
use tokio::io::AsyncWriteExt;

/// `path` locates the raw DUID sidecar beside the DHCP YAML file.
pub(super) fn path(config_path: &str) -> String {
    format!("{config_path}.duid")
}

/// `read_persisted` returns `None` only when the sidecar is absent.
/// Read failures and malformed identities retain the sidecar path and cause.
pub(super) async fn read_persisted(config_path: &str) -> Result<Option<DhcpV6ServerId>, DhcpError> {
    let identity_path = path(config_path);
    let result = match tokio::fs::read(&identity_path).await {
        Ok(bytes) => DhcpV6ServerId::try_from(bytes)
            .map(Some)
            .map_err(Into::into),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    };
    result.map_err(|source| DhcpError::ConfigFile {
        path: identity_path,
        source: Box::new(source),
    })
}

/// `resolve` selects the saved identity, then the live YAML's identity, then
/// the candidate's identity. Only absent files permit the next choice;
/// unreadable or malformed state is an error. Neither file is changed.
pub(super) async fn resolve(
    config_path: &str,
    candidate: &DhcpConfig,
) -> Result<DhcpV6ServerId, DhcpError> {
    if let Some(identity) = read_persisted(config_path).await? {
        return Ok(identity);
    }

    // Capture the live identity before an IPv6-only update removes its IPv4 seed.
    match tokio::fs::read_to_string(config_path).await {
        Ok(yaml) => {
            let previous: DhcpConfig =
                serde_yaml::from_str(&yaml).map_err(|error| DhcpError::ConfigFile {
                    path: config_path.to_string(),
                    source: Box::new(error.into()),
                })?;
            previous
                .server_identifier()
                .map_err(|error| DhcpError::ConfigFile {
                    path: config_path.to_string(),
                    source: Box::new(error.into()),
                })
        }
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(candidate.server_identifier()?),
        Err(error) => Err(DhcpError::ConfigFile {
            path: config_path.to_string(),
            source: Box::new(error.into()),
        }),
    }
}

/// `persist` installs and syncs an identity without replacing an existing one.
/// An existing identity must match. The containing directory is synced before
/// success, so callers may replace configuration or start packet service.
pub(super) async fn persist(config_path: &str, identity: &DhcpV6ServerId) -> Result<(), DhcpError> {
    let identity_path = path(config_path);
    persist_at(&identity_path, identity)
        .await
        .map_err(|source| match source {
            DhcpError::IoError(_) => DhcpError::ConfigFile {
                path: identity_path,
                source: Box::new(source),
            },
            source => source,
        })
}

async fn persist_at(identity_path: &str, identity: &DhcpV6ServerId) -> Result<(), DhcpError> {
    match tokio::fs::read(identity_path).await {
        Ok(bytes) => {
            check_identity(bytes, identity, identity_path)?;
            return sync_parent(identity_path).await;
        }
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let staged_path = stage_identity(identity_path, identity).await?;
    install_staged_identity(identity_path, identity, staged_path).await
}

async fn install_staged_identity(
    identity_path: &str,
    identity: &DhcpV6ServerId,
    staged_path: tempfile::TempPath,
) -> Result<(), DhcpError> {
    // Linking the complete file installs it atomically without replacing an
    // existing identity, including one created between the read and this call.
    match tokio::fs::hard_link(&staged_path, identity_path).await {
        Ok(()) => {}
        Err(error) if error.kind() == ErrorKind::AlreadyExists => {
            check_identity(
                tokio::fs::read(identity_path).await?,
                identity,
                identity_path,
            )?;
        }
        Err(error) => return Err(error.into()),
    }
    if let Err(error) = tokio::fs::remove_file(&staged_path).await {
        // The installed identity is complete. A leftover temporary link must
        // not prevent syncing its directory or make the caller reject it.
        tracing::warn!(
            path = %staged_path.display(),
            %error,
            "could not remove temporary DHCP server identity"
        );
    }
    sync_parent(identity_path).await
}

async fn stage_identity(
    identity_path: &str,
    identity: &DhcpV6ServerId,
) -> Result<tempfile::TempPath, DhcpError> {
    let parent = identity_directory(identity_path);
    // Each writer needs its own inode. Reopening a shared temporary name
    // could truncate the installed DUID after another writer hard-links it.
    let staged = tempfile::Builder::new()
        // The agent also reads the sidecar, but only its owner may write it.
        .permissions(std::fs::Permissions::from_mode(0o644))
        .tempfile_in(parent)
        .map_err(|error| DhcpError::ConfigFile {
            path: parent.display().to_string(),
            source: Box::new(error.into()),
        })?;
    write_staged_identity(staged, identity).await
}

async fn write_staged_identity(
    staged: tempfile::NamedTempFile,
    identity: &DhcpV6ServerId,
) -> Result<tempfile::TempPath, DhcpError> {
    let (file, staged_path) = staged.into_parts();
    let mut file = tokio::fs::File::from_std(file);
    let write_result = async {
        file.write_all(identity.as_bytes()).await?;
        // Tokio can defer write errors even when `sync_all` succeeds. Flush
        // first so a failed write cannot install an incomplete identity.
        file.flush().await?;
        file.sync_all().await
    }
    .await;
    write_result.map_err(|error| DhcpError::ConfigFile {
        path: staged_path.display().to_string(),
        source: Box::new(error.into()),
    })?;
    Ok(staged_path)
}

fn identity_directory(identity_path: &str) -> &Path {
    Path::new(identity_path)
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

async fn sync_parent(identity_path: &str) -> Result<(), DhcpError> {
    let parent = identity_directory(identity_path);
    let sync_result = async { tokio::fs::File::open(parent).await?.sync_all().await }.await;
    sync_result.map_err(|error| DhcpError::ConfigFile {
        path: parent.display().to_string(),
        source: Box::new(error.into()),
    })?;
    Ok(())
}

fn check_identity(bytes: Vec<u8>, identity: &DhcpV6ServerId, path: &str) -> Result<(), DhcpError> {
    let saved = DhcpV6ServerId::try_from(bytes).map_err(|error| DhcpError::ConfigFile {
        path: path.to_string(),
        source: Box::new(error.into()),
    })?;
    if &saved != identity {
        return Err(DhcpError::ConfigFile {
            path: path.to_string(),
            source: Box::new(DhcpError::InvalidInput(
                "DHCPv6 server identity changed while saving".to_string(),
            )),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn first_config_preserves_explicit_identity_with_ipv4() {
        let directory = tempfile::tempdir().unwrap();
        let config_path = directory.path().join("dhcp.yaml").display().to_string();
        let explicit = DhcpV6ServerId::from_remote_id("test-dpu").unwrap();
        let candidate = DhcpConfig {
            carbide_dhcp_server: Some("192.0.2.1".parse().unwrap()),
            carbide_provisioning_server_ipv4: Some("192.0.2.2".parse().unwrap()),
            dhcpv6_server_id: Some(explicit.clone()),
            ..Default::default()
        };
        assert_eq!(resolve(&config_path, &candidate).await.unwrap(), explicit);
    }

    #[tokio::test]
    async fn preserves_legacy_identity_after_ipv4_removal_and_restart() {
        let directory = tempfile::tempdir().unwrap();
        let config_path = directory.path().join("dhcp.yaml").display().to_string();
        let legacy = DhcpConfig {
            carbide_dhcp_server: Some("192.0.2.1".parse().unwrap()),
            carbide_provisioning_server_ipv4: Some("192.0.2.2".parse().unwrap()),
            ..Default::default()
        };
        tokio::fs::write(&config_path, serde_yaml::to_string(&legacy).unwrap())
            .await
            .unwrap();
        let candidate = DhcpConfig {
            dhcpv6_server_id: Some(
                DhcpV6ServerId::try_from(b"\x00\x02\x00\x00\x16\x47\xc0\x00\x02\x63".to_vec())
                    .unwrap(),
            ),
            ..Default::default()
        };
        let identity = resolve(&config_path, &candidate).await.unwrap();
        assert_eq!(identity, legacy.server_identifier().unwrap());
        persist(&config_path, &identity).await.unwrap();
        assert_eq!(
            std::fs::metadata(path(&config_path))
                .unwrap()
                .permissions()
                .mode()
                & 0o022,
            0,
            "only the owner may write the saved identity"
        );
        tokio::fs::write(&config_path, serde_yaml::to_string(&candidate).unwrap())
            .await
            .unwrap();
        assert_eq!(resolve(&config_path, &candidate).await.unwrap(), identity);
    }

    #[tokio::test]
    async fn unreadable_live_config_keeps_path_and_io_error() {
        let directory = tempfile::tempdir().unwrap();
        let config_path = directory.path().join("dhcp.yaml").display().to_string();
        tokio::fs::create_dir(&config_path).await.unwrap();

        let error = resolve(&config_path, &DhcpConfig::default())
            .await
            .unwrap_err();
        assert!(error.to_string().contains(&config_path));
        assert!(matches!(
            error,
            DhcpError::ConfigFile { source, .. } if matches!(*source, DhcpError::IoError(_))
        ));
    }

    #[tokio::test]
    async fn invalid_or_unreadable_identity_is_not_replaced() {
        for corrupt in [true, false] {
            let directory = tempfile::tempdir().unwrap();
            let config_path = directory.path().join("dhcp.yaml").display().to_string();
            let identity = DhcpV6ServerId::from_remote_id("test-dpu").unwrap();
            if corrupt {
                tokio::fs::write(path(&config_path), b"invalid")
                    .await
                    .unwrap();
            } else {
                // A directory cannot be read as a sidecar, even as root.
                tokio::fs::create_dir(path(&config_path)).await.unwrap();
            }
            let persist_error = persist(&config_path, &identity).await.unwrap_err();
            let resolve_error = resolve(&config_path, &DhcpConfig::default())
                .await
                .unwrap_err();
            for error in [persist_error, resolve_error] {
                assert!(error.to_string().contains(&path(&config_path)));
                assert!(std::error::Error::source(&error).is_some());
            }
            if corrupt {
                assert_eq!(
                    tokio::fs::read(path(&config_path)).await.unwrap(),
                    b"invalid"
                );
            }
        }
    }

    #[tokio::test]
    async fn failed_staged_write_keeps_the_path_and_io_error() {
        let directory = tempfile::tempdir().unwrap();
        let staged = tempfile::NamedTempFile::new_in(directory.path()).unwrap();
        let staged_path = staged.path().to_path_buf();
        let identity = DhcpV6ServerId::from_remote_id("test-dpu").unwrap();

        // A read-only descriptor produces a real deferred write failure even
        // as root. Syncing the descriptor alone does not report that failure.
        let read_only = std::fs::File::open(&staged_path).unwrap();
        let staged = tempfile::NamedTempFile::from_parts(read_only, staged.into_temp_path());
        let error = write_staged_identity(staged, &identity)
            .await
            .expect_err("reject a failed identity write");
        assert!(matches!(
            error,
            DhcpError::ConfigFile { path, source }
                if Path::new(&path) == staged_path
                    && matches!(*source, DhcpError::IoError(ref error)
                        if error.raw_os_error() == Some(libc::EBADF))
        ));
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn competing_writers_cannot_change_an_installed_identity() {
        let directory = tempfile::tempdir().expect("create identity directory");
        let config_path = directory.path().join("dhcp.yaml").display().to_string();
        let identity_path = path(&config_path);
        let winner = DhcpV6ServerId::from_remote_id("first-dpu").unwrap();
        let loser = DhcpV6ServerId::from_remote_id("second-dpu").unwrap();

        // Both writers found no sidecar. Pause the first after linking so
        // staging the second must leave that installed inode untouched.
        let first_staged = stage_identity(&identity_path, &winner)
            .await
            .expect("stage first identity");
        tokio::fs::hard_link(&first_staged, &identity_path)
            .await
            .expect("install first identity");

        let cases = [
            (&loser, false, "reject a different identity"),
            (&winner, true, "accept the same identity"),
        ];
        for (identity, accepted, scenario) in cases {
            let staged = stage_identity(&identity_path, identity)
                .await
                .expect("stage competing identity");
            assert_ne!(first_staged.as_os_str(), staged.as_os_str());
            assert_eq!(
                tokio::fs::read(&identity_path).await.unwrap(),
                winner.as_bytes(),
                "{scenario}: staging must not overwrite the installed inode"
            );
            let result = install_staged_identity(&identity_path, identity, staged).await;
            assert_eq!(result.is_ok(), accepted, "{scenario}: {result:?}");
            if let Err(error) = result {
                assert!(matches!(
                    error,
                    DhcpError::ConfigFile { path, source }
                        if path == identity_path && matches!(*source, DhcpError::InvalidInput(_))
                ));
            }
            assert_eq!(
                tokio::fs::read(&identity_path).await.unwrap(),
                winner.as_bytes(),
                "{scenario}: installation must not replace the saved identity"
            );
            assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 2);
        }
        drop(first_staged);
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
        persist(&config_path, &winner)
            .await
            .expect("retry the installed identity");
    }
}

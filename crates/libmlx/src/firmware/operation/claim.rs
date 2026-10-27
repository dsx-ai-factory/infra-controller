// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Serialize supervised firmware work and participating direct Scout commands
//! across the host because PCI locators do not establish independent reset
//! domains. The marker survives process loss until the caller can establish
//! command completion or an operator reconciles the uncertain work.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use serde::Serialize;

use super::SupervisorConfig;
use super::identity::current_boot_id;
use super::launch::ensure_state_directory;

/// `DirectMlxClaim` excludes supervised firmware work while a direct MLX
/// command runs. Dropping the claim leaves its marker, including after a panic
/// or process loss. The caller must release it only after every command has
/// stopped; a completed command may still have returned a vendor error.
pub struct DirectMlxClaim(MutationClaim);

impl DirectMlxClaim {
    /// `acquire` atomically claims the same host marker used by supervisors.
    /// It validates or creates the configured private state root and records
    /// the current boot and diagnostic device selector. An existing marker
    /// rejects the call without starting any hardware work.
    pub fn acquire(config: &SupervisorConfig, device: &str) -> io::Result<Self> {
        let boot_id = current_boot_id()?;
        ensure_state_directory(&config.state_directory)?;
        MutationClaim::acquire(
            &config.state_directory,
            "direct-scout-command",
            device,
            &boot_id,
        )
        .map(Self)
    }

    /// `release` removes exclusion after all direct commands have stopped.
    /// An unlink failure retains the marker. A directory-sync failure is
    /// returned even though the marker has already been removed.
    pub fn release(self) -> io::Result<()> {
        self.0.release()
    }
}

/// A durable host-wide claim. Dropping it deliberately leaves the marker.
pub(super) struct MutationClaim {
    path: PathBuf,
    directory: File,
}

#[derive(Serialize)]
struct ClaimRecord<'a> {
    operation_id: &'a str,
    device_identity: &'a str,
    boot_id: &'a str,
}

impl MutationClaim {
    /// Persist exclusion before admission. An existing marker requires the
    /// previous owner to finish or an operator to establish a safe recovery.
    pub(super) fn acquire(
        root: &Path,
        operation_id: &str,
        device_identity: &str,
        boot_id: &str,
    ) -> io::Result<Self> {
        let path = root.join("mutation.json");
        let directory = File::open(root)?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(nix::libc::O_NOFOLLOW)
            .open(&path)?;
        // Even an incomplete marker blocks admission. A write or sync failure
        // cannot erase evidence that this process might have claimed the host.
        let encoded = serde_json::to_vec(&ClaimRecord {
            operation_id,
            device_identity,
            boot_id,
        })?;
        file.write_all(&encoded)?;
        file.sync_all()?;
        directory.sync_all()?;
        Ok(Self { path, directory })
    }

    /// Release only after every tool has stopped. Supervised work also requires
    /// a durable safe terminal receipt; direct commands do not create receipts.
    pub(super) fn release(self) -> io::Result<()> {
        std::fs::remove_file(&self.path)?;
        self.directory.sync_all()
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::{PermissionsExt, symlink};

    use super::*;

    /// Claim tests need trusted ancestors even when the build's scratch root
    /// permits other users to write.
    fn fixture_root() -> tempfile::TempDir {
        let root = match std::env::var_os("LIBMLX_TEST_STATE_DIRECTORY") {
            Some(parent) => tempfile::tempdir_in(parent),
            None => tempfile::tempdir(),
        }
        .expect("create private claim fixture");
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        root
    }

    /// Losing the Rust owner cannot make another PCI locator eligible while an
    /// old tool may still be mutating shared hardware.
    #[test]
    fn dropped_claim_blocks_other_devices() {
        let root = tempfile::tempdir().unwrap();
        let claim = MutationClaim::acquire(root.path(), "first", "0000:01:00", "boot").unwrap();
        drop(claim);

        assert_eq!(
            MutationClaim::acquire(root.path(), "second", "0000:02:00", "boot")
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::AlreadyExists,
        );
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(root.path().join("mutation.json")).unwrap())
                .unwrap();
        assert_eq!(saved["operation_id"], "first");
    }

    /// Only the explicit safe completion path removes exclusion.
    #[test]
    fn completed_claim_allows_next_operation() {
        let root = tempfile::tempdir().unwrap();
        MutationClaim::acquire(root.path(), "first", "0000:01:00", "boot")
            .unwrap()
            .release()
            .unwrap();
        MutationClaim::acquire(root.path(), "second", "0000:02:00", "boot").unwrap();
    }

    /// Direct Scout commands and supervisor admission must compete for one
    /// marker, even when their device selectors and process owners differ.
    #[test]
    fn direct_and_supervised_claims_have_one_winner() {
        let root = fixture_root();
        let config = SupervisorConfig {
            state_directory: root.path().join("firmware"),
            ..SupervisorConfig::default()
        };
        ensure_state_directory(&config.state_directory).unwrap();
        let barrier = std::sync::Barrier::new(2);
        let (direct_won, supervised_won) = std::thread::scope(|scope| {
            let direct = scope.spawn(|| {
                barrier.wait();
                DirectMlxClaim::acquire(&config, "0000:01:00.0").is_ok()
            });
            let supervised = scope.spawn(|| {
                barrier.wait();
                MutationClaim::acquire(
                    &config.state_directory,
                    "firmware-operation",
                    "0000:02:00",
                    "boot",
                )
                .is_ok()
            });
            (direct.join().unwrap(), supervised.join().unwrap())
        });
        assert_ne!(direct_won, supervised_won);
        assert!(config.state_directory.join("mutation.json").is_file());
    }

    /// Existing roots still require validation before direct acquisition;
    /// create-new on the marker alone would follow a symlinked root.
    #[test]
    fn direct_claim_rejects_existing_symlink_root_without_writing_marker() {
        let root = fixture_root();
        let target = root.path().join("target");
        ensure_state_directory(&target).unwrap();
        let linked_root = root.path().join("linked-root");
        symlink(&target, &linked_root).unwrap();
        let config = SupervisorConfig {
            state_directory: linked_root.clone(),
            ..SupervisorConfig::default()
        };

        let error = DirectMlxClaim::acquire(&config, "0000:01:00.0")
            .err()
            .expect("direct acquisition rejects a symlink root");
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert!(!target.join("mutation.json").exists());
        assert_eq!(std::fs::read_link(linked_root).unwrap(), target);
    }
}

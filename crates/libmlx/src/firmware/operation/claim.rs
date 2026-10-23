// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Serialize supervised mutations across the host because PCI locators do not
//! establish independent reset domains. The marker survives process loss; a lock must
//! never authorize another tool while an earlier one may still be running.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use serde::Serialize;

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

    /// Release only after a durable safe terminal receipt and tool completion.
    pub(super) fn release(self) -> io::Result<()> {
        std::fs::remove_file(&self.path)?;
        self.directory.sync_all()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}

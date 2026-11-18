// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Direct Scout MLX commands share the supervisor's durable host claim.

use std::error::Error;
use std::io;

use libmlx::firmware::error::AttemptError;
use libmlx::firmware::operation::{
    DirectMlxClaim, SupervisorConfig, has_unresolved_firmware_claim,
};
use libmlx::lockdown::error::MlxError;
use libmlx::runner::error::MlxRunnerError;

#[cfg(all(test, target_os = "linux"))]
mod process_tests;

/// Keep exclusion across discovery, tool retries, and response conversion.
/// Callers inspect errors before flattening them into protobuf responses.
pub(super) struct DirectMlxAccess {
    claim: DirectMlxClaim,
    process_unsettled: bool,
}

/// Acquisition failure, unknown process completion, and release failure require
/// different caller decisions; none of them permits deleting another claim.
#[derive(Debug, thiserror::Error)]
pub(super) enum DirectMlxAccessError {
    #[error("cannot acquire direct MLX hardware access: {0}")]
    Acquire(#[source] io::Error),
    #[error(
        "direct MLX process completion is unknown; hardware access remains blocked pending operator reconciliation"
    )]
    ProcessUnsettled,
    #[error("failed to release direct MLX hardware access: {0}")]
    Release(#[source] io::Error),
}

impl DirectMlxAccess {
    pub(super) fn acquire(
        config: &SupervisorConfig,
        device: &str,
    ) -> Result<Self, DirectMlxAccessError> {
        let claim =
            DirectMlxClaim::acquire(config, device).map_err(DirectMlxAccessError::Acquire)?;
        Ok(Self {
            claim,
            process_unsettled: false,
        })
    }

    /// `acquire_or_defer` claims the host for a command that a later poll can
    /// repeat. It returns `None` without touching the card when other hardware
    /// work holds the claim, when that claim's marker is unsafe or unreadable,
    /// or when acquisition fails for another reason; the log says which.
    pub(super) fn acquire_or_defer(config: &SupervisorConfig, device: &str) -> Option<Self> {
        match Self::acquire(config, device) {
            Ok(access) => Some(access),
            Err(DirectMlxAccessError::Acquire(error))
                if error.kind() == io::ErrorKind::AlreadyExists =>
            {
                match has_unresolved_firmware_claim(config) {
                    Ok(_) => tracing::debug!(
                        pci_name = device,
                        "deferring automatic NIC command while hardware access is occupied"
                    ),
                    Err(error) => {
                        tracing::warn!(pci_name = device, %error, "deferring automatic NIC command because the hardware claim is unsafe or unreadable")
                    }
                }
                None
            }
            Err(error) => {
                tracing::warn!(pci_name = device, %error, "deferring automatic NIC command without probing hardware");
                None
            }
        }
    }

    /// An ordinary failed command has stopped and may release the host. A
    /// lost process result must keep the marker, even if a later operation
    /// succeeded or the caller converts this error into a boolean.
    pub(super) fn observe_error(&mut self, error: &(dyn Error + 'static)) {
        let mut current = Some(error);
        while let Some(error) = current {
            if matches!(
                error.downcast_ref::<MlxRunnerError>(),
                Some(MlxRunnerError::ProcessUnsettled { .. })
            ) || matches!(
                error.downcast_ref::<MlxError>(),
                Some(MlxError::ProcessUnsettled { .. })
            ) || matches!(
                error.downcast_ref::<AttemptError>(),
                Some(AttemptError::Started { status, cleanup_errors, .. })
                    if status.is_none() || !cleanup_errors.is_empty()
            ) {
                self.process_unsettled = true;
                return;
            }

            current = error.source();
        }
    }

    pub(super) fn finish(self) -> Result<(), DirectMlxAccessError> {
        if self.process_unsettled {
            return Err(DirectMlxAccessError::ProcessUnsettled);
        }
        self.claim.release().map_err(DirectMlxAccessError::Release)
    }
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::process::ExitStatusExt;

    use libmlx::device::discovery::DeviceDiscoveryError;
    use libmlx::firmware::operation::has_unresolved_firmware_claim;
    use libmlx::profile::error::MlxProfileError;

    use super::*;

    /// Vendor failure must release exclusion, while uncertainty survives the
    /// same error wrappers used by discovery and profile synchronization.
    #[test]
    fn direct_access_preserves_only_unsettled_process_claims() {
        let cases: [(&str, Box<dyn Error>, bool); 6] = [
            (
                "settled vendor error",
                Box::new(MlxError::CommandFailed("vendor rejected command".into())),
                false,
            ),
            (
                "lockdown lost completion",
                Box::new(MlxError::ProcessUnsettled {
                    operation: "enable",
                    source: io::Error::other("wait failed"),
                }),
                true,
            ),
            (
                "profile lost completion",
                Box::new(MlxProfileError::Runner {
                    error: MlxRunnerError::ProcessUnsettled {
                        command: "mlxconfig".into(),
                        source: io::Error::other("wait failed"),
                    },
                }),
                true,
            ),
            (
                "discovery lost completion",
                Box::new(DeviceDiscoveryError::DirectQuery(
                    MlxRunnerError::ProcessUnsettled {
                        command: "mlxfwmanager".into(),
                        source: io::Error::other("wait failed"),
                    },
                )),
                true,
            ),
            (
                "bounded inventory cleanup failed",
                Box::new(DeviceDiscoveryError::QueryAttempt(AttemptError::Started {
                    command: "mlxfwmanager".into(),
                    status: Some(std::process::ExitStatus::from_raw(0)),
                    stdout: Vec::new(),
                    stderr: Vec::new(),
                    reason: libmlx::firmware::error::AttemptFailure::OutputLimit,
                    cleanup_errors: vec![io::Error::other("descendant still alive")],
                })),
                true,
            ),
            (
                "bounded inventory exited with a vendor error",
                Box::new(DeviceDiscoveryError::QueryAttempt(AttemptError::Started {
                    command: "mlxfwmanager".into(),
                    status: Some(std::process::ExitStatus::from_raw(1 << 8)),
                    stdout: Vec::new(),
                    stderr: Vec::new(),
                    reason: libmlx::firmware::error::AttemptFailure::Nonzero,
                    cleanup_errors: Vec::new(),
                })),
                false,
            ),
        ];
        for (scenario, error, retains_claim) in cases {
            // Supervisor state rejects writable, non-sticky ancestors even
            // when the build uses one for ordinary temporary files.
            let root = match std::env::var_os("LIBMLX_TEST_STATE_DIRECTORY") {
                Some(parent) => tempfile::tempdir_in(parent),
                None => tempfile::tempdir(),
            }
            .expect("create private access fixture");
            std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
            let config = SupervisorConfig {
                state_directory: root.path().join("firmware"),
                ..SupervisorConfig::default()
            };
            let mut access = DirectMlxAccess::acquire(&config, "card").unwrap();
            access.observe_error(error.as_ref());
            assert_eq!(access.finish().is_err(), retains_claim, "{scenario}");
            assert_eq!(
                has_unresolved_firmware_claim(&config).unwrap(),
                retains_claim,
                "{scenario}"
            );
        }
    }
}

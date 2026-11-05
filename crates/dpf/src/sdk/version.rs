/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use std::time::Duration;

use semver::Version;

use super::{DPF_OPERATOR_CONFIG_NAME, DpfSdk};
use crate::DpfError;
use crate::repository::DpfOperatorConfigRepository;

const MIN_PER_DPU_BMC_CREDENTIALS_VERSION: Version = Version::new(26, 8, 0);
// Allow at most 10 seconds of SDK startup for this single read, including request waits.
const OPERATOR_VERSION_READ_TIMEOUT: Duration = Duration::from_secs(10);

impl<R, L> DpfSdk<R, L> {
    /// Installed operator version read from `DPFOperatorConfig.status.version` at construction.
    /// An optional leading `v` is removed. A missing, unreadable, invalid, or timed-out version
    /// fails SDK construction.
    /// This cached value is not refreshed during the SDK's lifetime.
    pub fn operator_version(&self) -> &Version {
        &self.operator_version
    }

    /// Whether the cached version supports per-DPU BMC credential Secrets (DPF >= 26.8.0).
    /// Prereleases of 26.8.0 do not qualify; prereleases of higher versions do.
    /// This check performs no Kubernetes I/O.
    pub fn supports_per_dpu_bmc_credentials(&self) -> bool {
        self.operator_version
            .cmp_precedence(&MIN_PER_DPU_BMC_CREDENTIALS_VERSION)
            .is_ge()
    }
}

pub(super) async fn read_operator_version(
    repo: &dyn DpfOperatorConfigRepository,
    namespace: &str,
) -> Result<Version, DpfError> {
    let version = tokio::time::timeout(
        OPERATOR_VERSION_READ_TIMEOUT,
        repo.get_version(DPF_OPERATOR_CONFIG_NAME, namespace),
    )
    .await
    .map_err(|_| {
        DpfError::timeout(
            "read DPF operator version",
            format!(
                "startup lookup exceeded {} seconds",
                OPERATOR_VERSION_READ_TIMEOUT.as_secs()
            ),
        )
    })??;
    let parsed = Version::parse(version.strip_prefix('v').unwrap_or(&version))
        .map_err(|error| DpfError::InvalidState(format!(
            "DPFOperatorConfig {DPF_OPERATOR_CONFIG_NAME} has invalid status.version {version:?}: {error}"
        )))?;
    tracing::info!(version = %parsed, "Cached installed DPF operator version");
    Ok(parsed)
}

#[cfg(test)]
mod tests;

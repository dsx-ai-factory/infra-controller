/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use semver::Version;

use super::{DPF_OPERATOR_CONFIG_NAME, DpfSdk};
use crate::repository::DpfOperatorConfigRepository;

const MIN_PER_DPU_BMC_CREDENTIALS_VERSION: Version = Version::new(26, 8, 0);

impl<R, L> DpfSdk<R, L> {
    /// Installed operator version read from `DPFOperatorConfig.status.version` at construction.
    /// An optional leading `v` is removed. Missing, unreadable, or invalid versions return `None`.
    /// This cached value is not refreshed during the SDK's lifetime.
    pub fn operator_version(&self) -> Option<&Version> {
        self.operator_version.as_ref()
    }

    /// Whether the cached version supports per-DPU BMC credential Secrets (DPF >= 26.8.0).
    /// Prereleases of 26.8.0 do not qualify; prereleases of higher versions do. Unknown versions
    /// return `false`. This check performs no Kubernetes I/O.
    pub fn supports_per_dpu_bmc_credentials(&self) -> bool {
        self.operator_version.as_ref().is_some_and(|version| {
            version
                .cmp_precedence(&MIN_PER_DPU_BMC_CREDENTIALS_VERSION)
                .is_ge()
        })
    }
}

pub(super) async fn read_operator_version(
    repo: &dyn DpfOperatorConfigRepository,
    namespace: &str,
) -> Option<Version> {
    let version = match repo.get_version(DPF_OPERATOR_CONFIG_NAME, namespace).await {
        Ok(version) => version,
        Err(error) => {
            tracing::warn!(%error, "Could not read DPF operator version; version-gated capabilities disabled");
            return None;
        }
    };
    let Some(version) = version else {
        tracing::warn!("DPF operator version is missing; version-gated capabilities disabled");
        return None;
    };
    match Version::parse(version.strip_prefix('v').unwrap_or(&version)) {
        Ok(parsed) => {
            tracing::info!(version = %parsed, "Cached installed DPF operator version");
            Some(parsed)
        }
        Err(error) => {
            tracing::warn!(%version, %error, "Invalid DPF operator version; version-gated capabilities disabled");
            None
        }
    }
}

#[cfg(test)]
mod tests;

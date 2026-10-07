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

use std::path::PathBuf;

use carbide_utils::config::as_duration;
use chrono::Duration;
use duration_str::deserialize_duration_chrono;
use serde::{Deserialize, Serialize};

/// Global firmware management settings controlling
/// update policies, concurrency, and retry behavior.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FirmwareGlobal {
    /// Enables automatic host firmware updates via the
    /// background firmware manager.
    #[serde(default)]
    pub autoupdate: bool,
    /// Host model names to force-enable autoupdate on,
    /// regardless of the global `autoupdate` setting.
    #[serde(default)]
    pub host_enable_autoupdate: Vec<String>,
    /// Host model names to force-disable autoupdate on,
    /// regardless of the global `autoupdate` setting.
    #[serde(default)]
    pub host_disable_autoupdate: Vec<String>,
    /// Frequency at which the firmware manager checks for
    /// and applies updates.
    /// Default is 30 seconds.
    #[serde(
        default = "FirmwareGlobal::run_interval_default",
        deserialize_with = "deserialize_duration_chrono",
        serialize_with = "as_duration"
    )]
    pub run_interval: Duration,
    /// Maximum concurrent firmware uploads allowed.
    /// Default is 4.
    #[serde(default = "FirmwareGlobal::max_uploads_default")]
    pub max_uploads: usize,
    /// Maximum concurrent firmware flashing operations
    /// across all machines.
    /// Default is 16.
    #[serde(default = "FirmwareGlobal::concurrency_limit_default")]
    pub concurrency_limit: usize,
    /// Local directory where firmware binaries are stored.
    /// Default probes `/opt/nico/firmware` first (helm-chart layout), then
    /// falls back to `/opt/carbide/firmware` (forged kustomize layout) if
    /// the first doesn't exist. A deployer can pin either explicitly.
    #[serde(default = "FirmwareGlobal::firmware_directory_default")]
    pub firmware_directory: PathBuf,
    /// Writable directory used to cache downloaded firmware artifacts.
    #[serde(default = "FirmwareGlobal::firmware_download_cache_directory_default")]
    pub firmware_download_cache_directory: PathBuf,
    /// Delay before retrying a failed host firmware
    /// upgrade.
    /// Default is 60 minutes.
    #[serde(
        default = "FirmwareGlobal::host_firmware_upgrade_retry_interval_default",
        deserialize_with = "deserialize_duration_chrono",
        serialize_with = "as_duration"
    )]
    pub host_firmware_upgrade_retry_interval: Duration,
    /// Requires manual tagging of instances before
    /// firmware updates are applied.
    #[serde(default = "FirmwareGlobal::instance_updates_manual_tagging_default")]
    pub instance_updates_manual_tagging: bool,
    /// Disables retry logic after BMC resets during
    /// firmware operations.
    #[serde(default)]
    pub no_reset_retries: bool,
    /// Delay after GPU reboot before the HGX BMC can be
    /// accessed again.
    /// Default is 30 seconds.
    #[serde(
        default = "FirmwareGlobal::hgx_bmc_gpu_reboot_delay_default",
        deserialize_with = "deserialize_duration_chrono",
        serialize_with = "as_duration"
    )]
    pub hgx_bmc_gpu_reboot_delay: Duration,
    /// Forces all firmware upgrades to require explicit
    /// administrator approval.
    #[serde(default)]
    pub requires_manual_upgrade: bool,
    #[serde(default = "FirmwareGlobal::max_concurrent_bfb_copies_default")]
    pub max_concurrent_bfb_copies: usize,
}

impl FirmwareGlobal {
    #[cfg(any(test, feature = "test-support"))]
    pub fn test_default() -> Self {
        FirmwareGlobal {
            autoupdate: true,
            host_enable_autoupdate: vec![],
            host_disable_autoupdate: vec![],
            max_uploads: 4,
            run_interval: Duration::seconds(5),
            concurrency_limit: FirmwareGlobal::concurrency_limit_default(),
            firmware_directory: PathBuf::default(),
            firmware_download_cache_directory: PathBuf::default(),
            host_firmware_upgrade_retry_interval: Self::get_retry_interval(),
            instance_updates_manual_tagging: false,
            no_reset_retries: false,
            hgx_bmc_gpu_reboot_delay: FirmwareGlobal::hgx_bmc_gpu_reboot_delay_default(),
            requires_manual_upgrade: false,
            max_concurrent_bfb_copies: FirmwareGlobal::max_concurrent_bfb_copies_default(),
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn get_retry_interval() -> Duration {
        Duration::seconds(1)
    }
}

impl FirmwareGlobal {
    pub fn instance_updates_manual_tagging_default() -> bool {
        true
    }
    pub fn run_interval_default() -> Duration {
        Duration::seconds(30)
    }
    pub fn max_uploads_default() -> usize {
        4
    }
    pub fn concurrency_limit_default() -> usize {
        16
    }
    pub fn firmware_directory_default() -> PathBuf {
        // Prefer the helm-chart layout (`/opt/nico/firmware`); fall back to
        // the forged-kustomize layout (`/opt/carbide/firmware`) if the
        // nico-style directory doesn't exist on disk yet.
        if std::path::Path::new("/opt/nico/firmware").exists() {
            return PathBuf::from("/opt/nico/firmware");
        }
        PathBuf::from("/opt/carbide/firmware")
    }
    pub fn firmware_download_cache_directory_default() -> PathBuf {
        PathBuf::from("/mnt/persistence/fw/download-cache")
    }
    pub fn host_firmware_upgrade_retry_interval_default() -> Duration {
        Duration::minutes(60)
    }
    pub fn hgx_bmc_gpu_reboot_delay_default() -> Duration {
        Duration::seconds(30)
    }
    pub fn max_concurrent_bfb_copies_default() -> usize {
        10
    }
}

impl Default for FirmwareGlobal {
    fn default() -> FirmwareGlobal {
        FirmwareGlobal {
            autoupdate: false,
            host_enable_autoupdate: vec![],
            host_disable_autoupdate: vec![],
            run_interval: FirmwareGlobal::run_interval_default(),
            max_uploads: FirmwareGlobal::max_uploads_default(),
            concurrency_limit: FirmwareGlobal::concurrency_limit_default(),
            firmware_directory: FirmwareGlobal::firmware_directory_default(),
            firmware_download_cache_directory:
                FirmwareGlobal::firmware_download_cache_directory_default(),
            host_firmware_upgrade_retry_interval:
                FirmwareGlobal::host_firmware_upgrade_retry_interval_default(),
            instance_updates_manual_tagging: false,
            no_reset_retries: false,
            hgx_bmc_gpu_reboot_delay: FirmwareGlobal::hgx_bmc_gpu_reboot_delay_default(),
            requires_manual_upgrade: false,
            max_concurrent_bfb_copies: FirmwareGlobal::max_concurrent_bfb_copies_default(),
        }
    }
}

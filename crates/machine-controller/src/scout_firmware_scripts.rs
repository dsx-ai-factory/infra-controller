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

//! Scout firmware upgrade script resolver.

use std::fs;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};

use carbide_utils::SCOUT_FIRMWARE_SCRIPTS_DIR;
use eyre::{WrapErr, bail};
use model::firmware::FirmwareComponentType;
use serde::Deserialize;
use sha2::{Digest, Sha256};

const SCOUT_FIRMWARE_SCRIPT_FILE: &str = "upgrade.sh";
const SCOUT_FIRMWARE_METADATA_FILE: &str = "metadata.toml";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ScoutFirmwareScript {
    pub(crate) url: String,
    pub(crate) sha256: String,
    pub(crate) execution_timeout_seconds: u32,
    pub(crate) artifact_download_timeout_seconds: u32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ScoutFirmwareScriptMetadata {
    execution_timeout_seconds: NonZeroU32,
    artifact_download_timeout_seconds: NonZeroU32,
}

/// Resolves the Scout firmware script for a vendor/model/component from the
/// script directory packaged into the NICo image.
///
/// NICo uses this before creating a Scout firmware upgrade task so the task is
/// built from a known local script tree instead of trusting script URLs from
/// firmware metadata.
pub(crate) fn find_scout_script(
    pxe_public_base_url: &str,
    vendor: bmc_vendor::BMCVendor,
    model: &str,
    component_type: FirmwareComponentType,
) -> eyre::Result<Option<ScoutFirmwareScript>> {
    find_scout_script_in(
        Path::new(SCOUT_FIRMWARE_SCRIPTS_DIR),
        pxe_public_base_url,
        vendor,
        model,
        component_type,
    )
}

/// Resolves a Scout firmware script under an explicit root directory.
///
/// Keeping the root injectable makes the filesystem behavior testable while
/// preserving the same lookup path that production uses:
/// `<root>/<vendor>/<model>/<component>/upgrade.sh` plus `metadata.toml`.
fn find_scout_script_in(
    root: &Path,
    pxe_public_base_url: &str,
    vendor: bmc_vendor::BMCVendor,
    model: &str,
    component_type: FirmwareComponentType,
) -> eyre::Result<Option<ScoutFirmwareScript>> {
    let Some(selector) = ScoutFirmwareScriptSelector::new(vendor, model, component_type) else {
        return Ok(None);
    };

    if !root.exists() {
        return Ok(None);
    }

    if !root.is_dir() {
        bail!(
            "scout firmware script root is not a directory: {}",
            root.display()
        );
    }

    let component_dir = selector.component_dir(root);
    let script_path = component_dir.join(SCOUT_FIRMWARE_SCRIPT_FILE);
    let metadata_path = component_dir.join(SCOUT_FIRMWARE_METADATA_FILE);

    if !script_path.exists() && !metadata_path.exists() {
        return Ok(None);
    }

    if !script_path.is_file() {
        bail!("missing scout firmware script {}", script_path.display());
    }

    if !metadata_path.is_file() {
        bail!(
            "missing scout firmware script metadata {}",
            metadata_path.display()
        );
    }

    let metadata = read_metadata(&metadata_path)?;

    let script = fs::read(&script_path).wrap_err_with(|| {
        format!(
            "failed to read scout firmware script {}",
            script_path.display()
        )
    })?;

    if script.is_empty() {
        bail!("scout firmware script is empty: {}", script_path.display());
    }

    Ok(Some(ScoutFirmwareScript {
        url: script_url(pxe_public_base_url, &selector.relative_script_path()),
        sha256: hex::encode(Sha256::digest(&script)),
        execution_timeout_seconds: metadata.execution_timeout_seconds.get(),
        artifact_download_timeout_seconds: metadata.artifact_download_timeout_seconds.get(),
    }))
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ScoutFirmwareScriptSelector {
    vendor: String,
    model: String,
    component_type: &'static str,
}

impl ScoutFirmwareScriptSelector {
    /// Builds a normalized filesystem selector for the requested script.
    ///
    /// Unknown vendors/components and unsafe path segments return `None` so a
    /// malformed inventory value cannot escape the script root or accidentally
    /// select an `unknown` script.
    fn new(
        vendor: bmc_vendor::BMCVendor,
        model: &str,
        component_type: FirmwareComponentType,
    ) -> Option<Self> {
        if vendor.is_unknown() {
            return None;
        }

        Some(Self {
            vendor: safe_lookup_key(&vendor.to_string())?,
            model: safe_lookup_key(model)?,
            component_type: firmware_component_directory_name(component_type)?,
        })
    }

    /// Returns the component directory for this selector under the script root.
    ///
    /// The resolver uses a fixed directory convention so script approval is
    /// represented by files existing at the expected vendor/model/component
    /// path.
    fn component_dir(&self, root: &Path) -> PathBuf {
        root.join(&self.vendor)
            .join(&self.model)
            .join(self.component_type)
    }

    /// Returns the URL path fragment for the script served by PXE.
    ///
    /// NICo reads and hashes the local file, but Scout downloads the script
    /// from PXE, so both sides must derive the same relative path.
    fn relative_script_path(&self) -> String {
        format!(
            "{}/{}/{}/{}",
            self.vendor, self.model, self.component_type, SCOUT_FIRMWARE_SCRIPT_FILE
        )
    }
}

/// Normalizes a lookup value into a safe single path segment.
///
/// This keeps vendor/model strings case-insensitive while preventing path
/// traversal or accidental nested directories from host inventory data.
fn safe_lookup_key(value: &str) -> Option<String> {
    let value = value
        .trim()
        .to_ascii_lowercase()
        .split_ascii_whitespace()
        .collect::<Vec<_>>()
        .join("_");

    if is_safe_path_segment(&value) {
        Some(value)
    } else {
        None
    }
}

/// Checks whether a value is safe to use as one filesystem path segment.
///
/// "dgxh100" -> allowed
/// "poweredge_r760" -> allowed
/// "../dgxh100" -> rejected
fn is_safe_path_segment(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_'))
}

/// Converts the typed firmware component into the canonical directory name.
///
/// The filesystem layout is intentionally tied to NICo's component enum so a
/// Scout script can only be selected for a component NICo already understands.
fn firmware_component_directory_name(
    component_type: FirmwareComponentType,
) -> Option<&'static str> {
    if component_type == FirmwareComponentType::Unknown {
        None
    } else {
        Some(component_type.slug())
    }
}

/// Builds the PXE URL Scout will use to download the script.
///
/// The local resolver computes metadata and SHA256, but the task still needs a
/// reachable URL because the script executes on the Scout host, not inside NICo.
fn script_url(pxe_public_base_url: &str, relative_path: &str) -> String {
    format!(
        "{}/public/scout-firmware-scripts/{relative_path}",
        pxe_public_base_url.trim_end_matches('/')
    )
}

/// Reads and parses the per-script metadata file.
///
/// Timeouts live beside the script because different components may require
/// different execution and artifact download limits without reintroducing a
/// central catalog.
fn read_metadata(path: &Path) -> eyre::Result<ScoutFirmwareScriptMetadata> {
    let metadata = fs::read_to_string(path).wrap_err_with(|| {
        format!(
            "failed to read scout firmware script metadata {}",
            path.display()
        )
    })?;

    toml::from_str(&metadata).wrap_err_with(|| {
        format!(
            "failed to parse scout firmware script metadata {}",
            path.display()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_PXE_PUBLIC_BASE_URL: &str = "http://carbide-pxe.forge:8080";

    #[test]
    fn repository_tree_resolves_current_cx7_script_case_insensitively() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../pxe/scout-firmware-scripts");

        let script = find_scout_script_in(
            &root,
            TEST_PXE_PUBLIC_BASE_URL,
            bmc_vendor::BMCVendor::Nvidia,
            "DGXH100",
            FirmwareComponentType::Cx7,
        )
        .unwrap()
        .expect("script should be registered");

        assert_eq!(
            script.url,
            "http://carbide-pxe.forge:8080/public/scout-firmware-scripts/nvidia/dgxh100/cx7/upgrade.sh"
        );
        assert_eq!(script.sha256.len(), 64);
        assert!(script.sha256.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(script.execution_timeout_seconds, 7200);
        assert_eq!(script.artifact_download_timeout_seconds, 600);
    }

    #[test]
    fn returns_none_for_missing_combination() {
        let tempdir = tempfile::tempdir().unwrap();

        assert!(
            find_scout_script_in(
                tempdir.path(),
                TEST_PXE_PUBLIC_BASE_URL,
                bmc_vendor::BMCVendor::Nvidia,
                "DGXH100",
                FirmwareComponentType::Bmc
            )
            .unwrap()
            .is_none()
        );
    }

    #[test]
    fn returns_none_when_root_is_missing() {
        let tempdir = tempfile::tempdir().unwrap();
        let missing_root = tempdir.path().join("missing");

        assert!(
            find_scout_script_in(
                &missing_root,
                TEST_PXE_PUBLIC_BASE_URL,
                bmc_vendor::BMCVendor::Nvidia,
                "DGXH100",
                FirmwareComponentType::Cx7
            )
            .unwrap()
            .is_none()
        );
    }

    #[test]
    fn returns_none_for_unknown_selector_values() {
        let tempdir = tempfile::tempdir().unwrap();

        for (vendor, component_type) in [
            (bmc_vendor::BMCVendor::Unknown, FirmwareComponentType::Cx7),
            (
                bmc_vendor::BMCVendor::Nvidia,
                FirmwareComponentType::Unknown,
            ),
        ] {
            assert!(
                find_scout_script_in(
                    tempdir.path(),
                    TEST_PXE_PUBLIC_BASE_URL,
                    vendor,
                    "DGXH100",
                    component_type,
                )
                .unwrap()
                .is_none()
            );
        }
    }

    #[test]
    fn resolves_script_from_component_directory() {
        let tempdir = tempfile::tempdir().unwrap();
        let component_dir = tempdir.path().join("nvidia/dgxh100/cx7");
        fs::create_dir_all(&component_dir).unwrap();
        fs::write(component_dir.join(SCOUT_FIRMWARE_SCRIPT_FILE), "echo ok\n").unwrap();
        fs::write(
            component_dir.join(SCOUT_FIRMWARE_METADATA_FILE),
            "execution_timeout_seconds = 10\nartifact_download_timeout_seconds = 20\n",
        )
        .unwrap();

        let script = find_scout_script_in(
            tempdir.path(),
            TEST_PXE_PUBLIC_BASE_URL,
            bmc_vendor::BMCVendor::Nvidia,
            "DGXH100",
            FirmwareComponentType::Cx7,
        )
        .unwrap()
        .expect("script should resolve");

        assert_eq!(
            script.url,
            "http://carbide-pxe.forge:8080/public/scout-firmware-scripts/nvidia/dgxh100/cx7/upgrade.sh"
        );
        assert_eq!(script.sha256, hex::encode(Sha256::digest(b"echo ok\n")));
        assert_eq!(script.execution_timeout_seconds, 10);
        assert_eq!(script.artifact_download_timeout_seconds, 20);
    }

    #[test]
    fn normalizes_model_name_spaces_to_underscores() {
        let tempdir = tempfile::tempdir().unwrap();
        let component_dir = tempdir.path().join("dell/poweredge_r760/bmc");
        fs::create_dir_all(&component_dir).unwrap();
        fs::write(component_dir.join(SCOUT_FIRMWARE_SCRIPT_FILE), "echo ok\n").unwrap();
        fs::write(
            component_dir.join(SCOUT_FIRMWARE_METADATA_FILE),
            "execution_timeout_seconds = 10\nartifact_download_timeout_seconds = 20\n",
        )
        .unwrap();

        let script = find_scout_script_in(
            tempdir.path(),
            TEST_PXE_PUBLIC_BASE_URL,
            bmc_vendor::BMCVendor::Dell,
            "PowerEdge R760",
            FirmwareComponentType::Bmc,
        )
        .unwrap()
        .expect("script should resolve");

        assert_eq!(
            script.url,
            "http://carbide-pxe.forge:8080/public/scout-firmware-scripts/dell/poweredge_r760/bmc/upgrade.sh"
        );
        assert_eq!(script.sha256, hex::encode(Sha256::digest(b"echo ok\n")));
        assert_eq!(script.execution_timeout_seconds, 10);
        assert_eq!(script.artifact_download_timeout_seconds, 20);
    }

    #[test]
    fn errors_when_script_or_metadata_pair_is_incomplete() {
        let tempdir = tempfile::tempdir().unwrap();
        let component_dir = tempdir.path().join("nvidia/dgxh100/cx7");
        fs::create_dir_all(&component_dir).unwrap();
        fs::write(component_dir.join(SCOUT_FIRMWARE_SCRIPT_FILE), "echo ok\n").unwrap();

        let error = find_scout_script_in(
            tempdir.path(),
            TEST_PXE_PUBLIC_BASE_URL,
            bmc_vendor::BMCVendor::Nvidia,
            "DGXH100",
            FirmwareComponentType::Cx7,
        )
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("missing scout firmware script metadata")
        );
    }

    #[test]
    fn errors_for_zero_timeout_metadata() {
        let tempdir = tempfile::tempdir().unwrap();
        let component_dir = tempdir.path().join("nvidia/dgxh100/cx7");
        fs::create_dir_all(&component_dir).unwrap();
        fs::write(component_dir.join(SCOUT_FIRMWARE_SCRIPT_FILE), "echo ok\n").unwrap();
        fs::write(
            component_dir.join(SCOUT_FIRMWARE_METADATA_FILE),
            "execution_timeout_seconds = 0\nartifact_download_timeout_seconds = 20\n",
        )
        .unwrap();

        let error = find_scout_script_in(
            tempdir.path(),
            TEST_PXE_PUBLIC_BASE_URL,
            bmc_vendor::BMCVendor::Nvidia,
            "DGXH100",
            FirmwareComponentType::Cx7,
        )
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("failed to parse scout firmware script metadata")
        );
    }
}

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

//! Read-only comparison of a live device with a firmware target and local inputs.
//!
//! The caller checks artifact digests and retains the staged files. This module
//! checks their availability and queries the selected device; it does not inspect
//! image/configuration contents or establish that flashing would be safe.

use std::path::{Path, PathBuf};
use std::time::Duration;

use carbide_libmlx_model::device::identity::MlxDeviceSelector;
use carbide_libmlx_model::device::info::MlxDeviceInfo;
use thiserror::Error;

use super::config::FirmwareSpec;
use crate::device::discovery::{DeviceDiscoveryError, QUERY_TIMEOUT, discover_device_with_program};
use crate::runner::FirmwareToolExecutor;

/// `LocalFirmwareFiles` borrows the caller's digest-checked staging paths.
///
/// Keep each supplied file present and unchanged until inspection returns. The
/// image and any supplied configuration must be readable, nonempty regular
/// files. No remote source or cache lookup is performed, and these paths do not
/// themselves prove digest verification.
#[derive(Clone, Copy, Debug)]
pub struct LocalFirmwareFiles<'a> {
    /// Local firmware image whose digest the caller has checked.
    pub image: &'a Path,
    /// Optional local device configuration, also checked by the caller.
    pub device_config: Option<&'a Path>,
}

/// Exact comparison with the requested firmware version, not upgrade ordering.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FirmwareComparison {
    /// The known installed version equals the target literally.
    ExactMatch,
    /// A known installed version differs, including when it is newer.
    ChangeRequired,
}

/// Live device facts and their comparison with a requested firmware target.
///
/// Neither result certifies image compatibility, configuration equivalence,
/// physical-card ownership, or permission to execute a firmware change.
#[derive(Debug)]
pub struct FirmwarePreflightResult {
    /// Fresh facts for the requested PCI address, with matching part number/PSID.
    pub device: MlxDeviceInfo,
    /// Literal firmware version comparison; an unknown version is an error.
    pub comparison: FirmwareComparison,
}

/// Failures before a read-only firmware comparison can be reported.
#[derive(Debug, Error)]
pub enum FirmwarePreflightError {
    /// Device-query failure, including process cleanup and unusable-report diagnostics.
    #[error(transparent)]
    Discovery(#[from] DeviceDiscoveryError),
    /// A requested locator or target field is unusable.
    #[error("invalid firmware preflight request: {0}")]
    InvalidRequest(String),
    /// A staged input could not be inspected or opened for reading.
    #[error("failed to read staged firmware input {path}: {source}")]
    InputIo {
        /// Local path that failed; remote credentials are never accepted here.
        path: PathBuf,
        /// Filesystem failure retained for diagnostics.
        source: std::io::Error,
    },
    /// A staged path does not refer to a nonempty regular file.
    #[error("staged firmware input is not a nonempty regular file: {0}")]
    InvalidInput(PathBuf),
    /// Returned device identity does not match the requested target.
    #[error("device {field} does not match: expected {expected:?}, observed {observed:?}")]
    IdentityMismatch {
        /// Identity field that failed: PCI address, part number, PSID, base MAC, or base GUID.
        field: &'static str,
        /// Requested identity value.
        expected: String,
        /// Observed identity, or absence when the tool omitted it.
        observed: Option<String>,
    },
    /// Missing installed firmware cannot be treated as an exact match or change.
    #[error("installed firmware version is unavailable")]
    UnknownFirmwareVersion,
}

/// `inspect_prepared_firmware` checks local inputs and compares one live device.
///
/// `device` is a PCI address (`[dddd:]bb:dd.f`), not an MST alias. Hexadecimal
/// case and an omitted zero domain are equivalent. Part number and PSID match
/// literally, as does the version; target fields must be nonempty, unpadded,
/// and free of control characters. Unknown installed firmware is an error.
///
/// Runs only `mlxfwmanager --dev <device> --query-format xml`, with a 30-second
/// overall query deadline and no retries. Linux and a sibling
/// `mlx-firmware-supervisor` executable are required. Successful completion waits
/// for the tool, all descendants, and both output streams within that deadline.
/// Failure or timeout requests termination and allows up to five seconds for
/// cleanup, then up to five more seconds for result delivery. Dropping this
/// future requests termination without waiting for cleanup confirmation; the
/// helper continues independently unless it is also terminated. Other platforms
/// fail before spawning.
/// Each output stream is limited to 1 MiB; unusable-query errors include at most
/// the first 4 KiB of stderr. No download, configuration apply, lock/unlock,
/// burn, reset, or binary-content validation is performed.
pub async fn inspect_prepared_firmware(
    device: &str,
    desired: &FirmwareSpec,
    files: LocalFirmwareFiles<'_>,
) -> Result<FirmwarePreflightResult, FirmwarePreflightError> {
    inspect_prepared_firmware_with_executor(
        device,
        desired,
        files,
        &FirmwareToolExecutor::default(),
    )
    .await
}

/// `inspect_prepared_firmware_with_executor` uses the caller's supervisor
/// location, with the same read-only checks and limits as
/// [`inspect_prepared_firmware`].
pub async fn inspect_prepared_firmware_with_executor(
    device: &str,
    desired: &FirmwareSpec,
    files: LocalFirmwareFiles<'_>,
    executor: &FirmwareToolExecutor,
) -> Result<FirmwarePreflightResult, FirmwarePreflightError> {
    inspect_with_program(
        device,
        desired,
        files,
        Path::new("mlxfwmanager"),
        QUERY_TIMEOUT,
        executor,
    )
    .await
}

/// `validate_preflight_request` checks a PCI locator and literal target fields
/// without filesystem or device I/O. Callers can reject invalid requests before
/// downloading artifacts; inspection repeats this check for direct callers.
///
/// Accepted locator and target formats are those of [`inspect_prepared_firmware`].
pub fn validate_preflight_request(
    device: &str,
    desired: &FirmwareSpec,
) -> Result<(), FirmwarePreflightError> {
    normalize_pci_address(device).ok_or_else(|| {
        FirmwarePreflightError::InvalidRequest("device must be a PCI address [dddd:]bb:dd.f".into())
    })?;
    for (field, value) in [
        ("part number", &desired.part_number),
        ("PSID", &desired.psid),
        ("firmware version", &desired.version),
    ] {
        if !is_usable_literal(value) {
            return Err(FirmwarePreflightError::InvalidRequest(format!(
                "{field} must be nonempty, unpadded, and free of control characters"
            )));
        }
    }
    Ok(())
}

/// Literal firmware identifiers and versions must be present without padding
/// or control characters; callers compare accepted values without normalization.
pub(super) fn is_usable_literal(value: &str) -> bool {
    !value.is_empty() && value.trim() == value && !value.chars().any(char::is_control)
}

/// Check only the exact part number and PSID. Locator policy belongs to callers:
/// preflight requires a PCI address, while the flasher also accepts MFT aliases.
pub(super) fn check_hardware_identity(
    device: &MlxDeviceInfo,
    desired: &FirmwareSpec,
) -> Result<(), FirmwarePreflightError> {
    for (field, expected, observed) in [
        (
            "part number",
            desired.part_number.as_str(),
            device.part_number.as_deref(),
        ),
        ("PSID", desired.psid.as_str(), device.psid.as_deref()),
    ] {
        if observed != Some(expected) {
            return Err(FirmwarePreflightError::IdentityMismatch {
                field,
                expected: expected.into(),
                observed: observed.map(str::to_owned),
            });
        }
    }
    Ok(())
}

/// Compare the frozen card selector with the existing pre-mutation query.
/// A replacement with the same part number and PSID must not inherit this work.
pub(super) fn check_expected_device(
    device: &MlxDeviceInfo,
    expected: &MlxDeviceSelector,
) -> Result<(), FirmwarePreflightError> {
    if expected.matches(device) {
        return Ok(());
    }
    let (field, expected, observed) = match expected {
        MlxDeviceSelector::Mac(mac) => (
            "base MAC",
            mac.to_string(),
            device.base_mac.map(|mac| mac.to_string()),
        ),
        MlxDeviceSelector::Guid(guid) => ("base GUID", guid.clone(), device.base_guid.clone()),
    };
    Err(FirmwarePreflightError::IdentityMismatch {
        field,
        expected,
        observed,
    })
}

async fn inspect_with_program(
    device: &str,
    desired: &FirmwareSpec,
    files: LocalFirmwareFiles<'_>,
    program: &Path,
    timeout: Duration,
    executor: &FirmwareToolExecutor,
) -> Result<FirmwarePreflightResult, FirmwarePreflightError> {
    validate_preflight_request(device, desired)?;

    // Check the image and any supplied configuration before asking the hardware
    // anything. The caller keeps the checked files available through inspection;
    // a successful query must not hide a missing input.
    for path in std::iter::once(files.image).chain(files.device_config) {
        check_readable_file(path).await?;
    }
    let report = discover_device_with_program(program, device, timeout, executor).await?;
    compare_device(report, device, desired)
}

/// Checks that the path is a readable, nonempty regular file, not its digest.
/// The caller keeps the staged path unchanged until the check returns.
pub(super) async fn check_readable_file(path: &Path) -> Result<(), FirmwarePreflightError> {
    let io_error = |source| FirmwarePreflightError::InputIo {
        path: path.to_path_buf(),
        source,
    };
    // Reject directories and special files before opening: a FIFO open could
    // otherwise wait indefinitely. The staging owner keeps the path unchanged.
    let metadata = tokio::fs::metadata(path).await.map_err(io_error)?;
    if !metadata.is_file() || metadata.len() == 0 {
        return Err(FirmwarePreflightError::InvalidInput(path.to_path_buf()));
    }
    tokio::fs::File::open(path).await.map_err(io_error)?;
    Ok(())
}

fn compare_device(
    device: MlxDeviceInfo,
    requested_pci: &str,
    desired: &FirmwareSpec,
) -> Result<FirmwarePreflightResult, FirmwarePreflightError> {
    let requested_pci = normalize_pci_address(requested_pci).ok_or_else(|| {
        FirmwarePreflightError::InvalidRequest("device must be a PCI address [dddd:]bb:dd.f".into())
    })?;
    if normalize_pci_address(&device.pci_name).as_deref() != Some(requested_pci.as_str()) {
        return Err(FirmwarePreflightError::IdentityMismatch {
            field: "PCI address",
            expected: requested_pci,
            // Even an unrecognized locator is useful evidence. Do not turn a
            // reported MST path or malformed PCI address into apparent absence.
            observed: Some(device.pci_name),
        });
    }
    check_hardware_identity(&device, desired)?;
    let installed = device
        .fw_version_current
        .as_deref()
        .filter(|version| is_usable_literal(version))
        .ok_or(FirmwarePreflightError::UnknownFirmwareVersion)?;
    let comparison = if installed == desired.version {
        FirmwareComparison::ExactMatch
    } else {
        FirmwareComparison::ChangeRequired
    };
    Ok(FirmwarePreflightResult { device, comparison })
}

fn normalize_pci_address(address: &str) -> Option<String> {
    let address = crate::device::discovery::normalize_pci_address(address)?;
    Some(
        address
            .strip_prefix("0000:")
            .unwrap_or(&address)
            .to_string(),
    )
}

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;

    use super::*;

    /// Equivalent PCI spellings match, while aliases cannot attest a PCI result.
    #[test]
    fn pci_locators_preserve_domain_and_reject_aliases() {
        value_scenarios!(normalize_pci_address:
            "zero domain and hexadecimal case are presentation differences" {
                "0000:AB:1F.7" => Some("ab:1f.7".to_string()),
                "ab:1f.7" => Some("ab:1f.7".to_string()),
            }
            "a nonzero domain identifies a different PCI function" {
                "0001:ab:1f.7" => Some("0001:ab:1f.7".to_string()),
            }
            "invalid function, device, and unresolved MST aliases are rejected" {
                "ab:01.8" => None,
                "ab:20.0" => None,
                "/dev/mst/mt4129_pciconf0" => None,
                "🦀:00.0" => None,
            }
            "a zero domain prefix cannot hide a second domain" {
                "0000:0001:ab:01.0" => None,
            }
        );
    }

    fn desired() -> FirmwareSpec {
        FirmwareSpec {
            part_number: "part".into(),
            psid: "PSID".into(),
            version: "32.43.1014".into(),
        }
    }

    fn device() -> MlxDeviceInfo {
        MlxDeviceInfo {
            pci_name: "ab:01.0".into(),
            part_number: Some("part".into()),
            psid: Some("PSID".into()),
            fw_version_current: Some("32.43.1014".into()),
            ..MlxDeviceInfo::create_test_device()
        }
    }

    /// A GUID can identify a card without a MAC, but mismatched or missing
    /// selector fields must preserve exact diagnostic evidence for the caller.
    #[test]
    fn expected_device_preserves_guid_and_missing_mac_evidence() {
        use MlxDeviceSelector::{Guid, Mac};
        use carbide_test_support::Outcome::{FailsWith, Yields};
        use carbide_test_support::scenarios;

        scenarios!(
            run = |(selector, device): (MlxDeviceSelector, MlxDeviceInfo)| {
                // Dropping the reported GUID, changing the diagnostic field,
                // or accepting a mismatch must fail these exact comparisons.
                check_expected_device(&device, &selector).map_err(|error| match error {
                    FirmwarePreflightError::IdentityMismatch { field, expected, observed } => {
                        (field, expected, observed)
                    }
                    other => panic!("unexpected preflight error: {other}"),
                })
            };
            "an exact GUID does not require a MAC" {
                (
                    Guid("opaque-Base_Guid".into()),
                    MlxDeviceInfo {
                        base_guid: Some("opaque-Base_Guid".into()),
                        base_mac: None,
                        ..device()
                    }
                ) => Yields(()),
            }
            "a GUID mismatch preserves the reported spelling" {
                (
                    Guid("opaque-Base_Guid".into()),
                    MlxDeviceInfo {
                        base_guid: Some("opaque-base_guid".into()),
                        ..device()
                    }
                ) => FailsWith(("base GUID", "opaque-Base_Guid".into(), Some("opaque-base_guid".into()))),
            }
            "an absent MAC remains absent instead of falling back to the GUID" {
                (
                    Mac("02:00:00:00:00:01".parse().unwrap()),
                    MlxDeviceInfo { base_mac: None, ..device() }
                ) => FailsWith(("base MAC", "02:00:00:00:00:01".into(), None)),
            }
        );
    }

    /// Identity and known firmware are prerequisites; both upgrades and downgrades
    /// require a change.
    #[test]
    fn comparison_requires_identity_and_exact_version() {
        for (name, device, expected) in [
            (
                "exact target with equivalent PCI spelling",
                MlxDeviceInfo {
                    pci_name: "0000:AB:01.0".into(),
                    ..device()
                },
                Ok(FirmwareComparison::ExactMatch),
            ),
            (
                "older installed",
                MlxDeviceInfo {
                    fw_version_current: Some("32.42.1000".into()),
                    ..device()
                },
                Ok(FirmwareComparison::ChangeRequired),
            ),
            (
                "newer installed",
                MlxDeviceInfo {
                    fw_version_current: Some("32.44.1000".into()),
                    ..device()
                },
                Ok(FirmwareComparison::ChangeRequired),
            ),
            (
                "wrong PCI domain",
                MlxDeviceInfo {
                    pci_name: "0001:ab:01.0".into(),
                    ..device()
                },
                Err("device PCI address does not match"),
            ),
            (
                "malformed reported PCI address remains visible",
                MlxDeviceInfo {
                    pci_name: "not-a-pci-address".into(),
                    ..device()
                },
                Err("observed Some(\"not-a-pci-address\")"),
            ),
            (
                "wrong part despite matching firmware",
                MlxDeviceInfo {
                    part_number: Some("PART".into()),
                    ..device()
                },
                Err("device part number does not match"),
            ),
            (
                "missing PSID",
                MlxDeviceInfo {
                    psid: None,
                    ..device()
                },
                Err("device PSID does not match"),
            ),
            (
                "unknown installed version",
                MlxDeviceInfo {
                    fw_version_current: None,
                    ..device()
                },
                Err("installed firmware version is unavailable"),
            ),
            (
                "padded installed version",
                MlxDeviceInfo {
                    fw_version_current: Some(" 32.43.1014".into()),
                    ..device()
                },
                Err("installed firmware version is unavailable"),
            ),
            (
                "control character in installed version",
                MlxDeviceInfo {
                    fw_version_current: Some("32.43.\n1014".into()),
                    ..device()
                },
                Err("installed firmware version is unavailable"),
            ),
        ] {
            let result = compare_device(device.clone(), "ab:01.0", &desired());
            match expected {
                Ok(comparison) => {
                    let report = result.unwrap_or_else(|error| panic!("{name}: {error}"));
                    assert_eq!(report.comparison, comparison, "{name}");
                    assert_eq!(report.device, device, "{name}");
                }
                Err(expected) => {
                    let error = result.unwrap_err();
                    assert!(error.to_string().contains(expected), "{name}: {error}");
                }
            }
        }
    }

    #[test]
    fn target_versions_reject_padding_and_control_characters() {
        use carbide_test_support::{Check, check_values};

        check_values(
            [
                Check {
                    scenario: "padding",
                    input: " 32.43.1014",
                    expect: true,
                },
                Check {
                    scenario: "control character",
                    input: "32.43.\n1014",
                    expect: true,
                },
            ],
            |version| {
                let target = FirmwareSpec {
                    version: version.into(),
                    ..desired()
                };
                matches!(
                    validate_preflight_request("ab:01.0", &target),
                    Err(FirmwarePreflightError::InvalidRequest(_))
                )
            },
        );
    }

    #[cfg(target_os = "linux")]
    mod process {
        use std::os::unix::fs::PermissionsExt;

        use tempfile::TempDir;

        use super::*;

        struct PreparedFixture {
            _directory: TempDir,
            image: PathBuf,
            config: PathBuf,
        }

        impl PreparedFixture {
            fn new() -> Self {
                let directory = tempfile::tempdir().unwrap();
                let image = directory.path().join("image.bin");
                let config = directory.path().join("device.conf");
                std::fs::write(&image, b"checked image").unwrap();
                std::fs::write(&config, b"checked config").unwrap();
                Self {
                    _directory: directory,
                    image,
                    config,
                }
            }

            fn files(&self) -> LocalFirmwareFiles<'_> {
                LocalFirmwareFiles {
                    image: &self.image,
                    device_config: Some(&self.config),
                }
            }
        }

        /// Rejected prepared inputs never query hardware. The same query fixture
        /// must return live facts and a comparison without changing valid inputs.
        #[tokio::test]
        async fn prepared_inputs_gate_the_query_and_preserve_successful_comparison() {
            let directory = tempfile::tempdir().unwrap();
            let program = directory.path().join("mlxfwmanager");
            let invocations = directory.path().join("invocations");
            // Isolate the writable executable descriptor from concurrent test
            // spawns, which could otherwise inherit it and cause ETXTBSY.
            let status = tokio::time::timeout(
                Duration::from_secs(10),
                tokio::process::Command::new("/bin/sh")
                    .args([
                        "-c",
                        "umask 077; printf '%s' \"$2\" > \"$1\"",
                        "fixture-writer",
                    ])
                    .arg(&program)
                    .arg(
                        r#"#!/bin/sh
set -eu
cd -- "${0%/*}"
printf '%s\n' "$@" >> invocations
printf '%s' '<Devices><Device pciName="ab:01.0" type="BlueField3" partNumber="part" psid="PSID"><Versions><FW current="32.42.1000"/></Versions></Device></Devices>'
"#,
                    )
                    .kill_on_drop(true)
                    .status(),
            )
            .await
            .expect("fixture writer finishes within its watchdog")
            .unwrap();
            assert!(status.success());
            std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();

            enum InputFault {
                None,
                MissingConfiguration,
                EmptyImage,
                NonregularImage,
                InvalidTarget,
            }
            for (name, fault) in [
                // A supplied configuration is checked as well as the image.
                ("missing configuration", InputFault::MissingConfiguration),
                // A present image must contain bytes and be a regular file.
                ("empty image", InputFault::EmptyImage),
                ("nonregular image", InputFault::NonregularImage),
                // Request validation takes precedence over filesystem errors.
                (
                    "invalid target before missing image",
                    InputFault::InvalidTarget,
                ),
                // This positive control proves every absence check uses a
                // working invocation record and exercises the complete query.
                ("valid inputs reach the command", InputFault::None),
            ] {
                let fixture = PreparedFixture::new();
                let mut desired = desired();
                match fault {
                    InputFault::None => {}
                    InputFault::MissingConfiguration => {
                        std::fs::remove_file(&fixture.config).unwrap();
                    }
                    InputFault::EmptyImage => std::fs::write(&fixture.image, []).unwrap(),
                    InputFault::NonregularImage => {
                        std::fs::remove_file(&fixture.image).unwrap();
                        std::fs::create_dir(&fixture.image).unwrap();
                    }
                    InputFault::InvalidTarget => {
                        desired.version.clear();
                        std::fs::remove_file(&fixture.image).unwrap();
                    }
                }
                let result = inspect_with_program(
                    "ab:01.0",
                    &desired,
                    fixture.files(),
                    &program,
                    QUERY_TIMEOUT,
                    &crate::runner::test_firmware_executor(),
                )
                .await;
                assert_eq!(
                    invocations.exists(),
                    matches!(fault, InputFault::None),
                    "{name}"
                );
                match fault {
                    InputFault::None => {
                        let report = result.unwrap_or_else(|error| panic!("{name}: {error}"));
                        assert_eq!(report.device.pci_name, "ab:01.0");
                        assert_eq!(report.device.part_number.as_deref(), Some("part"));
                        assert_eq!(report.device.psid.as_deref(), Some("PSID"));
                        assert_eq!(
                            report.device.fw_version_current.as_deref(),
                            Some("32.42.1000")
                        );
                        assert_eq!(report.comparison, FirmwareComparison::ChangeRequired);
                        assert_eq!(
                            std::fs::read_to_string(&invocations).unwrap(),
                            "--dev\nab:01.0\n--query-format\nxml\n"
                        );
                        assert_eq!(std::fs::read(&fixture.image).unwrap(), b"checked image");
                        assert_eq!(std::fs::read(&fixture.config).unwrap(), b"checked config");
                    }
                    InputFault::MissingConfiguration => assert!(
                        matches!(result, Err(FirmwarePreflightError::InputIo { .. })),
                        "{name}: {result:?}"
                    ),
                    InputFault::EmptyImage | InputFault::NonregularImage => assert!(
                        matches!(result, Err(FirmwarePreflightError::InvalidInput(_))),
                        "{name}: {result:?}"
                    ),
                    InputFault::InvalidTarget => assert!(
                        matches!(result, Err(FirmwarePreflightError::InvalidRequest(_))),
                        "{name}: {result:?}"
                    ),
                }
            }
        }
    }
}

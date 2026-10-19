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
use std::process::Stdio;
use std::time::Duration;

use carbide_libmlx_model::device::info::MlxDeviceInfo;
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;

use super::config::FirmwareSpec;
use crate::device::discovery::parse_mlxfwmanager_xml;

// Match the libmlx command runner's default. This is one complete read-only
// query, including output collection, with no automatic retry.
const QUERY_TIMEOUT: Duration = Duration::from_secs(30);
// A query selects one device, not a fleet. Stop malformed tool output from
// retaining an unbounded buffer before XML parsing.
const MAX_QUERY_OUTPUT_BYTES: u64 = 1024 * 1024;
// Keep the tool's explanation on failures without copying a full output buffer
// into an operator-facing error.
const MAX_QUERY_DIAGNOSTIC_BYTES: usize = 4096;

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
    /// The tool could not be spawned, read, waited for, or terminated.
    #[error("mlxfwmanager query I/O failed: {0}")]
    QueryIo(#[from] std::io::Error),
    /// The single query exceeded its complete attempt deadline.
    #[error("mlxfwmanager query exceeded its {0:?} deadline")]
    QueryTimeout(Duration),
    /// The tool failed, exceeded its output bound, or returned unusable XML.
    #[error("invalid mlxfwmanager query result: {0}")]
    InvalidReport(String),
    /// The tool's XML output is not valid UTF-8.
    #[error("mlxfwmanager query output is not UTF-8: {source}; stderr: {stderr}")]
    ReportEncoding {
        /// Invalid XML bytes retained with their decoding failure.
        source: std::string::FromUtf8Error,
        /// Tool diagnostics, limited to the first 4 KiB.
        stderr: String,
    },
    /// Returned device identity does not match the requested target.
    #[error("device {field} does not match: expected {expected:?}, observed {observed:?}")]
    IdentityMismatch {
        /// Identity field that failed: PCI address, part number, or PSID.
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
/// overall query deadline and no retries. Timeout kills and waits for the child;
/// dropping this future requests child termination through `kill_on_drop`.
/// Each output stream is limited to 1 MiB; unusable-query errors include at most
/// the first 4 KiB of stderr. No download, configuration apply, lock/unlock,
/// burn, reset, or binary-content validation is performed.
pub async fn inspect_prepared_firmware(
    device: &str,
    desired: &FirmwareSpec,
    files: LocalFirmwareFiles<'_>,
) -> Result<FirmwarePreflightResult, FirmwarePreflightError> {
    inspect_with_program(
        device,
        desired,
        files,
        Path::new("mlxfwmanager"),
        QUERY_TIMEOUT,
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
        if value.is_empty() || value.trim() != value || value.chars().any(char::is_control) {
            return Err(FirmwarePreflightError::InvalidRequest(format!(
                "{field} must be nonempty, unpadded, and free of control characters"
            )));
        }
    }
    Ok(())
}

async fn inspect_with_program(
    device: &str,
    desired: &FirmwareSpec,
    files: LocalFirmwareFiles<'_>,
    program: &Path,
    timeout: Duration,
) -> Result<FirmwarePreflightResult, FirmwarePreflightError> {
    validate_preflight_request(device, desired)?;

    // Check the image and any supplied configuration before asking the hardware
    // anything. The caller keeps the checked files available through inspection;
    // a successful query must not hide a missing input.
    files.check_readable().await?;
    let report = query_device(program, device, timeout).await?;
    compare_device(report, device, desired)
}

impl LocalFirmwareFiles<'_> {
    async fn check_readable(self) -> Result<(), FirmwarePreflightError> {
        for path in std::iter::once(self.image).chain(self.device_config) {
            let io_error = |source| FirmwarePreflightError::InputIo {
                path: path.to_path_buf(),
                source,
            };
            // Reject directories and special files before opening: a FIFO open
            // could otherwise wait indefinitely. The staging owner keeps these
            // paths unchanged for the duration of the call.
            let metadata = tokio::fs::metadata(path).await.map_err(io_error)?;
            if !metadata.is_file() || metadata.len() == 0 {
                return Err(FirmwarePreflightError::InvalidInput(path.to_path_buf()));
            }
            tokio::fs::File::open(path).await.map_err(io_error)?;
        }
        Ok(())
    }
}

async fn query_device(
    program: &Path,
    device: &str,
    timeout: Duration,
) -> Result<MlxDeviceInfo, FirmwarePreflightError> {
    let deadline = tokio::time::Instant::now() + timeout;
    let mut child = Command::new(program)
        .args(["--dev", device, "--query-format", "xml"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| std::io::Error::other("mlxfwmanager stdout pipe is unavailable"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| std::io::Error::other("mlxfwmanager stderr pipe is unavailable"))?;
    let result = tokio::time::timeout_at(deadline, async {
        tokio::try_join!(
            async { child.wait().await.map_err(FirmwarePreflightError::QueryIo) },
            read_query_output(stdout),
            read_query_output(stderr)
        )
    })
    .await;

    let (status, xml, diagnostics) = match result {
        Ok(Ok(output)) => output,
        Ok(Err(error)) => {
            child.kill().await?;
            return Err(error);
        }
        Err(_) => {
            child.kill().await?;
            return Err(FirmwarePreflightError::QueryTimeout(timeout));
        }
    };
    let diagnostics = if diagnostics.len() > MAX_QUERY_DIAGNOSTIC_BYTES {
        format!(
            "{} [truncated]",
            String::from_utf8_lossy(&diagnostics[..MAX_QUERY_DIAGNOSTIC_BYTES])
        )
    } else {
        String::from_utf8_lossy(&diagnostics).into_owned()
    };
    // MFT can return code 1 with usable XML, including devices in lockdown.
    // Required facts below still have to be present and match the target. Keep
    // stderr when that XML cannot be used, since it may explain the query failure.
    if !matches!(status.code(), Some(0) | Some(1)) {
        return Err(FirmwarePreflightError::InvalidReport(format!(
            "query exited with {status}: {diagnostics}"
        )));
    }
    let xml = String::from_utf8(xml).map_err(|source| FirmwarePreflightError::ReportEncoding {
        source,
        stderr: diagnostics.clone(),
    })?;
    let devices = parse_mlxfwmanager_xml(&xml).map_err(|error| {
        FirmwarePreflightError::InvalidReport(format!("{error}; stderr: {diagnostics}"))
    })?;
    let [device]: [MlxDeviceInfo; 1] =
        devices.try_into().map_err(|devices: Vec<MlxDeviceInfo>| {
            FirmwarePreflightError::InvalidReport(format!(
                "expected one device, received {}; stderr: {diagnostics}",
                devices.len()
            ))
        })?;
    Ok(device)
}

async fn read_query_output(
    reader: impl AsyncRead + Unpin,
) -> Result<Vec<u8>, FirmwarePreflightError> {
    let mut bytes = Vec::new();
    reader
        .take(MAX_QUERY_OUTPUT_BYTES + 1)
        .read_to_end(&mut bytes)
        .await?;
    if bytes.len() as u64 > MAX_QUERY_OUTPUT_BYTES {
        return Err(FirmwarePreflightError::InvalidReport(
            "query output exceeded 1 MiB per stream".into(),
        ));
    }
    Ok(bytes)
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
    let installed = device
        .fw_version_current
        .as_deref()
        .filter(|version| {
            !version.is_empty()
                && version.trim() == *version
                && !version.chars().any(char::is_control)
        })
        .ok_or(FirmwarePreflightError::UnknownFirmwareVersion)?;
    let comparison = if installed == desired.version {
        FirmwareComparison::ExactMatch
    } else {
        FirmwareComparison::ChangeRequired
    };
    Ok(FirmwarePreflightResult { device, comparison })
}

fn normalize_pci_address(address: &str) -> Option<String> {
    // Validate the entire locator before dropping a zero domain. Otherwise
    // `0000:0001:ab:01.0` could be accepted as a second, valid-looking domain.
    let (domain, address) = match address.len() {
        7 => (None, address),
        12 if address.is_ascii() && address.as_bytes()[4] == b':' => {
            (Some(&address[..4]), &address[5..])
        }
        _ => return None,
    };
    let bytes = address.as_bytes();
    if !address.is_ascii()
        || bytes[2] != b':'
        || bytes[5] != b'.'
        || !bytes[..2]
            .iter()
            .chain(&bytes[3..5])
            .all(u8::is_ascii_hexdigit)
        || !matches!(bytes[6], b'0'..=b'7')
        || u8::from_str_radix(&address[3..5], 16).ok()? > 31
        || domain.is_some_and(|value| !value.bytes().all(|byte| byte.is_ascii_hexdigit()))
    {
        return None;
    }
    Some(match domain {
        Some("0000") | None => address.to_ascii_lowercase(),
        Some(domain) => format!("{domain}:{address}").to_ascii_lowercase(),
    })
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

    #[cfg(unix)]
    mod process {
        use std::os::unix::fs::PermissionsExt;

        use tempfile::TempDir;

        use super::*;

        /// The fake tool records real argv; separate nonempty inputs expose any
        /// accidental substitution or write during the read-only comparison.
        struct QueryFixture {
            directory: TempDir,
            program: PathBuf,
            image: PathBuf,
            config: PathBuf,
        }

        impl QueryFixture {
            /// Each fake runs in its own directory without changing process PATH.
            fn new(body: &str) -> Self {
                let directory = tempfile::tempdir().unwrap();
                let program = directory.path().join("mlxfwmanager");
                std::fs::write(&program, format!(
                    "#!/bin/sh\ncd -- \"${{0%/*}}\"\nprintf '%s\\n' \"$@\" > arguments\n{body}\n"
                )).unwrap();
                std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
                let image = directory.path().join("image.bin");
                let config = directory.path().join("device.conf");
                std::fs::write(&image, b"checked image").unwrap();
                std::fs::write(&config, b"checked config").unwrap();
                Self {
                    directory,
                    program,
                    image,
                    config,
                }
            }

            /// Borrow both inputs so their temporary owner outlives inspection.
            fn files(&self) -> LocalFirmwareFiles<'_> {
                LocalFirmwareFiles {
                    image: &self.image,
                    device_config: Some(&self.config),
                }
            }
        }

        /// Hardware identity and known firmware are prerequisites, not fallbacks;
        /// any unequal installed version requires a change, including downgrades.
        #[tokio::test]
        async fn live_comparison_requires_identity_and_exact_version() {
            struct Case {
                name: &'static str,
                pci: &'static str,
                part_number: &'static str,
                psid: &'static str,
                version: &'static str,
                exit_code: u8,
                expected: Result<FirmwareComparison, &'static str>,
            }
            let cases = [
                // Case and an explicit zero domain must not select a different device.
                Case {
                    name: "exact target",
                    pci: "0000:AB:01.0",
                    part_number: "part",
                    psid: "PSID",
                    version: "32.43.1014",
                    exit_code: 0,
                    expected: Ok(FirmwareComparison::ExactMatch),
                },
                // An upgrade and a downgrade are both changes, not ordering-based skips.
                Case {
                    name: "older installed",
                    pci: "ab:01.0",
                    part_number: "part",
                    psid: "PSID",
                    version: "32.42.1000",
                    exit_code: 0,
                    expected: Ok(FirmwareComparison::ChangeRequired),
                },
                Case {
                    name: "newer installed",
                    pci: "ab:01.0",
                    part_number: "part",
                    psid: "PSID",
                    version: "32.44.1000",
                    exit_code: 0,
                    expected: Ok(FirmwareComparison::ChangeRequired),
                },
                // MFT code 1 can retain complete device facts; facts still govern admission.
                Case {
                    name: "usable code one",
                    pci: "ab:01.0",
                    part_number: "part",
                    psid: "PSID",
                    version: "32.43.1014",
                    exit_code: 1,
                    expected: Ok(FirmwareComparison::ExactMatch),
                },
                // A response for another PCI domain must not validate the selected device.
                Case {
                    name: "wrong PCI domain",
                    pci: "0001:ab:01.0",
                    part_number: "part",
                    psid: "PSID",
                    version: "32.43.1014",
                    exit_code: 0,
                    expected: Err("device PCI address does not match"),
                },
                // Unrecognized reported locators must remain visible for diagnosis.
                Case {
                    name: "malformed reported PCI address",
                    pci: "not-a-pci-address",
                    part_number: "part",
                    psid: "PSID",
                    version: "32.43.1014",
                    exit_code: 0,
                    expected: Err("observed Some(\"not-a-pci-address\")"),
                },
                // Hardware fields compare literally, even when the firmware version matches.
                Case {
                    name: "wrong part",
                    pci: "ab:01.0",
                    part_number: "PART",
                    psid: "PSID",
                    version: "32.43.1014",
                    exit_code: 0,
                    expected: Err("device part number does not match"),
                },
                Case {
                    name: "missing PSID",
                    pci: "ab:01.0",
                    part_number: "part",
                    psid: "",
                    version: "32.43.1014",
                    exit_code: 0,
                    expected: Err("device PSID does not match"),
                },
                // Missing/placeholder installed firmware cannot be reported as a change.
                Case {
                    name: "unknown version",
                    pci: "ab:01.0",
                    part_number: "part",
                    psid: "PSID",
                    version: "N/A",
                    exit_code: 1,
                    expected: Err("installed firmware version is unavailable"),
                },
            ];
            let desired = FirmwareSpec {
                part_number: "part".into(),
                psid: "PSID".into(),
                version: "32.43.1014".into(),
            };
            for case in cases {
                let fixture = QueryFixture::new(&format!(
                    "printf '%s' '<Devices><Device pciName=\"{}\" type=\"BlueField3\" partNumber=\"{}\" psid=\"{}\"><Versions><FW current=\"{}\"/></Versions></Device></Devices>'\nexit {}",
                    case.pci, case.part_number, case.psid, case.version, case.exit_code
                ));
                let result = inspect_with_program(
                    "ab:01.0",
                    &desired,
                    fixture.files(),
                    &fixture.program,
                    QUERY_TIMEOUT,
                )
                .await;
                match case.expected {
                    Ok(comparison) => {
                        let report =
                            result.unwrap_or_else(|error| panic!("{}: {error}", case.name));
                        assert_eq!(report.comparison, comparison, "{}", case.name);
                        assert_eq!(
                            report.device.fw_version_current.as_deref(),
                            Some(case.version),
                            "{}",
                            case.name
                        );
                    }
                    Err(message) => assert!(
                        result.unwrap_err().to_string().contains(message),
                        "{}",
                        case.name
                    ),
                }
                assert_eq!(
                    std::fs::read_to_string(fixture.directory.path().join("arguments")).unwrap(),
                    "--dev\nab:01.0\n--query-format\nxml\n",
                    "{}",
                    case.name
                );
                assert_eq!(std::fs::read(&fixture.image).unwrap(), b"checked image");
                assert_eq!(std::fs::read(&fixture.config).unwrap(), b"checked config");
            }
        }

        /// Local input failures stop before even a read-only hardware query.
        #[tokio::test]
        async fn invalid_prepared_inputs_do_not_run_the_tool() {
            enum InputFault {
                MissingConfiguration,
                EmptyImage,
                NonregularImage,
            }
            let desired = FirmwareSpec {
                part_number: "part".into(),
                psid: "PSID".into(),
                version: "32.43.1014".into(),
            };
            for (name, fault) in [
                // Digest checking one image does not excuse an unavailable optional config.
                ("missing configuration", InputFault::MissingConfiguration),
                // A regular file with no bytes cannot be a prepared firmware image.
                ("empty image", InputFault::EmptyImage),
                // A directory must fail the regular-file check before being opened.
                ("nonregular image", InputFault::NonregularImage),
            ] {
                let fixture = QueryFixture::new("exit 99");
                match fault {
                    InputFault::MissingConfiguration => {
                        std::fs::remove_file(&fixture.config).unwrap();
                    }
                    InputFault::EmptyImage => std::fs::write(&fixture.image, []).unwrap(),
                    InputFault::NonregularImage => {
                        std::fs::remove_file(&fixture.image).unwrap();
                        std::fs::create_dir(&fixture.image).unwrap();
                    }
                }
                let result = inspect_with_program(
                    "ab:01.0",
                    &desired,
                    fixture.files(),
                    &fixture.program,
                    QUERY_TIMEOUT,
                )
                .await;
                match fault {
                    InputFault::MissingConfiguration => assert!(
                        matches!(result, Err(FirmwarePreflightError::InputIo { .. })),
                        "{name}"
                    ),
                    InputFault::EmptyImage | InputFault::NonregularImage => assert!(
                        matches!(result, Err(FirmwarePreflightError::InvalidInput(_))),
                        "{name}"
                    ),
                }
                assert!(
                    !fixture.directory.path().join("arguments").exists(),
                    "{name}"
                );
            }

            // A caller cannot turn an empty target into an apparent mismatch.
            let fixture = QueryFixture::new("exit 99");
            let invalid_target = FirmwareSpec {
                version: String::new(),
                ..desired
            };
            let error = inspect_with_program(
                "ab:01.0",
                &invalid_target,
                fixture.files(),
                &fixture.program,
                QUERY_TIMEOUT,
            )
            .await
            .unwrap_err();
            assert!(matches!(error, FirmwarePreflightError::InvalidRequest(_)));
            assert!(!fixture.directory.path().join("arguments").exists());
        }

        /// A single-device query rejects unexpected multiplicity and bounded-output
        /// violations instead of selecting the first result or waiting on full pipes.
        #[tokio::test]
        async fn malformed_query_results_are_visible_failures() {
            for (name, body, expected) in [
                // Cardinality must remain exact even if the command succeeds.
                (
                    "two devices",
                    "printf '%s' '<Devices><Device pciName=\"ab:01.0\" type=\"BlueField3\"/><Device pciName=\"ab:02.0\" type=\"BlueField3\"/></Devices>'",
                    "expected one device, received 2",
                ),
                // A failed tool must not be reported as an unknown firmware version.
                (
                    "failed command",
                    "printf '%s' 'query unavailable' >&2; exit 2",
                    "query exited with",
                ),
                // Exit 1 is accepted only with usable XML; retain its explanation.
                (
                    "code one without XML",
                    "printf '%s' 'permission denied for selected device' >&2; exit 1",
                    "permission denied for selected device",
                ),
                // Diagnostics are useful, but their full stream need not fill the error.
                (
                    "truncated stderr",
                    "printf '%05000dhidden-tail' 0 >&2; exit 1",
                    "[truncated]",
                ),
                // More output than the bound must terminate the still-writing child.
                (
                    "oversized output",
                    "while :; do printf '%01024d' 0; done",
                    "query output exceeded 1 MiB",
                ),
                // stderr has its own bound; draining stdout does not bound diagnostics.
                (
                    "oversized stderr",
                    "while :; do printf '%01024d' 0 >&2; done",
                    "query output exceeded 1 MiB",
                ),
            ] {
                let fixture = QueryFixture::new(body);
                let error = query_device(&fixture.program, "ab:01.0", QUERY_TIMEOUT)
                    .await
                    .unwrap_err();
                let message = error.to_string();
                assert!(message.contains(expected), "{name}: {error}");
                assert!(!message.contains("hidden-tail"), "{name}: {error}");
                assert!(message.len() < MAX_QUERY_DIAGNOSTIC_BYTES + 1024, "{name}");
            }
        }

        /// Timeout and caller cancellation both terminate the owned process;
        /// neither leaves a blocking worker issuing hardware queries afterward.
        #[cfg(target_os = "linux")]
        #[tokio::test]
        async fn query_timeout_and_cancellation_stop_the_child() {
            // exec keeps the recorded PID on the process owned by Tokio.
            let fixture = QueryFixture::new("echo $$ > pid\nexec sleep 30");
            let error = query_device(&fixture.program, "ab:01.0", Duration::from_secs(1))
                .await
                .unwrap_err();
            assert!(matches!(error, FirmwarePreflightError::QueryTimeout(_)));
            let pid = std::fs::read_to_string(fixture.directory.path().join("pid")).unwrap();
            assert!(!Path::new("/proc").join(pid.trim()).exists());

            let fixture = QueryFixture::new("echo $$ > pid\nexec sleep 30");
            let program = fixture.program.clone();
            let task =
                tokio::spawn(async move { query_device(&program, "ab:01.0", QUERY_TIMEOUT).await });
            // Wait for the fake's own start signal rather than guessing when spawn ran.
            let pid_path = fixture.directory.path().join("pid");
            let pid = tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    if let Ok(pid) = tokio::fs::read_to_string(&pid_path).await
                        && !pid.trim().is_empty()
                    {
                        break pid;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            task.abort();
            assert!(task.await.unwrap_err().is_cancelled());
            // kill_on_drop requests termination; Tokio reaps the child asynchronously.
            tokio::time::timeout(Duration::from_secs(5), async {
                while Path::new("/proc").join(pid.trim()).exists() {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("cancelled query process must exit");
        }
    }
}

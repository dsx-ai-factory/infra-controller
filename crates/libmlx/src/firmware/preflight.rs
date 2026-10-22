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
use std::process::{Output, Stdio};
use std::time::Duration;

use carbide_libmlx_model::device::info::MlxDeviceInfo;
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::{Child, Command};
use tokio::time::Instant;

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
    inspect_with_command(device, desired, files, query_command(device)).await
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

async fn inspect_with_command(
    device: &str,
    desired: &FirmwareSpec,
    files: LocalFirmwareFiles<'_>,
    command: Command,
) -> Result<FirmwarePreflightResult, FirmwarePreflightError> {
    validate_preflight_request(device, desired)?;

    // Check the image and any supplied configuration before asking the hardware
    // anything. The caller keeps the checked files available through inspection;
    // a successful query must not hide a missing input.
    files.check_readable().await?;
    let report = query_device(command, QUERY_TIMEOUT).await?;
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

fn query_command(device: &str) -> Command {
    let mut command = Command::new("mlxfwmanager");
    command.args(["--dev", device, "--query-format", "xml"]);
    command
}

async fn query_device(
    command: Command,
    timeout: Duration,
) -> Result<MlxDeviceInfo, FirmwarePreflightError> {
    let deadline = Instant::now() + timeout;
    let child = spawn_query(command)?;
    parse_query_output(collect_query_output(child, deadline, timeout).await?)
}

fn spawn_query(mut command: Command) -> std::io::Result<Child> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
}

async fn collect_query_output(
    mut child: Child,
    deadline: Instant,
    timeout: Duration,
) -> Result<Output, FirmwarePreflightError> {
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

    let (status, stdout, stderr) = match result {
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
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

fn parse_query_output(output: Output) -> Result<MlxDeviceInfo, FirmwarePreflightError> {
    let Output {
        status,
        stdout: xml,
        stderr: diagnostics,
    } = output;
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

    #[cfg(unix)]
    const DEVICE_XML: &str = r#"<Devices><Device pciName="ab:01.0" type="BlueField3" partNumber="part" psid="PSID"><Versions><FW current="32.43.1014"/></Versions></Device></Devices>"#;

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

    #[test]
    fn query_command_only_requests_the_selected_device() {
        let command = query_command("ab:01.0");
        assert_eq!(command.as_std().get_program(), "mlxfwmanager");
        assert_eq!(
            command.as_std().get_args().collect::<Vec<_>>(),
            ["--dev", "ab:01.0", "--query-format", "xml"]
        );
    }

    /// Exit status, cardinality and stderr truncation apply to the collected `Output`.
    #[cfg(unix)]
    #[test]
    fn query_results_preserve_exit_policy_and_bounded_diagnostics() {
        use std::os::unix::process::ExitStatusExt;
        use std::process::ExitStatus;

        for (name, raw_status, xml, stderr, expected_error) in [
            ("successful query", 0, DEVICE_XML.as_bytes(), String::new(), None),
            ("usable code one", 1 << 8, DEVICE_XML.as_bytes(), String::new(), None),
            (
                "two devices",
                0,
                br#"<Devices><Device pciName="ab:01.0" type="BlueField3"/><Device pciName="ab:02.0" type="BlueField3"/></Devices>"#.as_slice(),
                String::new(),
                Some("expected one device, received 2"),
            ),
            (
                "failed command despite usable XML",
                2 << 8,
                DEVICE_XML.as_bytes(),
                "query unavailable".into(),
                Some("query exited with"),
            ),
            (
                "code one without XML",
                1 << 8,
                b"".as_slice(),
                "permission denied for selected device".into(),
                Some("permission denied for selected device"),
            ),
            (
                "truncated stderr",
                1 << 8,
                b"".as_slice(),
                format!("{}hidden-tail", "0".repeat(5000)),
                Some("[truncated]"),
            ),
            (
                "signal despite usable XML",
                9,
                DEVICE_XML.as_bytes(),
                String::new(),
                Some("query exited with"),
            ),
            (
                "invalid UTF-8 retains diagnostics",
                0,
                b"\xff".as_slice(),
                "invalid output from device".into(),
                Some("query output is not UTF-8"),
            ),
        ] {
            let result = parse_query_output(Output {
                status: ExitStatus::from_raw(raw_status),
                stdout: xml.to_vec(),
                stderr: stderr.as_bytes().to_vec(),
            });
            match expected_error {
                None => {
                    let report = result.unwrap_or_else(|error| panic!("{name}: {error}"));
                    assert_eq!(report.pci_name, "ab:01.0", "{name}");
                    assert_eq!(
                        report.fw_version_current.as_deref(),
                        Some("32.43.1014"),
                        "{name}"
                    );
                }
                Some(expected) => {
                    let message = result.unwrap_err().to_string();
                    assert!(message.contains(expected), "{name}: {message}");
                    if stderr.len() > 4096 {
                        assert!(message.ends_with(&format!("{} [truncated]", "0".repeat(4096))), "{name}: {message}");
                        assert!(!message.contains(&"0".repeat(4097)), "{name}: {message}");
                        assert!(!message.contains("hidden-tail"), "{name}: {message}");
                    } else {
                        assert!(message.ends_with(&stderr), "{name}: {message}");
                    }
                }
            }
        }
    }

    #[tokio::test]
    async fn output_reader_enforces_the_exact_byte_limit() {
        for (name, size, accepted) in [
            ("at the limit", MAX_QUERY_OUTPUT_BYTES as usize, true),
            ("one byte over", MAX_QUERY_OUTPUT_BYTES as usize + 1, false),
        ] {
            let bytes = vec![b'x'; size];
            let result = read_query_output(bytes.as_slice()).await;
            if accepted {
                assert_eq!(result.unwrap(), bytes, "{name}");
            } else {
                assert!(
                    matches!(result, Err(FirmwarePreflightError::InvalidReport(_))),
                    "{name}: {result:?}"
                );
            }
        }
    }

    #[cfg(unix)]
    mod process {
        use tempfile::TempDir;

        use super::*;

        struct PreparedFixture {
            directory: TempDir,
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
                    directory,
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

        /// Exercise file validation, process collection, parsing and comparison
        /// together with a system executable reading an ordinary data fixture.
        #[tokio::test]
        async fn inspection_uses_checked_files_and_query_output() {
            let fixture = PreparedFixture::new();
            let report_path = fixture.directory.path().join("report.xml");
            let observed_version = "32.42.1000";
            std::fs::write(
                &report_path,
                DEVICE_XML.replace("32.43.1014", observed_version),
            )
            .unwrap();
            let mut command = Command::new("cat");
            command.arg(&report_path);
            let report = inspect_with_command("ab:01.0", &desired(), fixture.files(), command)
                .await
                .unwrap();
            assert_eq!(report.comparison, FirmwareComparison::ChangeRequired);
            assert_eq!(report.device.pci_name, "ab:01.0");
            assert_eq!(
                report.device.fw_version_current.as_deref(),
                Some(observed_version)
            );
            assert_eq!(std::fs::read(&fixture.image).unwrap(), b"checked image");
            assert_eq!(std::fs::read(&fixture.config).unwrap(), b"checked config");
        }

        /// A failing installed command supplies real stderr and a nonzero status
        /// so collection cannot silently discard either before parsing.
        #[tokio::test]
        async fn query_failure_preserves_status_and_diagnostics() {
            let fixture = PreparedFixture::new();
            let missing_path = fixture.directory.path().join("missing-report.xml");
            let mut command = Command::new("cat");
            command.arg(&missing_path);
            let deadline = Instant::now() + QUERY_TIMEOUT;
            let output =
                collect_query_output(spawn_query(command).unwrap(), deadline, QUERY_TIMEOUT)
                    .await
                    .unwrap();

            assert_eq!(output.status.code(), Some(1));
            let diagnostics = String::from_utf8(output.stderr.clone()).unwrap();
            assert!(
                diagnostics.contains(missing_path.to_str().unwrap()),
                "{diagnostics}"
            );
            let error = parse_query_output(output).unwrap_err();
            assert!(error.to_string().contains(diagnostics.trim()), "{error}");
        }

        /// `touch` records execution without a generated application. Valid inputs
        /// establish that the marker is created; invalid inputs must leave it absent.
        #[tokio::test]
        async fn invalid_prepared_inputs_do_not_run_the_tool() {
            enum InputFault {
                None,
                MissingConfiguration,
                EmptyImage,
                NonregularImage,
                InvalidTarget,
            }
            for (name, fault) in [
                ("valid inputs reach the command", InputFault::None),
                ("missing configuration", InputFault::MissingConfiguration),
                ("empty image", InputFault::EmptyImage),
                ("nonregular image", InputFault::NonregularImage),
                (
                    "invalid target before missing image",
                    InputFault::InvalidTarget,
                ),
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
                let marker = fixture.directory.path().join("invoked");
                let mut command = Command::new("touch");
                command.arg(&marker);
                let result =
                    inspect_with_command("ab:01.0", &desired, fixture.files(), command).await;
                assert_eq!(marker.exists(), matches!(fault, InputFault::None), "{name}");
                match fault {
                    InputFault::None => assert!(
                        matches!(result, Err(FirmwarePreflightError::InvalidReport(_))),
                        "{name}: {result:?}"
                    ),
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

        /// `dd` continuously fills either pipe. An overflow must return an error
        /// and leave no child process to reap.
        #[cfg(target_os = "linux")]
        #[tokio::test]
        async fn excessive_output_stops_the_child_on_either_stream() {
            use std::os::fd::AsFd;

            for stream in ["/dev/stdout", "/dev/stderr"] {
                let mut command = Command::new("dd");
                command
                    .args(["if=/dev/zero", "bs=65536", "status=none"])
                    .arg(format!("of={stream}"));
                let deadline = Instant::now() + QUERY_TIMEOUT;
                let child = spawn_query(command).unwrap();
                let pid = child.id().unwrap();
                assert!(
                    Path::new("/proc").join(pid.to_string()).exists(),
                    "{stream}"
                );
                // Keep a reader open so dropping the collector's pipe cannot
                // terminate `dd` through SIGPIPE instead of the explicit kill.
                let _reader = if stream == "/dev/stdout" {
                    child.stdout.as_ref().unwrap().as_fd().try_clone_to_owned()
                } else {
                    child.stderr.as_ref().unwrap().as_fd().try_clone_to_owned()
                }
                .unwrap();
                let error = tokio::time::timeout(
                    Duration::from_secs(5),
                    collect_query_output(child, deadline, QUERY_TIMEOUT),
                )
                .await
                .expect("overflowing query must terminate promptly")
                .unwrap_err();
                assert!(
                    matches!(&error, FirmwarePreflightError::InvalidReport(message)
                        if message.contains("query output exceeded 1 MiB")),
                    "{stream}: {error}"
                );
                assert!(
                    !Path::new("/proc").join(pid.to_string()).exists(),
                    "{stream}"
                );
            }
        }

        /// Timeout waits for termination; cancellation requests it and Tokio
        /// reaps asynchronously.
        #[cfg(target_os = "linux")]
        #[tokio::test]
        async fn query_timeout_and_cancellation_stop_the_child() {
            let timeout = Duration::from_millis(100);
            let mut command = Command::new("sleep");
            command.arg("30");
            let error =
                tokio::time::timeout(Duration::from_secs(5), query_device(command, timeout))
                    .await
                    .expect("query must enforce its timeout")
                    .unwrap_err();
            assert!(
                matches!(error, FirmwarePreflightError::QueryTimeout(duration) if duration == timeout)
            );

            let mut command = Command::new("sleep");
            command.arg("30");
            let deadline = Instant::now() + timeout;
            let child = spawn_query(command).unwrap();
            let pid = child.id().unwrap();
            assert!(Path::new("/proc").join(pid.to_string()).exists());
            let error = tokio::time::timeout(
                Duration::from_secs(5),
                collect_query_output(child, deadline, timeout),
            )
            .await
            .expect("timed-out query must terminate promptly")
            .unwrap_err();
            assert!(
                matches!(error, FirmwarePreflightError::QueryTimeout(duration) if duration == timeout)
            );
            assert!(!Path::new("/proc").join(pid.to_string()).exists());

            let mut command = Command::new("sleep");
            command.arg("30");
            let child = spawn_query(command).unwrap();
            let pid_path = Path::new("/proc").join(child.id().unwrap().to_string());
            assert!(pid_path.exists());
            // `biased` polls collection before the ready cancellation branch,
            // so this drops an active collector with its pipes already taken.
            tokio::select! {
                biased;
                result = collect_query_output(child, Instant::now() + QUERY_TIMEOUT, QUERY_TIMEOUT) => {
                    panic!("sleep exited before cancellation: {result:?}");
                }
                () = std::future::ready(()) => {}
            }
            tokio::time::timeout(Duration::from_secs(5), async {
                while pid_path.exists() {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("cancelled query process must exit");
        }
    }
}

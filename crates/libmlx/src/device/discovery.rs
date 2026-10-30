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

use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::str::FromStr;
use std::time::Duration;

use carbide_libmlx_model::device::info::MlxDeviceInfo;
use mac_address::MacAddress;
use quick_xml::de::from_str;
use serde::Deserialize;
use thiserror::Error;
use tracing::{debug, warn};

use crate::device::filters::DeviceFilter;
use crate::runner::FirmwareToolExecutor;
use crate::runner::attempt::AttemptError;
use crate::runner::command_builder::CommandSpec;
use crate::runner::error::MlxRunnerError;

// Match the command runner's default for one query, including output collection.
pub(crate) const QUERY_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_QUERY_DIAGNOSTIC_BYTES: usize = 4096;

/// Failures from inventory queries, before validating hardware facts.
#[derive(Debug, Error)]
pub enum DeviceDiscoveryError {
    /// A direct synchronous query failed to start or collect its process result.
    #[error("mlxfwmanager query failed: {0}")]
    DirectQuery(#[from] MlxRunnerError),
    /// Process failure, including deadline, output-limit, and cleanup diagnostics.
    #[error("mlxfwmanager query failed: {0}")]
    QueryAttempt(#[from] AttemptError),
    /// The MST inventory query did not finish successfully within its limits.
    #[error("mst status failed: {0}")]
    MstQueryAttempt(#[source] MstQueryAttemptError),
    /// MST did not provide one unambiguous PCI address for a reported alias.
    #[error("invalid mst status result: {0}")]
    InvalidMstReport(String),
    /// An unsupported exit status, unusable XML, or unexpected query cardinality.
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
}

/// An MST attempt failure stored without enlarging every discovery error.
/// Its source exposes the concrete attempt, including process settlement evidence.
#[derive(Debug)]
pub struct MstQueryAttemptError(Box<AttemptError>);

impl From<AttemptError> for MstQueryAttemptError {
    fn from(error: AttemptError) -> Self {
        Self(Box::new(error))
    }
}

impl std::fmt::Display for MstQueryAttemptError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self.0.as_ref(), formatter)
    }
}

impl std::error::Error for MstQueryAttemptError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        // A boxed source's own source() skips AttemptError and loses its status
        // and cleanup evidence. Expose the inner error itself to chain walkers.
        Some(self.0.as_ref())
    }
}

impl DeviceDiscoveryError {
    /// Keep the String-returning discovery and report APIs' messages stable.
    /// Callers deciding whether to release hardware access use the typed APIs
    /// instead, so process completion evidence is not lost at that boundary.
    pub(super) fn into_legacy_message(self) -> String {
        match self {
            Self::DirectQuery(MlxRunnerError::Io(source)) => {
                format!("failed to build cmd: {source}")
            }
            Self::InvalidReport(message) => message,
            error => error.to_string(),
        }
    }
}

// DevicesXml represents the root XML structure
// from mlxfwmanager output.
#[derive(Debug, Deserialize)]
struct DevicesXml {
    #[serde(rename = "Device")]
    devices: Vec<DeviceXml>,
}

// DeviceXml represents a single device entry from
// mlxfwmanager XML output.
// Missing hardware facts use the same empty-to-None conversion as MFT's
// placeholders. The PCI locator and device type remain required.
#[derive(Debug, Deserialize)]
struct DeviceXml {
    #[serde(rename = "@pciName")]
    pci_name: String,
    #[serde(rename = "@type")]
    device_type: String,
    #[serde(rename = "@psid", default)]
    psid: String,
    #[serde(rename = "@partNumber", default)]
    part_number: String,
    #[serde(rename = "Versions", default)]
    versions: VersionsXml,
    #[serde(rename = "MACs", default)]
    macs: MacsXml,
    #[serde(rename = "GUIDs", default)]
    guids: GuidsXml,
    #[serde(rename = "Description", default)]
    description: String,
    #[serde(rename = "Status", default)]
    status: String,
}

// VersionsXml represents the version information section
// from mlxfwmanager XML.
#[derive(Debug, Default, Deserialize)]
struct VersionsXml {
    #[serde(rename = "FW", default)]
    fw: Option<VersionXml>,
    #[serde(rename = "PXE", default)]
    pxe: Option<VersionXml>,
    #[serde(rename = "UEFI", default)]
    uefi: Option<VersionXml>,
    #[serde(rename = "UEFI_Virtio_blk", default)]
    uefi_virtio_blk: Option<VersionXml>,
    #[serde(rename = "UEFI_Virtio_net", default)]
    uefi_virtio_net: Option<VersionXml>,
}

// VersionXml reads the installed version; available image versions are unused.
#[derive(Debug, Deserialize)]
struct VersionXml {
    #[serde(rename = "@current", default)]
    current: String,
}

// MacsXml represents MAC address information from
// mlxfwmanager XML.
#[derive(Debug, Default, Deserialize)]
struct MacsXml {
    #[serde(rename = "@Base_Mac", default)]
    base_mac: String,
}

#[derive(Debug, Default, Deserialize)]
struct GuidsXml {
    #[serde(rename = "@Base_Guid", default)]
    base_guid: String,
}

// discover_devices finds all devices using mlxfwmanager.
pub fn discover_devices() -> Result<Vec<MlxDeviceInfo>, String> {
    discover_devices_with_settlement().map_err(DeviceDiscoveryError::into_legacy_message)
}

/// `discover_devices_with_settlement` preserves process errors so callers
/// holding hardware exclusion can distinguish an ordinary failure from a
/// process whose termination could not be established. Like `discover_devices`,
/// this synchronous query has no deadline; use the bounded API for supervised
/// inventory.
pub(crate) fn discover_devices_with_settlement() -> Result<Vec<MlxDeviceInfo>, DeviceDiscoveryError>
{
    debug!("Running mlxfwmanager to discover devices");

    let output = execute_direct_query(&["--query-format", "xml"])?;

    // In cases where DPUs are returned, it looks like DPUs that are
    // currently in lockdown won't return data to mlxfwmanager. The
    // XML is still generated, just with some empty fields, and all
    // of the SuperNIC XML data is unaffected. The problem, though,
    // is that even though the full set of XML is returned, the
    // command itself returns exit code 1, so we need to allow that
    // here, and then just deal with issues as part of attempting
    // to parse the XML output.
    if !matches!(output.status.code(), Some(0) | Some(1)) {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(DeviceDiscoveryError::InvalidReport(format!(
            "mlxfwmanager failed with unexpected exit code: {stderr}"
        )));
    }

    {
        let xml_content = String::from_utf8_lossy(&output.stdout);
        warn!(xml = %xml_content, "mlxfwmanager XML output");
        parse_mlxfwmanager_xml(&xml_content).map_err(DeviceDiscoveryError::InvalidReport)
    }
}

// discover_device loads a specific device using mlxfwmanager.
// The actual XML returned is still "devices", but will only
// contain the target device.
pub fn discover_device(device: &str) -> Result<MlxDeviceInfo, String> {
    discover_device_with_settlement(device).map_err(DeviceDiscoveryError::into_legacy_message)
}

/// `discover_device_with_settlement` preserves the process completion evidence
/// for one device. This synchronous query has no deadline; use
/// [`discover_pci_devices_bounded`] for supervised inventory.
pub fn discover_device_with_settlement(
    device: &str,
) -> Result<MlxDeviceInfo, DeviceDiscoveryError> {
    debug!(device, "Running mlxfwmanager to discover device");

    let output = execute_direct_query(&["--dev", device, "--query-format", "xml"])?;

    // In cases where DPUs are returned, it looks like DPUs that are
    // currently in lockdown won't return data to mlxfwmanager. The
    // XML is still generated, just with some empty fields, and all
    // of the SuperNIC XML data is unaffected. The problem, though,
    // is that even though the full set of XML is returned, the
    // command itself returns exit code 1, so we need to allow that
    // here, and then just deal with issues as part of attempting
    // to parse the XML output.
    if !matches!(output.status.code(), Some(0) | Some(1)) {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(DeviceDiscoveryError::InvalidReport(format!(
            "mlxfwmanager failed with unexpected exit code: {stderr}"
        )));
    }

    let xml_content = String::from_utf8_lossy(&output.stdout);
    debug!(xml = %xml_content, "mlxfwmanager XML output");

    let devices =
        parse_mlxfwmanager_xml(&xml_content).map_err(DeviceDiscoveryError::InvalidReport)?;
    if devices.len() > 1 {
        return Err(DeviceDiscoveryError::InvalidReport(format!(
            "only expected a single device returned for device: {device}"
        )));
    }
    if devices.is_empty() {
        return Err(DeviceDiscoveryError::InvalidReport(format!(
            "no devices returned for device: {device}"
        )));
    }
    Ok(devices.into_iter().next().unwrap())
}

// Splitting spawn from wait keeps a lost child result from becoming an
// ordinary discovery failure that authorizes another hardware operation.
fn execute_direct_query(args: &[&str]) -> Result<Output, MlxRunnerError> {
    let child = Command::new("mlxfwmanager")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    child
        .wait_with_output()
        .map_err(|source| MlxRunnerError::ProcessUnsettled {
            command: "mlxfwmanager query".to_string(),
            source,
        })
}

/// Query all devices with the same deadline and output limits as
/// [`discover_device_bounded`]. Callers must defer this inventory probe while
/// firmware work owns the host; the query does not acquire mutation exclusion.
pub async fn discover_devices_bounded() -> Result<Vec<MlxDeviceInfo>, DeviceDiscoveryError> {
    discover_devices_with_program(
        Path::new("mlxfwmanager"),
        QUERY_TIMEOUT,
        &FirmwareToolExecutor::default(),
    )
    .await
}

/// Query inventory for Scout using full, lowercase PCI addresses.
///
/// Uses [`discover_devices_bounded`]'s process and output limits. If inventory
/// contains an MST path, runs `mst status` once with those same limits and
/// requires exactly one full PCI address in that path's device entry. Missing,
/// repeated, or invalid mappings reject the inventory; other device facts and
/// report order are preserved. Inventory containing only PCI addresses does not
/// run MST. Callers must defer both queries while firmware owns the host.
pub async fn discover_pci_devices_bounded() -> Result<Vec<MlxDeviceInfo>, DeviceDiscoveryError> {
    discover_pci_devices_with_programs(
        Path::new("mlxfwmanager"),
        Path::new("mst"),
        QUERY_TIMEOUT,
        &FirmwareToolExecutor::default(),
    )
    .await
}

/// Resolve MST inventory only before publication, never while polling firmware.
async fn discover_pci_devices_with_programs(
    query_program: &Path,
    mst_program: &Path,
    timeout: Duration,
    executor: &FirmwareToolExecutor,
) -> Result<Vec<MlxDeviceInfo>, DeviceDiscoveryError> {
    let mut devices = discover_devices_with_program(query_program, timeout, executor).await?;
    let mst_status = if devices
        .iter()
        .any(|device| device.pci_name.starts_with("/dev/mst/"))
    {
        let command = CommandSpec::new(mst_program.to_string_lossy()).args(["status"]);
        let output = executor
            .execute(&command, timeout)
            .await
            .map_err(|error| DeviceDiscoveryError::MstQueryAttempt(error.into()))?;
        let diagnostics = String::from_utf8_lossy(
            &output.stderr[..output.stderr.len().min(MAX_QUERY_DIAGNOSTIC_BYTES)],
        );
        if !output.status.success() {
            return Err(DeviceDiscoveryError::InvalidMstReport(format!(
                "query exited with {}: {diagnostics}",
                output.status,
            )));
        }
        Some(String::from_utf8(output.stdout).map_err(|error| {
            DeviceDiscoveryError::InvalidMstReport(format!(
                "output is not UTF-8: {error}; stderr: {diagnostics}",
            ))
        })?)
    } else {
        None
    };
    for device in &mut devices {
        device.pci_name = if let Some(status) = mst_status.as_deref()
            && device.pci_name.starts_with("/dev/mst/")
        {
            resolve_mst_pci_address(&device.pci_name, status)?
        } else {
            normalize_pci_address(&device.pci_name).ok_or_else(|| {
                DeviceDiscoveryError::InvalidReport(format!(
                    "unsupported PCI locator {}",
                    device.pci_name,
                ))
            })?
        };
    }
    Ok(devices)
}

/// Read the selected MST entry only; a following device's metadata cannot fill
/// in missing identity for this one. `mst status` indents each entry's metadata.
fn resolve_mst_pci_address(alias: &str, status: &str) -> Result<String, DeviceDiscoveryError> {
    let mut selected = false;
    let mut seen = false;
    let mut address = None;
    for line in status.lines() {
        let mut fields = line.split_whitespace();
        let Some(first) = fields.next() else {
            selected = false;
            continue;
        };
        if first.starts_with("/dev/mst/") {
            selected = first == alias;
            if selected {
                if seen || fields.next() != Some("-") {
                    return Err(DeviceDiscoveryError::InvalidMstReport(format!(
                        "invalid or repeated device entry for {alias}",
                    )));
                }
                seen = true;
            }
            continue;
        }
        if !line.starts_with(char::is_whitespace) {
            selected = false;
        }
        if !selected {
            continue;
        }
        for field in std::iter::once(first).chain(fields) {
            let Some(value) = field.strip_prefix("domain:bus:dev.fn=") else {
                continue;
            };
            if address.is_some() {
                return Err(DeviceDiscoveryError::InvalidMstReport(format!(
                    "multiple PCI mappings for {alias}",
                )));
            }
            address = Some(
                (value.len() == 12)
                    .then(|| normalize_pci_address(value))
                    .flatten()
                    .ok_or_else(|| {
                        DeviceDiscoveryError::InvalidMstReport(format!(
                            "invalid full PCI address for {alias}: {value}",
                        ))
                    })?,
            );
        }
    }
    address.ok_or_else(|| {
        DeviceDiscoveryError::InvalidMstReport(format!("no PCI mapping for {alias}"))
    })
}

/// Validate a complete PCI function and add domain zero to abbreviated BDFs.
/// Consumers use this same syntax check before matching or grouping devices.
pub(crate) fn normalize_pci_address(device: &str) -> Option<String> {
    let device = match device.len() {
        7 => format!("0000:{device}"),
        12 => device.to_string(),
        _ => return None,
    };
    let bytes = device.as_bytes();
    if bytes[4] != b':'
        || bytes[7] != b':'
        || bytes[10] != b'.'
        || !bytes[..4]
            .iter()
            .chain(&bytes[5..7])
            .chain(&bytes[8..10])
            .all(u8::is_ascii_hexdigit)
        || !matches!(bytes[8], b'0' | b'1')
        || !matches!(bytes[11], b'0'..=b'7')
    {
        return None;
    }
    Some(device.to_ascii_lowercase())
}

/// Keep the all-device query bounded without changing legacy synchronous readers.
async fn discover_devices_with_program(
    program: &Path,
    timeout: Duration,
    executor: &FirmwareToolExecutor,
) -> Result<Vec<MlxDeviceInfo>, DeviceDiscoveryError> {
    let command = CommandSpec::new(program.to_string_lossy()).args(["--query-format", "xml"]);
    let output = executor.execute(&command, timeout).await?;
    parse_devices_output(output).map(|(devices, _)| devices)
}

/// `discover_device_bounded` queries one PCI address, MST path, or other MFT-supported locator.
///
/// Runs `mlxfwmanager --dev <device> --query-format xml` once with a 30-second
/// deadline covering execution and output collection. Exit codes 0 and 1 are
/// accepted only with valid UTF-8 XML describing exactly one device. Missing
/// optional hardware facts remain absent; callers validate identity and firmware.
///
/// Each output stream is capped at 1 MiB. Linux and a sibling
/// `mlx-firmware-supervisor` executable are required. Successful completion waits
/// for the tool, all descendants, and both streams within the query deadline.
/// Failure or timeout requests termination with up to five seconds for cleanup,
/// then up to five more seconds for result delivery. Cancellation requests the
/// same cleanup without waiting for confirmation. Other platforms fail before
/// spawning. Unusable-report errors retain at most the first 4 KiB of stderr.
pub async fn discover_device_bounded(device: &str) -> Result<MlxDeviceInfo, DeviceDiscoveryError> {
    discover_device_bounded_with_executor(device, &FirmwareToolExecutor::default()).await
}

/// `discover_device_bounded_with_executor` uses the caller's supervisor location.
/// All query limits and validation follow [`discover_device_bounded`].
pub async fn discover_device_bounded_with_executor(
    device: &str,
    executor: &FirmwareToolExecutor,
) -> Result<MlxDeviceInfo, DeviceDiscoveryError> {
    discover_device_with_program(Path::new("mlxfwmanager"), device, QUERY_TIMEOUT, executor).await
}

pub(crate) async fn discover_device_with_program(
    program: &Path,
    device: &str,
    timeout: Duration,
    executor: &FirmwareToolExecutor,
) -> Result<MlxDeviceInfo, DeviceDiscoveryError> {
    let command = CommandSpec::new(program.to_string_lossy()).args([
        "--dev",
        device,
        "--query-format",
        "xml",
    ]);
    let output = executor.execute(&command, timeout).await?;
    parse_query_output(output)
}

fn parse_query_output(output: Output) -> Result<MlxDeviceInfo, DeviceDiscoveryError> {
    let (devices, diagnostics) = parse_devices_output(output)?;
    let [device]: [MlxDeviceInfo; 1] =
        devices.try_into().map_err(|devices: Vec<MlxDeviceInfo>| {
            DeviceDiscoveryError::InvalidReport(format!(
                "expected one device, received {}; stderr: {diagnostics}",
                devices.len()
            ))
        })?;
    Ok(device)
}

/// Apply the common exit, encoding, and diagnostic policy before cardinality checks.
fn parse_devices_output(
    output: Output,
) -> Result<(Vec<MlxDeviceInfo>, String), DeviceDiscoveryError> {
    let (status, xml, diagnostics) = (output.status, output.stdout, output.stderr);
    let diagnostics = if diagnostics.len() > MAX_QUERY_DIAGNOSTIC_BYTES {
        format!(
            "{} [truncated]",
            String::from_utf8_lossy(&diagnostics[..MAX_QUERY_DIAGNOSTIC_BYTES])
        )
    } else {
        String::from_utf8_lossy(&diagnostics).into_owned()
    };
    // Locked devices can return usable XML with code 1. Keep stderr when that
    // report cannot be used, since it may explain missing access or device facts.
    if !matches!(status.code(), Some(0) | Some(1)) {
        return Err(DeviceDiscoveryError::InvalidReport(format!(
            "query exited with {status}: {diagnostics}"
        )));
    }
    let xml = String::from_utf8(xml).map_err(|source| DeviceDiscoveryError::ReportEncoding {
        source,
        stderr: diagnostics.clone(),
    })?;
    let devices = parse_mlxfwmanager_xml(&xml).map_err(|error| {
        DeviceDiscoveryError::InvalidReport(format!("{error}; stderr: {diagnostics}"))
    })?;
    Ok((devices, diagnostics))
}

// discover_devices_with_filters finds devices that match
// the specified filters.
pub fn discover_devices_with_filters(filter: DeviceFilter) -> Result<Vec<MlxDeviceInfo>, String> {
    let all_devices = discover_devices()?;

    let filtered_devices: Vec<MlxDeviceInfo> = all_devices
        .into_iter()
        .filter(|device| {
            let matches = filter.matches(device);
            debug!(
                device = device.pci_name_pretty(),
                device_type = device.device_type_pretty(),
                part_number = device.part_number_pretty(),
                firmware_version = device.fw_version_current_pretty(),
                matches,
                "Device matches filter"
            );
            matches
        })
        .collect();

    debug!(
        device_count = filtered_devices.len(),
        %filter,
        "Found devices matching filter"
    );

    Ok(filtered_devices)
}

/// Parses MFT device XML, retaining devices with missing optional hardware facts.
/// Missing facts and MFT placeholders become `None`. Malformed XML, missing
/// `pciName` or `type` attributes, and reports without device entries are errors.
pub fn parse_mlxfwmanager_xml(xml_content: &str) -> Result<Vec<MlxDeviceInfo>, String> {
    let devices_xml: DevicesXml =
        from_str(xml_content).map_err(|e| format!("Failed to parse mlxfwmanager XML: {e}"))?;

    let mut devices = Vec::new();

    for device_xml in devices_xml.devices {
        let pci_name = convert_pci_name_to_address(&device_xml.pci_name)?;

        // If the MAC fails to parse, just return None.
        let base_mac = MacAddress::from_str(&device_xml.macs.base_mac).ok();
        // ...and if any of the "optional" fields look to have been
        // excluded by mlxfwmanager, parse it into None.
        let device_info = MlxDeviceInfo {
            pci_name,
            device_type: device_xml.device_type,
            psid: parse_optional_xml_field(&device_xml.psid),
            device_description: parse_optional_xml_field(&device_xml.description),
            part_number: parse_optional_xml_field(&device_xml.part_number),
            status: parse_optional_xml_field(&device_xml.status),
            fw_version_current: device_xml
                .versions
                .fw
                .as_ref()
                .and_then(|fw| parse_optional_xml_field(&fw.current)),
            pxe_version_current: device_xml
                .versions
                .pxe
                .as_ref()
                .and_then(|pxe| parse_optional_xml_field(&pxe.current)),
            uefi_version_current: device_xml
                .versions
                .uefi
                .as_ref()
                .and_then(|uefi| parse_optional_xml_field(&uefi.current)),
            uefi_version_virtio_blk_current: device_xml
                .versions
                .uefi_virtio_blk
                .as_ref()
                .and_then(|virtio_blk| parse_optional_xml_field(&virtio_blk.current)),
            uefi_version_virtio_net_current: device_xml
                .versions
                .uefi_virtio_net
                .as_ref()
                .and_then(|virtio_net| parse_optional_xml_field(&virtio_net.current)),
            base_mac,
            base_guid: parse_optional_xml_field(&device_xml.guids.base_guid),
        };
        devices.push(device_info);
    }

    debug!(device_count = devices.len(), "Discovered MLX devices");
    Ok(devices)
}

// parse_optional_xml_field converts common "missing" indicators to None
// for optional fields.
fn parse_optional_xml_field(value: &str) -> Option<String> {
    if value.is_empty() || value == "--" || value == "N/A" {
        None
    } else {
        Some(value.to_string())
    }
}

// convert_pci_name_to_address converts PCI device name from
// mlxfwmanager format to mlxconfig format.
pub fn convert_pci_name_to_address(pci_name: &str) -> Result<String, String> {
    // Clean up the PCI address format if needed
    let cleaned_address = if pci_name.starts_with("0000:") {
        // Remove leading domain if present: "0000:01:00.0" -> "01:00.0".
        pci_name
            .strip_prefix("0000:")
            .unwrap_or(pci_name)
            .to_string()
    } else {
        // Pass through MST device paths or already-clean PCI addresses.
        pci_name.to_string()
    };

    debug!(
        pci_name,
        pci_address = cleaned_address.as_str(),
        "Converted PCI name to address"
    );
    Ok(cleaned_address)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    // A sibling test's fork must not inherit a writable executable descriptor.
    #[cfg(target_os = "linux")]
    async fn write_fixture_executable(path: &Path, script: &str) {
        let status = tokio::time::timeout(
            Duration::from_secs(5),
            tokio::process::Command::new("/bin/sh")
                .args([
                    "-c",
                    "set -eu; umask 077; printf '%s' \"$2\" > \"$1\"; chmod 700 \"$1\"",
                    "fixture-writer",
                ])
                .arg(path)
                .arg(script)
                .kill_on_drop(true)
                .status(),
        )
        .await
        .expect("fixture writer must finish within its watchdog")
        .expect("start isolated fixture writer");
        assert!(status.success(), "fixture writer failed: {status}");
    }

    #[cfg(unix)]
    const DEVICE_XML: &str = r#"<Devices><Device pciName="ab:01.0" type="BlueField3" partNumber="part" psid="PSID"><Versions><FW current="32.43.1014"/></Versions></Device></Devices>"#;

    const MST_STATUS: &str = "MST modules:\n------------\nMST PCI module loaded\nMST PCI configuration module loaded\n\nMST devices:\n------------\n/dev/mst/mt41692_pciconf1         - PCI configuration cycles access.\n                                   domain:bus:dev.fn=0001:CA:00.1 addr.reg=88 data.reg=92 cr_bar.gw_offset=-1\n                                   Chip revision is: 00\n/dev/mst/mt41692_pciconf0         - PCI configuration cycles access.\n                                   domain:bus:dev.fn=0000:AB:01.0 addr.reg=88 data.reg=92 cr_bar.gw_offset=-1\n                                   Chip revision is: 00\n";

    /// An MST alias gets identity only from its own unique metadata block, never
    /// from a neighboring device, repeated row, or partial PCI spelling.
    #[test]
    fn mst_aliases_require_one_exact_device_mapping() {
        use carbide_test_support::Outcome::{Fails, Yields};
        use carbide_test_support::scenarios;

        scenarios!(run = |status| resolve_mst_pci_address("/dev/mst/mt41692_pciconf0", status).map_err(drop);
            // MFT emits full BDF metadata below each path; preserve its domain.
            "documented per-device metadata" {
                MST_STATUS => Yields("0000:ab:01.0".to_string()),
            }
            // A neighboring path or detached metadata cannot repair a missing mapping.
            "missing selected mapping" {
                // The next device's valid metadata belongs to that device only.
                "/dev/mst/mt41692_pciconf0 - PCI configuration cycles access.\n/dev/mst/other - PCI configuration cycles access.\n  domain:bus:dev.fn=0000:ab:01.0\n" => Fails,
                // Prefix matching must not confuse another function's MST path.
                "/dev/mst/mt41692_pciconf0.1 - PCI configuration cycles access.\n  domain:bus:dev.fn=0000:ab:01.0\n" => Fails,
                // Metadata before the selected header has no selected owner.
                "  domain:bus:dev.fn=0000:ab:01.0\n/dev/mst/mt41692_pciconf0 - PCI configuration cycles access.\n" => Fails,
                // A new status section ends the current device's metadata.
                "/dev/mst/mt41692_pciconf0 - PCI configuration cycles access.\nRemote MST devices:\n  domain:bus:dev.fn=0000:ab:01.0\n" => Fails,
            }
            // Even identical repeated rows are not a unique mapping.
            "repeated or ambiguous mappings" {
                // Do not pick the first of two rows naming the same alias.
                "/dev/mst/mt41692_pciconf0 - PCI configuration cycles access.\n  domain:bus:dev.fn=0000:ab:01.0\n/dev/mst/mt41692_pciconf0 - PCI configuration cycles access.\n  domain:bus:dev.fn=0000:ab:01.0\n" => Fails,
                // Multiple metadata values cannot silently choose a PCI domain.
                "/dev/mst/mt41692_pciconf0 - PCI configuration cycles access.\n  domain:bus:dev.fn=0000:ab:01.0 domain:bus:dev.fn=0001:ab:01.0\n" => Fails,
            }
            // Metadata must supply an explicit domain and a valid PCI device number.
            "invalid full PCI addresses" {
                // MST metadata must identify the domain rather than default it.
                "/dev/mst/mt41692_pciconf0 - PCI configuration cycles access.\n  domain:bus:dev.fn=ab:01.0\n" => Fails,
                // A full-width string still has to satisfy PCI field bounds.
                "/dev/mst/mt41692_pciconf0 - PCI configuration cycles access.\n  domain:bus:dev.fn=0000:ab:20.0\n" => Fails,
            }
        );
    }

    /// Startup resolves MST paths through the supervised command while preserving
    /// report order and hardware facts; an all-PCI report must not run MST at all.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn pci_inventory_resolves_mst_paths_before_publication() {
        struct Case {
            scenario: &'static str,
            xml: &'static str,
            addresses: &'static [&'static str],
            queries_mst: bool,
        }

        for case in [
            // The status table orders aliases differently from discovery, and a
            // direct PCI card between them must retain its position and facts.
            Case {
                scenario: "mixed aliases and PCI addresses",
                xml: r#"<Devices><Device pciName="/dev/mst/mt41692_pciconf0" type="BlueField3" partNumber="part-a" psid="PSID-A"><Versions><FW current="32.43.1014"/></Versions><MACs Base_Mac="020000000001"/><GUIDs Base_Guid="0011223344556677"/></Device><Device pciName="0002:AB:02.0" type="ConnectX7" partNumber="part-b" psid="PSID-B"/><Device pciName="/dev/mst/mt41692_pciconf1" type="BlueField3"/></Devices>"#,
                addresses: &["0000:ab:01.0", "0002:ab:02.0", "0001:ca:00.1"],
                queries_mst: true,
            },
            // Hosts that already report PCI addresses need no MST installation.
            Case {
                scenario: "PCI addresses need no MST query",
                xml: DEVICE_XML,
                addresses: &["0000:ab:01.0"],
                queries_mst: false,
            },
        ] {
            let directory = tempfile::tempdir().unwrap();
            std::fs::write(directory.path().join("inventory.xml"), case.xml).unwrap();
            std::fs::write(directory.path().join("mst-status"), MST_STATUS).unwrap();

            // Write executables in a short-lived process so another test's fork
            // cannot inherit a writable descriptor and cause ETXTBSY.
            let status = tokio::time::timeout(
                Duration::from_secs(5),
                tokio::process::Command::new("/bin/sh")
                    .args([
                        "-c",
                        "set -eu; umask 077; while [ \"$#\" -gt 0 ]; do printf '%s' \"$2\" > \"$1\"; chmod 700 \"$1\"; shift 2; done",
                        "fixture-writer",
                    ])
                    .arg(directory.path().join("mlxfwmanager"))
                    .arg("#!/bin/sh\ncd -- \"${0%/*}\"\nprintf '%s\\n' \"$@\" > query-arguments\ncat inventory.xml\nexit 1\n")
                    .arg(directory.path().join("mst"))
                    .arg("#!/bin/sh\ncd -- \"${0%/*}\"\nprintf '%s\\n' \"$@\" > mst-arguments\ncat mst-status\n")
                    .kill_on_drop(true)
                    .status(),
            )
            .await
            .expect("fixture writer must finish within its watchdog")
            .unwrap();
            assert!(status.success(), "{}: fixture writer", case.scenario);

            let devices = tokio::time::timeout(
                Duration::from_secs(5),
                discover_pci_devices_with_programs(
                    &directory.path().join("mlxfwmanager"),
                    &directory.path().join("mst"),
                    QUERY_TIMEOUT,
                    &crate::runner::test_firmware_executor(),
                ),
            )
            .await
            .expect("both inventory commands must finish within the fixture watchdog")
            .unwrap_or_else(|error| panic!("{}: {error}", case.scenario));
            let mut expected = parse_mlxfwmanager_xml(case.xml).unwrap();
            for (device, address) in expected.iter_mut().zip(case.addresses) {
                device.pci_name = (*address).to_string();
            }
            assert_eq!(devices, expected, "{}", case.scenario);
            assert_eq!(
                std::fs::read_to_string(directory.path().join("query-arguments")).unwrap(),
                "--query-format\nxml\n",
                "{}",
                case.scenario,
            );
            if case.queries_mst {
                assert_eq!(
                    std::fs::read_to_string(directory.path().join("mst-arguments")).unwrap(),
                    "status\n",
                    "{}",
                    case.scenario,
                );
            } else {
                assert!(
                    !directory.path().join("mst-arguments").exists(),
                    "{}",
                    case.scenario,
                );
            }
        }
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

    /// A usable query may report an MST locator and omit firmware facts. Preserve
    /// those values so each caller can apply its own validation.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn bounded_discovery_preserves_mft_locators_without_requiring_firmware_facts() {
        let directory = tempfile::tempdir().unwrap();
        let program = directory.path().join("mlxfwmanager");
        let locator = "/dev/mst/mt41692_pciconf0";
        write_fixture_executable(
            &program,
            &format!(
                "#!/bin/sh\ncd -- \"${{0%/*}}\"\nprintf '%s\\n' \"$@\" > arguments\n\
                 printf '%s' '<Devices><Device pciName=\"{locator}\" type=\"BlueField3\"/></Devices>'\n\
                 exit 1\n"
            ),
        )
        .await;

        let device = discover_device_with_program(
            &program,
            locator,
            QUERY_TIMEOUT,
            &crate::runner::test_firmware_executor(),
        )
        .await
        .unwrap();
        assert_eq!(device.pci_name, locator);
        assert_eq!(device.part_number, None);
        assert_eq!(device.psid, None);
        assert_eq!(device.fw_version_current, None);
        assert_eq!(
            std::fs::read_to_string(directory.path().join("arguments")).unwrap(),
            format!("--dev\n{locator}\n--query-format\nxml\n")
        );
    }

    /// Startup inventory queries the host once and retains every reported card;
    /// it must not accidentally inherit the single-device cardinality rule.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn bounded_inventory_preserves_all_devices() {
        let directory = tempfile::tempdir().unwrap();
        let program = directory.path().join("mlxfwmanager");
        write_fixture_executable(
            &program,
            concat!(
                "#!/bin/sh\ncd -- \"${0%/*}\"\nprintf '%s\\n' \"$@\" > arguments\n",
                "printf '%s' '<Devices>",
                "<Device pciName=\"0000:ab:01.0\" type=\"BlueField3\"/>",
                "<Device pciName=\"0001:ab:01.0\" type=\"BlueField3\"/>",
                "</Devices>'\nexit 1\n",
            ),
        )
        .await;
        let devices = tokio::time::timeout(
            Duration::from_secs(5),
            discover_devices_with_program(
                &program,
                QUERY_TIMEOUT,
                &crate::runner::test_firmware_executor(),
            ),
        )
        .await
        .expect("inventory fixture must finish within its independent watchdog")
        .unwrap();
        assert_eq!(
            devices
                .iter()
                .map(|device| device.pci_name.as_str())
                .collect::<Vec<_>>(),
            ["ab:01.0", "0001:ab:01.0"]
        );
        assert_eq!(
            std::fs::read_to_string(directory.path().join("arguments")).unwrap(),
            "--query-format\nxml\n"
        );
    }

    /// A stuck all-device query must release startup before its tool's natural
    /// exit, with the timed-out process already gone when the caller returns.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn bounded_inventory_reaps_a_stalled_query() {
        let directory = tempfile::tempdir().unwrap();
        let program = directory.path().join("mlxfwmanager");
        write_fixture_executable(
            &program,
            "#!/bin/sh\ncd -- \"${0%/*}\"\nprintf '%s' \"$$\" > pid\nexec /bin/sleep 30\n",
        )
        .await;
        let error = tokio::time::timeout(
            Duration::from_secs(5),
            discover_devices_with_program(
                &program,
                Duration::from_millis(500),
                &crate::runner::test_firmware_executor(),
            ),
        )
        .await
        .expect("inventory deadline must precede the fixture's 30-second exit")
        .unwrap_err();
        assert!(
            matches!(
                error,
                DeviceDiscoveryError::QueryAttempt(AttemptError::Started {
                    reason: crate::runner::attempt::AttemptFailure::Deadline(_),
                    ..
                })
            ),
            "{error}"
        );
        let pid: u32 = std::fs::read_to_string(directory.path().join("pid"))
            .unwrap()
            .parse()
            .unwrap();
        assert!(
            !Path::new(&format!("/proc/{pid}")).exists(),
            "timed-out inventory must be reaped before control polling resumes"
        );
    }
}

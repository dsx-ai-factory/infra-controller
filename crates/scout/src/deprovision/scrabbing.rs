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
use std::fs;
use std::str::FromStr;

use ::rpc::forge as rpc;
use carbide_host_support::hardware_enumeration::discovery_ibs;
use carbide_instrument::emit;
use carbide_uuid::machine::MachineId;
use regex::Regex;
use scout::CarbideClientError;
use serde::Deserialize;
use tracing::Instrument;

use crate::cfg::Options;
use crate::client::create_forge_client;
use crate::deprovision::cmdrun;
use crate::metrics::{ScoutStorageDeviceCleanup, StorageDeviceType};
use crate::{CarbideClientResult, IN_QEMU_VM, platform};

fn check_memory_overwrite_efi_var() -> Result<(), CarbideClientError> {
    let name = match efivar::efi::Variable::from_str(
        "MemoryOverwriteRequestControl-e20939be-32d4-41be-a150-897f85d49829",
    ) {
        Ok(o) => o,
        Err(e) => {
            return Err(CarbideClientError::GenericError(format!(
                "Can not build EFI variable name: {e}"
            )));
        }
    };
    let s = efivar::system();
    match s.read(&name) {
        Ok((buffer, _)) => {
            if buffer.len() == 1 && buffer[0] == 1 {
                return Ok(());
            }
            Err(CarbideClientError::GenericError(format!(
                "Invalid result when reading MemoryOverwriteRequestControl efivar size={} value={}",
                buffer.len(),
                buffer[0]
            )))
        }
        Err(e) => Err(CarbideClientError::GenericError(format!(
            "Failed to read MemoryOverwriteRequestControl efivar: {e}"
        ))),
    }
}

static NVME_CLI_PROG: &str = "/usr/sbin/nvme";
static HDPARM_CLI_PROG: &str = "/usr/sbin/hdparm";
static SG_SANITIZE_CLI_PROG: &str = "/usr/bin/sg_sanitize";
static DD_CLI_PROG: &str = "/usr/bin/dd";
static LENOVO_NVMI_CLI_PROG_CANDIDATES: [&str; 4] = [
    "/opt/forge/bin/mnv_cli",
    "/opt/forge/mnv_cli",
    "/build-output/extras/opt/forge/bin/mnv_cli",
    "/build-output/extras/opt/forge/mnv_cli",
];

fn resolve_lenovo_mnv_cli_prog() -> Option<&'static str> {
    LENOVO_NVMI_CLI_PROG_CANDIDATES
        .iter()
        .copied()
        .find(|path| std::path::Path::new(path).exists())
}

lazy_static::lazy_static! {
    static ref NVME_NS_RE: Regex = Regex::new(r".*:(0x[0-9]+)").unwrap();
    static ref NVME_NSID_RE: Regex = Regex::new(r".*nsid:([0-9]+)").unwrap();
    static ref NVME_DEV_RE: Regex = Regex::new(r"/dev/nvme[0-9]+$").unwrap();
    static ref SD_DEV_RE: Regex = Regex::new(r"/dev/sd[a-z]+$").unwrap();
}

#[derive(Deserialize, Debug)]
struct NvmeParams {
    // size of NVME drive in bytes
    tnvmcap: u64,

    // controller ID
    cntlid: u64,

    // Optional Admin Command Support (OACS)
    oacs: u64,

    // serial number
    sn: String,

    // manufacturer
    mn: String,

    // firmware version
    fr: String,
}

#[derive(Deserialize, Debug)]
struct NvmeLbaFormat {
    // metadata size
    ms: u16,

    // data size (as power of 2, e.g., 9 = 512B, 12 = 4096B)
    ds: u8,

    // relative performance
    rp: u8,
}

#[derive(Deserialize, Debug)]
struct NvmeNamespaceParams {
    // LBA formats array
    lbafs: Vec<NvmeLbaFormat>,
}

async fn get_nvme_params(nvmename: &str) -> Result<NvmeParams, CarbideClientError> {
    let nvme_params_lines =
        cmdrun::run_prog(NVME_CLI_PROG, ["id-ctrl", nvmename, "-o", "json"]).await?;
    let nvme_drive_params = match serde_json::from_str(&nvme_params_lines) {
        Ok(o) => o,
        Err(e) => {
            return Err(CarbideClientError::GenericError(format!(
                "nvme id-ctrl parse error: {e}"
            )));
        }
    };
    Ok(nvme_drive_params)
}

/// Select the best LBA format with the given data size (ds).
/// Prefers formats with no metadata (ms=0) and best performance (lowest rp).
/// Returns the index and a reference to the selected format, or None if not found.
fn select_best_lba_format(
    lbafs: &[NvmeLbaFormat],
    target_ds: u8,
) -> Option<(usize, &NvmeLbaFormat)> {
    lbafs
        .iter()
        .enumerate()
        .filter(|(_, lbaf)| lbaf.ds == target_ds)
        .reduce(|(best_idx, best_lbaf), (idx, lbaf)| {
            // Prefer formats with no metadata
            if lbaf.ms == 0 && best_lbaf.ms != 0
                // If metadata is same, prefer better performance (lower rp)
                || lbaf.ms == best_lbaf.ms && lbaf.rp < best_lbaf.rp
            {
                (idx, lbaf)
            } else {
                (best_idx, best_lbaf)
            }
        })
}

/// Get namespace parameters by running nvme id-ns command.
async fn get_namespace_params(nvmename: &str) -> Result<NvmeNamespaceParams, CarbideClientError> {
    let namespace_params_lines = cmdrun::run_prog(
        NVME_CLI_PROG,
        ["id-ns", nvmename, "-n", "0xffffffff", "-o", "json"],
    )
    .await?;

    let namespace_params: NvmeNamespaceParams = match serde_json::from_str(&namespace_params_lines)
    {
        Ok(o) => o,
        Err(e) => {
            return Err(CarbideClientError::GenericError(format!(
                "nvme id-ns parse error: {e}"
            )));
        }
    };

    Ok(namespace_params)
}

/// Get the best available LBA format for the given device.
/// Prefers 512B sectors (ds=9) with no metadata and best performance.
/// Falls back to 4K sectors (ds=12) if no 512B format is available.
/// If neither is found, falls back to FLBAS 0.
async fn get_best_lba_format(nvmename: &str) -> Result<(u8, u64), CarbideClientError> {
    let namespace_params = get_namespace_params(nvmename).await?;

    // Try 512B format first (ds=9 means 2^9 = 512 bytes)
    if let Some((idx, lbaf)) = select_best_lba_format(&namespace_params.lbafs, 9) {
        tracing::info!(
            flbas_index = idx,
            device = nvmename,
            sector_size_bytes = 512u64,
            metadata_size_bytes = lbaf.ms,
            relative_performance = lbaf.rp,
            "Selected NVMe LBA format",
        );
        return Ok((idx as u8, 512u64));
    }

    // Try 4K format (ds=12 means 2^12 = 4096 bytes)
    if let Some((idx, lbaf)) = select_best_lba_format(&namespace_params.lbafs, 12) {
        tracing::info!(
            flbas_index = idx,
            device = nvmename,
            sector_size_bytes = 4096u64,
            metadata_size_bytes = lbaf.ms,
            relative_performance = lbaf.rp,
            "Selected NVMe LBA format",
        );
        return Ok((idx as u8, 4096u64));
    }

    // Fallback to FLBAS 0 - determine its actual sector size
    tracing::warn!(
        device = nvmename,
        "No 512B or 4K NVMe LBA format found; falling back to FLBAS 0",
    );
    if let Some(lbaf0) = namespace_params.lbafs.first() {
        let sector_size = 1u64 << lbaf0.ds;
        tracing::warn!(
            sector_size_bytes = sector_size,
            data_size_exponent = lbaf0.ds,
            "NVMe FLBAS 0 sector size",
        );
        Ok((0u8, sector_size))
    } else {
        Err(CarbideClientError::GenericError(
            "No LBA formats available for device".to_string(),
        ))
    }
}

async fn clean_this_nvme(nvmename: &String) -> Result<(), CarbideClientError> {
    tracing::debug!(device = %nvmename, "Cleaning NVMe device");

    let nvme_drive_params = get_nvme_params(nvmename).await?;

    let namespaces_supported = nvme_drive_params.oacs & 0x8 == 0x8;

    tracing::debug!(
        device = %nvmename,
        total_capacity_bytes = nvme_drive_params.tnvmcap,
        controller_id = nvme_drive_params.cntlid,
        optional_admin_commands = nvme_drive_params.oacs,
        namespaces_supported,
        serial_number = %nvme_drive_params.sn,
        model_number = %nvme_drive_params.mn,
        firmware_revision = %nvme_drive_params.fr,
        "Read NVMe device parameters",
    );

    if nvme_drive_params.mn.trim() == "M.2 NVMe 2-Bay RAID Kit" {
        let lenovo_mnv_cli_prog = resolve_lenovo_mnv_cli_prog().ok_or_else(|| {
            CarbideClientError::GenericError(format!(
                "Device {} is a Lenovo M.2 NVMe 2-Bay RAID Kit and requires mnv_cli for \
                     cleanup, but the binary was not found in any known location: {}. \
                     Ensure the Lenovo mnv_cli tool is included in the carbide extras \
                     container and staged into the scout image.",
                nvmename,
                LENOVO_NVMI_CLI_PROG_CANDIDATES.join(", ")
            ))
        })?;

        tracing::info!(program = lenovo_mnv_cli_prog, "Using Lenovo mnv_cli",);

        let vd_out = cmdrun::run_prog(lenovo_mnv_cli_prog, ["info", "-o", "vd", "-i", "0"]).await?;

        // Some of the legacy raid kits were built with raid1. We need to remove the raid1
        // and the raid kit will replace it with two raid0's next reboot.
        if vd_out.contains("RAID1") {
            cmdrun::run_prog(lenovo_mnv_cli_prog, ["vd", "-a", "delete", "-i", "0"]).await?;
        } else if vd_out.contains("RAID0") {
            // assume it is two raid 0s created by the RAID kit if we see a single raid0 output
            cmdrun::run_prog(lenovo_mnv_cli_prog, ["vd", "-a", "delete", "-i", "0"]).await?;
            cmdrun::run_prog(lenovo_mnv_cli_prog, ["vd", "-a", "delete", "-i", "1"]).await?;
        }

        // Clean the disks
        cmdrun::run_prog(
            lenovo_mnv_cli_prog,
            [
                "passthru",
                "-i",
                "0",
                "-o",
                "0x80",
                "-n",
                "0xffffffff",
                "--cdw10=0x200",
                "-r",
                "none",
            ],
        )
        .await?;
        cmdrun::run_prog(
            lenovo_mnv_cli_prog,
            [
                "passthru",
                "-i",
                "1",
                "-o",
                "0x80",
                "-n",
                "0xffffffff",
                "--cdw10=0x200",
                "-r",
                "none",
            ],
        )
        .await?;
    } else {
        // list all namespaces
        let nvmens_output = cmdrun::run_prog(NVME_CLI_PROG, ["list-ns", nvmename, "-a"]).await?;

        // iterate over namespaces
        for nsline in nvmens_output.lines() {
            let caps = match NVME_NS_RE.captures(nsline) {
                Some(o) => o,
                None => continue,
            };
            let nsid = caps.get(1).map_or("", |m| m.as_str());
            tracing::debug!(namespace_id = nsid, "Found NVMe namespace");

            // format with "-s2" is secure erase
            match cmdrun::run_prog(NVME_CLI_PROG, ["format", nvmename, "-s2", "-f", "-n", nsid])
                .await
            {
                Ok(_) => (),
                Err(e) => {
                    if namespaces_supported {
                        // format can fail if there is a wrong params for namespace. We delete it anyway.
                        tracing::debug!(error = %e, "NVMe format error");
                    } else {
                        return Err(e);
                    }
                }
            }
            if namespaces_supported {
                // delete namespace
                cmdrun::run_prog(NVME_CLI_PROG, ["delete-ns", nvmename, "-n", nsid]).await?;
            }
        }

        if namespaces_supported {
            let (flbas_index, sector_size) = get_best_lba_format(nvmename).await?;
            let sectors = nvme_drive_params.tnvmcap / sector_size;
            let flbas_str = flbas_index.to_string();

            tracing::debug!(
                device = %nvmename,
                flbas_index,
                sector_size_bytes = sector_size,
                sector_count = sectors,
                "Creating NVMe namespace",
            );

            let line_created_ns_id = cmdrun::run_prog(
                NVME_CLI_PROG,
                [
                    "create-ns",
                    nvmename,
                    &format!("--nsze={sectors}"),
                    &format!("--ncap={sectors}"),
                    "--flbas",
                    &flbas_str,
                    "--dps=0",
                ],
            )
            .await?;
            let nsid = match NVME_NSID_RE.captures(&line_created_ns_id) {
                Some(o) => o.get(1).map_or("", |m| m.as_str()),
                None => {
                    return Err(CarbideClientError::GenericError(format!(
                        "nvme cant get nsid after create-ns {line_created_ns_id}"
                    )));
                }
            };
            // attaching namespace to controller
            cmdrun::run_prog(
                NVME_CLI_PROG,
                [
                    "attach-ns",
                    nvmename,
                    "-n",
                    nsid,
                    "-c",
                    &nvme_drive_params.cntlid.to_string(),
                ],
            )
            .await?;
        }
    }
    tracing::debug!(device = %nvmename, "Cleanup completed for NVMe device");
    Ok(())
}

/// Failed NVMe device cleanup with error context
struct CleanupFailure {
    device: String,
    duration: std::time::Duration,
    error: CarbideClientError,
}

async fn all_nvme_cleanup() -> Result<(), CarbideClientError> {
    let mut nvme_devicepaths: Vec<String> = Vec::new();
    if let Ok(paths) = fs::read_dir("/dev") {
        for entry in paths {
            let path = match entry {
                Ok(o) => o.path(),
                Err(_) => continue,
            };
            if path.is_dir() {
                continue;
            }

            let nvmename = path.to_string_lossy().to_string();
            if NVME_DEV_RE.is_match(&nvmename) {
                nvme_devicepaths.push(nvmename);
            }
        }
    }

    let device_count = nvme_devicepaths.len();
    if device_count == 0 {
        tracing::info!("No NVMe devices found to clean");
        return Ok(());
    }

    tracing::info!(device_count, "Starting NVMe cleanup");
    let start_time = std::time::Instant::now();

    // Spawn async tasks for each NVMe device cleanup
    let cleanup_futures: Vec<_> = nvme_devicepaths
        .into_iter()
        .map(|nvmename| {
            let device = nvmename.clone();
            let span = tracing::info_span!("nvme_cleanup", device = %nvmename);

            tokio::spawn(
                async move {
                    let device_start = std::time::Instant::now();

                    tracing::info!("Starting cleanup");
                    let result = clean_this_nvme(&nvmename).await;
                    let duration = device_start.elapsed();

                    emit(ScoutStorageDeviceCleanup::from_result(
                        StorageDeviceType::Nvme,
                        duration,
                        &result,
                    ));
                    match result {
                        Ok(()) => Ok(()),
                        Err(error) => Err(CleanupFailure {
                            device,
                            duration,
                            error,
                        }),
                    }
                }
                .instrument(span),
            )
        })
        .collect();

    // Wait for all cleanup tasks to complete
    let results = futures_util::future::join_all(cleanup_futures).await;
    let total_duration = start_time.elapsed();

    // Collect and categorize results
    let mut errors: Vec<String> = Vec::new();
    let mut success_count = 0;

    for join_result in results {
        let cleanup_result = join_result.expect("nvme cleanup task panicked");
        match cleanup_result {
            Ok(()) => success_count += 1,
            Err(failure) => errors.push(format!(
                "NVME_CLEAN_ERROR (device: {}; duration: {:?}): {}",
                failure.device, failure.duration, failure.error,
            )),
        }
    }

    tracing::info!(
        device_count,
        success_count,
        error_count = errors.len(),
        ?total_duration,
        "NVMe cleanup completed"
    );

    if !errors.is_empty() {
        return Err(CarbideClientError::GenericError(errors.join("\n")));
    }

    Ok(())
}

fn hdparm_has_security_section(output: &str) -> bool {
    output.contains("Security:")
}

fn hdparm_is_frozen(output: &str) -> bool {
    output.lines().any(|line| {
        let trimmed = line.trim();
        trimmed == "frozen" || (trimmed.ends_with("frozen") && !trimmed.contains("not"))
    })
}

fn hdparm_supports_enhanced_erase(output: &str) -> bool {
    output.contains("supported: enhanced erase")
}

async fn try_ata_secure_erase(devpath: &str) -> Result<(), CarbideClientError> {
    let info = cmdrun::run_prog(HDPARM_CLI_PROG, ["-I", devpath]).await?;

    if !hdparm_has_security_section(&info) {
        return Err(CarbideClientError::GenericError(format!(
            "Device {} has no ATA Security section; may be SAS/SCSI",
            devpath
        )));
    }

    if hdparm_is_frozen(&info) {
        return Err(CarbideClientError::GenericError(format!(
            "Device {} ATA security is frozen; cannot erase without power cycle",
            devpath
        )));
    }

    if !hdparm_supports_enhanced_erase(&info) {
        return Err(CarbideClientError::GenericError(format!(
            "Device {} does not support ATA enhanced erase; non-SED SATA drives are not supported",
            devpath
        )));
    }

    cmdrun::run_prog(HDPARM_CLI_PROG, ["--security-set-pass", "p", devpath]).await?;

    tracing::info!(device = devpath, "Using enhanced erase");
    cmdrun::run_prog(HDPARM_CLI_PROG, ["--security-erase-enhanced", "p", devpath]).await?;

    Ok(())
}

async fn try_scsi_sanitize(devpath: &str) -> Result<(), CarbideClientError> {
    // The supported SAS fleet uses SEDs; use crypto erase intentionally rather than --block.
    cmdrun::run_prog(SG_SANITIZE_CLI_PROG, ["-Q", "-w", "-C", devpath]).await?;
    // Some drives leave LBA 0 as random bytes after a crypto erase rather than zeroing it.
    // Random data can accidentally match the AHDI (Atari HDD) partition signature, causing
    // the Linux kernel to create phantom partition entries. Writing one sector of zeros with
    // oflag=direct bypasses the SAS HBA cache and ensures LBA 0 is clean.
    let of_arg = format!("of={devpath}");
    cmdrun::run_prog(
        DD_CLI_PROG,
        [
            "if=/dev/zero",
            &of_arg,
            "bs=4096",
            "count=1",
            "oflag=direct",
        ],
    )
    .await?;
    Ok(())
}

fn sysfs_path_is_sata(path: &std::path::Path) -> bool {
    // SATA devices resolve through an ATA host in the sysfs device tree (path contains "/ata").
    // SAS/SCSI devices do not.
    path.to_string_lossy().contains("/ata")
}

fn sysfs_symlink_basename(path: &std::path::Path) -> Option<String> {
    fs::canonicalize(path).ok().and_then(|target| {
        target
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
    })
}

fn sysfs_ancestor_is_usb(ancestor: &std::path::Path) -> bool {
    if sysfs_symlink_basename(&ancestor.join("subsystem")).as_deref() == Some("usb") {
        return true;
    }

    matches!(
        sysfs_symlink_basename(&ancestor.join("driver")).as_deref(),
        Some("usb-storage" | "uas")
    )
}

fn sysfs_path_is_usb(path: &std::path::Path) -> bool {
    path.ancestors().any(sysfs_ancestor_is_usb)
}

fn is_sata_device(devname: &str) -> bool {
    fs::canonicalize(format!("/sys/block/{devname}"))
        .map(|p| sysfs_path_is_sata(&p))
        .unwrap_or(false)
}

fn is_usb_device(devname: &str) -> bool {
    fs::canonicalize(format!("/sys/block/{devname}"))
        .map(|p| sysfs_path_is_usb(&p))
        .unwrap_or(false)
}

fn read_block_sysfs_attr(devname: &str, attr: &str) -> Option<String> {
    fs::read_to_string(format!("/sys/block/{devname}/{attr}"))
        .ok()
        .map(|value| value.trim().to_string())
}

#[derive(Debug, PartialEq, Eq)]
enum BlockDeviceCleanupSkipReason {
    Hidden,
    Removable { removable: String },
    ZeroSize,
    UsbTransport,
}

impl std::fmt::Display for BlockDeviceCleanupSkipReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Hidden => write!(f, "hidden block device"),
            Self::Removable { removable } => {
                write!(f, "removable block device (removable={removable})")
            }
            Self::ZeroSize => write!(f, "zero-size block device"),
            Self::UsbTransport => write!(f, "USB transport block device"),
        }
    }
}

fn block_device_skip_reason_from_attrs(
    hidden: Option<&str>,
    removable: Option<&str>,
    size: Option<&str>,
    is_usb: impl FnOnce() -> bool,
) -> Option<BlockDeviceCleanupSkipReason> {
    if hidden == Some("1") {
        return Some(BlockDeviceCleanupSkipReason::Hidden);
    }

    if let Some(removable) = removable
        && removable != "0"
    {
        return Some(BlockDeviceCleanupSkipReason::Removable {
            removable: removable.to_string(),
        });
    }

    if size == Some("0") {
        return Some(BlockDeviceCleanupSkipReason::ZeroSize);
    }

    if is_usb() {
        return Some(BlockDeviceCleanupSkipReason::UsbTransport);
    }

    None
}

fn block_device_cleanup_skip_reason(devname: &str) -> Option<BlockDeviceCleanupSkipReason> {
    block_device_skip_reason_from_attrs(
        read_block_sysfs_attr(devname, "hidden").as_deref(),
        read_block_sysfs_attr(devname, "removable").as_deref(),
        read_block_sysfs_attr(devname, "size").as_deref(),
        || is_usb_device(devname),
    )
}

async fn clean_this_block_device(devpath: &str) -> Result<(), CarbideClientError> {
    let devname = devpath.trim_start_matches("/dev/");
    if is_sata_device(devname) {
        tracing::info!(
            device = devpath,
            "Detected SATA device; using ATA Secure Erase",
        );
        try_ata_secure_erase(devpath).await
    } else {
        tracing::info!(
            device = devpath,
            "Detected SAS/SCSI device; using SCSI Sanitize",
        );
        try_scsi_sanitize(devpath).await
    }
}

async fn all_hdd_cleanup() -> Result<(), CarbideClientError> {
    let mut block_devicepaths: Vec<String> = Vec::new();
    if let Ok(entries) = fs::read_dir("/sys/block") {
        for entry in entries {
            let entry = match entry {
                Ok(o) => o,
                Err(_) => continue,
            };
            let devname = entry.file_name().to_string_lossy().to_string();
            let devpath = format!("/dev/{devname}");
            if SD_DEV_RE.is_match(&devpath) {
                if let Some(reason) = block_device_cleanup_skip_reason(&devname) {
                    tracing::info!(
                        device = %devpath,
                        reason = %reason,
                        "Skipping HDD/SAS cleanup candidate"
                    );
                    continue;
                }

                block_devicepaths.push(devpath);
            }
        }
    }

    let device_count = block_devicepaths.len();
    if device_count == 0 {
        tracing::info!("No SATA/SAS block devices found to clean");
        return Ok(());
    }

    tracing::info!(device_count, "Starting HDD/SAS cleanup");
    let start_time = std::time::Instant::now();

    let cleanup_futures: Vec<_> = block_devicepaths
        .into_iter()
        .map(|devpath| {
            let device = devpath.clone();
            let span = tracing::info_span!("hdd_cleanup", device = %devpath);

            tokio::spawn(
                async move {
                    let device_start = std::time::Instant::now();

                    tracing::info!("Starting cleanup");
                    let result = clean_this_block_device(&devpath).await;
                    let duration = device_start.elapsed();

                    emit(ScoutStorageDeviceCleanup::from_result(
                        StorageDeviceType::HddSas,
                        duration,
                        &result,
                    ));
                    match result {
                        Ok(()) => Ok(()),
                        Err(error) => Err(CleanupFailure {
                            device,
                            duration,
                            error,
                        }),
                    }
                }
                .instrument(span),
            )
        })
        .collect();

    let results = futures_util::future::join_all(cleanup_futures).await;
    let total_duration = start_time.elapsed();

    let mut errors: Vec<String> = Vec::new();
    let mut success_count = 0;

    for join_result in results {
        let cleanup_result = join_result.expect("hdd cleanup task panicked");
        match cleanup_result {
            Ok(()) => success_count += 1,
            Err(failure) => errors.push(format!(
                "HDD_CLEAN_ERROR (device: {}; duration: {:?}): {}",
                failure.device, failure.duration, failure.error,
            )),
        }
    }

    tracing::info!(
        device_count,
        success_count,
        error_count = errors.len(),
        ?total_duration,
        "HDD/SAS cleanup completed"
    );

    if !errors.is_empty() {
        return Err(CarbideClientError::GenericError(errors.join("\n")));
    }

    Ok(())
}

// #[derive(Debug)]
// struct StructOsMemInfo {
//     mem_total: u64,
//     mem_free: u64,
//     mem_available: u64,
//     mem_buffers: u64,
//     mem_cached: u64,
// }

// fn get_os_mem_info() -> Result<StructOsMemInfo, CarbideClientError> {
//     let mut meminfo = StructOsMemInfo {
//         mem_total: 0,
//         mem_free: 0,
//         mem_available: 0,
//         mem_buffers: 0,
//         mem_cached: 0,
//     };

//     let rust_meminfo = match Meminfo::new() {
//         Err(e) => {
//             return Err(CarbideClientError::GenericError(format!(
//                 "Failed to retrieve memory information: {}",
//                 e
//             )))
//         }
//         Ok(o) => o,
//     };
//     meminfo.mem_available = match rust_meminfo.mem_available {
//         None => {
//             return Err(CarbideClientError::GenericError(
//                 "mem_available is not available".to_string(),
//             ))
//         }
//         Some(s) => s,
//     };
//     meminfo.mem_total = rust_meminfo.mem_total;
//     meminfo.mem_free = rust_meminfo.mem_free;
//     meminfo.mem_buffers = rust_meminfo.buffers;
//     meminfo.mem_cached = rust_meminfo.cached;
//     tracing::debug!("{:?}", meminfo);
//     Ok(meminfo)
// }

// fn memclr(msize: u64) -> i64 {
//     // Allocate all available memory and fill it with 1

//     let orig_brk = unsafe { libc::sbrk(0) };
//     let new_brk = unsafe { orig_brk.offset(msize as isize) };

//     if unsafe { libc::brk(new_brk) } != 0 {
//         println!("brk set to new error");
//         return -1;
//     }
//     unsafe {
//         libc::memset(orig_brk, 1, msize as usize);
//     }
//     if unsafe { libc::brk(orig_brk) } != 0 {
//         println!("brk set to orig error");
//     }

//     println!(
//         "memclr done: size={} orig_brk={:?} new_brk={:?} ",
//         msize, orig_brk, new_brk
//     );

//     0
// }

// fn cleanup_ram() -> Result<(), CarbideClientError> {
//     if let Err(e) = Resource::AS.set(libc::RLIM_INFINITY, libc::RLIM_INFINITY) {
//         return Err(CarbideClientError::GenericError(format!(
//             "Failed to set rlimit: {}",
//             e
//         )));
//     }

//     let meminfo = get_os_mem_info()?;

//     tracing::debug!(
//         "Preparing to cleanup {} bytes of RAM",
//         meminfo.mem_available
//     );
//     let mut mem_clr_res: i64;

//     mem_clr_res = memclr(meminfo.mem_available);
//     let meminfo2 = get_os_mem_info()?;

//     if mem_clr_res != 0 {
//         return Err(CarbideClientError::GenericError(format!(
//             "Mem cleanup failed with code {}",
//             mem_clr_res
//         )));
//     }

//     if meminfo.mem_free >= meminfo2.mem_free {
//         return Err(CarbideClientError::GenericError(
//             format!("Incomplete memory cleanup. Memory free before cleanup: {}. Memory free after cleanup: {}.",
//             meminfo.mem_free,
//             meminfo2.mem_free
//         )));
//     }

//     mem_clr_res = memclr(meminfo2.mem_available);
//     if mem_clr_res != 0 {
//         return Err(CarbideClientError::GenericError(format!(
//             "Mem cleanup 2 failed with code {}",
//             mem_clr_res
//         )));
//     }

//     Ok(())
// }

// Set KEEP_IB_LINK_UP on all non-DPU IB devices.
// This ensures the port link state remains up independent of host OS,
// making the port visible to UFM regardless of driver state.
// Sets P1 (required) and P2 (optional, for dual-port devices).
async fn set_ib_link_up() -> Result<(), CarbideClientError> {
    match discovery_ibs() {
        Ok(ibs) => {
            for ib in ibs {
                if let Some(p) = ib.pci_properties {
                    let slot = p.slot.unwrap();
                    // Set P1 (required - all IB devices have P1)
                    match cmdrun::run_prog(
                        "mlxconfig",
                        ["-y", "-d", &slot, "set", "KEEP_IB_LINK_UP_P1=1"],
                    )
                    .await
                    {
                        Ok(_) => {
                            tracing::info!(
                                device = %slot,
                                "Set KEEP_IB_LINK_UP_P1=1 on IB device successfully",
                            );
                        }
                        Err(e) => {
                            tracing::error!(
                                device = %slot,
                                error = %e,
                                "Failed to set KEEP_IB_LINK_UP_P1=1 on IB device",
                            );
                            return Err(e);
                        }
                    }
                    // Set P2 (optional - only dual-port devices have P2)
                    match cmdrun::run_prog(
                        "mlxconfig",
                        ["-y", "-d", &slot, "set", "KEEP_IB_LINK_UP_P2=1"],
                    )
                    .await
                    {
                        Ok(_) => {
                            tracing::info!(
                                device = %slot,
                                "Set KEEP_IB_LINK_UP_P2=1 on IB device successfully",
                            );
                        }
                        Err(e) => {
                            // P2 may not exist on single-port devices, ignore error
                            tracing::debug!(
                                device = %slot,
                                error = %e,
                                "KEEP_IB_LINK_UP_P2 not available on IB device (single-port)",
                            );
                        }
                    }
                }
            }
        }
        Err(e) => {
            tracing::error!(error = %e, "Failed to discover IB devices");
            return Err(CarbideClientError::GenericError(format!(
                "Failed to get ibs: {e}"
            )));
        }
    }

    Ok(())
}

// reuse hardware_enumeration::discovery_ibs to get all the non-DPU devices.
// in forge case, all the non-DPU device should be VPI device or IB-only device
// `reset` will set the link_type to IB for all the devices.
// It calls set_ib_link_up() to set KEEP_IB_LINK_UP for P1/P2 which is now default state.
async fn reset_ib_devices() -> Result<(), CarbideClientError> {
    match discovery_ibs() {
        Ok(ibs) => {
            for ib in ibs {
                if let Some(p) = ib.pci_properties {
                    let slot = p.slot.unwrap();
                    match cmdrun::run_prog("mlxconfig", ["-y", "-d", &slot, "reset"]).await {
                        Ok(_) => {
                            tracing::info!(device = %slot, "Reset IB device successfully");
                        }
                        Err(e) => {
                            tracing::error!(
                                device = %slot,
                                error = %e,
                                "Failed to reset IB device",
                            );
                            return Err(e);
                        }
                    }
                }
            }
        }
        Err(e) => {
            tracing::error!(error = %e, "Failed to discover IB devices");
            return Err(CarbideClientError::GenericError(format!(
                "Failed to get ibs: {e}"
            )));
        }
    }

    set_ib_link_up().await
}

async fn do_cleanup(machine_id: &MachineId) -> CarbideClientResult<rpc::MachineCleanupInfo> {
    let mut cleanup_result = rpc::MachineCleanupInfo {
        machine_id: Some(*machine_id),
        nvme: None,
        ram: None,
        mem_overwrite: None,
        ib: None,
        hdd: None,
        result: rpc::machine_cleanup_info::CleanupResult::Ok as _,
    };

    // do nvme/hdd cleanup only if stdin is /dev/null. This is because we afraid to cleanup someone's drives.
    let stdin_link = match fs::read_link("/proc/self/fd/0") {
        Ok(o) => o.to_string_lossy().to_string(),
        Err(_) => "None".to_string(),
    };

    if stdin_link == "/dev/null" {
        let (nvme_result, hdd_result) = tokio::join!(all_nvme_cleanup(), all_hdd_cleanup());

        match nvme_result {
            Ok(_) => {
                cleanup_result.nvme = Some(rpc::machine_cleanup_info::CleanupStepResult {
                    result: rpc::machine_cleanup_info::CleanupResult::Ok as _,
                    message: "OK".to_string(),
                });
            }
            Err(e) => {
                tracing::error!(error = %e, "NVMe cleanup failed");
                cleanup_result.nvme = Some(rpc::machine_cleanup_info::CleanupStepResult {
                    result: rpc::machine_cleanup_info::CleanupResult::Error as _,
                    message: e.to_string(),
                });
                cleanup_result.result = rpc::machine_cleanup_info::CleanupResult::Error as _;
            }
        }

        match hdd_result {
            Ok(_) => {
                cleanup_result.hdd = Some(rpc::machine_cleanup_info::CleanupStepResult {
                    result: rpc::machine_cleanup_info::CleanupResult::Ok as _,
                    message: "OK".to_string(),
                });
            }
            Err(e) => {
                tracing::error!(error = %e, "HDD/SAS cleanup failed");
                cleanup_result.hdd = Some(rpc::machine_cleanup_info::CleanupStepResult {
                    result: rpc::machine_cleanup_info::CleanupResult::Error as _,
                    message: e.to_string(),
                });
                cleanup_result.result = rpc::machine_cleanup_info::CleanupResult::Error as _;
            }
        }
    } else {
        tracing::info!(
            stdin = %stdin_link,
            "Skipping NVMe and HDD cleanup because stdin is not /dev/null",
        );
    }

    match check_memory_overwrite_efi_var() {
        Ok(_) => {
            cleanup_result.mem_overwrite = Some(rpc::machine_cleanup_info::CleanupStepResult {
                result: rpc::machine_cleanup_info::CleanupResult::Ok as _,
                message: "OK".to_string(),
            });
        }
        Err(e) => {
            tracing::error!(error = %e, "Memory overwrite check failed");
            cleanup_result.mem_overwrite = Some(rpc::machine_cleanup_info::CleanupStepResult {
                result: rpc::machine_cleanup_info::CleanupResult::Error as _,
                message: e.to_string(),
            });
            if !IN_QEMU_VM.read().await.in_qemu {
                cleanup_result.result = rpc::machine_cleanup_info::CleanupResult::Error as _;
            }
        }
    }

    // Memory cleanup is disabled until we can guarantee the system won't run out of memory while performing it
    // match cleanup_ram() {
    //     Ok(_) => {
    //         cleanup_result.ram = Some(rpc::machine_cleanup_info::CleanupStepResult {
    //             result: rpc::machine_cleanup_info::CleanupResult::Ok as _,
    //             message: "OK".to_string(),
    //         });
    //     }
    //     Err(e) => {
    //         tracing::error!("{}", e);
    //         cleanup_result.ram = Some(rpc::machine_cleanup_info::CleanupStepResult {
    //             result: rpc::machine_cleanup_info::CleanupResult::Error as _,
    //             message: e.to_string(),
    //         });
    //         cleanup_result.result = rpc::machine_cleanup_info::CleanupResult::Error as _;
    //     }
    // }

    match reset_ib_devices().await {
        Ok(_) => {
            cleanup_result.ib = Some(rpc::machine_cleanup_info::CleanupStepResult {
                result: rpc::machine_cleanup_info::CleanupResult::Ok as _,
                message: "OK".to_string(),
            });
        }
        Err(e) => {
            tracing::error!(error = %e, "IB device reset failed");
            cleanup_result.ib = Some(rpc::machine_cleanup_info::CleanupStepResult {
                result: rpc::machine_cleanup_info::CleanupResult::Error as _,
                message: e.to_string(),
            });
            cleanup_result.result = rpc::machine_cleanup_info::CleanupResult::Error as _;
        }
    }

    Ok(cleanup_result)
}

pub(crate) async fn run(config: &Options, machine_id: &MachineId) -> CarbideClientResult<()> {
    tracing::info!("full deprovision starts.");
    if !platform::is_host() {
        tracing::info!("full deprovision skipped, we are not running on a host.");
        // do not send API cleanup_machine_completed
        return Ok(());
    }
    tracing::info!("Machine cleanup starting, we are running on a host.");
    let info = do_cleanup(machine_id).await?;
    let mut client = create_forge_client(config).await?;
    let request = tonic::Request::new(info);
    client.cleanup_machine_completed(request).await?;
    Ok(())
}

pub(crate) async fn run_no_api(tpm_path: &str) -> Result<(), CarbideClientError> {
    if !platform::is_host() {
        tracing::info!("No cleanup needed on DPU.");
        return Ok(());
    }
    tracing::info!("no_api deprovision starts.");

    crate::tpm::clear_tpm(tpm_path)?;

    tracing::info!("Skipping NVMe and HDD/SAS cleanup in no-api path; RESET owns storage cleanup.");

    // P1 errors are propagated (fail startup), P2 errors are handled internally in reset_ib_devices()
    reset_ib_devices().await?;
    tracing::debug!("IB devices reset OK");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_namespace_params_512b_first() {
        let json = r#"{
            "lbafs": [
                {"ms": 0, "ds": 9, "rp": 0},
                {"ms": 8, "ds": 9, "rp": 1},
                {"ms": 0, "ds": 12, "rp": 0}
            ]
        }"#;

        let params: NvmeNamespaceParams = serde_json::from_str(json).unwrap();
        assert_eq!(params.lbafs.len(), 3);
        assert_eq!(params.lbafs[0].ds, 9);
        assert_eq!(params.lbafs[0].ms, 0);
        assert_eq!(params.lbafs[2].ds, 12);
    }

    #[test]
    fn test_parse_namespace_params_4096b_first() {
        let json = r#"{
            "lbafs": [
                {"ms": 0, "ds": 12, "rp": 0},
                {"ms": 8, "ds": 12, "rp": 1},
                {"ms": 0, "ds": 9, "rp": 0}
            ]
        }"#;

        let params: NvmeNamespaceParams = serde_json::from_str(json).unwrap();
        assert_eq!(params.lbafs.len(), 3);
        assert_eq!(params.lbafs[0].ds, 12);
        assert_eq!(params.lbafs[2].ds, 9);
    }

    #[test]
    fn test_select_best_512b_format() {
        // Test case: Multiple 512B formats, prefer ms=0 and rp=0
        let lbafs = vec![
            NvmeLbaFormat {
                ms: 8,
                ds: 9,
                rp: 1,
            },
            NvmeLbaFormat {
                ms: 0,
                ds: 9,
                rp: 0,
            },
            NvmeLbaFormat {
                ms: 0,
                ds: 12,
                rp: 0,
            },
        ];

        let best_format = select_best_lba_format(&lbafs, 9);
        assert!(best_format.is_some());
        let (idx, lbaf) = best_format.unwrap();
        assert_eq!(idx, 1); // Should select format at index 1 (ms=0, rp=0)
        assert_eq!(lbaf.ms, 0);
        assert_eq!(lbaf.rp, 0);
    }

    #[test]
    fn test_select_best_4k_format() {
        // Test case: Multiple 4K formats, prefer ms=0 and best rp
        let lbafs = vec![
            NvmeLbaFormat {
                ms: 8,
                ds: 12,
                rp: 2,
            },
            NvmeLbaFormat {
                ms: 0,
                ds: 12,
                rp: 1,
            },
            NvmeLbaFormat {
                ms: 0,
                ds: 12,
                rp: 0,
            },
        ];

        let best_format = select_best_lba_format(&lbafs, 12);
        assert!(best_format.is_some());
        let (idx, lbaf) = best_format.unwrap();
        assert_eq!(idx, 2); // Should select format at index 2 (ms=0, rp=0)
        assert_eq!(lbaf.ms, 0);
        assert_eq!(lbaf.rp, 0);
    }

    #[test]
    fn test_select_format_prefer_no_metadata() {
        // Test case: Prefer no metadata even with slightly worse performance
        let lbafs = vec![
            NvmeLbaFormat {
                ms: 8,
                ds: 9,
                rp: 0,
            },
            NvmeLbaFormat {
                ms: 0,
                ds: 9,
                rp: 1,
            },
        ];

        let best_format = select_best_lba_format(&lbafs, 9);
        assert!(best_format.is_some());
        let (idx, lbaf) = best_format.unwrap();
        assert_eq!(idx, 1); // Should prefer ms=0 even though rp is worse
        assert_eq!(lbaf.ms, 0);
    }

    #[test]
    fn test_select_format_not_found() {
        // Test case: No matching format
        let lbafs = vec![NvmeLbaFormat {
            ms: 0,
            ds: 12,
            rp: 0,
        }];

        let best_format = select_best_lba_format(&lbafs, 9);
        assert!(best_format.is_none());
    }

    #[test]
    fn test_sector_count_calculations() {
        // Test with 512B sectors
        let tnvmcap = 1_000_000_000_000u64; // 1TB
        let sector_size_512 = 512u64;
        let sectors_512 = tnvmcap / sector_size_512;
        assert_eq!(sectors_512, 1_953_125_000);

        // Test with 4096B sectors
        let sector_size_4096 = 4096u64;
        let sectors_4096 = tnvmcap / sector_size_4096;
        assert_eq!(sectors_4096, 244_140_625);

        // Verify the ratio is 8:1
        assert_eq!(sectors_512 / sectors_4096, 8);
    }

    #[test]
    fn test_lbaf_sector_size_calculation() {
        // ds=9 means 2^9 = 512 bytes
        let ds_512 = 9u8;
        let sector_size_512 = 1u64 << ds_512;
        assert_eq!(sector_size_512, 512);

        // ds=12 means 2^12 = 4096 bytes
        let ds_4096 = 12u8;
        let sector_size_4096 = 1u64 << ds_4096;
        assert_eq!(sector_size_4096, 4096);
    }

    #[test]
    fn test_hdparm_has_security_section() {
        let with_security = "ATA device, with non-removable media\nSecurity:\n\tMaster password revision code = 65534\n\tsupported\n\tnot\tlocked\n\tnot\tfrozen\n";
        let without_security =
            "ATA device, with non-removable media\nCapabilities:\n\tLBA, IORDY\n";

        assert!(hdparm_has_security_section(with_security));
        assert!(!hdparm_has_security_section(without_security));
    }

    #[test]
    fn test_hdparm_is_frozen_not_frozen() {
        let not_frozen = "Security:\n\tsupported\n\tnot\tfrozen\n";
        assert!(!hdparm_is_frozen(not_frozen));
    }

    #[test]
    fn test_hdparm_is_frozen_frozen() {
        let frozen = "Security:\n\tsupported\n\tfrozen\n";
        assert!(hdparm_is_frozen(frozen));
    }

    #[test]
    fn test_hdparm_supports_enhanced_erase() {
        let with_enhanced = "Security:\n\tsupported\n\tnot\tfrozen\n\tsupported: enhanced erase\n";
        let without_enhanced = "Security:\n\tsupported\n\tnot\tfrozen\n";

        assert!(hdparm_supports_enhanced_erase(with_enhanced));
        assert!(!hdparm_supports_enhanced_erase(without_enhanced));
    }

    #[test]
    fn test_sysfs_path_is_sata() {
        use std::path::Path;
        // SATA: path contains /ata host component
        assert!(sysfs_path_is_sata(Path::new(
            "/sys/devices/pci0000:00/0000:00:17.0/ata1/host0/target0:0:0/0:0:0:0/block/sda"
        )));
        // SAS: path goes through SAS host, no /ata component
        assert!(!sysfs_path_is_sata(Path::new(
            "/sys/devices/pci0000:00/0000:00:01.0/0000:01:00.0/host0/port-0:0/end_device-0:0/target0:0:0/0:0:0:0/block/sda"
        )));
    }

    #[test]
    fn test_sysfs_path_is_usb_by_subsystem_ancestor() {
        let tempdir = tempfile::tempdir().unwrap();
        let usb_bus = tempdir.path().join("sys/bus/usb");
        let usb_interface = tempdir
            .path()
            .join("sys/devices/pci0000:00/0000:00:14.0/usb1/1-5/1-5:1.0");
        let block_device = usb_interface.join("host0/target0:0:0/0:0:0:0/block/sda");

        std::fs::create_dir_all(&usb_bus).unwrap();
        std::fs::create_dir_all(&block_device).unwrap();
        std::os::unix::fs::symlink(&usb_bus, usb_interface.join("subsystem")).unwrap();

        assert!(sysfs_path_is_usb(&block_device));
    }

    #[test]
    fn test_sysfs_path_is_usb_by_driver_ancestor() {
        let tempdir = tempfile::tempdir().unwrap();
        let uas_driver = tempdir.path().join("sys/bus/usb/drivers/uas");
        let usb_interface = tempdir
            .path()
            .join("sys/devices/pci0000:00/0000:00:14.0/usb1/1-5/1-5:1.0");
        let block_device = usb_interface.join("host0/target0:0:0/0:0:0:0/block/sda");

        std::fs::create_dir_all(&uas_driver).unwrap();
        std::fs::create_dir_all(&block_device).unwrap();
        std::os::unix::fs::symlink(&uas_driver, usb_interface.join("driver")).unwrap();

        assert!(sysfs_path_is_usb(&block_device));
    }

    #[test]
    fn test_sysfs_path_is_usb_false_without_usb_ancestor_metadata() {
        let tempdir = tempfile::tempdir().unwrap();
        let block_device = tempdir
            .path()
            .join("sys/devices/pci0000:00/0000:00:01.0/host0/target0:0:0/0:0:0:0/block/sda");

        std::fs::create_dir_all(&block_device).unwrap();

        assert!(!sysfs_path_is_usb(&block_device));
    }

    #[test]
    fn test_sata_dev_re_matches_whole_disk() {
        assert!(SD_DEV_RE.is_match("/dev/sda"));
        assert!(SD_DEV_RE.is_match("/dev/sdb"));
        assert!(SD_DEV_RE.is_match("/dev/sdz"));
    }

    #[test]
    fn test_sata_dev_re_does_not_match_partitions() {
        assert!(!SD_DEV_RE.is_match("/dev/sda1"));
        assert!(!SD_DEV_RE.is_match("/dev/sdb2"));
    }

    #[test]
    fn test_sata_dev_re_does_not_match_nvme() {
        assert!(!SD_DEV_RE.is_match("/dev/nvme0"));
        assert!(!SD_DEV_RE.is_match("/dev/nvme0n1"));
    }

    #[test]
    fn test_block_device_skip_reason_from_attrs() {
        struct Case {
            name: &'static str,
            hidden: Option<&'static str>,
            removable: Option<&'static str>,
            size: Option<&'static str>,
            is_usb: bool,
            expected: Option<BlockDeviceCleanupSkipReason>,
        }

        let cases = [
            Case {
                name: "hidden device is skipped",
                hidden: Some("1"),
                removable: Some("0"),
                size: Some("1000"),
                is_usb: false,
                expected: Some(BlockDeviceCleanupSkipReason::Hidden),
            },
            Case {
                name: "removable device is skipped",
                hidden: Some("0"),
                removable: Some("1"),
                size: Some("1000"),
                is_usb: false,
                expected: Some(BlockDeviceCleanupSkipReason::Removable {
                    removable: "1".to_string(),
                }),
            },
            Case {
                name: "zero-size device is skipped",
                hidden: Some("0"),
                removable: Some("0"),
                size: Some("0"),
                is_usb: false,
                expected: Some(BlockDeviceCleanupSkipReason::ZeroSize),
            },
            Case {
                name: "normal non-zero size device is cleaned",
                hidden: Some("0"),
                removable: Some("0"),
                size: Some("1000"),
                is_usb: false,
                expected: None,
            },
            Case {
                name: "usb device is skipped as fallback",
                hidden: Some("0"),
                removable: Some("0"),
                size: Some("1000"),
                is_usb: true,
                expected: Some(BlockDeviceCleanupSkipReason::UsbTransport),
            },
        ];

        for case in cases {
            let actual =
                block_device_skip_reason_from_attrs(case.hidden, case.removable, case.size, || {
                    case.is_usb
                });
            assert_eq!(actual, case.expected, "case: {}", case.name);
        }
    }
}

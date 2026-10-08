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

//! Build helper for BlueField bootstream (BFB) artifacts.
//!
//! This binary wraps NVIDIA's `mlx-mkbfb` tool for the custom kernel/initramfs
//! flow used by the PXE cargo-make tasks. It intentionally stays small: the
//! repository's production BFB pipeline remains in `pxe/Makefile.toml`, while
//! this helper owns input validation, command construction, and post-build
//! artifact validation for the custom payload path.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use clap::{Parser, Subcommand};

/// Command-line entry point for BlueField BFB helper operations.
#[derive(Parser, Debug)]
#[command(name = "carbide-bfb")]
struct Cli {
    /// Operation to run.
    #[command(subcommand)]
    command: Commands,
}

/// Supported BFB helper commands.
#[derive(Subcommand, Debug)]
enum Commands {
    /// Build a BFB from caller-supplied kernel, initramfs, and boot arguments.
    #[command(about = "Create a BlueField BFB from a kernel, initramfs, and boot arguments")]
    CreateCustomKernelInitramfs(CreateCustomKernelInitramfs),
}

/// Arguments for creating a BFB from custom Linux boot artifacts.
#[derive(Parser, Debug)]
struct CreateCustomKernelInitramfs {
    /// Compatible carrier BFB used as the base artifact.
    #[arg(long)]
    base_bfb: PathBuf,

    /// Kernel image to embed in the generated BFB.
    #[arg(long)]
    image: PathBuf,

    /// Initramfs image to embed in the generated BFB.
    #[arg(long)]
    initramfs: PathBuf,

    /// Kernel command line for the generated boot entry.
    #[arg(long)]
    boot_args: String,

    /// Human-readable boot entry description.
    #[arg(long, default_value = "Custom BlueField boot image")]
    description: String,

    /// Destination path for the generated BFB.
    #[arg(long)]
    output: PathBuf,

    /// Optional BFB boot-path image value.
    #[arg(long)]
    boot_path: Option<String>,

    /// Path to the NVIDIA `mlx-mkbfb` helper.
    #[arg(long, default_value = "/tmp/bfb-dump/mlx-mkbfb")]
    mlx_mkbfb: PathBuf,

    /// Optional URL to download the base BFB from when it is missing locally.
    #[arg(long)]
    download_url: Option<String>,
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    match Cli::parse().command {
        Commands::CreateCustomKernelInitramfs(args) => create_custom_kernel_initramfs(args),
    }
}

/// Creates a custom BFB and validates it with `mlx-mkbfb -c`.
fn create_custom_kernel_initramfs(args: CreateCustomKernelInitramfs) -> Result<(), String> {
    ensure_base_bfb(&args.base_bfb, args.download_url.as_deref())?;
    require_file(&args.base_bfb, "base BFB")?;
    require_file(&args.image, "kernel image")?;
    require_file(&args.initramfs, "initramfs")?;
    let boot_versions = inspect_carrier_boot_versions(&args.mlx_mkbfb, &args.base_bfb)?;

    if let Some(parent) = args.output.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
    }

    run_mlx_mkbfb(&args, &boot_versions)?;
    run_mlx_mkbfb_check(&args.mlx_mkbfb, &args.output)?;
    println!("Wrote {}", args.output.display());
    Ok(())
}

/// Ensures the base BFB exists, downloading it only when a URL was supplied.
fn ensure_base_bfb(base_bfb: &Path, download_url: Option<&str>) -> Result<(), String> {
    if base_bfb.is_file() {
        return Ok(());
    }

    let Some(download_url) = download_url else {
        return Ok(());
    };
    let Some(parent) = base_bfb.parent() else {
        return Err(format!(
            "base BFB has no parent directory: {}",
            base_bfb.display()
        ));
    };

    std::fs::create_dir_all(parent)
        .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
    let status = Command::new("wget")
        .arg("-Nnv")
        .arg(download_url)
        .arg("-P")
        .arg(parent)
        .status()
        .map_err(|error| format!("failed to run wget: {error}"))?;
    if !status.success() {
        return Err(format!("wget failed with status {status}"));
    }

    Ok(())
}

/// Returns an error when `path` is not a regular file.
fn require_file(path: &Path, label: &str) -> Result<(), String> {
    if path.is_file() {
        Ok(())
    } else {
        Err(format!("{label} does not exist: {}", path.display()))
    }
}

/// Runs `mlx-mkbfb` with arguments built from the custom payload request.
fn run_mlx_mkbfb(
    args: &CreateCustomKernelInitramfs,
    boot_versions: &BTreeSet<u8>,
) -> Result<(), String> {
    let mut command = Command::new(&args.mlx_mkbfb);
    for arg in mlx_mkbfb_args(args, boot_versions) {
        command.arg(arg);
    }
    let status = command
        .status()
        .map_err(|error| format!("failed to run {}: {error}", args.mlx_mkbfb.display()))?;
    if !status.success() {
        return Err(format!(
            "{} failed with status {status}",
            args.mlx_mkbfb.display()
        ));
    }

    Ok(())
}

/// Builds the `mlx-mkbfb` argv vector for custom kernel/initramfs embedding.
fn mlx_mkbfb_args(args: &CreateCustomKernelInitramfs, boot_versions: &BTreeSet<u8>) -> Vec<String> {
    let mut command_args = vec![args.base_bfb.display().to_string()];
    for version in boot_versions {
        command_args.extend(mlx_mkbfb_payload_args_for_version(args, *version));
    }
    command_args.push(args.output.display().to_string());
    command_args
}

/// Builds version-specific payload replacement options for one carrier boot entry.
fn mlx_mkbfb_payload_args_for_version(
    args: &CreateCustomKernelInitramfs,
    version: u8,
) -> Vec<String> {
    let version_suffix = match version {
        0 => String::new(),
        _ => format!("-v{version}"),
    };
    let mut command_args = vec![
        format!("--image{version_suffix}={}", args.image.display()),
        format!("--initramfs{version_suffix}={}", args.initramfs.display()),
        // mlx-mkbfb expects the extra `=` so values that contain `=` remain
        // part of the option value rather than being split by its parser.
        format!("--boot-args{version_suffix}=={}", args.boot_args),
        format!("--boot-desc{version_suffix}=={}", args.description),
    ];
    if let Some(boot_path) = &args.boot_path {
        command_args.push(format!("--boot-path{version_suffix}=={boot_path}"));
    }
    command_args
}

/// Extracts carrier metadata and returns every boot-entry version that must be replaced.
fn inspect_carrier_boot_versions(
    mlx_mkbfb: &Path,
    base_bfb: &Path,
) -> Result<BTreeSet<u8>, String> {
    let dump_dir = std::env::temp_dir().join(format!(
        "carbide-bfb-dump-{}-{}",
        std::process::id(),
        monotonic_millis()
    ));
    std::fs::create_dir(&dump_dir)
        .map_err(|error| format!("failed to create {}: {error}", dump_dir.display()))?;

    let result = inspect_carrier_boot_versions_in_dir(mlx_mkbfb, base_bfb, &dump_dir);
    let cleanup_result = std::fs::remove_dir_all(&dump_dir);
    match (result, cleanup_result) {
        (Ok(versions), Ok(())) => Ok(versions),
        (Ok(versions), Err(error)) => {
            eprintln!("warning: failed to remove {}: {error}", dump_dir.display());
            Ok(versions)
        }
        (Err(error), _) => Err(error),
    }
}

/// Runs `mlx-mkbfb -x` in a caller-provided directory and parses dump filenames.
fn inspect_carrier_boot_versions_in_dir(
    mlx_mkbfb: &Path,
    base_bfb: &Path,
    dump_dir: &Path,
) -> Result<BTreeSet<u8>, String> {
    let status = Command::new(mlx_mkbfb)
        .arg("-x")
        .arg(base_bfb)
        .current_dir(dump_dir)
        .status()
        .map_err(|error| format!("failed to run {} -x: {error}", mlx_mkbfb.display()))?;
    if !status.success() {
        return Err(format!(
            "{} -x failed for {} with status {status}",
            mlx_mkbfb.display(),
            base_bfb.display()
        ));
    }

    let mut versions = BTreeSet::new();
    for entry in std::fs::read_dir(dump_dir)
        .map_err(|error| format!("failed to read {}: {error}", dump_dir.display()))?
    {
        let entry = entry.map_err(|error| format!("failed to read dump entry: {error}"))?;
        let Some(file_name) = entry.file_name().to_str().map(ToOwned::to_owned) else {
            continue;
        };
        if let Some(version) = boot_entry_version_from_dump_name(&file_name)? {
            versions.insert(version);
        }
    }

    validate_boot_versions(&versions)?;
    Ok(versions)
}

/// Parses `mlx-mkbfb -x` files such as `dump-image-v0` and `dump-boot-args-v2`.
fn boot_entry_version_from_dump_name(file_name: &str) -> Result<Option<u8>, String> {
    if matches!(
        file_name,
        "dump-image" | "dump-initramfs" | "dump-boot-args" | "dump-boot-desc" | "dump-boot-path"
    ) {
        return Ok(Some(0));
    }

    for prefix in [
        "dump-image-v",
        "dump-initramfs-v",
        "dump-boot-args-v",
        "dump-boot-desc-v",
        "dump-boot-path-v",
    ] {
        if let Some(version) = file_name.strip_prefix(prefix) {
            return version.parse::<u8>().map(Some).map_err(|error| {
                format!("invalid BFB boot-entry version in {file_name}: {error}")
            });
        }
    }
    Ok(None)
}

/// Allows only boot-entry versions that this helper replaces explicitly.
fn validate_boot_versions(versions: &BTreeSet<u8>) -> Result<(), String> {
    if versions.is_empty() {
        return Err("carrier BFB did not expose boot-entry versions in mlx-mkbfb dump".to_string());
    }
    let unsupported: Vec<String> = versions
        .iter()
        .filter(|version| !matches!(version, 0 | 2))
        .map(ToString::to_string)
        .collect();
    if !unsupported.is_empty() {
        return Err(format!(
            "carrier BFB uses unsupported boot-entry version(s): {}",
            unsupported.join(", ")
        ));
    }
    Ok(())
}

/// Returns a best-effort monotonic timestamp for unique temporary dump directories.
fn monotonic_millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}

/// Verifies the generated BFB with `mlx-mkbfb -c`.
fn run_mlx_mkbfb_check(mlx_mkbfb: &Path, output: &Path) -> Result<(), String> {
    let status = Command::new(mlx_mkbfb)
        .arg("-c")
        .arg(output)
        .status()
        .map_err(|error| format!("failed to run {} -c: {error}", mlx_mkbfb.display()))?;

    if !status.success() {
        return Err(format!(
            "{} -c failed for {} with status {status}",
            mlx_mkbfb.display(),
            output.display()
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_mlx_mkbfb_arguments() {
        let args = CreateCustomKernelInitramfs {
            base_bfb: "/carrier.bfb".into(),
            image: "/Image".into(),
            initramfs: "/initramfs".into(),
            boot_args: "console=hvc0".to_string(),
            description: "Recovery".to_string(),
            output: "/out.bfb".into(),
            boot_path: Some("Image".to_string()),
            mlx_mkbfb: "/mlx-mkbfb".into(),
            download_url: None,
        };
        let boot_versions = BTreeSet::from([0]);

        assert_eq!(
            mlx_mkbfb_args(&args, &boot_versions),
            vec![
                "/carrier.bfb",
                "--image=/Image",
                "--initramfs=/initramfs",
                "--boot-args==console=hvc0",
                "--boot-desc==Recovery",
                "--boot-path==Image",
                "/out.bfb",
            ]
        );
    }

    #[test]
    fn builds_mlx_mkbfb_arguments_for_all_carrier_boot_versions() {
        let args = CreateCustomKernelInitramfs {
            base_bfb: "/carrier.bfb".into(),
            image: "/Image".into(),
            initramfs: "/initramfs".into(),
            boot_args: "console=hvc0 root=LABEL=nixos".to_string(),
            description: "Recovery".to_string(),
            output: "/out.bfb".into(),
            boot_path: None,
            mlx_mkbfb: "/mlx-mkbfb".into(),
            download_url: None,
        };
        let boot_versions = BTreeSet::from([0, 2]);

        assert_eq!(
            mlx_mkbfb_args(&args, &boot_versions),
            vec![
                "/carrier.bfb",
                "--image=/Image",
                "--initramfs=/initramfs",
                "--boot-args==console=hvc0 root=LABEL=nixos",
                "--boot-desc==Recovery",
                "--image-v2=/Image",
                "--initramfs-v2=/initramfs",
                "--boot-args-v2==console=hvc0 root=LABEL=nixos",
                "--boot-desc-v2==Recovery",
                "/out.bfb",
            ]
        );
    }

    #[test]
    fn parses_dump_boot_entry_versions() {
        assert_eq!(
            boot_entry_version_from_dump_name("dump-image").unwrap(),
            Some(0)
        );
        assert_eq!(
            boot_entry_version_from_dump_name("dump-boot-args-v2").unwrap(),
            Some(2)
        );
        assert_eq!(
            boot_entry_version_from_dump_name("other-file").unwrap(),
            None
        );
    }

    #[test]
    fn rejects_unsupported_boot_entry_versions() {
        let versions = BTreeSet::from([0, 3]);
        assert!(validate_boot_versions(&versions).is_err());
    }

    #[test]
    fn validates_required_files() {
        let missing = Path::new("/definitely/missing");
        assert!(require_file(missing, "kernel image").is_err());
    }
}

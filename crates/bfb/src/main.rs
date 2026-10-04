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

    if let Some(parent) = args.output.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
    }

    run_mlx_mkbfb(&args)?;
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
fn run_mlx_mkbfb(args: &CreateCustomKernelInitramfs) -> Result<(), String> {
    let mut command = Command::new(&args.mlx_mkbfb);
    for arg in mlx_mkbfb_args(args) {
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
fn mlx_mkbfb_args(args: &CreateCustomKernelInitramfs) -> Vec<String> {
    let mut command_args = vec![
        args.base_bfb.display().to_string(),
        format!("--image={}", args.image.display()),
        format!("--initramfs={}", args.initramfs.display()),
        // mlx-mkbfb expects the extra `=` so values that contain `=` remain
        // part of the option value rather than being split by its parser.
        format!("--boot-args=={}", args.boot_args),
        format!("--boot-desc=={}", args.description),
    ];
    if let Some(boot_path) = &args.boot_path {
        command_args.push(format!("--boot-path=={boot_path}"));
    }
    command_args.push(args.output.display().to_string());
    command_args
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

        assert_eq!(
            mlx_mkbfb_args(&args),
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
    fn validates_required_files() {
        let missing = Path::new("/definitely/missing");
        assert!(require_file(missing, "kernel image").is_err());
    }
}

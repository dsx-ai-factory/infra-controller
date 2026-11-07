// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out_dir = PathBuf::from(std::env::var("OUT_DIR")?);

    // Compile NSM and PSM protos into separate subdirectories so their
    // identical `package v1;` declarations don't collide.
    let nsm_out = out_dir.join("nsm");
    std::fs::create_dir_all(&nsm_out)?;
    tonic_prost_build::configure()
        .out_dir(&nsm_out)
        .compile_protos(&["proto/nvswitch-manager.proto"], &["proto/"])?;

    let psm_out = out_dir.join("psm");
    std::fs::create_dir_all(&psm_out)?;
    tonic_prost_build::configure()
        .out_dir(&psm_out)
        .compile_protos(&["proto/powershelf-manager.proto"], &["proto/"])?;

    Ok(())
}

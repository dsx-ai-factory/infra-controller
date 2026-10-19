//
// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0
//

/// Generate only the provider service, without adding it to Forge reflection.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let manifest_dir = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR")?);
    let proto_dir = manifest_dir.join("../rpc/proto");
    let proto_file = proto_dir.join("resource_pool/v1/resource_pool.proto");

    println!("cargo:rerun-if-changed={}", proto_file.display());
    tonic_prost_build::configure()
        .protoc_arg("--experimental_allow_proto3_optional")
        .compile_protos(&[proto_file], &[proto_dir])?;

    Ok(())
}

//
// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0
//
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let manifest_dir = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR")?);
    let proto_dir = manifest_dir.join("../rpc/proto");
    let proto_file = proto_dir.join("nmx_c.proto");

    tonic_prost_build::configure()
        .build_server(std::env::var_os("CARGO_FEATURE_SERVER").is_some())
        .build_client(true)
        .protoc_arg("--experimental_allow_proto3_optional")
        .type_attribute(".nmx_c", "#[derive(serde::Deserialize, serde::Serialize)]")
        .compile_protos(&[proto_file], &[proto_dir])?;

    Ok(())
}

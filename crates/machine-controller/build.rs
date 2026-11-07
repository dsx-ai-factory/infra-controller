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
use std::path::PathBuf;

use carbide_proto_compiler::{CompilerConfig, TonicBuilderCodegenExt, compile};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=../rpc/proto");

    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    let schema = compile(&CompilerConfig {
        proto_files: vec![
            PathBuf::from("../rpc/proto/codegen/v1/rust_type.proto"),
            PathBuf::from("../rpc/proto/scout_firmware_upgrade.proto"),
        ],
        include_paths: vec![PathBuf::from("../rpc/proto")],
        protoc_args: Vec::new(),
    })?;
    let codegen = schema.collect_codegen()?;
    let rust_descriptor_set = codegen.rust_file_descriptor_set(&schema.file_descriptor_set)?;

    tonic_prost_build::configure()
        .out_dir(out_dir)
        .apply_codegen(&codegen)
        .compile_fds(rust_descriptor_set)?;

    Ok(())
}

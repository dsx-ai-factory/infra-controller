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
use std::{fs, io};

use carbide_proto_compiler::{CompilerConfig, compile};

// RPC methods mocked by ssh-console-mock-api-server. We don't implement all of forge.proto because
// we would need an unreasonably large number of stub functions.
static KEEP_RPCS: &[&str] = &[
    "Version",
    "ValidateTenantPublicKey",
    "FindInstancesByIds",
    "FindMachineIds",
    "GetBMCMetaData",
];

static RPC_CRATE_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../rpc");
static RPC_PROTO_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../rpc/proto");

fn main() -> Result<(), Box<dyn std::error::Error>> {
    carbide_version::build();

    // Copy protos from the rpc crate first
    copy_protos_from_rpc_crate()?;
    println!("cargo:rerun-if-changed=../rpc/proto");

    let schema = compile(&CompilerConfig {
        proto_files: vec![
            PathBuf::from("codegen/v1/machine_id_types.proto"),
            PathBuf::from("proto/common.proto"),
            PathBuf::from("proto/scout_firmware_upgrade.proto"),
            PathBuf::from("proto/dns.proto"),
            PathBuf::from("proto/forge.proto"),
            PathBuf::from("proto/machine_discovery.proto"),
            PathBuf::from("proto/site_explorer.proto"),
        ],
        include_paths: vec![PathBuf::from("proto"), PathBuf::from(RPC_PROTO_DIR)],
        protoc_args: Vec::new(),
    })?;
    let codegen = schema.collect_codegen()?;
    let rust_descriptor_set = codegen.rust_file_descriptor_set(&schema.file_descriptor_set)?;

    // Then codegen them.
    tonic_prost_build::configure()
        .build_server(true)
        .build_client(false) // we're using ForgeApiClient from rpc crate
        .extern_path(".common.MachineId", "::carbide_uuid::machine::MachineId")
        .extern_path(
            ".common.DpuMachineId",
            "::carbide_uuid::machine::DpuMachineId",
        )
        .extern_path(
            ".common.HostMachineId",
            "::carbide_uuid::machine::HostMachineId",
        )
        .extern_path(
            ".common.StableHostMachineId",
            "::carbide_uuid::machine::StableHostMachineId",
        )
        .extern_path(
            ".common.PredictedHostMachineId",
            "::carbide_uuid::machine::PredictedHostMachineId",
        )
        .extern_path(".common.RackId", "::carbide_uuid::rack::RackId")
        .extern_path(
            ".common.RackProfileId",
            "::carbide_uuid::rack::RackProfileId",
        )
        .out_dir("src/generated")
        .compile_fds(rust_descriptor_set)?;

    Ok(())
}

/// Take protos from the rpc crate, but omit all RPC methods except the ones we're mocking (so that
/// we don't have to stub out hundreds of methods.)
fn copy_protos_from_rpc_crate() -> io::Result<()> {
    let rpc_crate_path = PathBuf::from(RPC_CRATE_DIR).canonicalize()?;
    let this_crate_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).canonicalize()?;

    for source_proto in fs::read_dir(rpc_crate_path.join("proto"))? {
        let source_proto = source_proto?;
        let source = match source_proto.file_name().to_str() {
            Some("forge.proto") => filter_forge_rpcs(&fs::read_to_string(source_proto.path())?),
            Some(fname) if fname.ends_with(".proto") => fs::read_to_string(source_proto.path())?,
            _ => continue,
        };

        let dest_path = this_crate_path.join("proto").join(source_proto.file_name());
        let do_rewrite = match fs::read_to_string(&dest_path) {
            Err(_) => true,
            // Don't write it unless it changed, we don't want to bump timestamps and cause rebuilds
            Ok(contents) => contents != source,
        };

        if do_rewrite {
            fs::write(dest_path, source)?;
        }
    }

    Ok(())
}

fn filter_forge_rpcs(source: &str) -> String {
    let mut output = Vec::new();
    let mut in_service = false;
    let mut rpc_keep = None;
    let mut rpc_brace_depth = 0_i32;

    for line in source.lines() {
        if !in_service {
            output.push(line);
            if line.contains("service Forge {") {
                in_service = true;
            }
            continue;
        }

        if let Some(keep) = rpc_keep {
            if keep {
                output.push(line);
            }
            rpc_brace_depth += line.matches('{').count() as i32;
            rpc_brace_depth -= line.matches('}').count() as i32;
            if (rpc_brace_depth == 0 && line.contains(';'))
                || (rpc_brace_depth == 0 && line.contains('}'))
            {
                rpc_keep = None;
            }
            continue;
        }

        if line.trim() == "}" {
            output.push(line);
            in_service = false;
            continue;
        }

        if let Some(rpc_start) = line.find("rpc ") {
            let rpc_name = line[rpc_start + 4..]
                .split('(')
                .next()
                .unwrap_or_default()
                .trim();
            let keep = KEEP_RPCS.contains(&rpc_name);
            if keep {
                output.push(line);
            }
            rpc_brace_depth = line.matches('{').count() as i32 - line.matches('}').count() as i32;
            if !line.contains(';') && !(line.contains('{') && rpc_brace_depth == 0) {
                rpc_keep = Some(keep);
            }
        }
    }

    output.join("\n")
}

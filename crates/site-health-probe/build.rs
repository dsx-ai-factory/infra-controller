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

//! TEST-ONLY codegen: generates the in-process stub Forge *server* that the
//! nicoapi probe's mTLS tests run against (`src/probes/nicoapi.rs`, mod
//! `stubpb`). Nothing here is production code and nothing generated is
//! committed — output lands in OUT_DIR.
//!
//! Why this exists at all: the probe itself takes everything from the
//! `carbide-rpc` crate (its `ForgeTlsClient`, generated `ForgeClient`, and
//! message types). What `carbide-rpc` cannot provide is a *test server*:
//! implementing its full `forge_server::Forge` trait means stubbing every RPC
//! in forge.proto, and the existing `ssh-console-mock-api-server` mocks a
//! different RPC subset with hard-wired dev certificates (no client-CA
//! verification, which these tests exist to exercise). So, exactly like that
//! crate's build.rs, this one compiles a copy of `forge.proto` filtered down
//! to the probed RPCs and generates only the server side; the stub's message
//! types are wire-compatible with the client's without sharing Rust types.

use std::error::Error;
use std::path::PathBuf;
use std::{env, fs};

/// The RPCs the probe exercises — the only ones the stub server implements.
static KEEP_RPCS: &[&str] = &["FindMachineIds", "FindMachinesByIds"];

static RPC_PROTO_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../rpc/proto");

fn main() -> Result<(), Box<dyn Error>> {
    println!("cargo:rerun-if-changed=../rpc/proto");

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR is set by cargo"));
    let proto_dir = out_dir.join("proto");
    fs::create_dir_all(&proto_dir)?;
    let filtered_forge = proto_dir.join("forge.proto");
    fs::write(&filtered_forge, filtered_forge_proto()?)?;

    tonic_prost_build::configure()
        .build_server(true)
        .build_client(false) // the probe uses the rpc crate's client
        .protoc_arg("--experimental_allow_proto3_optional")
        .compile_protos(
            &[filtered_forge],
            &[proto_dir, PathBuf::from(RPC_PROTO_DIR)],
        )?;

    Ok(())
}

/// `forge.proto` with every RPC except [`KEEP_RPCS`] removed. Message
/// definitions and imports are untouched; imports resolve against the rpc
/// crate's proto directory.
fn filtered_forge_proto() -> std::io::Result<String> {
    let source = fs::read_to_string(PathBuf::from(RPC_PROTO_DIR).join("forge.proto"))?;
    let mut in_rpc_section = false;
    Ok(source
        .lines()
        .filter(|line| match in_rpc_section {
            false => {
                if line.contains("service Forge {") {
                    in_rpc_section = true;
                }
                true
            }
            true => {
                if *line == "}" {
                    in_rpc_section = false;
                    true
                } else {
                    KEEP_RPCS
                        .iter()
                        .any(|keep_rpc| line.contains(&format!("rpc {keep_rpc}(")))
                }
            }
        })
        .collect::<Vec<_>>()
        .join("\n"))
}

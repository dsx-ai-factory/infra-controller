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

fn main() -> Result<(), Box<dyn std::error::Error>> {
    carbide_version::build();

    let proto_dir = PathBuf::from("proto");

    println!("cargo:rerun-if-changed=proto/");

    // vendored from openconfig/gnmi v0.11.0
    // gnmi_ext compiled separately so gnmi.proto can extern_path it and reuse the types
    tonic_prost_build::configure()
        .build_client(true)
        .build_server(false)
        .compile_protos(
            &[proto_dir.join("github.com/openconfig/gnmi/proto/gnmi_ext/gnmi_ext.proto")],
            std::slice::from_ref(&proto_dir),
        )?;

    tonic_prost_build::configure()
        .build_client(true)
        // The transport test runs a gNMI server; keep server bindings out of
        // production builds.
        .build_server(true)
        .server_mod_attribute("gnmi", "#[cfg(test)]")
        .extern_path(
            ".gnmi_ext",
            "crate::collectors::nvue::gnmi::proto::gnmi_ext",
        )
        .compile_protos(
            &[proto_dir.join("github.com/openconfig/gnmi/proto/gnmi/gnmi.proto")],
            std::slice::from_ref(&proto_dir),
        )?;

    Ok(())
}

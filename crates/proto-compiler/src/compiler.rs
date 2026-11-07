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

use std::ffi::OsString;
use std::fs;
use std::path::PathBuf;

use prost_reflect::DescriptorPool;

use crate::{Error, Schema};

/// Inputs to one protobuf frontend invocation.
#[derive(Clone, Debug, Default)]
pub struct CompilerConfig {
    /// Root protobuf files to compile.
    pub proto_files: Vec<PathBuf>,
    /// Directories used to resolve protobuf imports.
    pub include_paths: Vec<PathBuf>,
    /// Additional arguments passed directly to `protoc`.
    pub protoc_args: Vec<OsString>,
}

/// Compiles the configured protobuf files into shared descriptor views.
///
/// `protoc` emits descriptors only; downstream generators render Rust from the
/// returned [`crate::Schema`].
///
/// Imported files and source locations are always included. Support for
/// proto3 optional fields is enabled in addition to any caller-provided
/// `protoc` arguments.
///
/// # Errors
///
/// Returns an error if temporary descriptor storage cannot be created,
/// `protoc` rejects the schema, the raw descriptor set cannot be read, or
/// the raw bytes cannot be decoded into a semantic descriptor pool.
pub fn compile(config: &CompilerConfig) -> Result<Schema, Error> {
    // An explicit temporary output path lets us retain prost-build's decoded
    // view and the exact bytes from the same protoc invocation.
    let descriptor_directory = tempfile::Builder::new()
        .prefix("carbide-proto-compiler")
        .tempdir()
        .map_err(|source| Error::CreateTemporaryDescriptor { source })?;
    let descriptor_path = descriptor_directory.path().join("schema.bin");

    let mut prost_config = prost_build::Config::new();
    prost_config
        .file_descriptor_set_path(&descriptor_path)
        .protoc_arg("--experimental_allow_proto3_optional");
    for argument in &config.protoc_args {
        prost_config.protoc_arg(argument);
    }

    // `load_fds` performs the sole protoc invocation and decodes its output; it
    // does not render Rust. Its defaults include imports and source locations.
    let file_descriptor_set = prost_config
        .load_fds(&config.proto_files, &config.include_paths)
        .map_err(|source| Error::CompileProtobuf { source })?;

    // Do not reconstruct these bytes by encoding `file_descriptor_set`:
    // prost_types drops unknown wire fields, including custom option values,
    // while decoding.
    let raw_descriptor_set =
        fs::read(&descriptor_path).map_err(|source| Error::ReadDescriptorSet {
            path: descriptor_path,
            source,
        })?;

    // Decode the semantic view directly from the raw bytes so resolved type
    // and extension lookup includes custom option values.
    let descriptor_pool = DescriptorPool::decode(raw_descriptor_set.as_slice())
        .map_err(|source| Error::DecodeDescriptorPool { source })?;

    Ok(Schema {
        raw_descriptor_set,
        file_descriptor_set,
        descriptor_pool,
    })
}

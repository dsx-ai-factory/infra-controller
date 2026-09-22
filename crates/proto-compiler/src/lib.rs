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

//! Build-time orchestration for Carbide's shared protobuf schema.
//!
//! This crate is not an implementation or replacement of `protoc`.
//! [`crate::compile`] configures [`prost_build::Config`] to invoke `protoc` once
//! as a protobuf frontend: `protoc` parses and validates the input files,
//! resolves imports and type references, and emits a serialized
//! [`prost_types::FileDescriptorSet`]. That descriptor set is the schema
//! intermediate representation (IR) consumed by the Rust generators; `protoc`
//! does not generate Rust in this pipeline.
//!
//! ```text
//! .proto files
//!     -> one protoc invocation
//!     -> serialized FileDescriptorSet
//!     -> shared Schema
//!         -> prost/tonic Rust generation
//!         -> Forge wrapper generation
//!         -> NMX-C wrapper generation
//!         -> forge.bin for runtime gRPC reflection
//! ```
//!
//! [`crate::Schema`] retains three representations of the same schema because
//! no one representation satisfies every backend: exact serialized bytes, a
//! decoded [`prost_types::FileDescriptorSet`], and a semantic, indexed
//! [`prost_reflect::DescriptorPool`]. See the type's field documentation for
//! the role and fidelity of each view. The fixture tests
//! `raw_descriptor_pool_preserves_custom_option_values` and
//! `prost_reencoding_drops_custom_option_values` demonstrate why the exact
//! bytes and semantic view must not be reconstructed from the decoded
//! `prost_types` view.
//!
//! Carbide runs this pipeline from `crates/rpc/build.rs` as a Cargo build
//! script. The in-memory [`crate::Schema`] is discarded when the build script
//! exits; generated Rust and the `forge.bin` reflection descriptor remain in
//! Cargo's `OUT_DIR`.

mod codegen;
mod compiler;
mod error;
mod schema;

pub use codegen::{Codegen, ExternPathSearchIndex, TonicBuilderCodegenExt};
pub use compiler::{CompilerConfig, compile};
pub use error::Error;
pub use schema::Schema;
pub use syn;

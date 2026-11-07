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

use prost_reflect::DescriptorPool;
use prost_types::FileDescriptorSet;

/// Three complementary views of one `protoc`-produced `FileDescriptorSet`.
///
/// The views come from one frontend invocation, not three compilations.
#[derive(Clone, Debug)]
pub struct Schema {
    /// The exact serialized `FileDescriptorSet` emitted by `protoc`.
    ///
    /// This representation preserves all wire data, including custom option
    /// values that `prost_types` cannot represent. The RPC build writes these
    /// bytes unchanged to `forge.bin` for runtime gRPC reflection.
    pub raw_descriptor_set: Vec<u8>,

    /// The schema decoded into `prost_types` structures.
    ///
    /// Existing prost, tonic, and wrapper generators consume this convenient
    /// structural view. Unknown wire fields, including project-defined custom
    /// option values, are not retained in it, so it must not be re-encoded to
    /// reconstruct [`Self::raw_descriptor_set`].
    pub file_descriptor_set: FileDescriptorSet,

    /// The semantic, indexed view decoded directly from the raw bytes.
    ///
    /// The pool resolves messages, services, and extensions and supports typed
    /// custom-option lookup.
    pub descriptor_pool: DescriptorPool,
}

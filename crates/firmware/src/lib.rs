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

#[cfg(test)]
mod tests;

mod artifact_cache;

mod artifact_resolution;

pub mod config;

pub mod defaults;

pub mod downloader;

#[cfg(feature = "test-support")]
pub mod test_support;

pub use artifact_cache::firmware_cache_filename;
pub use artifact_resolution::{
    ResolvedFirmwareArtifact, ResolvedFirmwareArtifactSource, resolve_files_firmware_artifact,
};
pub use config::{FirmwareConfig, FirmwareConfigSnapshot};
pub use downloader::FirmwareDownloader;

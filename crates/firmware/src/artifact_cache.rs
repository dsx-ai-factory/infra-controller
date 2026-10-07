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

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use url::Url;

pub fn firmware_cache_filename(firmware_cache_directory: &Path, url: &str) -> Option<PathBuf> {
    let url_hash = hex::encode(Sha256::digest(url.as_bytes()));
    let filename = filename_from_url(url)?;

    Some(firmware_cache_directory.join(url_hash).join(filename))
}

fn filename_from_url(raw_url: &str) -> Option<String> {
    let url = Url::parse(raw_url).ok()?;
    let filename = url.path_segments()?.next_back()?.trim();

    if filename.is_empty() || filename == "." || filename == ".." {
        None
    } else {
        Some(filename.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn firmware_cache_filename_is_under_cache_directory() {
        let firmware_cache_directory = Path::new("/mnt/persistence/fw/download-cache");
        let filename = firmware_cache_filename(
            firmware_cache_directory,
            "https://firmware.example.invalid/path/iDRAC-with-Lifecycle-Controller_Firmware_WN31M_LN64_7.20.60.50_A00.BIN?token=secret",
        )
        .unwrap();

        assert!(filename.starts_with(firmware_cache_directory));
        assert_eq!(
            filename
                .parent()
                .unwrap()
                .file_name()
                .unwrap()
                .to_string_lossy()
                .len(),
            64
        );
        assert_eq!(
            filename.file_name().unwrap(),
            "iDRAC-with-Lifecycle-Controller_Firmware_WN31M_LN64_7.20.60.50_A00.BIN"
        );
    }

    #[test]
    fn firmware_cache_filename_returns_none_without_filename() {
        let firmware_cache_directory = Path::new("/mnt/persistence/fw/download-cache");
        let filename = firmware_cache_filename(
            firmware_cache_directory,
            "https://firmware.example.invalid/",
        );

        assert_eq!(filename, None);
    }
}

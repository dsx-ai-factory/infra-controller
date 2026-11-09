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

//! Persistent record of the host's `retain_hbn_config_on_boot` setting.
//!
//! The flag reaches the DPU in `ManagedHostNetworkConfigResponse`. The agent
//! keeps the last value Core sent beside the HBN files the flag protects.

use std::fs;
use std::path::{Path, PathBuf};

use eyre::Context;
use serde::{Deserialize, Serialize};

use crate::ethernet_virtualization::{self, FPath};

/// Beside `nvue_startup.yaml` and the DPU-local DHCP files, on the same
/// persistent DPU storage they rely on.
const PATH: &str = "var/support/nico-retain-hbn-config.json";

/// The record holds a single boolean, so anything approaching this size means
/// the file is not one of ours. The shared writer rejects a larger candidate
/// rather than overwrite the path.
const MAX_SIZE: u64 = 4 * 1024;

/// A JSON object rather than a bare boolean so later fields, such as the
/// retention bound this does not yet implement, do not change the format.
#[derive(Debug, Deserialize, Serialize)]
struct Record {
    retain_hbn_config_on_boot: bool,
}

fn path(hbn_root: &Path) -> PathBuf {
    hbn_root.join(PATH)
}

/// The last value Core sent, or `false` when none was ever recorded.
///
/// Every failure reads as unset. An agent that cannot tell whether it retains
/// its configuration keeps the unconditional reset behavior, so a damaged file
/// cannot grant retention. The next successful fetch rewrites the file.
pub(super) fn load(hbn_root: &Path) -> bool {
    let path = path(hbn_root);
    match read(&path) {
        Ok(retain_hbn_config_on_boot) => retain_hbn_config_on_boot,
        Err(err) => {
            tracing::warn!(
                error = format!("{err:#}"),
                path = %path.display(),
                "Cannot read the HBN retention flag; this DPU will not retain its configuration"
            );
            false
        }
    }
}

fn read(path: &Path) -> eyre::Result<bool> {
    match fs::read_to_string(path) {
        Ok(contents) => Ok(serde_json::from_str::<Record>(&contents)?.retain_hbn_config_on_boot),
        // Never recorded, which is the state of a DPU that has not yet reached
        // a Core that sends the flag.
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err.into()),
    }
}

/// Records the value Core sent. Called on every successful fetch; the shared
/// file writer skips the disk when the contents already match. A failure here
/// only costs the next cold start its retention, so it is logged rather than
/// propagated into the main loop.
pub(super) fn store(hbn_root: &Path, retain_hbn_config_on_boot: bool) {
    if let Err(err) = write(hbn_root, retain_hbn_config_on_boot) {
        tracing::error!(
            error = format!("{err:#}"),
            "Failed to record the HBN retention flag"
        );
    }
}

fn write(hbn_root: &Path, retain_hbn_config_on_boot: bool) -> eyre::Result<()> {
    let path = FPath(path(hbn_root));
    let parent = path
        .0
        .parent()
        .ok_or_else(|| eyre::eyre!("{path} has no parent directory"))?;
    fs::create_dir_all(parent)
        .wrap_err_with(|| format!("create directory {}", parent.display()))?;

    let contents = serde_json::to_string(&Record {
        retain_hbn_config_on_boot,
    })?;
    ethernet_virtualization::write(contents, &path, "HBN retention flag", MAX_SIZE, false)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_without_a_record_does_not_retain() {
        let root = tempfile::tempdir().unwrap();
        assert!(!load(root.path()));
    }

    #[test]
    fn store_then_load_round_trips() {
        let root = tempfile::tempdir().unwrap();
        for retain in [true, false, true] {
            store(root.path(), retain);
            assert_eq!(load(root.path()), retain, "stored {retain}");
        }
    }

    #[test]
    fn an_unparsable_record_does_not_retain_and_is_repaired() {
        let root = tempfile::tempdir().unwrap();
        store(root.path(), true);
        fs::write(path(root.path()), "{not json").unwrap();
        assert!(!load(root.path()));

        // Storing on every successful fetch is what repairs the file.
        store(root.path(), true);
        assert!(load(root.path()));
    }
}

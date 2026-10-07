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

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use eyre::{ContextCompat, WrapErr};
use temp_dir::TempDir;

/// Creates a script which exec's ipmitool, available at `path`, while recording all calls
/// to it to the path in `invocations_path`.
pub(crate) struct RecordingIpmitool {
    pub(crate) path: PathBuf,
    pub(crate) invocations_path: PathBuf,
    _test_dir: TempDir,
}

impl RecordingIpmitool {
    pub(crate) fn new(conflicting_activation: bool) -> eyre::Result<Self> {
        let test_dir = TempDir::new().context("failed to create ipmitool recorder directory")?;
        let invocations_path = test_dir.path().join("invocations");
        std::fs::write(&invocations_path, [])?;
        let path = test_dir.path().join("ipmitool");
        let real_ipmitool = find_executable("ipmitool")
            .context("ipmitool is not available in PATH")?
            .canonicalize()?;
        let conflicting_activation = if conflicting_activation {
            r#"
previous=
last=
for argument in "$@"; do
    previous=$last
    last=$argument
done
if [ "$previous" = "sol" ] && [ "$last" = "activate" ]; then
    printf 'Info: SOL payload already active on another session\n'
    exec sleep 600
fi
"#
        } else {
            ""
        };
        let script = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> {}\n{}exec {} \"$@\"\n",
            shell_quote(&invocations_path)?,
            conflicting_activation,
            shell_quote(&real_ipmitool)?,
        );
        std::fs::write(&path, script)?;
        let mut permissions = std::fs::metadata(&path)?.permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&path, permissions)?;

        Ok(Self {
            path,
            invocations_path,
            _test_dir: test_dir,
        })
    }

    pub(crate) fn invocations(&self) -> eyre::Result<Vec<String>> {
        Ok(std::fs::read_to_string(&self.invocations_path)?
            .lines()
            .map(str::to_owned)
            .collect())
    }
}

fn find_executable(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|directory| directory.join(name))
        .find(|candidate| candidate.is_file())
}
fn shell_quote(path: &Path) -> eyre::Result<String> {
    let path = path
        .to_str()
        .with_context(|| format!("path is not valid UTF-8: {}", path.display()))?;
    Ok(format!("'{}'", path.replace('\'', "'\"'\"'")))
}

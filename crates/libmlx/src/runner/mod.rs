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

pub mod applier;
pub(crate) mod attempt;
pub use attempt::FirmwareToolExecutor;
pub mod command_builder;
pub mod error;
pub mod exec_options;
pub mod executor;
pub mod json_parser;
pub mod result_types;
#[allow(clippy::module_inception)]
pub mod runner;
#[cfg(target_os = "linux")]
mod supervisor_protocol;
pub mod traits;

// Recording firmware fixtures use the real framing without exposing the worker
// protocol to production callers outside the runner.
#[cfg(all(test, target_os = "linux"))]
pub(crate) use supervisor_protocol::{HELLO, Request, Response};

/// Unit-test binaries live in `deps`, while Cargo places the helper beside it.
#[cfg(all(test, target_os = "linux"))]
pub(crate) fn test_firmware_executor() -> FirmwareToolExecutor {
    let executable = std::env::current_exe().unwrap();
    let helper = executable
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("mlx-firmware-supervisor");
    assert!(
        helper.is_file(),
        "build the firmware supervisor before unit tests"
    );
    FirmwareToolExecutor::with_supervisor_path(helper)
}

/// `write_fixture_executables` writes named programs under `directory` with
/// mode `0700`. A child writer keeps writable executable descriptors out of
/// concurrent test forks, which could otherwise cause `ETXTBSY` on execution.
/// Panics if the writer cannot start, fails, or exceeds its 10-second watchdog.
#[cfg(all(test, target_os = "linux"))]
pub(crate) async fn write_fixture_executables(
    directory: &std::path::Path,
    programs: &[(&str, &str)],
) {
    let mut writer = tokio::process::Command::new("/bin/sh");
    writer
        .args([
            "-c",
            "set -eu; umask 077; while [ \"$#\" -gt 0 ]; do printf '%s' \"$2\" > \"$1\"; chmod 700 \"$1\"; shift 2; done",
            "fixture-writer",
        ])
        .kill_on_drop(true);
    for (name, contents) in programs {
        writer.arg(directory.join(name)).arg(contents);
    }
    let status = tokio::time::timeout(std::time::Duration::from_secs(10), writer.status())
        .await
        .unwrap_or_else(|_| {
            panic!(
                "fixture writer exceeded its 10-second watchdog in {}",
                directory.display(),
            )
        })
        .unwrap_or_else(|error| {
            panic!(
                "start isolated fixture writer in {}: {error}",
                directory.display(),
            )
        });
    assert!(
        status.success(),
        "fixture writer failed in {}: {status}",
        directory.display(),
    );
}

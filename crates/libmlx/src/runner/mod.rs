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

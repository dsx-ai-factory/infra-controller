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
// src/bin/cargo-carbide-lints.rs
use std::env;
use std::process::Command;

fn main() -> Result<(), i32> {
    let cargo = env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let mut cmd = Command::new(cargo);
    let driver = env::current_exe().unwrap().with_file_name("carbide-lints");

    // Invoke carbide-lints with args as if we're running `cargo check`. `carbide-lints` will invoke
    // rustc with those args and intercept certain phases.
    //
    // Incoming args for a command like `cargo carbide-lints -p api-model` look like:
    //
    // ["/home/user/.cargo/bin/cargo-carbide-lints", "carbide-lints", "-p", "carbide-api-model"]
    //
    // So skip the first two and forward the rest.
    let args = env::args_os().skip(2);
    let status = cmd
        .arg("check")
        .args(args)
        .env("RUSTC", driver)
        .status()
        .unwrap();
    match status.code() {
        Some(0) => Ok(()),
        Some(other) => Err(other),
        None => Err(-1),
    }
}

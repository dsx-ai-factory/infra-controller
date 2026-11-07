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
use clap::{Parser, Subcommand};

use crate::device::cmd::device::args::DeviceArgs;
pub mod device;

// Cli represents the main CLI structure for the application.
#[derive(Parser)]
#[command(
    author,
    version,
    about = "mlxconfig-device - mellanox device discovery"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

// Commands defines the available top-level commands.
#[derive(Subcommand)]
pub enum Commands {
    // Device management commands for discovering and
    // inspecting Mellanox devices.
    Device(DeviceArgs),
}

// dispatch_command routes CLI commands to their
// appropriate handlers.
pub fn dispatch_command(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    match cli.command {
        Commands::Device(args) => crate::device::cmd::device::cmds::handle(args),
    }
}

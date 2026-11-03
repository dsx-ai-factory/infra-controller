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

use clap::{Parser, Subcommand, ValueEnum};

// Cli is a parent Cli struct to give this a complete command
// spec for doing tests. The actual command will be put into
// the mlxconfig-embedded reference CLI example.
#[derive(Parser)]
#[command(name = "mlxconfig-lockdown")]
#[command(about = "Manage Mellanox NIC hardware access locks")]
#[command(version)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

// Commands are the available CLI commands.
#[derive(Subcommand)]
pub enum Commands {
    // lockdown manages device lockdown status.
    Lockdown {
        #[command(subcommand)]
        action: LockdownAction,
    },
}

// LockdownAction are the lockdown subcommands.
#[derive(Clone, Subcommand)]
pub enum LockdownAction {
    // lock locks hardware access on the device.
    #[command(about = "Lock hardware access from a given device (PCI address or mst path).")]
    Lock {
        // device_id is the device identifier (PCI address or device path).
        device_id: String,
        /// Hardware access key: 1–16 bare ASCII hexadecimal digits (no 0x prefix).
        /// Passed byte-for-byte unchanged to flint.
        key: String,
        // format is the output format for status.
        #[arg(short, long, value_enum, default_value_t = OutputFormat::Text)]
        format: OutputFormat,
        // dry_run shows what would be executed without actually running it.
        #[arg(long)]
        dry_run: bool,
    },
    // unlock unlocks hardware access on the device.
    #[command(about = "Unlock hardware access to the given device.")]
    Unlock {
        // device_id is the device identifier (PCI address or device path).
        device_id: String,
        /// Hardware access key: 1–16 bare ASCII hexadecimal digits (no 0x prefix).
        /// Passed byte-for-byte unchanged to flint.
        key: String,
        // format is the output format for status.
        #[arg(short, long, value_enum, default_value_t = OutputFormat::Text)]
        format: OutputFormat,
        // dry_run shows what would be executed without actually running it.
        #[arg(long)]
        dry_run: bool,
    },
    // status checks current lock and key status of the device.
    #[command(about = "Get the current lock/unlock status of the given device.")]
    Status {
        // device_id is the device identifier (PCI address or device path).
        device_id: String,
        // format is the output format for status.
        #[arg(short, long, value_enum, default_value_t = OutputFormat::Text)]
        format: OutputFormat,
        // dry_run shows what would be executed without actually running it.
        #[arg(long)]
        dry_run: bool,
    },
    // set-key sets or updates the hardware access key.
    #[command(
        name = "set-key",
        about = "Set a hardware access key on the given device, effectively locking it."
    )]
    SetKey {
        // device_id is the device identifier (PCI address or device path).
        device_id: String,
        /// Hardware access key: 1–16 bare ASCII hexadecimal digits (no 0x prefix).
        /// Passed byte-for-byte unchanged to flint.
        key: String,
        // format is the output format for status.
        #[arg(short, long, value_enum, default_value_t = OutputFormat::Text)]
        format: OutputFormat,
        // dry_run shows what would be executed without actually running it.
        #[arg(long)]
        dry_run: bool,
    },
}

// OutputFormat are the supported output formats.
#[derive(ValueEnum, Clone, Debug)]
pub enum OutputFormat {
    Text,
    Json,
    Yaml,
}

/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use std::path::PathBuf;

use clap::Parser;

/// Stateful Xplane maintenance mock for interactive DPU agent verification.
/// All 32 Astra ports start in maintenance mode on each launch.
#[derive(Parser)]
struct Args {
    /// Unix socket to bind. The DPU agent uses /var/run/xplane/core.sock.
    /// The parent directory must exist; an existing socket is never replaced.
    #[arg(long, default_value = "/var/run/xplane/core.sock")]
    socket_path: PathBuf,
}

#[tokio::main]
async fn main() -> eyre::Result<()> {
    let args = Args::parse();
    carbide_host_support::init_logging("xplane-maintenance-mock")?;
    agent::xplane_mock_server::run_standalone(&args.socket_path).await
}

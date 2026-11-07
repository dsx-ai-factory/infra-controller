// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

mod list;

use clap::Parser;

use crate::cfg::dispatch::Dispatch;

#[derive(Dispatch, Parser, Debug)]
pub(crate) enum Cmd {
    #[clap(about = "List machine attestation statuses")]
    List(list::Args),
}

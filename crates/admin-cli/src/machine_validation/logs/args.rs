/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use carbide_uuid::machine_validation::MachineValidationId;
use clap::{ArgGroup, Args as ClapArgs, Parser};
use uuid::Uuid;

#[derive(Parser, Debug)]
pub(crate) enum Args {
    #[clap(
        about = "Show stored logs for a validation attempt",
        after_long_help = "\
EXAMPLES:

Show all stored logs for an attempt:
    $ nico-admin-cli machine-validation logs show --attempt-id 12345678-1234-5678-90ab-cdef01234567

Show the current attempt for a test in a run:
    $ nico-admin-cli machine-validation logs show --validation-id 12345678-1234-5678-90ab-cdef01234567 --test-id basic-machine-validation

Print only stderr content without metadata:
    $ nico-admin-cli machine-validation logs show --attempt-id 12345678-1234-5678-90ab-cdef01234567 --stderr-only --raw

"
    )]
    Show(Options),
    #[clap(
        about = "Show stored logs and follow an active validation attempt",
        after_long_help = "\
EXAMPLES:

Follow a known attempt:
    $ nico-admin-cli machine-validation logs follow --attempt-id 12345678-1234-5678-90ab-cdef01234567

Follow the current attempt for a test in a run:
    $ nico-admin-cli machine-validation logs follow --validation-id 12345678-1234-5678-90ab-cdef01234567 --test-id basic-machine-validation

Follow stdout content without metadata:
    $ nico-admin-cli machine-validation logs follow --attempt-id 12345678-1234-5678-90ab-cdef01234567 --stdout-only --raw

"
    )]
    Follow(Options),
    #[clap(
        about = "List attempt IDs for one test in a validation run",
        after_long_help = "\
EXAMPLES:

Find current and earlier attempt IDs for a test:
    $ nico-admin-cli machine-validation logs attempts --validation-id 12345678-1234-5678-90ab-cdef01234567 --test-id basic-machine-validation

"
    )]
    Attempts(AttemptOptions),
}

#[derive(ClapArgs, Debug)]
pub(crate) struct AttemptOptions {
    #[arg(long, help = "Run ID containing the test")]
    pub(super) validation_id: MachineValidationId,

    #[arg(long, help = "Test ID within the run")]
    pub(super) test_id: String,
}

#[derive(ClapArgs, Debug)]
#[command(group(ArgGroup::new("selector").required(true).args(["attempt_id", "validation_id"])))]
pub(crate) struct Options {
    #[arg(long, help = "Attempt UUID, including completed or retried attempts")]
    pub(super) attempt_id: Option<Uuid>,

    #[arg(long, requires = "test_id", help = "Run ID containing the test")]
    pub(super) validation_id: Option<MachineValidationId>,

    #[arg(long, requires = "validation_id", help = "Test ID within the run")]
    pub(super) test_id: Option<String>,

    #[arg(
        long,
        conflicts_with = "stderr_only",
        help = "Print stdout chunks only"
    )]
    pub(super) stdout_only: bool,

    #[arg(long, help = "Print stderr chunks only")]
    pub(super) stderr_only: bool,

    #[arg(
        long,
        help = "Print chunk content without timestamp, stream, or sequence"
    )]
    pub(super) raw: bool,
}

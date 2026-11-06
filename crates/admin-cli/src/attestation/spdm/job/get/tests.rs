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

use carbide_test_support::Outcome::*;
use carbide_test_support::scenarios;
use clap::Parser;
use clap::error::ErrorKind;
use uuid::Uuid;

use crate::attestation::Cmd as AttestationCmd;
use crate::attestation::spdm::Cmd as SpdmCmd;
use crate::attestation::spdm::job::Cmd as JobCmd;
use crate::cfg::cli_options::{CliCommand, CliOptions};

#[test]
fn job_get_command_path() {
    let job_id = "12345678-1234-5678-90ab-cdef01234567";
    let expected = Uuid::parse_str(job_id).unwrap();

    // Parse the public command tree to protect the resource grouping and alias.
    scenarios!(
        run = |args: &[&str]| {
            CliOptions::try_parse_from(
                std::iter::once("nico-admin-cli").chain(args.iter().copied()),
            )
            .map(|options| {
                let Some(CliCommand::Attestation(AttestationCmd::Spdm(SpdmCmd::Job(
                    JobCmd::Get(args),
                )))) = options.commands else {
                    panic!("expected the public SPDM job get command path");
                };
                args.job_id
            })
            .map_err(|error| error.kind())
        };
        "canonical path" {
            &["attestation", "spdm", "job", "get", job_id][..] => Yields(expected),
        }
        "attestation alias" {
            &["att", "spdm", "job", "get", job_id][..] => Yields(expected),
        }
        "job ID is required" {
            &["att", "spdm", "job", "get"][..] => FailsWith(ErrorKind::MissingRequiredArgument),
        }
        "job ID must be a UUID" {
            &["att", "spdm", "job", "get", "invalid"][..] => FailsWith(ErrorKind::ValueValidation),
        }
        "ungrouped get is removed" {
            &["att", "spdm", "get", job_id][..] => FailsWith(ErrorKind::InvalidSubcommand),
        }
    );
}

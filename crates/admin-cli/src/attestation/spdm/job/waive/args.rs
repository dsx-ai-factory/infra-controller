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

use clap::Parser;
use uuid::Uuid;

#[derive(Parser, Debug)]
#[command(about = "Request a waiver for an attestation job, including completed failures")]
#[command(
    long_about = "Waive an attestation job, including completed failures. Unfinished devices finish BMC evidence collection cleanup before the waiver takes effect. Terminal results immediately count as Waived without rerunning attestation. Later waive or fail requests replace the previous target and refresh the request time. Changing the request does not restart ongoing cleanup. Stored attestation results and completion times stay available as history."
)]
#[command(after_long_help = "\
EXAMPLES:

Waive an SPDM attestation job after BMC cleanup:
    $ nico-admin-cli attestation spdm job waive 12345678-1234-5678-90ab-cdef01234567

")]
pub(crate) struct Args {
    #[clap(help = "Attestation job ID")]
    pub(super) job_id: Uuid,
}

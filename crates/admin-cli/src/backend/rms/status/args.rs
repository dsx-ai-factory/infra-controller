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

#[derive(Parser, Debug)]
#[command(
    long_about = "Probe the RMS backend connectivity and version.\n\n\
        Sends a `GetRmsVersion` rpc to nico-api, which proxies the request to its \
        configured RMS backend. Text output prints three lines — `status` (a \
        machine-readable token), `message` (a human-readable sentence), and `version` \
        (the string the RMS backend returned, or `-` when the probe did not get one). \
        The global `-f json` flag emits the same three fields as a JSON object.\n\n\
        Exits `0` only when the status is `connected`; every other status exits `1`.\n\n\
        Status tokens:\n\n\
        `connected` — the probe reached RMS and received a version.\n\n\
        `not-configured` — no RMS endpoint is set on this nico-api instance.\n\n\
        `api-unreachable` — the cli could not connect to nico-api, because the server \
        is down or the url is wrong.\n\n\
        `rms-unreachable` — nico-api was reached but cannot contact the RMS backend.\n\n\
        `cli-config-error` — a local cli configuration problem, such as a missing CA \
        file, prevented connecting to nico-api.\n\n\
        `auth-failed` — a certificate was rejected on the cli→nico-api or nico-api→RMS \
        path.\n\n\
        `auth-or-version-mismatch` — permission denied: either the cli certificate \
        lacks the required role, or this nico-api server predates `GetRmsVersion` and \
        its rbac rules reject the call before dispatch.\n\n\
        `api-version-mismatch` — nico-api returned `Unimplemented`; the server predates \
        this rpc and has no rbac layer to intercept it.\n\n\
        `timeout` — the connection attempt exceeded the deadline.\n\n\
        `error` — an unexpected error; the `message` field carries the grpc code and \
        detail.",
    after_long_help = "\
EXAMPLES:

Check RMS connectivity and print the version (requires RMS to be configured in nico-api):
    $ nico-admin-cli backend rms status

Same check, JSON output:
    $ nico-admin-cli -f json backend rms status

"
)]
pub(crate) struct Args;

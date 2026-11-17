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

//! `nico-pre-ingest`: ingestion readiness before the first host is approved.

use crate::harness::{Check, todo};

pub(crate) const CHECKS: &[Check] = &[
    Check {
        id: "ingest.expected-machines.parse",
        expected: "the expected-machines file parses and matches the fleet",
        detail: "Reads: `collaterals.expectedMachines`, parsed with the type the seed and \
                 `nico-admin-cli` use; `fleet.hosts` (model, gpu, count) and `fleet.dpu`.\n\
                 Pass: the file parses; the entry count equals the sum of `fleet.hosts[].count`; \
                 every entry's model is in `fleet`; serials unique; observed `128 entries, models \
                 match fleet, serials unique`.\n\
                 Warn: fewer entries than the fleet count (partial ingestion planned).\n\
                 Fail: a parse error (line quoted), a serial whose model is not in `fleet`, a \
                 duplicate serial, more entries than the fleet; fix: edit the file or `fleet`.\n\
                 Skip: when `collaterals.expectedMachines` is not named.\n\
                 Test: fixture files; cases: matching passes; a model not in fleet fails naming \
                 the serial; a duplicate serial fails; 100 of 128 warns; malformed YAML fails \
                 with the line.",
        run: todo,
    },
    Check {
        id: "ingest.credentials.sources",
        expected: "the BMC root and UEFI credential sources resolve",
        detail: "Reads: `credentials.bmcRoot` and `credentials.uefi` by source (a `file` path, an \
                 `env` name, or a store); whether each resolves to a non-empty value. The value \
                 is never read into the output.\n\
                 Pass: both sources resolve to a non-empty value; observed `bmcRoot: file \
                 present; uefi: env set`.\n\
                 Fail: a file missing or empty, a variable unset or empty, a store refusing; \
                 observed names the source, never the value; fix: set it.\n\
                 Skip: never.\n\
                 Test: a temporary file and an environment variable set in the test; cases: both \
                 present passes; an empty file fails; the variable unset fails naming it; the \
                 observed text never contains the test value (asserted in every case).",
        run: todo,
    },
    Check {
        id: "ingest.bmc.reach",
        expected: "a sample of managed host BMCs answers from the BMC probe host",
        detail: "Reads: `hosts.bmcProbeHost`; a sample of managed host BMC addresses from \
                 `collaterals.expectedMachines`; a `GET /redfish/v1` from the probe host with \
                 `credentials.bmcRoot`.\n\
                 Pass: every sampled BMC answers; observed `10 of 10 sampled BMCs answer from \
                 10.10.250.20`.\n\
                 Warn: some unanswered, below a threshold (hosts not racked yet); observed names \
                 them.\n\
                 Fail: none answering, or 401 on all (a credential problem); observed names the \
                 addresses tried; fix: the probe host's route or the credential.\n\
                 Skip: when no BMC address is known yet.\n\
                 Test: bmc-mock instances with the test host as the probe host; cases: all answer \
                 passes; a few unreachable warns naming them; all 401 fails naming the \
                 credential.",
        run: todo,
    },
];

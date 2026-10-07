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

//! The boot-interface command family: inspect the stores a machine's boot
//! interface lives in (`show`), list the candidate NICs with the picks the
//! system computes among them (`candidates`), and set the boot interface
//! (`set`). The views are projections of the `GetMachineBootInterfaces` RPC;
//! `set` fronts the same `SetPrimaryInterface` RPC as
//! `managed-host set-primary-interface`.

mod candidates;
mod set;
pub(crate) mod show;

use clap::Parser;

use crate::cfg::dispatch::Dispatch;

/// Align summary values across both boot interface inspection commands.
const SUMMARY_LABEL_WIDTH: usize = 29;

#[derive(Parser, Debug, Dispatch)]
pub(crate) enum Cmd {
    // Note for the abouts below: possessives are deliberately avoided -- the
    // man-page path (clap_mangen -> pandoc) drops apostrophes, so "a machine's
    // boot interface" renders as "a machines boot interface" in the generated
    // reference.
    #[clap(
        visible_alias = "details",
        about = "Show boot interfaces for a machine from every store (troubleshooting)",
        long_about = "Gather the boot-interface view for one machine from all four stores and \
            print them together: the managed `machine_interfaces` rows (owned candidates and \
            primary selection), `predicted_machine_interfaces` (pre-first-lease candidates), \
            the `explored_endpoints` default, and retained post-deletion pairs (including stale \
            records). Also reports the effective owned pick and flags when current selection \
            signals disagree (the effective owned pick, explored defaults, and declared-primary \
            predictions; retained history and the desired target are excluded). The persisted \
            desired target and reconciliation progress appear separately. Read-only."
    )]
    Show(show::Args),
    #[clap(
        about = "List boot-interface candidates for a machine and the picks among them",
        long_about = "List every NIC that could be the boot interface for a machine -- the \
            managed `machine_interfaces` rows and the pre-first-lease predictions -- and \
            mark the picks among them: `current` (the effective managed pick, or the predicted \
            pick before the first lease), `default` (the lowest-MAC non-underlay managed \
            interface if no primary interface were set), and `explored` (the default \
            site-explorer recorded for the BMC endpoint of the machine). The predicted pick is \
            a declared primary, or the sole non-underlay prediction; with several eligible \
            undeclared predictions the system refuses to guess. For selection, an already-declared \
            primary is eligible regardless of segment; otherwise underlay rows are excluded from \
            the automatic fallback. The selection/default picks are computed server-side; use \
            `boot-interface show` to compare them with the persisted desired target and controller \
            progress. Read-only."
    )]
    Candidates(candidates::Args),
    #[clap(
        about = "Set the boot interface for a managed host (promotes it to the primary interface)",
        long_about = "Make an interface both the primary interface and persisted desired boot \
            target for a managed host. This is the same operation as `managed-host \
            set-primary-interface`: the primary row and desired target commit together, then \
            machine-controller reconciles the BMC when the host is eligible. The interface can \
            be named by machine-interface UUID or by MAC address; a MAC must match exactly one \
            managed interface row on the machine. If the host has a DPU-backed Admin interface, \
            the selected interface must also be on the Admin segment."
    )]
    Set(set::Args),
}

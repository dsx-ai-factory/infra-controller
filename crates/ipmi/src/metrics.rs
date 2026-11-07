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

//! Attempt counters for the IPMI reset commands. A `restart` may issue up to
//! two commands -- the DPU legacy raw reset, then the chassis power reset --
//! and an intermediate failure used to vanish: collected into a `Vec` whose
//! entries were then dropped. Counting every command execution by command
//! and outcome makes those attempts and failures visible, without an
//! attempt-index label (attempt counts are unbounded; the totals are the
//! signal). The event is metric-only (`log = off`), matching the sites'
//! existing logging: a final failure still propagates to callers, and the
//! intermediate ones stay out of the logs as before.

use carbide_instrument::{Event, LabelValue, Outcome, emit};

/// The IPMI command being executed, as a bounded metric label. This is the
/// crate's whole command vocabulary, shared by the `ipmitool` and bmc-mock
/// HTTP implementations of [`crate::IPMITool`].
// Every command is a reset of one kind or another, and the variant names
// are exported verbatim as label values -- the full command name is the
// contract there, so the shared `Reset` postfix stays.
#[allow(clippy::enum_variant_names)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, LabelValue)]
pub(crate) enum IpmiCommand {
    /// `chassis power reset` -- the standard host power reset.
    ChassisPowerReset,
    /// The raw OEM power reset legacy-boot DPUs need (`raw 0x32 0xA1 0x01`).
    DpuLegacyPowerReset,
    /// `bmc reset cold` -- reboot the BMC itself.
    BmcColdReset,
}

/// An IPMI command execution completed, successfully or not. One count is
/// one dispatched command: the `ipmitool` runner's internal subprocess
/// retries ride inside a single count, the bmc-mock implementation counts
/// one per dispatched HTTP request, and a command that never reached the
/// wire (credentials unavailable) does not count at all.
#[derive(Event)]
#[event(
    event_name = "ipmi_command_completed",
    metric_name = "carbide_ipmi_commands_total",
    component = "carbide-ipmi",
    log = off,
    metric = counter,
    describe = "Number of IPMI command executions, by command and outcome."
)]
struct IpmiCommandCompleted {
    #[label]
    command: IpmiCommand,
    #[label]
    outcome: Outcome,
}

/// Counts one completed execution of `command` from its result; the result
/// itself is left for the caller to handle.
pub(crate) fn count_ipmi_command<T, E>(command: IpmiCommand, result: &Result<T, E>) {
    emit(IpmiCommandCompleted {
        command,
        outcome: result.into(),
    });
}

#[cfg(test)]
mod tests {
    use carbide_instrument::testing::{MetricsCapture, capture_logs};

    use super::*;

    #[test]
    fn count_ipmi_command_counts_by_command_and_outcome() {
        struct Case {
            name: &'static str,
            command: IpmiCommand,
            result: Result<(), ()>,
            expected_command: &'static str,
            expected_outcome: &'static str,
        }

        let cases = [
            Case {
                name: "chassis power reset ok",
                command: IpmiCommand::ChassisPowerReset,
                result: Ok(()),
                expected_command: "chassis_power_reset",
                expected_outcome: "ok",
            },
            Case {
                name: "chassis power reset failure",
                command: IpmiCommand::ChassisPowerReset,
                result: Err(()),
                expected_command: "chassis_power_reset",
                expected_outcome: "error",
            },
            Case {
                name: "dpu legacy power reset failure",
                command: IpmiCommand::DpuLegacyPowerReset,
                result: Err(()),
                expected_command: "dpu_legacy_power_reset",
                expected_outcome: "error",
            },
            Case {
                name: "bmc cold reset ok",
                command: IpmiCommand::BmcColdReset,
                result: Ok(()),
                expected_command: "bmc_cold_reset",
                expected_outcome: "ok",
            },
        ];

        for case in cases {
            let metrics = MetricsCapture::start();
            let logs = capture_logs(|| count_ipmi_command(case.command, &case.result));

            assert_eq!(
                metrics.counter_delta(
                    "carbide_ipmi_commands_total",
                    &[
                        ("command", case.expected_command),
                        ("outcome", case.expected_outcome),
                    ],
                ),
                1.0,
                "{}",
                case.name,
            );
            assert!(
                logs.is_empty(),
                "{}: the event is metric-only, but logged {logs:?}",
                case.name,
            );
        }
    }
}

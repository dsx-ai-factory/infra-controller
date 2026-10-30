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

// Queries in this module are large enough that it makes sense to have them as separate files rather
// than huge inline rust strings.

use lazy_static::lazy_static;

static MANAGED_HOSTS_TEMPLATE: &str = include_str!("sql/managed_hosts.sql.template");
static MANAGED_HOSTS_COMPACT_TEMPLATE: &str =
    include_str!("sql/managed_hosts_compact.sql.template");
static MACHINE_SNAPSHOTS_TEMPLATE: &str = include_str!("sql/machine_snapshots.sql.template");
static MANAGED_HOST_HISTORY_JOIN_SNIPPET: &str =
    include_str!("sql/managed_host_history_join.snippet");
static MACHINE_SNAPSHOT_HISTORY_JOIN_SNIPPET: &str =
    include_str!("sql/machine_snapshot_history_join.snippet");
static HISTORY_SELECT_SNIPPET: &str = include_str!("sql/history_select.snippet");

fn replace_sql<F: Fn(&str) -> Option<&str>>(input: &str, f: F) -> String {
    input
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with("--") {
                return None;
            }
            f(trimmed)
        })
        .collect::<Vec<_>>()
        .join(" ")
}

lazy_static! {
    pub static ref MANAGED_HOSTS_COMPACT: String =
        replace_sql(MANAGED_HOSTS_COMPACT_TEMPLATE, |line| Some(line));
    pub static ref MANAGED_HOSTS_WITH_HISTORY: String =
        replace_sql(MANAGED_HOSTS_TEMPLATE, |line| {
            match line {
                "__HISTORY_SELECT__" => Some(HISTORY_SELECT_SNIPPET),
                "__HISTORY_JOIN__" => Some(MANAGED_HOST_HISTORY_JOIN_SNIPPET),
                default => Some(default),
            }
        });
    pub static ref MANAGED_HOSTS_NO_HISTORY: String = replace_sql(MANAGED_HOSTS_TEMPLATE, |line| {
        match line {
            "__HISTORY_SELECT__" => None,
            "__HISTORY_JOIN__" => None,
            default => Some(default),
        }
    });
    pub static ref MACHINE_SNAPSHOTS_WITH_HISTORY: String =
        replace_sql(MACHINE_SNAPSHOTS_TEMPLATE, |line| {
            match line {
                "__HISTORY_SELECT__" => Some(HISTORY_SELECT_SNIPPET),
                "__HISTORY_JOIN__" => Some(MACHINE_SNAPSHOT_HISTORY_JOIN_SNIPPET),
                default => Some(default),
            }
        });
    pub static ref MACHINE_SNAPSHOTS_NO_HISTORY: String =
        replace_sql(MACHINE_SNAPSHOTS_TEMPLATE, |line| {
            match line {
                "__HISTORY_SELECT__" => None,
                "__HISTORY_JOIN__" => None,
                default => Some(default),
            }
        });
}

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;

    use super::*;

    /// The compact query keeps the allocation and health inputs and leaves out
    /// the heavy parts of the full query.
    #[test]
    fn compact_template_shape() {
        value_scenarios!(
            run = |fragment: &str| (
                MANAGED_HOSTS_COMPACT.contains(fragment),
                MANAGED_HOSTS_NO_HISTORY.contains(fragment)
            );
            "kept" {
                "OFFSET 0" => (true, true),
                "LEFT JOIN machine_boot_interfaces boot_interface" => (true, true),
                "INNER JOIN network_segments ns" => (true, true),
                "AND bmc_i.interface_type = 'Bmc'" => (true, true),
                "FROM racks rack" => (true, true),
                "mi_parent.attached_dpu_machine_id" => (true, true),
            }
            "left out" {
                "machine_nic_firmware_overrides" => (false, true),
                "power_options" => (false, true),
                "machine_skus" => (false, true),
                "dhcp_entries" => (false, true),
                "'topology', mt2.topology" => (false, true),
                "SELECT json_agg(a2.address)" => (false, true),
            }
        );
    }
}

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

use std::collections::BTreeMap;
use std::net::IpAddr;

use carbide_uuid::machine::HostMachineId;
use config_version::ConfigVersion;
use sqlx::FromRow;
use sqlx::types::Json;

use crate::site_explorer::Service;

#[derive(Debug, FromRow)]
pub struct HostMachineUpdate {
    pub id: HostMachineId,
}

/// One BMC address row of a host's firmware update gates. The boolean columns
/// are the `find_upgrade_needed` predicates, each evaluated on its own so a
/// reader can name the one that stops the host.
#[derive(Debug, Clone, FromRow)]
pub struct HostUpdateGateRow {
    pub host_id: HostMachineId,
    pub bmc_address: Option<IpAddr>,
    pub report_version: Option<ConfigVersion>,
    pub waiting_for_explorer_refresh: Option<bool>,
    pub report_vendor: Option<String>,
    pub report_model: Option<String>,
    pub report_versions: Option<Json<BTreeMap<String, String>>>,
    pub report_error: Option<Json<serde_json::Value>>,
    pub report_service: Option<Json<Vec<Service>>>,
    pub has_target: bool,
    pub stable_host: bool,
    /// `None` when the report or the target has no versions to compare.
    pub versions_differ: Option<bool>,
    pub no_open_request: bool,
    pub state_ok: bool,
    pub autoupdate_on: bool,
    pub window_ok: bool,
}

impl HostUpdateGateRow {
    /// The explored report's vendor. `None` without a report or when unknown.
    pub fn vendor(&self) -> Option<bmc_vendor::BMCVendor> {
        let vendor = serde_json::from_value(self.report_vendor.clone()?.into()).ok()?;
        (vendor != bmc_vendor::BMCVendor::Unknown).then_some(vendor)
    }

    /// Whether `find_upgrade_needed` returns this row.
    pub fn passes_manager_query(&self) -> bool {
        self.first_closed_gate(None).is_none()
    }

    /// The first gate this row fails, in the manager's predicate order. The
    /// host's own `firmware_autoupdate` flag only names the autoupdate gate.
    fn first_closed_gate(&self, firmware_autoupdate: Option<bool>) -> Option<UpdateBlockingReason> {
        use UpdateBlockingReason::*;
        [
            (self.stable_host, NotStableHost),
            (self.report_version.is_some(), NoExploredReport),
            (self.has_target, NoFirmwareTarget),
            (self.versions_differ.is_some(), FirmwareVersionsUnavailable),
            (self.versions_differ == Some(true), FirmwareCurrent),
            (self.no_open_request, RequestOpen),
            (self.state_ok, NotReady),
            (self.autoupdate_on, autoupdate_off(firmware_autoupdate)),
            (self.window_ok, OutsideUpdateWindow),
        ]
        .into_iter()
        .find_map(|(open, reason)| (!open).then_some(reason))
    }
}

/// Why the update manager would not select a host, or why firmware policy
/// forbids automatic updates on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdateBlockingReason {
    NotStableHost,
    NoExploredReport,
    NoFirmwareTarget,
    FirmwareVersionsUnavailable,
    FirmwareCurrent,
    RequestOpen,
    NotReady,
    AutoupdateOffHost,
    AutoupdateOffGlobal,
    OutsideUpdateWindow,
    DisableList,
}

impl UpdateBlockingReason {
    /// The stable snake_case code reported by the API.
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotStableHost => "not_stable_host",
            Self::NoExploredReport => "no_explored_report",
            Self::NoFirmwareTarget => "no_firmware_target",
            Self::FirmwareVersionsUnavailable => "firmware_versions_unavailable",
            Self::FirmwareCurrent => "firmware_current",
            Self::RequestOpen => "request_open",
            Self::NotReady => "not_ready",
            Self::AutoupdateOffHost => "autoupdate_off_host",
            Self::AutoupdateOffGlobal => "autoupdate_off_global",
            Self::OutsideUpdateWindow => "outside_update_window",
            Self::DisableList => "disable_list",
        }
    }
}

fn autoupdate_off(firmware_autoupdate: Option<bool>) -> UpdateBlockingReason {
    if firmware_autoupdate == Some(false) {
        UpdateBlockingReason::AutoupdateOffHost
    } else {
        UpdateBlockingReason::AutoupdateOffGlobal
    }
}

/// `Ok` when the update manager's next pass with spare capacity would select
/// the host. This is the per-host rule, so the manager's early stop at a
/// disable-listed host is not modeled. If no row passes, the reason is the
/// first closed gate of the row for `bmc_ip`, else of the lowest address.
pub fn host_update_eligibility(
    rows: &[HostUpdateGateRow],
    bmc_ip: Option<IpAddr>,
    firmware_autoupdate: Option<bool>,
    on_disable_list: bool,
) -> Result<(), UpdateBlockingReason> {
    if rows.iter().any(HostUpdateGateRow::passes_manager_query) {
        return if on_disable_list {
            Err(UpdateBlockingReason::DisableList)
        } else {
            Ok(())
        };
    }
    let row = rows
        .iter()
        .find(|row| bmc_ip.is_some() && row.bmc_address == bmc_ip)
        .or_else(|| rows.iter().min_by_key(|row| row.bmc_address));
    Err(row
        .and_then(|row| row.first_closed_gate(firmware_autoupdate))
        .unwrap_or(UpdateBlockingReason::NoExploredReport))
}

/// Whether firmware policy allows automatic updates on the host: its own flag
/// or the global default, and not on the disable list. Lifecycle state,
/// versions and the update window are ignored. `None` without a gate row.
pub fn firmware_policy(
    rows: &[HostUpdateGateRow],
    firmware_autoupdate: Option<bool>,
    on_disable_list: bool,
) -> Option<Result<(), UpdateBlockingReason>> {
    let row = rows.first()?;
    Some(if !row.autoupdate_on {
        Err(autoupdate_off(firmware_autoupdate))
    } else if on_disable_list {
        Err(UpdateBlockingReason::DisableList)
    } else {
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;

    use super::*;
    use crate::test_support::machine_snapshot::host_machine_id;

    fn open_row(bmc_address: [u8; 4]) -> HostUpdateGateRow {
        HostUpdateGateRow {
            host_id: (&host_machine_id()).into(),
            bmc_address: Some(IpAddr::from(bmc_address)),
            report_version: Some(ConfigVersion::initial()),
            waiting_for_explorer_refresh: Some(false),
            report_vendor: Some("Dell".to_string()),
            report_model: Some("PowerEdge R750".to_string()),
            report_versions: None,
            report_error: None,
            report_service: None,
            has_target: true,
            stable_host: true,
            versions_differ: Some(true),
            no_open_request: true,
            state_ok: true,
            autoupdate_on: true,
            window_ok: true,
        }
    }

    /// A row whose gates are closed from `first` on, in manager order.
    fn row_closed_from(first: usize, bmc_address: [u8; 4]) -> HostUpdateGateRow {
        let closers: [fn(&mut HostUpdateGateRow); 9] = [
            |row| row.stable_host = false,
            |row| row.report_version = None,
            |row| row.has_target = false,
            |row| row.versions_differ = None,
            |row| row.versions_differ = Some(false),
            |row| row.no_open_request = false,
            |row| row.state_ok = false,
            |row| row.autoupdate_on = false,
            |row| row.window_ok = false,
        ];
        let mut row = open_row(bmc_address);
        // Later gates first, so the earlier closer of a shared column wins.
        closers[first..]
            .iter()
            .rev()
            .for_each(|close| close(&mut row));
        row
    }

    #[test]
    fn eligibility_reason_follows_manager_order() {
        value_scenarios!(
            run = |(first, firmware_autoupdate, on_disable_list)| {
                let rows = [row_closed_from(first, [10, 0, 0, 1])];
                host_update_eligibility(&rows, None, firmware_autoupdate, on_disable_list)
                    .map_err(|reason| reason.code())
            };
            "the first closed gate names the reason" {
                (0, None, false) => Err("not_stable_host"),
                (1, None, false) => Err("no_explored_report"),
                (2, None, false) => Err("no_firmware_target"),
                (3, None, false) => Err("firmware_versions_unavailable"),
                (4, None, false) => Err("firmware_current"),
                (5, None, false) => Err("request_open"),
                (6, None, false) => Err("not_ready"),
                (7, Some(false), false) => Err("autoupdate_off_host"),
                (7, None, false) => Err("autoupdate_off_global"),
                (8, None, true) => Err("outside_update_window"),
            }
            "a passing row is eligible unless the host is disable-listed" {
                (9, None, false) => Ok(()),
                (9, Some(true), true) => Err("disable_list"),
            }
        );
    }

    #[test]
    fn eligibility_prefers_row_for_snapshot_bmc_ip() {
        let [low, high] = [[10, 0, 0, 1], [10, 0, 0, 2]];
        value_scenarios!(
            run = |(rows, bmc_ip): (Vec<HostUpdateGateRow>, Option<[u8; 4]>)| {
                host_update_eligibility(&rows, bmc_ip.map(IpAddr::from), None, false)
                    .map_err(|reason| reason.code())
            };
            "the snapshot BMC IP row names the reason" {
                (vec![row_closed_from(8, low), row_closed_from(1, high)], Some(high)) =>
                    Err("no_explored_report"),
            }
            "without a matching row the lowest address names it" {
                (vec![row_closed_from(1, high), row_closed_from(8, low)], None) =>
                    Err("outside_update_window"),
                (vec![row_closed_from(1, high), row_closed_from(8, low)], Some([10, 0, 0, 3])) =>
                    Err("outside_update_window"),
            }
            "any passing row makes the host eligible" {
                (vec![row_closed_from(8, low), open_row(high)], Some(low)) => Ok(()),
            }
            "a host without rows has no report" {
                (vec![], None) => Err("no_explored_report"),
            }
        );
    }

    #[test]
    fn policy_ignores_state_versions_and_window() {
        let disabled = HostUpdateGateRow {
            autoupdate_on: false,
            window_ok: false,
            ..open_row([10, 0, 0, 1])
        };
        let current = HostUpdateGateRow {
            versions_differ: Some(false),
            state_ok: false,
            window_ok: false,
            ..open_row([10, 0, 0, 1])
        };
        value_scenarios!(
            run = |(rows, firmware_autoupdate, on_disable_list): (Vec<HostUpdateGateRow>, _, _)| {
                firmware_policy(&rows, firmware_autoupdate, on_disable_list)
                    .map(|policy| policy.map_err(|reason| reason.code()))
            };
            "autoupdate off blocks even with a closed window" {
                (vec![disabled.clone()], Some(false), true) => Some(Err("autoupdate_off_host")),
                (vec![disabled], None, false) => Some(Err("autoupdate_off_global")),
            }
            "autoupdate on allows a current, not ready host" {
                (vec![current.clone()], Some(true), false) => Some(Ok(())),
                (vec![current], None, true) => Some(Err("disable_list")),
            }
            "no gate row leaves the policy unknown" {
                (vec![], None, false) => None,
            }
        );
    }
}

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

//! What the probe knows about one host. The compact read
//! (FindHostStatesByIds) gives allocatability, update eligibility and the
//! explored report. The full machine record (FindMachinesByIds), on a
//! nico-api without the compact read, gives model and versions from DMI and
//! the BMC record and none of the rest: those are `Unknown`, published as
//! `unknown` and `null`, never guessed.

use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};

use bmc_vendor::BMCVendor;
use carbide_site_health_probe_api::VersionSource;
use carbide_uuid::machine::MachineId;
use chrono::{DateTime, Utc};
use rpc::forge;

use crate::config::HostReadMode;
use crate::gate::CallError;

/// A value the current host read may not be able to supply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Known<T> {
    Value(T),
    /// Why the value is unknown.
    Unknown(&'static str),
}

/// Why allocatability and update eligibility are unknown under the full read.
pub(crate) const COMPACT_HOST_READ_MISSING: &str = "compact_host_read_missing";

/// Why a host's allocatability or update eligibility is unknown under the
/// compact read: its row did not carry it.
pub(crate) const HOST_STATE_INCOMPLETE: &str = "host_state_incomplete";

/// Blocking reasons and error types become label or field values, so any
/// code outside the known set is published as this.
const OTHER: &str = "other";

/// `NotAllocatableReason::code()` in nico-api.
const ALLOCATION_REASONS: [&str; 8] = [
    "invalid_state",
    "pending_instance_creation",
    "pending_decommission",
    "pending_boot_configuration",
    "dpu_reprovisioning_started",
    "no_dpu_snapshots",
    "maintenance_mode",
    "health_alert",
];

/// `UpdateBlockingReason::code()` in nico-api.
const UPDATE_REASONS: [&str; 11] = [
    "not_stable_host",
    "no_explored_report",
    "no_firmware_target",
    "firmware_versions_unavailable",
    "firmware_current",
    "request_open",
    "not_ready",
    "autoupdate_off_host",
    "autoupdate_off_global",
    "outside_update_window",
    "disable_list",
];

/// `EndpointExplorationError` variant names, its serde `Type` tag. The
/// explored report's last error is that JSON, with BMC text and response
/// bodies in it, so only the tag is published.
const EXPLORATION_ERRORS: [&str; 16] = [
    "ConnectionTimeout",
    "ConnectionRefused",
    "Unreachable",
    "UnsupportedVendor",
    "RedfishError",
    "Unauthorized",
    "MissingCredentials",
    "SecretsEngineError",
    "SetCredentials",
    "MissingRedfish",
    "MissingVendor",
    "AvoidLockout",
    "Other",
    "VikingFWInventoryForbiddenError",
    "InvalidDpuRedfishBiosResponse",
    "IntermittentUnauthorized",
];

/// One host as the device reader sees it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct HostDetail {
    pub id: MachineId,
    /// The `ManagedHostState` Display string.
    pub state: String,
    /// The last state handler outcome message.
    pub state_reason: Option<String>,
    pub vendor: Option<String>,
    pub model: Option<String>,
    pub instance_type: Option<String>,
    pub bmc_ip: Option<String>,
    pub rack_id: Option<String>,
    pub nvlink_domain: Option<String>,
    pub dpu_ids: Vec<MachineId>,
    pub update_complete: bool,
    pub firmware_autoupdate: Option<bool>,
    pub bmc_version: Option<String>,
    pub uefi_version: Option<String>,
    pub version_source: VersionSource,
    pub report_at: Option<DateTime<Utc>>,
    pub report_error: Option<String>,
    /// `Err` carries the blocking reason.
    pub allocatable: Known<Result<(), String>>,
    /// `Err` carries the blocking reason.
    pub update_eligible: Known<Result<(), String>>,
}

/// Sweeps `auto` reads in full after nico-api denies the compact read, before
/// it tries the compact read again.
pub(crate) const RETRY_COMPACT_AFTER_SWEEPS: u32 = 10;

/// What `auto` last announced about the compact read.
const NOT_FALLEN_BACK: u8 = 0;
const FELL_BACK_DENIED: u8 = 1;
const FELL_BACK_UNIMPLEMENTED: u8 = 2;

/// Which read supplies each sweep's host details. `auto` starts on the
/// compact read. It moves to the full read for good when nico-api answers
/// UNIMPLEMENTED. PERMISSION_DENIED is what an older nico-api's RBAC answers
/// for a method it has no rule for, but also what a rolling upgrade or an
/// identity hiccup answers, so it moves to the full read for
/// [`RETRY_COMPACT_AFTER_SWEEPS`] sweeps and then tries the compact read
/// again.
pub(crate) struct HostRead {
    mode: HostReadMode,
    unimplemented: AtomicBool,
    /// Full read sweeps left before the compact read is tried again.
    denied_sweeps: AtomicU32,
    announced: AtomicU8,
}

impl HostRead {
    pub(crate) fn new(mode: HostReadMode) -> Self {
        Self {
            mode,
            unimplemented: AtomicBool::new(false),
            denied_sweeps: AtomicU32::new(0),
            announced: AtomicU8::new(NOT_FALLEN_BACK),
        }
    }

    /// Whether the next sweep starts on the compact read.
    pub(crate) fn begin_sweep(&self) -> bool {
        match self.mode {
            HostReadMode::Full => false,
            HostReadMode::Compact => true,
            HostReadMode::Auto => {
                !self.unimplemented.load(Ordering::SeqCst)
                    && self
                        .denied_sweeps
                        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |left| {
                            left.checked_sub(1)
                        })
                        .is_err()
            }
        }
    }

    /// Whether a failed compact read is read again in full: under `auto`,
    /// when nico-api does not serve the RPC. Any other error, and any error
    /// under `compact`, is the call's own failure.
    pub(crate) fn fall_back(&self, err: &CallError) -> bool {
        if self.mode != HostReadMode::Auto || !err.is_unserved_rpc() {
            return false;
        }
        let state = if err.is_unimplemented() {
            self.unimplemented.store(true, Ordering::SeqCst);
            FELL_BACK_UNIMPLEMENTED
        } else {
            self.denied_sweeps
                .store(RETRY_COMPACT_AFTER_SWEEPS, Ordering::SeqCst);
            FELL_BACK_DENIED
        };
        // Loud on purpose, and once per change: a misconfigured identity on
        // a nico-api that has the RPC also answers PERMISSION_DENIED.
        if self.announced.swap(state, Ordering::SeqCst) != state {
            let code = err.category();
            if state == FELL_BACK_UNIMPLEMENTED {
                tracing::warn!(
                    code = %code,
                    "FindHostStatesByIds is not served, falling back to full host read until the \
                     probe restarts"
                );
            } else {
                tracing::warn!(
                    code = %code,
                    retry_after_sweeps = RETRY_COMPACT_AFTER_SWEEPS,
                    "FindHostStatesByIds is denied, falling back to full host read and trying the \
                     compact read again later"
                );
            }
        }
        true
    }

    /// A sweep read every host batch compact, so nico-api serves the RPC
    /// again.
    pub(crate) fn served(&self) {
        if self.announced.swap(NOT_FALLEN_BACK, Ordering::SeqCst) != NOT_FALLEN_BACK {
            tracing::info!("FindHostStatesByIds is served again, reading hosts compact");
        }
    }
}

/// Maps a FindMachinesByIds row. Model and versions come from the machine
/// record, not the explored report.
pub(crate) fn full_host_detail(machine: forge::Machine) -> Result<HostDetail, CallError> {
    let id = machine
        .id
        .ok_or_else(|| CallError::Decode("machine without an ID".to_string()))?;
    let config = machine.config.unwrap_or_default();
    let status = machine.status.unwrap_or_default();
    let dmi = status
        .discovery_info
        .and_then(|info| info.dmi_data)
        .unwrap_or_default();
    let bmc_ip = match machine.bmc {
        Some(bmc) => bmc.ip,
        #[allow(deprecated)] // The probe can connect to older Core versions.
        None => machine.bmc_info.as_ref().and_then(|bmc| bmc.ip.clone()),
    };
    let bmc_version = match status.bmc_status {
        Some(bmc) => bmc.firmware_version,
        #[allow(deprecated)] // The probe can connect to older Core versions.
        None => machine.bmc_info.and_then(|bmc| bmc.firmware_version),
    };
    Ok(HostDetail {
        id,
        state: machine.state,
        state_reason: machine
            .state_reason
            .and_then(|reason| reason.outcome_msg)
            .filter(|msg| !msg.is_empty()),
        // Some Dell hosts spell DMI's vendor `Dell, Inc.`.
        vendor: vendor(BMCVendor::from_udev_dmi(&dmi.sys_vendor.replace(", ", " "))),
        model: non_empty(dmi.product_name),
        instance_type: config.instance_type_id.filter(|id| !id.is_empty()),
        bmc_ip: bmc_ip.filter(|ip| !ip.is_empty()),
        rack_id: machine.rack_id.map(|rack| rack.to_string()),
        nvlink_domain: status
            .nvlink_info
            .and_then(|info| info.domain_uuid)
            .map(|domain| domain.to_string()),
        dpu_ids: status
            .associated_dpu_machine_ids
            .into_iter()
            .map(MachineId::from)
            .collect(),
        update_complete: status.update_complete,
        firmware_autoupdate: config.firmware_autoupdate,
        bmc_version: bmc_version.filter(|v| !v.is_empty()),
        uefi_version: non_empty(dmi.bios_version),
        version_source: VersionSource::MachineRecord,
        report_at: None,
        report_error: None,
        allocatable: Known::Unknown(COMPACT_HOST_READ_MISSING),
        update_eligible: Known::Unknown(COMPACT_HOST_READ_MISSING),
    })
}

/// Maps a FindHostStatesByIds row. Model and versions come from the explored
/// report at the host's BMC address, and are absent without one.
pub(crate) fn compact_host_detail(host: forge::HostState) -> Result<HostDetail, CallError> {
    let id = host
        .id
        .ok_or_else(|| CallError::Decode("host state without an ID".to_string()))?;
    let explored = host.explored.unwrap_or_default();
    let flag = |ok: bool, code: Option<String>, known: &[&str]| {
        Known::Value(if ok {
            Ok(())
        } else {
            Err(bounded(code, known))
        })
    };
    Ok(HostDetail {
        id,
        state: host.state,
        state_reason: host
            .state_reason
            .and_then(|reason| reason.outcome_msg)
            .filter(|msg| !msg.is_empty()),
        vendor: explored
            .vendor
            .and_then(|vendor| self::vendor(BMCVendor::from(vendor.as_str()))),
        model: explored.model.filter(|m| !m.is_empty()),
        instance_type: host.instance_type_id.filter(|id| !id.is_empty()),
        bmc_ip: host.bmc_ip.filter(|ip| !ip.is_empty()),
        rack_id: host.rack_id.map(|rack| rack.to_string()),
        nvlink_domain: host.nvlink_domain_id.map(|domain| domain.to_string()),
        dpu_ids: host.dpu_ids.into_iter().map(MachineId::from).collect(),
        update_complete: host.update_complete,
        firmware_autoupdate: host.firmware_autoupdate,
        bmc_version: explored.bmc_version.filter(|v| !v.is_empty()),
        uefi_version: explored.uefi_version.filter(|v| !v.is_empty()),
        version_source: VersionSource::ExploredReport,
        report_at: explored.report_at.and_then(timestamp),
        report_error: explored
            .last_exploration_error
            .filter(|error| !error.is_empty())
            .map(|error| exploration_error_type(&error)),
        // nico-api sends both, so a row without one is a version skew. Its
        // host has no answer, and the rest of the batch is unharmed.
        allocatable: match host.allocation {
            Some(allocation) => flag(
                allocation.allocatable,
                allocation.blocking_reason,
                &ALLOCATION_REASONS,
            ),
            None => Known::Unknown(HOST_STATE_INCOMPLETE),
        },
        update_eligible: match host.update {
            Some(update) => flag(update.eligible, update.blocking_reason, &UPDATE_REASONS),
            None => Known::Unknown(HOST_STATE_INCOMPLETE),
        },
    })
}

/// The time of `ts`, or `None` outside the range a date can hold, with one
/// debug line the first time. `DateTime::try_from` panics on such a time.
pub(crate) fn timestamp(ts: rpc::Timestamp) -> Option<DateTime<Utc>> {
    static LOGGED: AtomicBool = AtomicBool::new(false);
    let time = u32::try_from(ts.nanos)
        .ok()
        .and_then(|nanos| DateTime::from_timestamp(ts.seconds, nanos));
    if time.is_none() && !LOGGED.swap(true, Ordering::Relaxed) {
        tracing::debug!("a nico-api time out of range, read as no time");
    }
    time
}

/// The `Type` tag of an exploration error's JSON when it names a known
/// variant, else [`OTHER`]. Nothing else of the error is kept.
fn exploration_error_type(error: &str) -> String {
    let tag = serde_json::from_str::<serde_json::Value>(error)
        .ok()
        .and_then(|error| error.get("Type")?.as_str().map(str::to_string));
    bounded(tag, &EXPLORATION_ERRORS)
}

/// A host's maker in nico-api's form, `BMCVendor`'s lowercase name, as the
/// compact read sends it. Only makers the full read's DMI vendor can name
/// are kept, and a Lenovo AMI BMC is a Lenovo host, so both reads give a
/// host one value. Any other vendor is none under both.
fn vendor(vendor: BMCVendor) -> Option<String> {
    let maker = match vendor {
        BMCVendor::Lenovo | BMCVendor::LenovoAMI => BMCVendor::Lenovo,
        maker @ (BMCVendor::GigaComputing
        | BMCVendor::Dell
        | BMCVendor::Supermicro
        | BMCVendor::Hpe
        | BMCVendor::Nvidia) => maker,
        BMCVendor::Liteon | BMCVendor::Delta | BMCVendor::Sushy | BMCVendor::Unknown => {
            return None;
        }
    };
    Some(maker.to_string())
}

/// `code` when nico-api documents it, else [`OTHER`].
fn bounded(code: Option<String>, known: &[&str]) -> String {
    code.filter(|code| known.contains(&code.as_str()))
        .unwrap_or_else(|| OTHER.to_string())
}

fn non_empty(value: String) -> Option<String> {
    (!value.is_empty()).then_some(value)
}

#[cfg(test)]
mod tests {
    use carbide_test_support::{Check, check_values};
    use carbide_uuid::machine::{MachineIdSource, MachineType};

    use super::*;

    #[test]
    fn full_host_read_reports_unknown_allocatable() {
        let id = |byte, machine_type| {
            MachineId::new(
                MachineIdSource::ProductBoardChassisSerial,
                [byte; 32],
                machine_type,
            )
        };
        let host = id(1, MachineType::Host);
        let dpu = id(2, MachineType::Dpu);
        let machine = forge::Machine {
            id: Some(host),
            state: "Ready".to_string(),
            state_reason: Some(forge::ControllerStateReason {
                outcome_msg: Some("all good".to_string()),
                ..Default::default()
            }),
            bmc: Some(forge::BmcEndpoint {
                ip: Some("10.0.0.5".to_string()),
                ..Default::default()
            }),
            config: Some(forge::MachineConfig {
                instance_type_id: Some("it-1".to_string()),
                firmware_autoupdate: Some(true),
                ..Default::default()
            }),
            status: Some(forge::MachineStatus {
                bmc_status: Some(forge::BmcStatus {
                    firmware_version: Some("25.06-2".to_string()),
                    ..Default::default()
                }),
                discovery_info: Some(rpc::machine_discovery::DiscoveryInfo {
                    dmi_data: Some(rpc::machine_discovery::DmiData {
                        sys_vendor: "NVIDIA".to_string(),
                        product_name: "GB200 NVL".to_string(),
                        bios_version: "00000083".to_string(),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                associated_dpu_machine_ids: vec![dpu.try_into().expect("dpu id")],
                update_complete: true,
                ..Default::default()
            }),
            ..Default::default()
        };
        let detail = full_host_detail(machine).expect("maps");
        assert_eq!(detail.id, host);
        assert_eq!(detail.state_reason.as_deref(), Some("all good"));
        assert_eq!(detail.vendor.as_deref(), Some("nvidia"));
        assert_eq!(detail.model.as_deref(), Some("GB200 NVL"));
        assert_eq!(detail.instance_type.as_deref(), Some("it-1"));
        assert_eq!(detail.bmc_ip.as_deref(), Some("10.0.0.5"));
        assert_eq!(detail.bmc_version.as_deref(), Some("25.06-2"));
        assert_eq!(detail.uefi_version.as_deref(), Some("00000083"));
        assert_eq!(detail.version_source, VersionSource::MachineRecord);
        assert_eq!(detail.dpu_ids, vec![dpu]);
        assert!(detail.update_complete);
        assert_eq!(detail.firmware_autoupdate, Some(true));
        assert_eq!(detail.report_at, None);
        // The full read cannot see allocatability or update eligibility.
        assert_eq!(
            detail.allocatable,
            Known::Unknown(COMPACT_HOST_READ_MISSING)
        );
        assert_eq!(
            detail.update_eligible,
            Known::Unknown(COMPACT_HOST_READ_MISSING)
        );

        let err = full_host_detail(forge::Machine {
            id: None,
            ..Default::default()
        })
        .expect_err("a row without an ID");
        assert!(matches!(err, CallError::Decode(_)));
    }

    #[test]
    #[allow(deprecated)] // Exercise responses from Core before the BMC split.
    fn full_host_read_bmc_message_precedence() {
        let endpoint = forge::BmcEndpoint {
            ip: Some("192.0.2.10".to_string()),
            ..Default::default()
        };
        let status = forge::BmcStatus {
            firmware_version: Some("new-version".to_string()),
            ..Default::default()
        };
        let legacy = forge::BmcInfo {
            ip: Some("192.0.2.20".to_string()),
            firmware_version: Some("old-version".to_string()),
            ..Default::default()
        };
        check_values(
            [
                Check {
                    scenario: "split messages override legacy values",
                    input: (
                        Some(endpoint.clone()),
                        Some(status.clone()),
                        Some(legacy.clone()),
                    ),
                    expect: (
                        Some("192.0.2.10".to_string()),
                        Some("new-version".to_string()),
                    ),
                },
                Check {
                    scenario: "missing split status falls back independently",
                    input: (Some(endpoint), None, Some(legacy.clone())),
                    expect: (
                        Some("192.0.2.10".to_string()),
                        Some("old-version".to_string()),
                    ),
                },
                Check {
                    scenario: "missing split endpoint falls back independently",
                    input: (None, Some(status), Some(legacy.clone())),
                    expect: (
                        Some("192.0.2.20".to_string()),
                        Some("new-version".to_string()),
                    ),
                },
                Check {
                    scenario: "present empty split messages are authoritative",
                    input: (
                        Some(forge::BmcEndpoint::default()),
                        Some(forge::BmcStatus::default()),
                        Some(legacy.clone()),
                    ),
                    expect: (None, None),
                },
                Check {
                    scenario: "legacy-only Core response",
                    input: (None, None, Some(legacy)),
                    expect: (
                        Some("192.0.2.20".to_string()),
                        Some("old-version".to_string()),
                    ),
                },
                Check {
                    scenario: "all BMC information is absent",
                    input: (None, None, None),
                    expect: (None, None),
                },
            ],
            |(bmc, bmc_status, bmc_info)| {
                let detail = full_host_detail(forge::Machine {
                    id: Some(MachineId::new(
                        MachineIdSource::ProductBoardChassisSerial,
                        [1; 32],
                        MachineType::Host,
                    )),
                    bmc,
                    bmc_info,
                    status: Some(forge::MachineStatus {
                        bmc_status,
                        ..Default::default()
                    }),
                    ..Default::default()
                })
                .expect("maps");
                (detail.bmc_ip, detail.bmc_version)
            },
        );
    }

    fn host_id() -> MachineId {
        MachineId::new(
            MachineIdSource::ProductBoardChassisSerial,
            [1; 32],
            MachineType::Host,
        )
    }

    fn report_at() -> DateTime<Utc> {
        DateTime::from_timestamp(1_800_000_000, 0).expect("valid time")
    }

    /// A compact row of a Ready host that cannot take an update.
    fn compact_row() -> forge::HostState {
        forge::HostState {
            id: Some(host_id()),
            state: "Ready".to_string(),
            instance_type_id: Some("it-1".to_string()),
            bmc_ip: Some("10.0.0.5".to_string()),
            update_complete: true,
            firmware_autoupdate: Some(false),
            allocation: Some(forge::HostAllocation {
                allocatable: true,
                ..Default::default()
            }),
            explored: Some(forge::HostExploredFirmware {
                address: "10.0.0.5".to_string(),
                vendor: Some("nvidia".to_string()),
                model: Some("GB200 NVL".to_string()),
                bmc_version: Some("25.06-2".to_string()),
                uefi_version: Some("00000083".to_string()),
                report_at: Some(report_at().into()),
                ..Default::default()
            }),
            update: Some(forge::HostUpdateEligibility {
                eligible: false,
                blocking_reason: Some("firmware_current".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn compact_read_fills_allocatable_and_eligibility() {
        let detail = compact_host_detail(compact_row()).expect("maps");
        assert_eq!(detail.id, host_id());
        assert_eq!(detail.allocatable, Known::Value(Ok(())));
        assert_eq!(
            detail.update_eligible,
            Known::Value(Err("firmware_current".to_string()))
        );
        assert_eq!(detail.version_source, VersionSource::ExploredReport);
        assert_eq!(detail.vendor.as_deref(), Some("nvidia"));
        assert_eq!(detail.model.as_deref(), Some("GB200 NVL"));
        assert_eq!(detail.bmc_version.as_deref(), Some("25.06-2"));
        assert_eq!(detail.uefi_version.as_deref(), Some("00000083"));
        assert_eq!(detail.report_at, Some(report_at()));
        assert_eq!(detail.instance_type.as_deref(), Some("it-1"));
        assert_eq!(detail.bmc_ip.as_deref(), Some("10.0.0.5"));
        assert!(detail.update_complete);
        assert_eq!(detail.firmware_autoupdate, Some(false));

        // Without a report there are no versions, and they are still not
        // the machine record's.
        let mut row = compact_row();
        row.explored = None;
        let detail = compact_host_detail(row).expect("maps");
        assert_eq!((detail.model, detail.report_at), (None, None));
        assert_eq!(detail.version_source, VersionSource::ExploredReport);

        let mut row = compact_row();
        row.id = None;
        assert!(matches!(
            compact_host_detail(row),
            Err(CallError::Decode(_))
        ));
    }

    /// One host is one vendor under both reads: DMI's `NVIDIA` and the
    /// compact read's `nvidia` are both `nvidia`, DMI's `Lenovo` and a Lenovo
    /// AMI BMC's `lenovoami` are both `lenovo`, so a vendor label or filter
    /// never splits a fleet by read. A vendor DMI cannot name is none under
    /// both.
    #[test]
    fn both_reads_give_one_vendor() {
        let full = |sys_vendor: &str| {
            full_host_detail(forge::Machine {
                id: Some(host_id()),
                status: Some(forge::MachineStatus {
                    discovery_info: Some(rpc::machine_discovery::DiscoveryInfo {
                        dmi_data: Some(rpc::machine_discovery::DmiData {
                            sys_vendor: sys_vendor.to_string(),
                            ..Default::default()
                        }),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            })
            .expect("maps")
            .vendor
        };
        let compact = |vendor: Option<&str>| {
            let mut row = compact_row();
            row.explored.as_mut().unwrap().vendor = vendor.map(str::to_string);
            compact_host_detail(row).expect("maps").vendor
        };
        for (dmi, nico, want) in [
            ("NVIDIA", Some("nvidia"), Some("nvidia")),
            ("https://www.mellanox.com", Some("nvidia"), Some("nvidia")),
            ("Dell Inc.", Some("dell"), Some("dell")),
            ("Dell, Inc.", Some("dell"), Some("dell")),
            ("Lenovo", Some("lenovo"), Some("lenovo")),
            ("Lenovo", Some("lenovoami"), Some("lenovo")),
            (
                "Giga Computing",
                Some("gigacomputing"),
                Some("gigacomputing"),
            ),
            ("Supermicro", Some("supermicro"), Some("supermicro")),
            ("HPE", Some("hpe"), Some("hpe")),
            // A virtual BMC: DMI names no maker, so neither read does.
            ("QEMU", Some("sushy"), None),
            ("Quanta", None, None),
            ("", Some("unknown"), None),
        ] {
            let want = want.map(str::to_string);
            assert_eq!(full(dmi), want, "full {dmi:?}");
            assert_eq!(compact(nico), want, "compact {nico:?}");
        }
    }

    /// A row without allocation or update eligibility leaves that host
    /// unknown, and keeps the other answer.
    #[test]
    fn incomplete_row_is_unknown_for_its_host_only() {
        let mut row = compact_row();
        row.allocation = None;
        let detail = compact_host_detail(row).expect("maps");
        assert_eq!(detail.allocatable, Known::Unknown(HOST_STATE_INCOMPLETE));
        assert_eq!(
            detail.update_eligible,
            Known::Value(Err("firmware_current".to_string()))
        );
        let mut row = compact_row();
        row.update = None;
        let detail = compact_host_detail(row).expect("maps");
        assert_eq!(detail.allocatable, Known::Value(Ok(())));
        assert_eq!(
            detail.update_eligible,
            Known::Unknown(HOST_STATE_INCOMPLETE)
        );
    }

    #[test]
    fn unknown_blocking_reason_maps_to_other() {
        check_values(
            [
                Check {
                    scenario: "a known allocation code",
                    input: ("allocation", Some("maintenance_mode")),
                    expect: "maintenance_mode".into(),
                },
                Check {
                    scenario: "a new allocation code",
                    input: ("allocation", Some("quarantined")),
                    expect: OTHER.into(),
                },
                Check {
                    scenario: "an update code is no allocation code",
                    input: ("allocation", Some("disable_list")),
                    expect: OTHER.into(),
                },
                Check {
                    scenario: "blocked without a code",
                    input: ("allocation", None),
                    expect: OTHER.into(),
                },
                Check {
                    scenario: "a known update code",
                    input: ("update", Some("outside_update_window")),
                    expect: "outside_update_window".into(),
                },
                Check {
                    scenario: "free text never becomes a label",
                    input: ("update", Some("BMC at 10.0.0.5 said no")),
                    expect: OTHER.into(),
                },
            ],
            |(field, code): (&str, Option<&str>)| {
                let mut row = compact_row();
                let code = code.map(str::to_string);
                if field == "allocation" {
                    let allocation = row.allocation.as_mut().unwrap();
                    allocation.allocatable = false;
                    allocation.blocking_reason = code;
                } else {
                    row.update.as_mut().unwrap().blocking_reason = code;
                }
                let detail = compact_host_detail(row).expect("maps");
                let known = if field == "allocation" {
                    detail.allocatable
                } else {
                    detail.update_eligible
                };
                match known {
                    Known::Value(Err(reason)) => reason,
                    other => panic!("{other:?}"),
                }
            },
        );
    }

    /// The published report error is the exploration error's type and
    /// nothing nico-api sent in it.
    #[test]
    fn report_error_is_the_error_type_only() {
        let hostile = serde_json::json!({
            "Type": "RedfishError",
            "Details": "GET https://10.0.0.5/redfish/v1 failed",
            "ResponseBody": "SENTINEL-body Authorization: Basic c2VjcmV0",
            "ResponseCode": 500,
        })
        .to_string();
        check_values(
            [
                Check {
                    scenario: "a known type loses its details and body",
                    input: hostile,
                    expect: Some("RedfishError".to_string()),
                },
                Check {
                    scenario: "a unit variant",
                    input: r#"{"Type":"AvoidLockout"}"#.to_string(),
                    expect: Some("AvoidLockout".to_string()),
                },
                Check {
                    scenario: "a type nico-api may add is other",
                    input: r#"{"Type":"BmcOnFire","Details":"SENTINEL"}"#.to_string(),
                    expect: Some(OTHER.to_string()),
                },
                Check {
                    scenario: "free text is other",
                    input: "Unauthorized at https://10.0.0.5 SENTINEL".to_string(),
                    expect: Some(OTHER.to_string()),
                },
                Check {
                    scenario: "a type that is not a string is other",
                    input: r#"{"Type":{"Details":"SENTINEL"}}"#.to_string(),
                    expect: Some(OTHER.to_string()),
                },
                Check {
                    scenario: "no error",
                    input: String::new(),
                    expect: None,
                },
            ],
            |error: String| {
                let mut row = compact_row();
                row.explored.as_mut().unwrap().last_exploration_error = Some(error);
                let reported = compact_host_detail(row).expect("maps").report_error;
                assert!(
                    !reported.as_deref().unwrap_or_default().contains("SENTINEL"),
                    "{reported:?}"
                );
                reported
            },
        );
    }

    /// A time a date cannot hold reads as no time. The first is past the
    /// date range but a valid system time, which is where
    /// `DateTime::try_from` panics.
    #[test]
    fn out_of_range_time_is_none() {
        for (seconds, nanos) in [(9_000_000_000_000, 0), (i64::MAX, 0), (0, -1)] {
            let mut ts: rpc::Timestamp = report_at().into();
            ts.seconds = seconds;
            ts.nanos = nanos;
            assert_eq!(timestamp(ts), None, "{seconds} {nanos}");
        }
        let mut row = compact_row();
        row.explored
            .as_mut()
            .unwrap()
            .report_at
            .as_mut()
            .unwrap()
            .seconds = 9_000_000_000_000;
        assert_eq!(compact_host_detail(row).expect("maps").report_at, None);
    }

    fn denied() -> CallError {
        CallError::Status("gRPC PermissionDenied".to_string())
    }

    fn unimplemented() -> CallError {
        CallError::Status("gRPC Unimplemented".to_string())
    }

    #[test]
    fn auto_latches_on_unimplemented_for_good() {
        let read = HostRead::new(HostReadMode::Auto);
        assert!(read.begin_sweep());
        assert!(read.fall_back(&unimplemented()));
        for sweep in 0..3 * RETRY_COMPACT_AFTER_SWEEPS {
            assert!(!read.begin_sweep(), "sweep {sweep}");
        }
    }

    #[test]
    fn auto_retries_compact_after_permission_denied() {
        let read = HostRead::new(HostReadMode::Auto);
        assert!(read.begin_sweep());
        assert!(read.fall_back(&denied()));
        for sweep in 0..RETRY_COMPACT_AFTER_SWEEPS {
            assert!(!read.begin_sweep(), "sweep {sweep}");
        }
        assert!(read.begin_sweep(), "the retry");
        // Denied again, it waits again.
        assert!(read.fall_back(&denied()));
        assert!(!read.begin_sweep());
        // A denial then UNIMPLEMENTED still latches.
        assert!(read.fall_back(&unimplemented()));
        for _ in 0..2 * RETRY_COMPACT_AFTER_SWEEPS {
            assert!(!read.begin_sweep());
        }
    }

    #[test]
    fn full_and_compact_modes_ignore_fallback() {
        let full = HostRead::new(HostReadMode::Full);
        assert!(!full.begin_sweep() && !full.fall_back(&unimplemented()));
        let compact = HostRead::new(HostReadMode::Compact);
        assert!(compact.begin_sweep() && !compact.fall_back(&denied()));
        assert!(compact.begin_sweep());
        let auto = HostRead::new(HostReadMode::Auto);
        let internal = CallError::Status("gRPC Internal".to_string());
        assert!(!auto.fall_back(&internal) && auto.begin_sweep());
    }
}

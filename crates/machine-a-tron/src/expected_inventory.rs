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
//! Startup registration of expected inventory records with bounded retry,
//! and the records machine-a-tron serves on `/expected-inventory/records`.

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use bmc_mock::{HardwareType, HostMachineInfo};
use carbide_uuid::rack::{RackGroupId, RackId, RackProfileId};
use futures::{StreamExt, stream};
use model::expected_machine::HostDpuPolicy;
use rand::RngExt;
use rpc::admission_retry::{
    MAX_ADMISSION_BACKOFF, MIN_ADMISSION_BACKOFF, PushbackAdvice, admission_retry_delay,
};
use rpc::forge::{
    ExpectedInterface, ExpectedRackGroup, ExpectedRackGroupMember, ExpectedRackGroupRack,
    NetworkSegmentType,
};
use serde::{Deserialize, Serialize};
use tonic::Code;

use crate::api_client::{ClientApiError, ExpectedRecord};
use crate::config::{MachineATronConfig, MachineConfig};
use crate::device_simulator::{DeviceSimulator, SimulatorLifecycle};
use crate::rack::{RackRegistration, ensure_derived_profile};
use crate::simulator_registry::SimulatorRegistry;
use crate::status::DeviceKind;

/// Rounds per registration pass, including the first one.
const MAX_ATTEMPTS: u32 = 30;
/// Backoff before the first retry; doubles on each further retry.
const INITIAL_BACKOFF: Duration = Duration::from_millis(500);
/// Upper bound on the backoff between attempts.
const MAX_BACKOFF: Duration = Duration::from_secs(30);
/// Records registered concurrently at startup.
pub(crate) const CONCURRENCY: usize = 8;

/// How a failed registration attempt is handled.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Disposition {
    /// The API already holds the record.
    AlreadyPresent,
    /// The failure may clear on its own; retry after a backoff.
    Retry,
    /// The failure will not clear by retrying.
    Fail,
}

fn classify(error: &ClientApiError) -> Disposition {
    match error {
        ClientApiError::ConnectFailed(_) => Disposition::Retry,
        ClientApiError::ConfigError(_) => Disposition::Fail,
        ClientApiError::InvocationError(status) => match status.code() {
            Code::AlreadyExists => Disposition::AlreadyPresent,
            Code::ResourceExhausted => match admission_retry_delay(status) {
                PushbackAdvice::StopRetrying => Disposition::Fail,
                PushbackAdvice::Absent | PushbackAdvice::Delay(_) => Disposition::Retry,
            },
            Code::Unavailable
            | Code::DeadlineExceeded
            | Code::Internal
            | Code::Unknown
            | Code::Aborted
            | Code::Cancelled => Disposition::Retry,
            _ => Disposition::Fail,
        },
    }
}

/// Delay nico-api asked for on an admission rejection, clamped to the
/// advertised admission backoff range.
fn pushback_delay(error: &ClientApiError) -> Option<Duration> {
    let ClientApiError::InvocationError(status) = error else {
        return None;
    };
    if status.code() != Code::ResourceExhausted {
        return None;
    }
    match admission_retry_delay(status) {
        PushbackAdvice::Delay(delay) => {
            Some(delay.clamp(MIN_ADMISSION_BACKOFF, MAX_ADMISSION_BACKOFF))
        }
        PushbackAdvice::Absent | PushbackAdvice::StopRetrying => None,
    }
}

/// Delay before retry number `retry` (zero-based). `jitter` in `[0.0, 1.0]`
/// places the result between half of the base and the base.
fn backoff_delay(retry: u32, jitter: f64) -> Duration {
    let base = (INITIAL_BACKOFF * 2_u32.pow(retry)).min(MAX_BACKOFF);
    let half = base / 2;
    half + half.mul_f64(jitter.clamp(0.0, 1.0))
}

/// Counts of the device records from one startup registration pass, exposed
/// on `/expected-inventory/status`. Racks are not counted: a rack that cannot
/// be registered aborts startup before any device record is attempted.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct ExpectedInventorySummary {
    /// Records created by this pass.
    pub registered: usize,
    /// Records the API already held.
    pub already_present: usize,
    /// Identifiers of the records that could not be registered, sorted; a
    /// non-empty list fails startup, so a serving pod always reports none.
    pub failed_identifiers: Vec<String>,
}

impl ExpectedInventorySummary {
    /// Emits one summary line.
    pub(crate) fn log(&self) {
        tracing::info!(
            registered_count = self.registered,
            already_present_count = self.already_present,
            failed_count = self.failed_identifiers.len(),
            "expected inventory registration finished"
        );
    }
}

/// The expected inventory machine-a-tron serves on
/// `/expected-inventory/records`: what registration sends, built from the
/// live devices, with the loaded settings a reader needs to judge it. It is
/// served whether or not registration is enabled.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExpectedRecordsResponse {
    pub inventory_id: String,
    pub register_expected_machines: bool,
    pub carbide_api_url: String,
    pub racks: Vec<RackIntent>,
    pub devices: Vec<DeviceRecord>,
    pub dpus: Vec<DpuKey>,
}

/// A rack with the expected rack group it declares when no group declares
/// it yet.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RackIntent {
    pub rack_id: String,
    pub rack_profile_id: String,
    pub group: RackGroupIntent,
}

/// A one-rack expected rack group, as `RackRegistration::expected_rack_group`
/// builds it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RackGroupIntent {
    pub rack_group_id: String,
    pub topology: String,
    pub protocol: String,
    pub members: Vec<MemberIntent>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemberIntent {
    pub r#type: String,
    pub manufacturer: String,
    pub id: String,
}

/// One host, switch or power shelf record. `serial` is the serial number the
/// record declares. `dpu_policy`, `dpf_enabled` and `interfaces` apply to
/// hosts only, `nvos_macs` to switches only.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceRecord {
    pub kind: DeviceKind,
    pub hardware_type: String,
    pub bmc_mac: String,
    pub serial: String,
    pub rack_id: Option<String>,
    pub dpu_policy: Option<HostDpuPolicy>,
    pub dpf_enabled: bool,
    pub interfaces: Vec<NicIntent>,
    pub nvos_macs: Vec<String>,
}

/// A host in-band NIC a host record declares.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NicIntent {
    pub mac_address: String,
    pub primary: bool,
}

/// A DPU of a served host. DPUs have no record of their own; nico-api finds
/// them through their parent host.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DpuKey {
    pub bmc_mac: String,
    pub serial: String,
    pub parent_bmc_mac: String,
}

/// The records built from the live devices, before the loaded settings are
/// attached.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ServedRecords {
    pub racks: Vec<RackIntent>,
    pub devices: Vec<DeviceRecord>,
    pub dpus: Vec<DpuKey>,
}

impl ServedRecords {
    pub fn into_response(
        self,
        inventory_id: String,
        config: &MachineATronConfig,
    ) -> ExpectedRecordsResponse {
        ExpectedRecordsResponse {
            inventory_id,
            register_expected_machines: config.register_expected_machines,
            carbide_api_url: config.carbide_api_url.clone(),
            racks: self.racks,
            devices: self.devices,
            dpus: self.dpus,
        }
    }

    /// The device records registration sends, in device order.
    pub(crate) fn device_records(&self) -> eyre::Result<Vec<ExpectedRecord>> {
        self.devices
            .iter()
            .map(DeviceRecord::expected_record)
            .collect()
    }
}

impl ExpectedRecordsResponse {
    /// The records to send on an empty site: each rack with its own group,
    /// then every device record. Fails on a rack whose configured profile is
    /// not the one nico-api derives from its own group.
    pub fn expected_records(&self) -> eyre::Result<Vec<ExpectedRecord>> {
        let racks = self.racks.iter().map(RackIntent::expected_record);
        let devices = self.devices.iter().map(DeviceRecord::expected_record);
        racks.chain(devices).collect()
    }
}

impl RackIntent {
    fn new(rack: &RackRegistration) -> eyre::Result<Self> {
        let group = rack.expected_rack_group()?;
        let members = group
            .racks
            .into_iter()
            .flat_map(|rack| rack.members)
            .map(|member| MemberIntent {
                r#type: member.r#type,
                manufacturer: member.manufacturer,
                id: member.id,
            })
            .collect();
        Ok(Self {
            rack_id: rack.rack_id.to_string(),
            rack_profile_id: rack.rack_profile_id.to_string(),
            group: RackGroupIntent {
                rack_group_id: group
                    .rack_group_id
                    .as_ref()
                    .map(RackGroupId::to_string)
                    .unwrap_or_default(),
                topology: group.topology,
                protocol: group.protocol,
                members,
            },
        })
    }

    /// The rack record declaring the rack's own group. Fails when the
    /// configured profile is not the one nico-api derives from that group,
    /// as registration checks it: nico-api would store its own derived
    /// profile instead.
    pub fn expected_record(&self) -> eyre::Result<ExpectedRecord> {
        let rack_id = RackId::new(self.rack_id.as_str());
        let rack_profile_id = RackProfileId::new(self.rack_profile_id.as_str());
        let members = self
            .group
            .members
            .iter()
            .map(|member| ExpectedRackGroupMember {
                r#type: member.r#type.clone(),
                manufacturer: member.manufacturer.clone(),
                id: member.id.clone(),
            })
            .collect();
        let group = ExpectedRackGroup {
            rack_group_id: Some(RackGroupId::new(self.group.rack_group_id.as_str())),
            topology: self.group.topology.clone(),
            protocol: self.group.protocol.clone(),
            metadata: None,
            racks: vec![ExpectedRackGroupRack {
                rack_id: Some(rack_id.clone()),
                members,
            }],
        };
        ensure_derived_profile(&rack_id, &rack_profile_id, group.clone())?;
        Ok(ExpectedRecord::Rack {
            rack_id,
            rack_profile_id,
            group: Some(group),
        })
    }
}

impl DeviceRecord {
    pub fn expected_record(&self) -> eyre::Result<ExpectedRecord> {
        let rack_id = self.rack_id.as_deref().map(RackId::new);
        Ok(match self.kind {
            DeviceKind::Machine => ExpectedRecord::Machine {
                bmc_mac_address: self.bmc_mac.clone(),
                chassis_serial_number: self.serial.clone(),
                rack_id,
                dpu_policy: self.dpu_policy,
                dpf_enabled: self.dpf_enabled,
                interfaces: self
                    .interfaces
                    .iter()
                    .map(NicIntent::expected_interface)
                    .collect(),
            },
            DeviceKind::Switch => ExpectedRecord::Switch {
                bmc_mac_address: self.bmc_mac.clone(),
                switch_serial_number: self.serial.clone(),
                nvos_mac_addresses: self.nvos_macs.clone(),
                rack_id,
            },
            DeviceKind::PowerShelf => ExpectedRecord::PowerShelf {
                bmc_mac_address: self.bmc_mac.clone(),
                shelf_serial_number: self.serial.clone(),
                rack_id,
            },
            DeviceKind::Dpu => eyre::bail!("DPU {} has no expected record", self.bmc_mac),
        })
    }
}

impl NicIntent {
    fn expected_interface(&self) -> ExpectedInterface {
        ExpectedInterface {
            mac_address: Some(self.mac_address.clone()),
            nic_type: None,
            fixed_ip: None,
            fixed_mask: None,
            fixed_gateway: None,
            primary: Some(self.primary),
            network_segment_type: Some(NetworkSegmentType::HostInband as i32),
            ..Default::default()
        }
    }
}

/// The hardware type as its serde name, e.g. `wiwynn_gb200_nvl`: what the
/// device status reports and the MAT controller copies onto its Service.
fn hardware_type_name(hw_type: HardwareType) -> String {
    match serde_json::to_value(hw_type) {
        Ok(serde_json::Value::String(name)) => name,
        _ => unreachable!("a hardware type serializes as its name"),
    }
}

/// Host NICs a host record declares, by its DPU policy.
fn expected_nics(host_info: &HostMachineInfo, dpu_policy: Option<HostDpuPolicy>) -> Vec<NicIntent> {
    let mac_addresses = match dpu_policy {
        Some(HostDpuPolicy::Nic) => host_info
            .dpus
            .iter()
            .map(|dpu| dpu.host_mac_address)
            .collect::<Vec<_>>(),
        Some(HostDpuPolicy::Ignore) => host_info.non_dpu_mac_address.into_iter().collect(),
        _ => Vec::new(),
    };

    mac_addresses
        .into_iter()
        .enumerate()
        .map(|(index, mac_address)| NicIntent {
            mac_address: mac_address.to_string(),
            primary: index == 0,
        })
        .collect()
}

/// Derive the expected `dpu_policy` from the machine's MachineConfig:
/// zero-DPU hosts declare `Ignore`, hosts running their DPUs as NICs declare
/// `Nic`, and everything else defers to the default (`Manage`).
/// Site-explorer's ingestion gate requires this explicit declaration for any
/// host without DPU PCIe devices.
fn host_dpu_policy(machine_config: &MachineConfig) -> Option<HostDpuPolicy> {
    if machine_config.dpu_per_host_count == 0 {
        Some(HostDpuPolicy::Ignore)
    } else if machine_config.dpus_in_nic_mode {
        Some(HostDpuPolicy::Nic)
    } else {
        None
    }
}

/// Builds the records of the live devices and the racks they belong to.
/// Restored devices carry their persisted identities, so the records match
/// what is simulated.
pub(crate) fn served_records(
    simulators: &SimulatorRegistry,
    machines: &BTreeMap<String, Arc<MachineConfig>>,
    racks: &[RackRegistration],
) -> eyre::Result<ServedRecords> {
    let racks = racks
        .iter()
        .map(RackIntent::new)
        .collect::<eyre::Result<Vec<_>>>()?;
    let mut devices = Vec::with_capacity(simulators.devices().len());
    let mut dpus = Vec::new();
    for device in simulators.devices() {
        let machine = device.handle();
        let host_info = machine.host_info();
        let machine_config = machines
            .get(machine.machine_config_section())
            .ok_or_else(|| {
                eyre::eyre!(
                    "device {} has no machine config section {}",
                    host_info.bmc_mac_address,
                    machine.machine_config_section()
                )
            })?;
        let bmc_mac = host_info.bmc_mac_address.to_string();
        let mut record = DeviceRecord {
            kind: device.kind(),
            hardware_type: hardware_type_name(host_info.hw_type),
            bmc_mac: bmc_mac.clone(),
            serial: host_info.serial.clone(),
            rack_id: machine_config.rack_id.as_ref().map(RackId::to_string),
            dpu_policy: None,
            dpf_enabled: false,
            interfaces: Vec::new(),
            nvos_macs: Vec::new(),
        };
        match device {
            DeviceSimulator::PowerShelf(_) => {}
            DeviceSimulator::Switch(_) => {
                record.serial = host_info
                    .switch_serial_number
                    .clone()
                    .unwrap_or_else(|| host_info.serial.clone());
                record.nvos_macs = host_info
                    .nvos_mac_addresses
                    .iter()
                    .map(|mac| mac.to_string())
                    .collect();
            }
            DeviceSimulator::Machine(_) => {
                let dpu_policy = host_dpu_policy(machine_config);
                record.dpu_policy = dpu_policy;
                record.dpf_enabled = machine_config.dpf_enabled;
                record.interfaces = expected_nics(host_info, dpu_policy);
                dpus.extend(host_info.dpus.iter().map(|dpu| DpuKey {
                    bmc_mac: dpu.bmc_mac_address.to_string(),
                    serial: dpu.serial.clone(),
                    parent_bmc_mac: bmc_mac.clone(),
                }));
            }
        }
        devices.push(record);
    }
    Ok(ServedRecords {
        racks,
        devices,
        dpus,
    })
}

/// Runs one API call under the registration retry policy: a transient
/// failure is retried after a backoff for up to `MAX_ATTEMPTS` rounds.
pub(crate) async fn with_retry<T, F, Fut>(
    operation_name: &str,
    operation: F,
) -> Result<T, ClientApiError>
where
    F: Fn() -> Fut,
    Fut: Future<Output = Result<T, ClientApiError>>,
{
    let mut round = 1;
    loop {
        let error = match operation().await {
            Ok(value) => return Ok(value),
            Err(error) if round < MAX_ATTEMPTS && classify(&error) == Disposition::Retry => error,
            Err(error) => return Err(error),
        };
        let mut delay = backoff_delay(round - 1, rand::rng().random::<f64>());
        if let Some(pushback) = pushback_delay(&error) {
            delay = delay.max(pushback);
        }
        tracing::warn!(
            operation = operation_name,
            round,
            max_attempts = MAX_ATTEMPTS,
            error = %error,
            retry_delay_milliseconds = delay.as_millis(),
            "transient error; retrying"
        );
        tokio::time::sleep(delay).await;
        round += 1;
    }
}

/// Registers every record with at most `concurrency` in flight and returns
/// the aggregate outcome. Records that fail transiently are retried together
/// after one shared backoff for up to `MAX_ATTEMPTS` rounds.
pub(crate) async fn register_all<F, Fut>(
    records: Vec<ExpectedRecord>,
    concurrency: usize,
    register: F,
) -> ExpectedInventorySummary
where
    F: Fn(ExpectedRecord) -> Fut,
    Fut: Future<Output = Result<(), ClientApiError>>,
{
    let register = &register;
    let mut summary = ExpectedInventorySummary::default();
    let mut pending = records;
    for round in 1..=MAX_ATTEMPTS {
        let results = stream::iter(std::mem::take(&mut pending))
            .map(|record| async move {
                let result = register(record.clone()).await;
                (record, result)
            })
            .buffer_unordered(concurrency)
            .collect::<Vec<_>>()
            .await;

        let mut delay = backoff_delay(round - 1, rand::rng().random::<f64>());
        for (record, result) in results {
            let error = match result {
                Ok(()) => {
                    summary.registered += 1;
                    continue;
                }
                Err(error) => error,
            };
            match classify(&error) {
                Disposition::AlreadyPresent => summary.already_present += 1,
                Disposition::Retry if round < MAX_ATTEMPTS => {
                    if let Some(pushback) = pushback_delay(&error) {
                        delay = delay.max(pushback);
                    }
                    pending.push(record);
                }
                Disposition::Retry | Disposition::Fail => {
                    let identifier = record.identifier();
                    tracing::error!(
                        identifier,
                        error = %error,
                        "failed to register expected inventory record"
                    );
                    summary.failed_identifiers.push(identifier);
                }
            }
        }
        if pending.is_empty() {
            break;
        }
        tracing::warn!(
            round,
            max_attempts = MAX_ATTEMPTS,
            pending_count = pending.len(),
            retry_delay_milliseconds = delay.as_millis(),
            "transient errors registering expected inventory records; retrying"
        );
        tokio::time::sleep(delay).await;
    }
    summary.failed_identifiers.sort();
    summary
}

/// Registers the racks, then the served device records. A rack keeps the
/// group that already declares it, if any; otherwise it declares its own.
/// Fails when a rack, or any device record, cannot be registered.
pub(crate) async fn register_served<G, GFut, R, RFut>(
    racks: &[RackRegistration],
    served: &ServedRecords,
    declared_rack_groups: G,
    register: R,
) -> eyre::Result<ExpectedInventorySummary>
where
    G: FnOnce() -> GFut,
    GFut: Future<Output = Result<BTreeMap<RackId, ExpectedRackGroup>, ClientApiError>>,
    R: Fn(ExpectedRecord) -> RFut,
    RFut: Future<Output = Result<(), ClientApiError>>,
{
    // A group that already declares a rack is used as is.
    let declared_rack_groups = if racks.is_empty() {
        BTreeMap::new()
    } else {
        declared_rack_groups().await?
    };
    let rack_records = racks
        .iter()
        .map(|rack| rack.expected_record(declared_rack_groups.get(&rack.rack_id)))
        .collect::<eyre::Result<Vec<_>>>()?;
    let failed = register_all(rack_records, CONCURRENCY, &register)
        .await
        .failed_identifiers;
    if !failed.is_empty() {
        eyre::bail!("failed to register expected {}", failed.join(", "));
    }

    let summary = register_all(served.device_records()?, CONCURRENCY, &register).await;
    summary.log();
    if !summary.failed_identifiers.is_empty() {
        let failed = &summary.failed_identifiers;
        let mut listed = failed.iter().take(20).cloned().collect::<Vec<_>>();
        if failed.len() > listed.len() {
            listed.push(format!("and {} more", failed.len() - listed.len()));
        }
        eyre::bail!(
            "failed to register {} expected device records: {}",
            failed.len(),
            listed.join(", ")
        );
    }
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, VecDeque};
    use std::slice;
    use std::sync::Mutex;

    use bmc_mock::mac_address_pool::{MacAddressPool, PoolConfig as MacAddressPoolConfig};
    use bmc_mock::{DpuMachineInfo, DpuSettings, HardwareType, RackInfo, RackType};
    use carbide_test_support::{Check, check_values};
    use mac_address::MacAddress;
    use rpc::admission_retry::ADMISSION_RETRY_PUSHBACK_HEADER;
    use tonic::Status;
    use tonic::metadata::MetadataValue;

    use super::*;
    use crate::DeviceHandle;
    use crate::config::MachineATronContext;
    use crate::power_shelf_simulator::PowerShelfActor;
    use crate::rack::RackMemberRegistration;

    fn invocation(status: Status) -> ClientApiError {
        ClientApiError::InvocationError(status)
    }

    fn admission_rejected(pushback: &str) -> ClientApiError {
        let mut status = Status::resource_exhausted("admission");
        status.metadata_mut().insert(
            ADMISSION_RETRY_PUSHBACK_HEADER,
            MetadataValue::try_from(pushback).unwrap(),
        );
        invocation(status)
    }

    fn machine_record(serial: &str) -> ExpectedRecord {
        ExpectedRecord::Machine {
            bmc_mac_address: format!("02:00:00:00:00:{:02x}", serial.len()),
            chassis_serial_number: serial.to_string(),
            rack_id: None,
            dpu_policy: None,
            dpf_enabled: true,
            interfaces: Vec::new(),
        }
    }

    #[test]
    fn classify_registration_errors() {
        check_values(
            [
                Check {
                    scenario: "AlreadyExists is already present",
                    input: invocation(Status::already_exists("rack exists")),
                    expect: Disposition::AlreadyPresent,
                },
                Check {
                    scenario: "NVOS MAC claimed by another expected switch is a conflict",
                    input: invocation(Status::failed_precondition(
                        "NVOS MAC address is already claimed by another expected switch: 02:00:00:00:00:02",
                    )),
                    expect: Disposition::Fail,
                },
                Check {
                    scenario: "Internal is transient",
                    input: invocation(Status::internal("database error")),
                    expect: Disposition::Retry,
                },
                Check {
                    scenario: "Unavailable is transient",
                    input: invocation(Status::unavailable("connection refused")),
                    expect: Disposition::Retry,
                },
                Check {
                    scenario: "connection failure is transient",
                    input: ClientApiError::ConnectFailed("dns".to_string()),
                    expect: Disposition::Retry,
                },
                Check {
                    scenario: "admission rejection with a pushback delay is transient",
                    input: admission_rejected("5000"),
                    expect: Disposition::Retry,
                },
                Check {
                    scenario: "admission rejection with a negative pushback is permanent",
                    input: admission_rejected("-1"),
                    expect: Disposition::Fail,
                },
                Check {
                    scenario: "InvalidArgument is permanent",
                    input: invocation(Status::invalid_argument("bad serial")),
                    expect: Disposition::Fail,
                },
                Check {
                    scenario: "client configuration error is permanent",
                    input: ClientApiError::ConfigError("profile mismatch".to_string()),
                    expect: Disposition::Fail,
                },
            ],
            |error| classify(&error),
        );
    }

    #[test]
    fn backoff_doubles_to_cap_with_equal_jitter() {
        check_values(
            [
                Check {
                    scenario: "first retry with no jitter waits half the initial backoff",
                    input: (0, 0.0),
                    expect: Duration::from_millis(250),
                },
                Check {
                    scenario: "first retry with full jitter waits the initial backoff",
                    input: (0, 1.0),
                    expect: Duration::from_millis(500),
                },
                Check {
                    scenario: "base doubles per retry",
                    input: (3, 1.0),
                    expect: Duration::from_secs(4),
                },
                Check {
                    scenario: "base is capped at MAX_BACKOFF",
                    input: (7, 1.0),
                    expect: Duration::from_secs(30),
                },
            ],
            |(retry, jitter)| backoff_delay(retry, jitter),
        );
    }

    /// Runs `register_all` over one machine record per scripted serial; each
    /// attempt on a record pops its next response. Returns the summary, the
    /// attempts per serial, and the peak number of in-flight attempts.
    async fn register_scripted(
        script: Vec<(&str, Vec<Result<(), ClientApiError>>)>,
        concurrency: usize,
    ) -> (ExpectedInventorySummary, HashMap<String, u32>, usize) {
        let records = script
            .iter()
            .map(|(serial, _)| machine_record(serial))
            .collect();
        let responses: HashMap<String, VecDeque<_>> = script
            .into_iter()
            .map(|(serial, responses)| (serial.to_string(), responses.into_iter().collect()))
            .collect();
        let responses = Arc::new(Mutex::new(responses));
        let attempts = Arc::new(Mutex::new(HashMap::<String, u32>::new()));
        let in_flight = Arc::new(Mutex::new((0_usize, 0_usize)));

        let summary = register_all(records, concurrency, |record| {
            let responses = responses.clone();
            let attempts = attempts.clone();
            let in_flight = in_flight.clone();
            async move {
                {
                    let mut counters = in_flight.lock().unwrap();
                    counters.0 += 1;
                    counters.1 = counters.1.max(counters.0);
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
                let ExpectedRecord::Machine {
                    chassis_serial_number,
                    ..
                } = record
                else {
                    unreachable!("only machine records are scripted");
                };
                *attempts
                    .lock()
                    .unwrap()
                    .entry(chassis_serial_number.clone())
                    .or_default() += 1;
                let response = responses
                    .lock()
                    .unwrap()
                    .get_mut(&chassis_serial_number)
                    .and_then(VecDeque::pop_front)
                    .expect("more attempts than scripted responses");
                in_flight.lock().unwrap().0 -= 1;
                response
            }
        })
        .await;

        let attempts = attempts.lock().unwrap().clone();
        let peak_in_flight = in_flight.lock().unwrap().1;
        (summary, attempts, peak_in_flight)
    }

    /// Runs `with_retry` over scripted responses and returns the outcome
    /// with the number of attempts made.
    async fn retry_scripted(
        responses: Vec<Result<u32, ClientApiError>>,
    ) -> (Result<u32, String>, usize) {
        let responses = Arc::new(Mutex::new(VecDeque::from(responses)));
        let attempts = Arc::new(Mutex::new(0_usize));
        let result = with_retry("lookup", || {
            let responses = responses.clone();
            let attempts = attempts.clone();
            async move {
                *attempts.lock().unwrap() += 1;
                responses
                    .lock()
                    .unwrap()
                    .pop_front()
                    .expect("more attempts than scripted responses")
            }
        })
        .await
        .map_err(|error| error.to_string());
        let attempts = *attempts.lock().unwrap();
        (result, attempts)
    }

    #[tokio::test(start_paused = true)]
    async fn with_retry_retries_only_transient_failures() {
        struct RetryCase {
            scenario: &'static str,
            responses: Vec<Result<u32, ClientApiError>>,
            expect: Result<u32, String>,
            attempts: usize,
        }
        let transient = || Err(invocation(Status::unavailable("busy")));
        let cases = [
            RetryCase {
                scenario: "succeeds after transient failures",
                responses: vec![transient(), transient(), Ok(7)],
                expect: Ok(7),
                attempts: 3,
            },
            RetryCase {
                scenario: "permanent failure is not retried",
                responses: vec![Err(invocation(Status::invalid_argument("bad")))],
                expect: Err("the API call to the forge API server returned code: 'Client specified an invalid argument', message: \"bad\"".to_string()),
                attempts: 1,
            },
            RetryCase {
                scenario: "gives up after MAX_ATTEMPTS",
                responses: (0..MAX_ATTEMPTS).map(|_| transient()).collect(),
                expect: Err("the API call to the forge API server returned code: 'The service is currently unavailable', message: \"busy\"".to_string()),
                attempts: MAX_ATTEMPTS as usize,
            },
        ];
        for case in cases {
            let (result, attempts) = retry_scripted(case.responses).await;
            assert_eq!(result, case.expect, "{}", case.scenario);
            assert_eq!(attempts, case.attempts, "{}", case.scenario);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn register_all_retries_transient_failures_by_round() {
        let concurrency = 2;
        let transient = || Err(invocation(Status::unavailable("busy")));
        let (summary, attempts, peak_in_flight) = register_scripted(
            vec![
                ("ok", vec![Ok(())]),
                (
                    "dup",
                    vec![Err(invocation(Status::already_exists("present")))],
                ),
                (
                    "bad",
                    vec![Err(invocation(Status::invalid_argument("serial")))],
                ),
                ("late", vec![transient(), Ok(())]),
                ("never", (0..MAX_ATTEMPTS).map(|_| transient()).collect()),
            ],
            concurrency,
        )
        .await;

        assert_eq!(
            summary,
            ExpectedInventorySummary {
                registered: 2,
                already_present: 1,
                failed_identifiers: vec![
                    machine_record("bad").identifier(),
                    machine_record("never").identifier(),
                ],
            }
        );
        assert_eq!(
            attempts,
            HashMap::from([
                ("ok".to_string(), 1),
                ("dup".to_string(), 1),
                ("bad".to_string(), 1),
                ("late".to_string(), 2),
                ("never".to_string(), MAX_ATTEMPTS),
            ])
        );
        assert!(
            peak_in_flight <= concurrency,
            "in-flight registrations exceeded the concurrency bound"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn register_all_waits_for_admission_pushback() {
        let start = tokio::time::Instant::now();

        let (summary, ..) = register_scripted(
            vec![("slow", vec![Err(admission_rejected("5000")), Ok(())])],
            1,
        )
        .await;

        assert_eq!(summary.registered, 1);
        assert!(
            start.elapsed() >= Duration::from_secs(5),
            "retried after {:?}, before the 5 s the server asked for",
            start.elapsed()
        );
    }

    const MANAGED: &str = "managed";
    const NIC_MODE: &str = "nic-mode";
    const ZERO_DPU: &str = "zero-dpu";
    const SWITCH: &str = "switch";
    const SHELF: &str = "shelf";
    const RACK: &str = "rack-001";

    fn mac(last: u8) -> MacAddress {
        MacAddress::new([2, 0, 0, 0, 0, last])
    }

    /// Port `index` of the `family` the device with BMC MAC `mac(device)` owns.
    fn port_mac(family: u8, device: u8, index: u8) -> MacAddress {
        MacAddress::new([2, 0, 0, family, device, index])
    }

    const DPU_BMC: u8 = 1;
    const DPU_HOST: u8 = 2;
    const DPU_OOB: u8 = 3;
    const NVOS: u8 = 4;

    fn host_info(
        hw_type: HardwareType,
        bmc_mac: u8,
        dpu_count: u8,
        non_dpu_mac_address: Option<MacAddress>,
    ) -> HostMachineInfo {
        HostMachineInfo {
            hw_type,
            rack_placement: None,
            bmc_mac_address: mac(bmc_mac),
            serial: format!("serial-{bmc_mac}"),
            dpus: (0..dpu_count)
                .map(|index| DpuMachineInfo {
                    hw_type: HardwareType::WiwynnGB200Nvl,
                    bmc_mac_address: port_mac(DPU_BMC, bmc_mac, index),
                    host_mac_address: port_mac(DPU_HOST, bmc_mac, index),
                    oob_mac_address: port_mac(DPU_OOB, bmc_mac, index),
                    serial: format!("dpu-{bmc_mac}-{index}"),
                    settings: DpuSettings::default(),
                })
                .collect(),
            non_dpu_mac_address,
            nvos_mac_addresses: Vec::new(),
            switch_serial_number: None,
            hw_mac_addr_pool: MacAddressPoolConfig::new(mac(bmc_mac), 24).unwrap(),
            delta_psu_power: None,
            initial_host_firmware: None,
            desired_host_firmware: None,
        }
    }

    /// One device of each record shape: a host with managed DPUs, a host
    /// running its DPUs as NICs, a zero-DPU host, two switches (one without
    /// a switch serial number) and a power shelf.
    struct Fixture {
        simulators: SimulatorRegistry,
        machines: BTreeMap<String, Arc<MachineConfig>>,
        shelf: HostMachineInfo,
    }

    impl Fixture {
        fn new() -> Self {
            let app_context = MachineATronContext::for_test();
            let template = app_context.app_config.machines["config"].clone();
            let config = |hw_type, dpus, nic_mode, dpf_enabled, rack: Option<&str>| {
                Arc::new(MachineConfig {
                    hw_type,
                    dpu_per_host_count: dpus,
                    dpus_in_nic_mode: nic_mode,
                    dpf_enabled,
                    rack_id: rack.map(RackId::new),
                    ..(*template).clone()
                })
            };
            let machines = BTreeMap::from([
                (
                    MANAGED.to_string(),
                    config(HardwareType::WiwynnGB200Nvl, 2, false, true, Some(RACK)),
                ),
                (
                    NIC_MODE.to_string(),
                    config(HardwareType::DellPowerEdgeR750, 2, true, false, None),
                ),
                (
                    ZERO_DPU.to_string(),
                    config(HardwareType::DellPowerEdgeR750, 0, false, true, None),
                ),
                (
                    SWITCH.to_string(),
                    config(
                        HardwareType::NvidiaSwitchNd5200Ld,
                        0,
                        false,
                        true,
                        Some(RACK),
                    ),
                ),
                (
                    SHELF.to_string(),
                    config(HardwareType::LiteOnPowerShelf, 0, false, true, Some(RACK)),
                ),
            ]);

            let switch = |bmc_mac: u8, switch_serial_number: Option<&str>| {
                let mut info = host_info(HardwareType::NvidiaSwitchNd5200Ld, bmc_mac, 0, None);
                info.switch_serial_number = switch_serial_number.map(str::to_string);
                info.nvos_mac_addresses =
                    vec![port_mac(NVOS, bmc_mac, 0), port_mac(NVOS, bmc_mac, 1)];
                DeviceHandle::for_control_test_switch(info, SWITCH, None)
            };
            let shelf_base = MacAddress::new([0x0a, 0, 0, 0, 0, 0]);
            let mut mac_pool =
                MacAddressPool::new_pool(MacAddressPoolConfig::new(shelf_base, 24).unwrap());
            let shelf = PowerShelfActor::new(
                app_context,
                SHELF.to_string(),
                machines[SHELF].clone(),
                &mut mac_pool,
                MacAddressPoolConfig::new(shelf_base, 24).unwrap(),
            )
            .start(true);
            let shelf = DeviceHandle::power_shelf(shelf);
            let shelf_info = shelf.host_info().clone();

            let handles = vec![
                DeviceHandle::for_control_test_host(
                    host_info(HardwareType::WiwynnGB200Nvl, 1, 2, None),
                    MANAGED,
                ),
                DeviceHandle::for_control_test_host(
                    host_info(HardwareType::DellPowerEdgeR750, 2, 2, None),
                    NIC_MODE,
                ),
                DeviceHandle::for_control_test_host(
                    host_info(HardwareType::DellPowerEdgeR750, 3, 0, Some(mac(0x30))),
                    ZERO_DPU,
                ),
                switch(4, Some("MT4")),
                switch(5, None),
                shelf,
            ];
            Self {
                simulators: SimulatorRegistry::try_from_handles(handles).unwrap(),
                machines,
                shelf: shelf_info,
            }
        }

        /// The records registration built inline for these devices before
        /// it built them from the served records.
        fn previous_records(&self) -> Vec<ExpectedRecord> {
            let rack_id = Some(RackId::new(RACK));
            let nic = |mac_address: MacAddress, primary: bool| ExpectedInterface {
                mac_address: Some(mac_address.to_string()),
                nic_type: None,
                fixed_ip: None,
                fixed_mask: None,
                fixed_gateway: None,
                primary: Some(primary),
                network_segment_type: Some(NetworkSegmentType::HostInband as i32),
                ..Default::default()
            };
            let switch = |bmc_mac: u8, switch_serial_number: &str| ExpectedRecord::Switch {
                bmc_mac_address: mac(bmc_mac).to_string(),
                switch_serial_number: switch_serial_number.to_string(),
                nvos_mac_addresses: vec![
                    port_mac(NVOS, bmc_mac, 0).to_string(),
                    port_mac(NVOS, bmc_mac, 1).to_string(),
                ],
                rack_id: rack_id.clone(),
            };
            vec![
                ExpectedRecord::Machine {
                    bmc_mac_address: mac(1).to_string(),
                    chassis_serial_number: "serial-1".to_string(),
                    rack_id: rack_id.clone(),
                    dpu_policy: None,
                    dpf_enabled: true,
                    interfaces: Vec::new(),
                },
                ExpectedRecord::Machine {
                    bmc_mac_address: mac(2).to_string(),
                    chassis_serial_number: "serial-2".to_string(),
                    rack_id: None,
                    dpu_policy: Some(HostDpuPolicy::Nic),
                    dpf_enabled: false,
                    interfaces: vec![
                        nic(port_mac(DPU_HOST, 2, 0), true),
                        nic(port_mac(DPU_HOST, 2, 1), false),
                    ],
                },
                ExpectedRecord::Machine {
                    bmc_mac_address: mac(3).to_string(),
                    chassis_serial_number: "serial-3".to_string(),
                    rack_id: None,
                    dpu_policy: Some(HostDpuPolicy::Ignore),
                    dpf_enabled: true,
                    interfaces: vec![nic(mac(0x30), true)],
                },
                switch(4, "MT4"),
                switch(5, "serial-5"),
                ExpectedRecord::PowerShelf {
                    bmc_mac_address: self.shelf.bmc_mac_address.to_string(),
                    shelf_serial_number: self.shelf.serial.clone(),
                    rack_id,
                },
            ]
        }
    }

    /// A GB200 rack of the fixture's host, switch and shelf, configured with
    /// the profile nico-api derives from its own group.
    fn rack_registration() -> RackRegistration {
        let rack = RackInfo {
            rack_type: RackType::WiwynnGb200Nvl72,
        };
        let member = |position, hardware_type, section: &str| RackMemberRegistration {
            placement: rack.placement(position),
            hardware_type,
            machine_config_section: section.to_string(),
        };
        let mut registration = RackRegistration {
            rack_id: RackId::new(RACK),
            rack_profile_id: RackProfileId::new("derived below"),
            rack_type: RackType::WiwynnGb200Nvl72,
            version: 1,
            members: vec![
                member(11, HardwareType::WiwynnGB200Nvl, MANAGED),
                member(19, HardwareType::NvidiaSwitchNd5200Ld, SWITCH),
                member(1, HardwareType::LiteOnPowerShelf, SHELF),
            ],
        };
        registration.rack_profile_id = registration
            .derived_rack_profile_id(registration.expected_rack_group().unwrap())
            .unwrap();
        registration
    }

    /// Runs `register_served` with a fake API that declares `declared` and
    /// accepts every record, returning what was sent.
    async fn register_capturing(
        racks: &[RackRegistration],
        served: &ServedRecords,
        declared: BTreeMap<RackId, ExpectedRackGroup>,
    ) -> (ExpectedInventorySummary, Vec<ExpectedRecord>) {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let summary = register_served(
            racks,
            served,
            || async move { Ok(declared) },
            |record| {
                let sent = sent.clone();
                async move {
                    sent.lock().unwrap().push(record);
                    Ok(())
                }
            },
        )
        .await
        .unwrap();
        let mut sent = sent.lock().unwrap().clone();
        sent.sort_by_key(ExpectedRecord::identifier);
        (summary, sent)
    }

    #[tokio::test]
    async fn registration_sends_served_records() {
        let fixture = Fixture::new();
        let served = served_records(&fixture.simulators, &fixture.machines, &[]).unwrap();

        let (summary, sent) = register_capturing(&[], &served, BTreeMap::new()).await;

        let mut expected = fixture.previous_records();
        expected.sort_by_key(ExpectedRecord::identifier);
        assert_eq!(sent, expected);
        assert_eq!(summary.registered, expected.len());
    }

    #[tokio::test]
    async fn registration_uses_existing_group() {
        let rack = rack_registration();
        let own_group = rack.expected_rack_group().unwrap();
        let mut site_group = own_group.clone();
        site_group.rack_group_id = Some(RackGroupId::new("site-group"));
        let rack_record = |group| ExpectedRecord::Rack {
            rack_id: rack.rack_id.clone(),
            rack_profile_id: rack.rack_profile_id.clone(),
            group,
        };

        let (_, sent) = register_capturing(
            slice::from_ref(&rack),
            &ServedRecords::default(),
            BTreeMap::from([(rack.rack_id.clone(), site_group)]),
        )
        .await;
        assert_eq!(sent, vec![rack_record(None)], "an existing group is kept");

        let (_, sent) = register_capturing(
            slice::from_ref(&rack),
            &ServedRecords::default(),
            BTreeMap::new(),
        )
        .await;
        assert_eq!(
            sent,
            vec![rack_record(Some(own_group))],
            "a rack no group declares declares its own"
        );
    }

    #[test]
    fn rack_intent_matches_registration_group() {
        let rack = rack_registration();
        let served = served_records(
            &SimulatorRegistry::try_from_handles(Vec::new()).unwrap(),
            &BTreeMap::new(),
            slice::from_ref(&rack),
        )
        .unwrap();

        assert_eq!(served.racks.len(), 1);
        assert_eq!(
            served.racks[0].expected_record().unwrap(),
            rack.expected_record(None).unwrap()
        );
    }

    #[tokio::test]
    async fn mismatched_profile_conversion_fails() {
        let rack = RackRegistration {
            rack_profile_id: RackProfileId::new("GB300_NVL72R1_C2G4_LENOVO"),
            ..rack_registration()
        };
        let served = served_records(
            &SimulatorRegistry::try_from_handles(Vec::new()).unwrap(),
            &BTreeMap::new(),
            slice::from_ref(&rack),
        )
        .expect("the route serves a mismatched rack, registration may bind it to another group");
        let message = "rack rack-001 configures rack_profile_id GB300_NVL72R1_C2G4_LENOVO, \
                       but nico-api derives GB200_NVL72R1_C2G4_WIWYNN from expected rack group rack-001";

        let error = served.racks[0].expected_record().unwrap_err();
        assert_eq!(error.to_string(), message);
        let config = MachineATronContext::for_test().app_config.clone();
        let response = served.into_response("mat-06:00:00:00:00:00".to_string(), &config);
        assert_eq!(
            response.expected_records().unwrap_err().to_string(),
            message
        );
    }

    #[tokio::test]
    async fn hardware_type_matches_device_status() {
        let fixture = Fixture::new();
        let served = served_records(&fixture.simulators, &fixture.machines, &[]).unwrap();
        let config = crate::status::DeviceStatusConfig::new(1266);
        for (device, record) in fixture.simulators.devices().iter().zip(&served.devices) {
            let status = serde_json::to_value(device.handle().status(&config)).unwrap();
            assert_eq!(
                status["hardware_type"].as_str(),
                Some(record.hardware_type.as_str()),
                "{}",
                record.bmc_mac
            );
        }
        assert_eq!(served.devices[0].hardware_type, "wiwynn_gb200_nvl");
    }

    #[tokio::test]
    async fn dpu_keys_carry_parent_bmc_mac() {
        let fixture = Fixture::new();
        let served = served_records(&fixture.simulators, &fixture.machines, &[]).unwrap();

        let key = |parent: u8, index: u8| DpuKey {
            bmc_mac: port_mac(DPU_BMC, parent, index).to_string(),
            serial: format!("dpu-{parent}-{index}"),
            parent_bmc_mac: mac(parent).to_string(),
        };
        assert_eq!(
            served.dpus,
            vec![key(1, 0), key(1, 1), key(2, 0), key(2, 1)]
        );
    }

    #[tokio::test]
    async fn response_round_trip_gives_same_requests() {
        let fixture = Fixture::new();
        let rack = rack_registration();
        let served = served_records(
            &fixture.simulators,
            &fixture.machines,
            slice::from_ref(&rack),
        )
        .unwrap();
        let registered = std::iter::once(rack.expected_record(None).unwrap())
            .chain(served.device_records().unwrap())
            .flat_map(ExpectedRecord::into_requests)
            .collect::<Vec<_>>();

        let config = MachineATronContext::for_test().app_config.clone();
        let json = serde_json::to_string(
            &served.into_response("mat-06:00:00:00:00:00".to_string(), &config),
        )
        .unwrap();
        let response: ExpectedRecordsResponse = serde_json::from_str(&json).unwrap();
        let requests = response
            .expected_records()
            .unwrap()
            .into_iter()
            .flat_map(ExpectedRecord::into_requests)
            .collect::<Vec<_>>();

        assert_eq!(requests, registered);
    }

    #[test]
    fn expected_interface_derivation() {
        let first_dpu_mac = port_mac(DPU_HOST, 2, 0);
        let second_dpu_mac = port_mac(DPU_HOST, 2, 1);
        let integrated_mac = mac(0x30);
        let expected_nic = |mac_address: MacAddress, primary| ExpectedInterface {
            mac_address: Some(mac_address.to_string()),
            nic_type: None,
            fixed_ip: None,
            fixed_mask: None,
            fixed_gateway: None,
            primary: Some(primary),
            network_segment_type: Some(NetworkSegmentType::HostInband as i32),
            ..Default::default()
        };

        check_values(
            [
                Check {
                    scenario: "NIC-mode host declares every host-facing DPU PF",
                    input: (
                        host_info(HardwareType::DellPowerEdgeR750, 2, 2, None),
                        Some(HostDpuPolicy::Nic),
                    ),
                    expect: vec![
                        expected_nic(first_dpu_mac, true),
                        expected_nic(second_dpu_mac, false),
                    ],
                },
                Check {
                    scenario: "zero-DPU host declares its integrated NIC",
                    input: (
                        host_info(HardwareType::DellPowerEdgeR750, 3, 0, Some(integrated_mac)),
                        Some(HostDpuPolicy::Ignore),
                    ),
                    expect: vec![expected_nic(integrated_mac, true)],
                },
                Check {
                    scenario: "managed-DPU host relies on automatic DPU discovery",
                    input: (host_info(HardwareType::WiwynnGB200Nvl, 2, 1, None), None),
                    expect: Vec::new(),
                },
            ],
            |(host_info, dpu_policy)| {
                expected_nics(&host_info, dpu_policy)
                    .iter()
                    .map(NicIntent::expected_interface)
                    .collect::<Vec<_>>()
            },
        );
    }
}

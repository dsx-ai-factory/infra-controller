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

//! What the probe knows about one host. Today it comes from the full machine
//! record (FindMachinesByIds): model and versions from DMI and the BMC record,
//! and no allocatability or update eligibility, which no read exposes yet.
//! Those are `Unknown`, published as `unknown` and `null`, never guessed.

use carbide_site_health_probe_api::VersionSource;
use carbide_uuid::machine::MachineId;
use chrono::{DateTime, Utc};
use rpc::forge;

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
    let bmc = machine.bmc_info.unwrap_or_default();
    Ok(HostDetail {
        id,
        state: machine.state,
        state_reason: machine
            .state_reason
            .and_then(|reason| reason.outcome_msg)
            .filter(|msg| !msg.is_empty()),
        vendor: non_empty(dmi.sys_vendor),
        model: non_empty(dmi.product_name),
        instance_type: config.instance_type_id.filter(|id| !id.is_empty()),
        bmc_ip: bmc.ip.filter(|ip| !ip.is_empty()),
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
        bmc_version: bmc.firmware_version.filter(|v| !v.is_empty()),
        uefi_version: non_empty(dmi.bios_version),
        version_source: VersionSource::MachineRecord,
        report_at: None,
        report_error: None,
        allocatable: Known::Unknown(COMPACT_HOST_READ_MISSING),
        update_eligible: Known::Unknown(COMPACT_HOST_READ_MISSING),
    })
}

fn non_empty(value: String) -> Option<String> {
    (!value.is_empty()).then_some(value)
}

#[cfg(test)]
mod tests {
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
            bmc_info: Some(forge::BmcInfo {
                ip: Some("10.0.0.5".to_string()),
                firmware_version: Some("25.06-2".to_string()),
                ..Default::default()
            }),
            config: Some(forge::MachineConfig {
                instance_type_id: Some("it-1".to_string()),
                firmware_autoupdate: Some(true),
                ..Default::default()
            }),
            status: Some(forge::MachineStatus {
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
        assert_eq!(detail.vendor.as_deref(), Some("NVIDIA"));
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
}

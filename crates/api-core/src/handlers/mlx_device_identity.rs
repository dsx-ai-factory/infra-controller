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

use std::collections::BTreeSet;

use carbide_uuid::machine::HostMachineId;
use mac_address::MacAddress;
use model::machine::MachineInterfaceSnapshot;
use model::machine::machine_search_config::MachineSearchConfig;
use model::machine::status::MlxDeviceObservation;
use rpc::protos::mlx_device::{
    MlxAdminDeviceIdentitiesRequest, MlxAdminDeviceIdentitiesResponse, MlxDeviceIdentity,
    MlxDeviceIdentityReport,
};
use tonic::{Request, Response, Status};

use crate::CarbideError;
use crate::api::{Api, log_request_data};
use crate::handlers::utils::convert_and_log_machine_id;

/// `show` reports stored Scout identity evidence and current managed-DPU
/// associations. Reading the report does not require Scout or authorize updates.
pub(crate) async fn show(
    api: &Api,
    request: Request<MlxAdminDeviceIdentitiesRequest>,
) -> Result<Response<MlxAdminDeviceIdentitiesResponse>, Status> {
    log_request_data(&request);
    let machine_id: HostMachineId =
        convert_and_log_machine_id(request.get_ref().machine_id.as_ref())?;
    let machine = db::machine::find_one(
        &api.database_connection,
        &machine_id,
        MachineSearchConfig::default(),
    )
    .await?
    .ok_or_else(|| CarbideError::NotFoundError {
        kind: "machine",
        id: machine_id.to_string(),
    })?;

    // The snapshot query already limits interfaces to this host's non-BMC
    // rows. Keep secondary interfaces: each attached DPU has its own match.
    let report = machine
        .status
        .mlx_device_observation
        .as_ref()
        .map(|observation| identity_report(observation, &machine.status.interfaces));
    Ok(Response::new(MlxAdminDeviceIdentitiesResponse { report }))
}

fn identity_report(
    observation: &MlxDeviceObservation,
    interfaces: &[MachineInterfaceSnapshot],
) -> MlxDeviceIdentityReport {
    let devices = observation
        .devices
        .iter()
        .map(|device| {
            let mac = device.base_mac.filter(valid_mac);

            // Site exploration records the DPU's host PF MAC on its host
            // interface, normally from Redfish's `base_mac`. If a fallback
            // derives a different MAC, ownership stays unknown here.
            // These are current database associations, not proof that an old
            // observation still describes the installed hardware. Preserve
            // conflicts instead of choosing the first matching DPU.
            let managed_dpu_machine_ids = interfaces
                .iter()
                .filter(|interface| Some(interface.mac_address) == mac)
                .filter_map(|interface| interface.attached_dpu_machine_id)
                .collect::<BTreeSet<_>>()
                .into_iter()
                .map(Into::into)
                .collect();

            MlxDeviceIdentity {
                device_info: Some(device.clone().into()),
                managed_dpu_machine_ids,
            }
        })
        .collect();

    MlxDeviceIdentityReport {
        observed_at: Some(observation.observed_at.into()),
        devices,
    }
}

fn valid_mac(mac: &MacAddress) -> bool {
    let bytes = mac.bytes();
    bytes != [0; 6] && bytes[0] & 1 == 0
}

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;
    use carbide_uuid::machine::MachineId;
    use rpc::protos::mlx_device::MlxDeviceInfo;

    use super::*;

    fn observation(devices: Vec<MlxDeviceInfo>) -> MlxDeviceObservation {
        MlxDeviceObservation {
            observed_at: chrono::DateTime::from_timestamp(42, 0).unwrap(),
            devices: devices
                .into_iter()
                .map(|device| device.try_into().unwrap())
                .collect(),
        }
    }

    fn device(mac: &str) -> MlxDeviceInfo {
        MlxDeviceInfo {
            pci_name: "01:00.0".into(),
            device_type: "BlueField3".into(),
            base_mac: mac.into(),
            ..Default::default()
        }
    }

    #[test]
    fn ownership_reports_distinct_matches_without_guessing() {
        struct Input<'a> {
            observation: &'a MlxDeviceObservation,
            interfaces: &'a [MachineInterfaceSnapshot],
        }

        let first = model::test_support::machine_snapshot::dpu_machine_id(0);
        let second = model::test_support::machine_snapshot::dpu_machine_id(1);
        let mac = "02:00:00:00:00:01".parse().unwrap();
        let mut interface = MachineInterfaceSnapshot::mock_with_mac(mac);
        interface.attached_dpu_machine_id = Some(first);
        let observed = observation(vec![device("02:00:00:00:00:01"), device("")]);
        let mut relocated = observed.clone();
        relocated.devices[0].pci_name = "86:00.0".into();
        let mut other = interface.clone();
        other.attached_dpu_machine_id = Some(second);
        let mut conflicting: Vec<MachineId> = vec![first.into(), second.into()];
        conflicting.sort();

        value_scenarios!(
            run = |Input { observation, interfaces }| {
                identity_report(observation, interfaces)
                    .devices
                    .into_iter()
                    .map(|device| device.managed_dpu_machine_ids)
                    .collect::<Vec<_>>()
            };
            "duplicate associations and missing MAC" {
                Input {
                    observation: &observed,
                    interfaces: &[interface.clone(), interface.clone()],
                } => vec![vec![first.into()], vec![]],
            }
            "PCI relocation" {
                Input {
                    observation: &relocated,
                    interfaces: &[interface.clone()],
                } => vec![vec![first.into()], vec![]],
            }
            "no interfaces" {
                Input {
                    observation: &observed,
                    interfaces: &[],
                } => vec![vec![], vec![]],
            }
            "conflicting associations" {
                Input {
                    observation: &observed,
                    interfaces: &[other, interface],
                } => vec![conflicting, vec![]],
            }
        );
    }

    #[test]
    fn ownership_does_not_match_zero_or_multicast_macs() {
        value_scenarios!(
            run = |mac: &str| {
                let mut interface = MachineInterfaceSnapshot::mock_with_mac(mac.parse().unwrap());
                interface.attached_dpu_machine_id = Some(model::test_support::machine_snapshot::dpu_machine_id(0));
                identity_report(&observation(vec![device(mac)]), &[interface])
                    .devices[0].managed_dpu_machine_ids.is_empty()
            };
            "zero MAC" { "00:00:00:00:00:00" => true, }
            "multicast MAC" { "01:00:00:00:00:01" => true, }
        );
    }
}

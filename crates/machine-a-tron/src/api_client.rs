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

use base64::prelude::*;
use bmc_mock::{DUMMY_FACTORY_PASSWORD, DUMMY_FACTORY_USERNAME, DUMMY_NVOS_USERNAME, MachineInfo};
use carbide_uuid::instance::InstanceId;
use carbide_uuid::machine::{DpuMachineId, MachineId, MachineInterfaceId};
use carbide_uuid::machine_validation::MachineValidationId;
use carbide_uuid::power_shelf::PowerShelfId;
use carbide_uuid::rack::{RackGroupId, RackId, RackProfileId};
use carbide_uuid::switch::SwitchId;
use mac_address::MacAddress;
use model::expected_machine::HostDpuPolicy;
use rpc::forge::machine_cleanup_info::CleanupStepResult;
use rpc::forge::{
    ConfigSetting, ExpectedInterface, ExpectedMachine, ExpectedPowerShelf, ExpectedRack,
    ExpectedRackGroup, ExpectedRackGroupRequest, ExpectedRackRequest, ExpectedSwitch,
    SetDynamicConfigRequest,
};
use rpc::protos::carbide_api_client::CarbideApiClient;

use crate::MachineConfig;
use crate::status::DeviceKind;

#[derive(thiserror::Error, Debug)]
pub enum ClientApiError {
    #[error("configuration error: {0}")]
    ConfigError(String),

    #[error("unable to connect to carbide API: {0}")]
    ConnectFailed(String),

    #[error("the API call to the forge API server returned {0}")]
    InvocationError(#[from] tonic::Status),
}

type ClientApiResult<T> = Result<T, ClientApiError>;

// Simple wrapper around the inputs to discover_machine so that callers can see the field names
pub struct MockDiscoveryData {
    pub machine_interface_id: MachineInterfaceId,
    pub tpm_ek_certificate: Option<Vec<u8>>,
}

#[derive(Debug, Clone)]
pub struct ApiClient(pub CarbideApiClient);

impl From<CarbideApiClient> for ApiClient {
    fn from(value: CarbideApiClient) -> Self {
        ApiClient(value)
    }
}

/// One expected inventory record that machine-a-tron registers at startup.
#[derive(Clone, Debug, PartialEq)]
pub enum ExpectedRecord {
    Rack {
        rack_id: RackId,
        rack_profile_id: RackProfileId,
        /// `None` when an expected rack group already declares the rack.
        group: Option<ExpectedRackGroup>,
    },
    Machine {
        bmc_mac_address: String,
        chassis_serial_number: String,
        rack_id: Option<RackId>,
        dpu_policy: Option<HostDpuPolicy>,
        dpf_enabled: bool,
        interfaces: Vec<ExpectedInterface>,
    },
    Switch {
        bmc_mac_address: String,
        switch_serial_number: String,
        nvos_mac_addresses: Vec<String>,
        rack_id: Option<RackId>,
    },
    PowerShelf {
        bmc_mac_address: String,
        shelf_serial_number: String,
        rack_id: Option<RackId>,
    },
}

/// One nico-api `Add*` request body of an expected record.
#[derive(Clone, Debug, PartialEq)]
pub enum ExpectedRequest {
    RackGroup(ExpectedRackGroup),
    Rack(ExpectedRack),
    Machine(ExpectedMachine),
    Switch(ExpectedSwitch),
    PowerShelf(ExpectedPowerShelf),
}

impl ExpectedRecord {
    /// Human-readable identity used in logs and the registration summary.
    pub fn identifier(&self) -> String {
        let (kind, serial, bmc_mac_address) = match self {
            Self::Rack { rack_id, .. } => return format!("rack {rack_id}"),
            Self::Machine {
                chassis_serial_number,
                bmc_mac_address,
                ..
            } => (DeviceKind::Machine, chassis_serial_number, bmc_mac_address),
            Self::Switch {
                switch_serial_number,
                bmc_mac_address,
                ..
            } => (DeviceKind::Switch, switch_serial_number, bmc_mac_address),
            Self::PowerShelf {
                shelf_serial_number,
                bmc_mac_address,
                ..
            } => (DeviceKind::PowerShelf, shelf_serial_number, bmc_mac_address),
        };
        format!("{kind} {serial} ({bmc_mac_address})")
    }

    /// The request bodies that register this record, in send order: a rack
    /// declares its own group, when it has one, before the rack itself.
    /// Static BMC addresses are left unset; real environments set them
    /// through the admin CLI or API when DHCP discovery is not used.
    pub fn into_requests(self) -> Vec<ExpectedRequest> {
        match self {
            Self::Rack {
                rack_id,
                rack_profile_id,
                group,
            } => group
                .map(ExpectedRequest::RackGroup)
                .into_iter()
                .chain([ExpectedRequest::Rack(ExpectedRack {
                    rack_group_id: None,
                    rack_id: Some(rack_id),
                    rack_profile_id: Some(rack_profile_id),
                    metadata: None,
                })])
                .collect(),
            Self::Machine {
                bmc_mac_address,
                chassis_serial_number,
                rack_id,
                dpu_policy,
                dpf_enabled,
                interfaces,
            } => vec![ExpectedRequest::Machine(ExpectedMachine {
                bmc_mac_address,
                bmc_username: DUMMY_FACTORY_USERNAME.to_string(),
                bmc_password: DUMMY_FACTORY_PASSWORD.to_string(),
                chassis_serial_number,
                fallback_dpu_serial_numbers: Vec::new(),
                metadata: None,
                sku_id: None,
                id: None,
                host_nics: interfaces,
                replace_host_nics: false,
                replace_dpu_loopbacks: false,
                rack_id,
                default_pause_ingestion_and_poweron: None,
                #[allow(deprecated)]
                dpf_enabled,
                is_dpf_enabled: Some(dpf_enabled),
                bmc_ip_address: None,
                bmc_retain_credentials: None,
                dpu_mode: dpu_policy.map(|policy| rpc::forge::DpuMode::from(policy) as i32),
                bmc_ip_allocation: None,
                host_lifecycle_profile: None,
                server_name: None,
            })],
            Self::Switch {
                bmc_mac_address,
                switch_serial_number,
                nvos_mac_addresses,
                rack_id,
            } => {
                // The first MAC is the primary NVOS port; any remainder are non-primary.
                let nvos_interfaces = nvos_mac_addresses
                    .into_iter()
                    .enumerate()
                    .map(|(index, mac_address)| rpc::forge::ExpectedNvosInterface {
                        mac_address,
                        is_primary: index == 0,
                    })
                    .collect();
                vec![ExpectedRequest::Switch(ExpectedSwitch {
                    expected_switch_id: None,
                    bmc_mac_address,
                    nvos_interfaces,
                    bmc_username: DUMMY_FACTORY_USERNAME.to_string(),
                    bmc_password: DUMMY_FACTORY_PASSWORD.to_string(),
                    switch_serial_number,
                    // The credentials the simulated switch's NVOS accepts until
                    // NICo rotates them.
                    nvos_username: Some(DUMMY_NVOS_USERNAME.to_string()),
                    nvos_password: Some(DUMMY_FACTORY_PASSWORD.to_string()),
                    bmc_ip_address: String::new(),
                    nvos_ip_address: None,
                    metadata: None,
                    rack_id,
                    bmc_retain_credentials: None,
                })]
            }
            Self::PowerShelf {
                bmc_mac_address,
                shelf_serial_number,
                rack_id,
            } => vec![ExpectedRequest::PowerShelf(ExpectedPowerShelf {
                expected_power_shelf_id: None,
                bmc_mac_address,
                bmc_username: DUMMY_FACTORY_USERNAME.to_string(),
                bmc_password: DUMMY_FACTORY_PASSWORD.to_string(),
                shelf_serial_number,
                bmc_ip_address: String::new(),
                metadata: None,
                rack_id,
                bmc_retain_credentials: Some(true),
            })],
        }
    }
}

pub struct DpuNetworkStatusArgs<'a> {
    pub dpu_machine_id: DpuMachineId,
    pub network_config_version: String,
    pub instance_network_config_version: Option<String>,
    pub instance_config_version: Option<String>,
    pub instance_id: Option<InstanceId>,
    pub interfaces: Vec<rpc::forge::InstanceInterfaceStatusObservation>,
    pub machine_config: &'a MachineConfig,
}

impl ApiClient {
    pub async fn discover_dhcp(
        &self,
        mac_address: MacAddress,
        relay_address: String,
        circuit_id: Option<String>,
        vendor_class: Option<&str>,
    ) -> ClientApiResult<rpc::forge::DhcpRecord> {
        let dhcp_discovery = rpc::forge::DhcpDiscovery {
            mac_address: mac_address.to_string(),
            relay_address,
            vendor_string: vendor_class.map(str::to_owned),
            link_address: None,
            circuit_id,
            remote_id: None,
            desired_address: None,
            address_family: None,
            message_kind: None,
            duid: None,
        };
        let out = self
            .0
            .discover_dhcp(dhcp_discovery)
            .await
            .map_err(ClientApiError::InvocationError)?;

        Ok(out)
    }

    pub async fn get_machine_interface(
        &self,
        id: MachineInterfaceId,
    ) -> ClientApiResult<rpc::forge::InterfaceList> {
        let interface_search_query = rpc::forge::InterfaceSearchQuery {
            id: Some(id),
            ip: None,
        };
        let out = self
            .0
            .find_interfaces(interface_search_query)
            .await
            .map_err(ClientApiError::InvocationError)?;

        Ok(out)
    }

    pub async fn discover_machine(
        &self,
        machine_info: &MachineInfo,
        discovery_data: MockDiscoveryData,
    ) -> ClientApiResult<rpc::forge::MachineDiscoveryResult> {
        let MockDiscoveryData {
            machine_interface_id,
            tpm_ek_certificate,
        } = discovery_data;
        let mut machine_discovery_info = crate::discovery_info::for_machine(machine_info);
        if matches!(machine_info, MachineInfo::Host(_)) {
            machine_discovery_info.tpm_ek_certificate =
                Some(BASE64_STANDARD.encode(tpm_ek_certificate.ok_or_else(|| {
                    ClientApiError::ConfigError("No TPM EK certificate waa supplied".to_string())
                })?))
        }
        let discovery_reporter = match machine_info {
            MachineInfo::Host(_) => rpc::MachineDiscoveryReporter::Scout,
            MachineInfo::Dpu(_) => rpc::MachineDiscoveryReporter::DpuAgent,
        };
        let mdi = rpc::forge::MachineDiscoveryInfo {
            machine_interface_id: Some(machine_interface_id),
            discovery_data: Some(rpc::DiscoveryData::Info(machine_discovery_info)),
            create_machine: true,
            discovery_reporter: discovery_reporter as i32,
            ..Default::default()
        };

        let out = self
            .0
            .discover_machine(mdi)
            .await
            .map_err(ClientApiError::InvocationError)?;

        Ok(out)
    }

    pub async fn record_dpu_network_status(
        &self,
        DpuNetworkStatusArgs {
            dpu_machine_id,
            network_config_version,
            instance_network_config_version,
            instance_config_version,
            instance_id,
            interfaces,
            machine_config,
        }: DpuNetworkStatusArgs<'_>,
    ) -> ClientApiResult<()> {
        let dpu_machine_id = Some(dpu_machine_id);

        let dpu_agent_version = machine_config
            .dpu_agent_version
            .clone()
            .or(Some(carbide_version::v!(build_version).to_string()));

        self.0
            .record_dpu_network_status(rpc::forge::DpuNetworkStatus {
                dpu_health: Some(rpc::health::HealthReport {
                    source: "forge-dpu-agent".to_string(),
                    triggered_by: None,
                    observed_at: None,
                    successes: Vec::new(),
                    alerts: Vec::new(),
                }),
                dpu_machine_id,
                observed_at: None,
                network_config_version: Some(network_config_version),
                instance_config_version,
                instance_network_config_version,
                interfaces,
                network_config_error: None,
                instance_id,
                dpu_agent_version,
                client_certificate_expiry_unix_epoch_secs: None,
                fabric_interfaces: vec![],
                last_dhcp_requests: vec![],
                dpu_extension_service_version: None,
                dpu_extension_services: vec![],
                astra_config_status: None,
                lldp: None,
                lldp_snapshot_fingerprint: None,
                service_vpc_network_observation: None,
            })
            .await
            .map_err(ClientApiError::InvocationError)
    }

    pub async fn force_delete_machine(
        &self,
        machine_id: String,
    ) -> ClientApiResult<rpc::forge::AdminForceDeleteMachineResponse> {
        self.0
            .admin_force_delete_machine(rpc::forge::AdminForceDeleteMachineRequest {
                host_query: machine_id,
                delete_interfaces: true,
                delete_bmc_interfaces: true,
                delete_bmc_credentials: false,
                allow_delete_with_orphaned_dpf_crds: false,
                delete_bmc_suppressions: false,
                delete_retained_boot_interfaces: false,
                allow_delete_with_instance_type: true,
                allow_delete_with_instance: true,
                release_preserved_addresses: false,
                wait_for_instance_dpu: false,
            })
            .await
            .map_err(ClientApiError::InvocationError)
    }

    pub async fn force_delete_switch_by_bmc(
        &self,
        bmc_mac: String,
    ) -> ClientApiResult<Option<SwitchId>> {
        let mut ids = self
            .0
            .find_switch_ids(rpc::forge::SwitchSearchFilter {
                bmc_mac: Some(bmc_mac.clone()),
                ..Default::default()
            })
            .await
            .map_err(ClientApiError::InvocationError)?
            .ids;
        if ids.len() > 1 {
            return Err(ClientApiError::ConfigError(format!(
                "multiple switches found for BMC MAC address {bmc_mac}"
            )));
        }
        let Some(switch_id) = ids.pop() else {
            return Ok(None);
        };
        self.0
            .admin_force_delete_switch(rpc::forge::AdminForceDeleteSwitchRequest {
                switch_id: Some(switch_id),
                delete_interfaces: true,
                delete_bmc_suppressions: false,
            })
            .await
            .map_err(ClientApiError::InvocationError)?;
        Ok(Some(switch_id))
    }

    pub async fn force_delete_power_shelf_by_bmc(
        &self,
        bmc_mac: String,
    ) -> ClientApiResult<Option<PowerShelfId>> {
        let mut ids = self
            .0
            .find_power_shelf_ids(rpc::forge::PowerShelfSearchFilter {
                bmc_mac: Some(bmc_mac.clone()),
                ..Default::default()
            })
            .await
            .map_err(ClientApiError::InvocationError)?
            .ids;
        if ids.len() > 1 {
            return Err(ClientApiError::ConfigError(format!(
                "multiple power shelves found for BMC MAC address {bmc_mac}"
            )));
        }
        let Some(power_shelf_id) = ids.pop() else {
            return Ok(None);
        };
        self.0
            .admin_force_delete_power_shelf(rpc::forge::AdminForceDeletePowerShelfRequest {
                power_shelf_id: Some(power_shelf_id),
                delete_interfaces: true,
                delete_bmc_suppressions: false,
            })
            .await
            .map_err(ClientApiError::InvocationError)?;
        Ok(Some(power_shelf_id))
    }

    pub async fn machine_validation_complete(
        &self,
        machine_id: &MachineId,
        validation_id: &MachineValidationId,
    ) -> ClientApiResult<()> {
        self.0
            .machine_validation_completed(rpc::forge::MachineValidationCompletedRequest {
                machine_id: Some(*machine_id),
                machine_validation_error: None,
                validation_id: Some(*validation_id),
            })
            .await
            .map_err(ClientApiError::InvocationError)
            .map(|_| ())
    }

    pub async fn cleanup_complete(&self, machine_id: &MachineId) -> ClientApiResult<()> {
        let cleanup_info = rpc::MachineCleanupInfo {
            machine_id: Some(*machine_id),
            nvme: Some(CleanupStepResult {
                result: 0,
                message: "".to_string(),
            }),
            ram: Some(CleanupStepResult {
                result: 0,
                message: "".to_string(),
            }),
            mem_overwrite: Some(CleanupStepResult {
                result: 0,
                message: "".to_string(),
            }),
            ib: Some(CleanupStepResult {
                result: 0,
                message: "".to_string(),
            }),
            ..Default::default()
        };

        self.0
            .cleanup_machine_completed(cleanup_info)
            .await
            .map_err(ClientApiError::InvocationError)
            .map(|_| ())
    }

    pub async fn configure_bmc_proxy_host(&self, host: String) -> ClientApiResult<()> {
        self.0
            .set_dynamic_config(SetDynamicConfigRequest {
                setting: ConfigSetting::BmcProxy as i32,
                value: host,
                expiry: None,
            })
            .await
            .map_err(ClientApiError::InvocationError)
    }

    /// Registers one expected inventory record of any supported kind by
    /// sending its request bodies in order.
    pub(crate) async fn add_expected_record(&self, record: ExpectedRecord) -> ClientApiResult<()> {
        for request in record.into_requests() {
            match request {
                ExpectedRequest::RackGroup(group) => self.ensure_expected_rack_group(group).await?,
                ExpectedRequest::Rack(rack) => self.ensure_expected_rack(rack).await?,
                ExpectedRequest::Machine(machine) => self
                    .0
                    .add_expected_machine(machine)
                    .await
                    .map_err(ClientApiError::InvocationError)?,
                ExpectedRequest::Switch(switch) => self
                    .0
                    .add_expected_switch(switch)
                    .await
                    .map_err(ClientApiError::InvocationError)?,
                ExpectedRequest::PowerShelf(power_shelf) => self
                    .0
                    .add_expected_power_shelf(power_shelf)
                    .await
                    .map_err(ClientApiError::InvocationError)?,
            }
        }
        Ok(())
    }

    /// The expected rack group declaring each rack, for every declared rack.
    pub(crate) async fn declared_rack_groups(
        &self,
    ) -> ClientApiResult<BTreeMap<RackId, ExpectedRackGroup>> {
        let groups = self
            .0
            .get_all_expected_rack_groups()
            .await
            .map_err(ClientApiError::InvocationError)?;
        Ok(groups
            .expected_rack_groups
            .into_iter()
            .flat_map(|group| {
                let rack_ids = group
                    .racks
                    .iter()
                    .filter_map(|rack| rack.rack_id.clone())
                    .collect::<Vec<_>>();
                rack_ids
                    .into_iter()
                    .map(move |rack_id| (rack_id, group.clone()))
            })
            .collect())
    }

    /// Declares a per-rack expected rack group. A group already present under
    /// that ID is accepted only if it declares the group's rack.
    async fn ensure_expected_rack_group(&self, group: ExpectedRackGroup) -> ClientApiResult<()> {
        let rack_group_id = group
            .rack_group_id
            .as_ref()
            .map(RackGroupId::to_string)
            .unwrap_or_default();
        let rack_ids = group
            .racks
            .iter()
            .filter_map(|rack| rack.rack_id.clone())
            .collect::<Vec<_>>();
        match self.0.add_expected_rack_group(group).await {
            Ok(()) => Ok(()),
            Err(status) if status.code() == tonic::Code::AlreadyExists => {
                let existing = self
                    .0
                    .get_expected_rack_group(ExpectedRackGroupRequest { rack_group_id })
                    .await
                    .map_err(ClientApiError::InvocationError)?;
                rack_ids
                    .iter()
                    .try_for_each(|rack_id| existing_group_declares_rack(&existing, rack_id))
            }
            Err(status) => Err(ClientApiError::InvocationError(status)),
        }
    }

    /// Declares the expected rack. A rack already present is accepted only
    /// if it carries the same profile.
    async fn ensure_expected_rack(&self, expected_rack: ExpectedRack) -> ClientApiResult<()> {
        let (Some(rack_id), Some(rack_profile_id)) = (
            expected_rack.rack_id.clone(),
            expected_rack.rack_profile_id.clone(),
        ) else {
            return Err(ClientApiError::ConfigError(
                "an expected rack needs a rack_id and a rack_profile_id".to_string(),
            ));
        };

        match self.0.add_expected_rack(expected_rack).await {
            Ok(()) => Ok(()),
            Err(status) if status.code() == tonic::Code::AlreadyExists => {
                let existing = self
                    .0
                    .get_expected_rack(ExpectedRackRequest {
                        rack_id: rack_id.to_string(),
                    })
                    .await
                    .map_err(ClientApiError::InvocationError)?;
                existing_rack_has_profile(&existing, &rack_id, &rack_profile_id)
            }
            Err(status) => Err(ClientApiError::InvocationError(status)),
        }
    }
}

/// Accepts an existing rack only if it carries the configured profile.
pub(crate) fn existing_rack_has_profile(
    existing: &ExpectedRack,
    rack_id: &RackId,
    rack_profile_id: &RackProfileId,
) -> ClientApiResult<()> {
    if existing.rack_profile_id.as_ref() == Some(rack_profile_id) {
        return Ok(());
    }
    let existing_profile_id = existing
        .rack_profile_id
        .as_ref()
        .map(RackProfileId::as_str)
        .unwrap_or("<missing>");
    Err(ClientApiError::ConfigError(format!(
        "Expected rack {rack_id} already exists with rack_profile_id {existing_profile_id}, not {rack_profile_id}"
    )))
}

/// Accepts an existing group only if it declares the rack.
fn existing_group_declares_rack(
    group: &ExpectedRackGroup,
    rack_id: &RackId,
) -> ClientApiResult<()> {
    if group
        .racks
        .iter()
        .any(|rack| rack.rack_id.as_ref() == Some(rack_id))
    {
        return Ok(());
    }
    let rack_group_id = group
        .rack_group_id
        .as_ref()
        .map(RackGroupId::as_str)
        .unwrap_or("<missing>");
    Err(ClientApiError::ConfigError(format!(
        "Expected rack group {rack_group_id} already exists but does not declare rack {rack_id}"
    )))
}

#[cfg(test)]
mod tests {
    use carbide_test_support::{Case, Check, Outcome, check_cases, check_values};
    use rpc::forge::{ExpectedNvosInterface, ExpectedRackGroupRack, NetworkSegmentType};

    use super::*;

    fn group(rack_group_id: &str, rack_ids: &[&str]) -> ExpectedRackGroup {
        ExpectedRackGroup {
            rack_group_id: Some(RackGroupId::new(rack_group_id)),
            topology: "nvl72".to_string(),
            protocol: "NVLINK_V5".to_string(),
            metadata: None,
            racks: rack_ids
                .iter()
                .map(|rack_id| ExpectedRackGroupRack {
                    rack_id: Some(RackId::new(*rack_id)),
                    members: Vec::new(),
                })
                .collect(),
        }
    }

    #[test]
    fn existing_group_must_declare_the_rack() {
        let rack_id = RackId::new("rack-001");
        check_cases(
            [
                Case {
                    scenario: "group declares the rack",
                    input: group("rack-001", &["rack-001"]),
                    expect: Outcome::Yields(()),
                },
                Case {
                    scenario: "group declares another rack only",
                    input: group("rack-001", &["rack-002"]),
                    expect: Outcome::FailsWith(
                        "configuration error: Expected rack group rack-001 already exists but does not declare rack rack-001"
                            .to_string(),
                    ),
                },
            ],
            |group| {
                existing_group_declares_rack(&group, &rack_id).map_err(|error| error.to_string())
            },
        );
    }

    /// The bodies `add_expected_machine`, `add_expected_switch`,
    /// `add_expected_power_shelf` and `ensure_expected_rack` built inline
    /// before the records produced them.
    #[test]
    fn into_requests_matches_previous_bodies() {
        let rack_id = RackId::new("rack-001");
        let profile = RackProfileId::new("GB200_NVL72R1_C2G4_WIWYNN");
        let nic = ExpectedInterface {
            mac_address: Some("02:00:00:00:00:11".to_string()),
            primary: Some(true),
            network_segment_type: Some(NetworkSegmentType::HostInband as i32),
            ..Default::default()
        };
        let expected_rack = ExpectedRack {
            rack_group_id: None,
            rack_id: Some(rack_id.clone()),
            rack_profile_id: Some(profile.clone()),
            metadata: None,
        };
        check_values(
            [
                Check {
                    scenario: "host in NIC mode",
                    input: ExpectedRecord::Machine {
                        bmc_mac_address: "02:00:00:00:00:01".to_string(),
                        chassis_serial_number: "HOST1".to_string(),
                        rack_id: Some(rack_id.clone()),
                        dpu_policy: Some(HostDpuPolicy::Nic),
                        dpf_enabled: false,
                        interfaces: vec![nic.clone()],
                    },
                    expect: vec![ExpectedRequest::Machine(ExpectedMachine {
                        bmc_mac_address: "02:00:00:00:00:01".to_string(),
                        bmc_username: DUMMY_FACTORY_USERNAME.to_string(),
                        bmc_password: DUMMY_FACTORY_PASSWORD.to_string(),
                        chassis_serial_number: "HOST1".to_string(),
                        fallback_dpu_serial_numbers: Vec::new(),
                        metadata: None,
                        sku_id: None,
                        id: None,
                        host_nics: vec![nic],
                        replace_host_nics: false,
                        replace_dpu_loopbacks: false,
                        rack_id: Some(rack_id.clone()),
                        default_pause_ingestion_and_poweron: None,
                        #[allow(deprecated)]
                        dpf_enabled: false,
                        is_dpf_enabled: Some(false),
                        bmc_ip_address: None,
                        bmc_retain_credentials: None,
                        dpu_mode: Some(rpc::forge::DpuMode::from(HostDpuPolicy::Nic) as i32),
                        bmc_ip_allocation: None,
                        host_lifecycle_profile: None,
                        server_name: None,
                    })],
                },
                Check {
                    scenario: "host with managed DPUs declares no DPU mode",
                    input: ExpectedRecord::Machine {
                        bmc_mac_address: "02:00:00:00:00:02".to_string(),
                        chassis_serial_number: "HOST2".to_string(),
                        rack_id: None,
                        dpu_policy: None,
                        dpf_enabled: true,
                        interfaces: Vec::new(),
                    },
                    expect: vec![ExpectedRequest::Machine(ExpectedMachine {
                        bmc_mac_address: "02:00:00:00:00:02".to_string(),
                        bmc_username: DUMMY_FACTORY_USERNAME.to_string(),
                        bmc_password: DUMMY_FACTORY_PASSWORD.to_string(),
                        chassis_serial_number: "HOST2".to_string(),
                        fallback_dpu_serial_numbers: Vec::new(),
                        metadata: None,
                        sku_id: None,
                        id: None,
                        host_nics: Vec::new(),
                        replace_host_nics: false,
                        replace_dpu_loopbacks: false,
                        rack_id: None,
                        default_pause_ingestion_and_poweron: None,
                        #[allow(deprecated)]
                        dpf_enabled: true,
                        is_dpf_enabled: Some(true),
                        bmc_ip_address: None,
                        bmc_retain_credentials: None,
                        dpu_mode: None,
                        bmc_ip_allocation: None,
                        host_lifecycle_profile: None,
                        server_name: None,
                    })],
                },
                Check {
                    scenario: "switch declares its first NVOS MAC as primary",
                    input: ExpectedRecord::Switch {
                        bmc_mac_address: "02:00:00:00:00:03".to_string(),
                        switch_serial_number: "MT1".to_string(),
                        nvos_mac_addresses: vec![
                            "02:00:00:00:00:31".to_string(),
                            "02:00:00:00:00:32".to_string(),
                        ],
                        rack_id: Some(rack_id.clone()),
                    },
                    expect: vec![ExpectedRequest::Switch(ExpectedSwitch {
                        expected_switch_id: None,
                        bmc_mac_address: "02:00:00:00:00:03".to_string(),
                        nvos_interfaces: vec![
                            ExpectedNvosInterface {
                                mac_address: "02:00:00:00:00:31".to_string(),
                                is_primary: true,
                            },
                            ExpectedNvosInterface {
                                mac_address: "02:00:00:00:00:32".to_string(),
                                is_primary: false,
                            },
                        ],
                        bmc_username: DUMMY_FACTORY_USERNAME.to_string(),
                        bmc_password: DUMMY_FACTORY_PASSWORD.to_string(),
                        switch_serial_number: "MT1".to_string(),
                        nvos_username: Some(DUMMY_NVOS_USERNAME.to_string()),
                        nvos_password: Some(DUMMY_FACTORY_PASSWORD.to_string()),
                        bmc_ip_address: String::new(),
                        nvos_ip_address: None,
                        metadata: None,
                        rack_id: Some(rack_id.clone()),
                        bmc_retain_credentials: None,
                    })],
                },
                Check {
                    scenario: "power shelf retains its BMC credentials",
                    input: ExpectedRecord::PowerShelf {
                        bmc_mac_address: "02:00:00:00:00:04".to_string(),
                        shelf_serial_number: "PS1".to_string(),
                        rack_id: None,
                    },
                    expect: vec![ExpectedRequest::PowerShelf(ExpectedPowerShelf {
                        expected_power_shelf_id: None,
                        bmc_mac_address: "02:00:00:00:00:04".to_string(),
                        bmc_username: DUMMY_FACTORY_USERNAME.to_string(),
                        bmc_password: DUMMY_FACTORY_PASSWORD.to_string(),
                        shelf_serial_number: "PS1".to_string(),
                        bmc_ip_address: String::new(),
                        metadata: None,
                        rack_id: None,
                        bmc_retain_credentials: Some(true),
                    })],
                },
                Check {
                    scenario: "rack declares its own group first",
                    input: ExpectedRecord::Rack {
                        rack_id: rack_id.clone(),
                        rack_profile_id: profile.clone(),
                        group: Some(group("rack-001", &["rack-001"])),
                    },
                    expect: vec![
                        ExpectedRequest::RackGroup(group("rack-001", &["rack-001"])),
                        ExpectedRequest::Rack(expected_rack.clone()),
                    ],
                },
                Check {
                    scenario: "rack an existing group declares sends the rack only",
                    input: ExpectedRecord::Rack {
                        rack_id,
                        rack_profile_id: profile,
                        group: None,
                    },
                    expect: vec![ExpectedRequest::Rack(expected_rack)],
                },
            ],
            ExpectedRecord::into_requests,
        );
    }
}

#[cfg(test)]
mod force_delete_tests {
    use std::convert::Infallible;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use bmc_mock::mac_address_pool::{
        Config as MacAddressConfig, MacAddressPool, PoolConfig, RangesConfig,
    };
    use bytes::Bytes;
    use http_body_util::{BodyExt, Full, StreamBody};
    use hyper::body::{Frame, Incoming};
    use hyper::server::conn::http2;
    use hyper::service::service_fn;
    use hyper::{Request, Response};
    use hyper_util::rt::{TokioExecutor, TokioIo};
    use prost::Message;
    use rpc::carbide_tls_client::{ApiConfig, CarbideClientConfig};
    use rpc::forge::{AdminForceDeleteMachineRequest, AdminForceDeleteMachineResponse, BuildInfo};
    use tokio::net::TcpListener;
    use tokio::sync::{Notify, mpsc};

    use super::*;
    use crate::{MachineATron, MachineATronContext, SimulatorLifecycle};

    /// Stores shutdown RPC observations and synchronization so a cleanup can remain pending while its sibling fails.
    #[derive(Default)]
    struct ShutdownCalls {
        completed_queries: Mutex<Vec<String>>,
        proxy_values: Mutex<Vec<String>>,
        proxy_removal_started: Mutex<Option<tokio::time::Instant>>,
        sibling_started: Notify,
        release_sibling: Notify,
        incomplete_replied: Notify,
    }

    /// Sends explicit Instance-deletion consent and preserves an incomplete response so callers decide when cleanup is finished.
    #[tokio::test]
    async fn cleanup_authorizes_deleting_the_simulated_attached_instance() {
        // Require Instance-deletion consent and return incomplete cleanup from the mock.
        let (client, server) = cleanup_client(false, "simulated-machine").await;

        // Request one force-delete attempt through the production API client.
        let result = client
            .force_delete_machine("simulated-machine".to_string())
            .await;

        // Preserve the incomplete result rather than converting request acceptance into completion.
        server.abort();
        assert!(
            server
                .await
                .expect_err("mock remains available")
                .is_cancelled(),
            "mock cleanup server must only be stopped by the test"
        );
        assert!(
            !result
                .expect("explicit cleanup authorizes its simulated Machine and attached Instance")
                .all_done
        );
    }

    /// Accepts completed cleanup and rejects retained resources with Core's Machine ID so shutdown cannot report a false success.
    #[tokio::test]
    async fn cleanup_reports_incomplete_force_deletion() {
        use crate::device_handle::DeviceHandle;
        use crate::host_machine::MachineHandle;

        for (scenario, all_done, retained_machine) in [
            // Completed deletion remains successful for existing cleanup-on-quit callers.
            ("completed deletion", true, None),
            // Pending service cleanup needs the returned Machine ID because the initial query is a MAC.
            (
                "retained service resources",
                false,
                Some("simulated-machine"),
            ),
        ] {
            // Use the MAC fallback with a stopped host handle, matching shutdown cleanup.
            // The remaining HostMachineInfo fields provide ordinary simulated host inventory.
            let mut host_info = MachineHandle::for_control_test(Vec::new(), None)
                .host_info()
                .clone();
            // Provide a MAC lookup because this stopped handle has no observed Machine ID.
            host_info.non_dpu_mac_address = Some(MacAddress::new([2, 0, 0, 0, 0, 3]));
            let handle = MachineHandle::for_control_test_host(host_info, Vec::new(), "test");
            let device = DeviceHandle::machine(handle);
            let (client, server) = cleanup_client(all_done, "02:00:00:00:00:03").await;

            // Exercise the same deletion entrypoint used by DeviceSimulator cleanup.
            let result = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                device.delete_machine_from_api(client),
            )
            .await
            .expect("one cleanup attempt must finish without waiting for stopped actors");

            // Require success only after deletion, and retain Core's returned Machine ID for an explicit retry.
            server.abort();
            assert!(
                server
                    .await
                    .expect_err("mock remains available")
                    .is_cancelled(),
                "{scenario}: mock cleanup server must only be stopped by the test"
            );
            match retained_machine {
                Some(machine_id) => {
                    let error = result.expect_err(scenario).to_string();
                    assert!(error.contains(machine_id), "{scenario}: {error}");
                    assert!(error.contains("incomplete"), "{scenario}: {error}");
                    assert!(
                        error.contains("AdminForceDeleteMachine"),
                        "{scenario}: {error}"
                    );
                }
                None => result.expect(scenario),
            }
        }
    }

    /// When one cleanup is incomplete, shutdown finishes or times out its sibling, saves both devices, and clears the BMC proxy before returning the failure.
    ///
    /// Preserve restart recovery and remove stale BMC routing even when API cleanup needs an explicit retry.
    #[tokio::test]
    async fn shutdown_finalizes_after_incomplete_cleanup() {
        for (scenario, sibling_times_out) in [
            // A released sibling must finish before the incomplete result leaves shutdown.
            ("sibling cleanup completes", false),
            // A stalled sibling must expire without preventing snapshot persistence or proxy removal.
            ("sibling cleanup reaches its deadline", true),
        ] {
            // Use two real paused hosts so run can resume and stop actors through its normal entrypoint.
            let listener = TcpListener::bind("127.0.0.1:0")
                .await
                .expect("mock Core binds");
            let address = listener.local_addr().expect("mock Core has an address");
            let persist_dir = tempfile::tempdir().expect("snapshot directory is available");
            let mut app_context = MachineATronContext::for_test();
            let context = Arc::get_mut(&mut app_context).expect("new context is not shared");
            // Skip expected-inventory RPCs because the mock only serves shutdown operations.
            context.app_config.register_expected_machines = false;
            // Enable cleanup, saved device snapshots, and BMC-proxy configuration for the shutdown call.
            context.app_config.cleanup_on_quit = true;
            context.app_config.persist_dir = Some(persist_dir.path().to_path_buf());
            context.app_config.configure_carbide_bmc_proxy_host = Some("127.0.0.1".to_string());
            // Give the two zero-DPU actors distinct MAC queries without unrelated DPU cleanup.
            Arc::make_mut(
                context
                    .app_config
                    .machines
                    .get_mut("config")
                    .expect("host config exists"),
            )
            .host_count = 2;
            context.mac_address_pool =
                Arc::new(Mutex::new(MacAddressPool::new(MacAddressConfig {
                    pool: Some(
                        PoolConfig::new(MacAddress::new([2, 0, 0, 0, 0, 0]), 24)
                            .expect("valid MAC pool"),
                    ),
                    ranges: Some(
                        RangesConfig::new(MacAddress::new([6, 0, 0, 0, 0, 0]), 32, 8)
                            .expect("valid MAC ranges"),
                    ),
                })));
            context.carbide_api_client = CarbideApiClient::new(
                &ApiConfig::new(
                    &format!("http://{address}"),
                    &CarbideClientConfig::default(),
                ),
                carbide_instrument::Instrumentation::noop(),
            );
            let mut mat = MachineATron::new(app_context.clone());
            let (simulators, _) = mat
                .make_devices(true)
                .await
                .expect("paused hosts are constructed");
            // Capture the snapshots and MAC queries before either paused host has observed a Machine ID.
            let expected_devices = simulators
                .devices()
                .iter()
                .map(SimulatorLifecycle::persisted)
                .collect::<Vec<_>>();
            let pending_query = expected_devices[0]
                .non_dpu_mac_address
                .expect("zero-DPU host has a MAC")
                .to_string();
            let sibling_query = expected_devices[1]
                .non_dpu_mac_address
                .expect("zero-DPU sibling has a MAC")
                .to_string();

            // Return incomplete cleanup after the sibling has returned headers and its body is held.
            let calls = Arc::new(ShutdownCalls::default());
            let server_calls = calls.clone();
            let server_pending_query = pending_query.clone();
            let server_sibling_query = sibling_query.clone();
            let server = tokio::spawn(async move {
                let (stream, _) = listener.accept().await.expect("mock accepts Core client");
                http2::Builder::new(TokioExecutor::new())
                    .serve_connection(
                        TokioIo::new(stream),
                        service_fn(move |request: Request<Incoming>| {
                            let calls = server_calls.clone();
                            let pending_query = server_pending_query.clone();
                            let sibling_query = server_sibling_query.clone();
                            async move {
                                // Complete the handshake before observing cleanup and proxy configuration.
                                let method = request.uri().path().to_string();
                                if method == rpc::service_path!("Version") {
                                    return Ok::<_, Infallible>(
                                        grpc_response(BuildInfo::default())
                                            .map(BodyExt::boxed_unsync),
                                    );
                                }
                                let body = request
                                    .into_body()
                                    .collect()
                                    .await
                                    .expect("RPC body is readable")
                                    .to_bytes();
                                let payload = body.get(5..).expect("RPC has a gRPC frame");
                                if method == rpc::service_path!("SetDynamicConfig") {
                                    let request = SetDynamicConfigRequest::decode(payload)
                                        .expect("proxy request decodes");
                                    assert_eq!(request.setting, ConfigSetting::BmcProxy as i32);
                                    // A timed-out sibling has no body; a released sibling must finish before proxy removal.
                                    if request.value.is_empty() {
                                        // Record proxy removal before replying so its timing is independent of shutdown completion.
                                        *calls
                                            .proxy_removal_started
                                            .lock()
                                            .expect("proxy removal time lock") =
                                            Some(tokio::time::Instant::now());
                                        assert_eq!(
                                            calls
                                                .completed_queries
                                                .lock()
                                                .expect("cleanup observations lock")
                                                .len(),
                                            if sibling_times_out { 1 } else { 2 },
                                            "proxy removal must follow both cleanup attempts"
                                        );
                                    }
                                    calls
                                        .proxy_values
                                        .lock()
                                        .expect("proxy observations lock")
                                        .push(request.value);
                                    return Ok(grpc_response(()).map(BodyExt::boxed_unsync));
                                }
                                assert_eq!(method, rpc::service_path!("AdminForceDeleteMachine"));
                                let request = AdminForceDeleteMachineRequest::decode(payload)
                                    .expect("cleanup request decodes");
                                // Return headers before holding the sibling body so the header timeout cannot satisfy this proof.
                                if request.host_query != pending_query {
                                    assert_eq!(request.host_query, sibling_query);
                                    let (parts, body) =
                                        grpc_response(AdminForceDeleteMachineResponse {
                                            all_done: true,
                                            ..Default::default()
                                        })
                                        .into_parts();
                                    let payload = body
                                        .collect()
                                        .await
                                        .expect("mock response encodes")
                                        .to_bytes();
                                    let body = StreamBody::new(futures::stream::once(async move {
                                        calls.sibling_started.notify_one();
                                        calls.release_sibling.notified().await;
                                        calls
                                            .completed_queries
                                            .lock()
                                            .expect("cleanup observations lock")
                                            .push(request.host_query);
                                        Ok::<_, Infallible>(Frame::data(payload))
                                    }));
                                    return Ok(Response::from_parts(parts, body.boxed_unsync()));
                                }
                                // Wait for the sibling body to be polled before returning incomplete cleanup.
                                calls.sibling_started.notified().await;
                                calls
                                    .completed_queries
                                    .lock()
                                    .expect("cleanup observations lock")
                                    .push(request.host_query);
                                calls.incomplete_replied.notify_one();
                                Ok(grpc_response(AdminForceDeleteMachineResponse {
                                    all_done: false,
                                    managed_host_machine_id: "retained-machine".to_string(),
                                    ..Default::default()
                                })
                                .map(BodyExt::boxed_unsync))
                            }
                        }),
                    )
                    .await
                    .expect("mock serves shutdown RPCs");
            });

            // Queue stop before run resumes the actors, avoiding unrelated DHCP or discovery operations.
            let (stop_tx, stop_rx) = mpsc::channel(1);
            stop_tx.send(()).await.expect("stop is queued");
            // Bound device timer startup between entering run and observing the stalled response body.
            let earliest_deadline = tokio::time::Instant::now() + Duration::from_secs(300);
            let mut shutdown = tokio::spawn(async move { mat.run(simulators, stop_rx).await });
            tokio::time::timeout(Duration::from_secs(5), calls.incomplete_replied.notified())
                .await
                .expect("incomplete cleanup response is produced");
            let latest_deadline = tokio::time::Instant::now() + Duration::from_secs(300);
            // Require the held sibling to keep shutdown pending after incomplete cleanup is reported.
            assert!(
                tokio::time::timeout(Duration::from_millis(50), &mut shutdown)
                    .await
                    .is_err(),
                "shutdown must await the held sibling after another cleanup is incomplete"
            );
            // Let the sibling finish or exhaust its own attempt before observing finalization.
            if sibling_times_out {
                // Account for live loopback setup before advancing close to the device deadline.
                tokio::time::pause();
                tokio::time::advance(
                    (earliest_deadline - Duration::from_secs(1))
                        .saturating_duration_since(tokio::time::Instant::now()),
                )
                .await;
                // Keep time live across the deadline so proxy removal cannot be delayed until a second clock jump.
                tokio::time::resume();
            } else {
                calls.release_sibling.notify_one();
            }
            // Allow the remaining device timeout before applying the existing finalization budget.
            let finalization_deadline = if sibling_times_out {
                latest_deadline + Duration::from_secs(5)
            } else {
                tokio::time::Instant::now() + Duration::from_secs(5)
            };
            let result = tokio::time::timeout_at(finalization_deadline, shutdown)
                .await
                .expect("device attempts and finalizers finish")
                .expect("shutdown task finishes normally");

            // Preserve the incomplete result after the sibling attempt and finalizers finish.
            let error = result
                .expect_err("incomplete cleanup remains an error")
                .to_string();
            assert!(
                error.contains("retained-machine") && error.contains("incomplete"),
                "{error}"
            );
            // Require the stalled sibling's deadline to pass before proxy removal begins.
            if sibling_times_out {
                assert!(
                    calls
                        .proxy_removal_started
                        .lock()
                        .expect("proxy removal time lock")
                        .expect("shutdown removes the proxy")
                        >= earliest_deadline,
                    "{scenario}: proxy removal must wait for the device deadline"
                );
            }
            // Observe the completed cleanup replies and the configured proxy followed by its removal.
            assert_eq!(
                *calls
                    .completed_queries
                    .lock()
                    .expect("cleanup observations lock"),
                if sibling_times_out {
                    vec![pending_query]
                } else {
                    vec![pending_query, sibling_query]
                }
            );
            assert_eq!(
                *calls.proxy_values.lock().expect("proxy observations lock"),
                vec![
                    app_context
                        .app_config
                        .bmc_proxy_address()
                        .expect("proxy is configured"),
                    String::new()
                ]
            );
            // Reload the saved snapshots to prove the shutdown write includes both devices.
            let persisted = app_context
                .app_config
                .read_persisted_devices()
                .expect("shutdown snapshot can be read")
                .expect("persistence is configured");
            assert_eq!(
                serde_json::to_value(&persisted["config"]).expect("saved devices serialize"),
                serde_json::to_value(&expected_devices).expect("original devices serialize")
            );
            // Release any response body still held by the timeout case before stopping the mock server.
            calls.release_sibling.notify_one();
            server.abort();
            assert!(
                server
                    .await
                    .expect_err("mock remains available")
                    .is_cancelled()
            );
        }
    }

    /// Serves a configurable force-delete result and validates the original query so cleanup tests exercise the real transport and request flags.
    async fn cleanup_client(
        all_done: bool,
        expected_query: &'static str,
    ) -> (ApiClient, tokio::task::JoinHandle<()>) {
        // Bind a local Core substitute that checks the production client's deletion request.
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("mock Core listener binds");
        let address = listener.local_addr().expect("mock listener has an address");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("mock accepts the client");
            http2::Builder::new(TokioExecutor::new())
                .serve_connection(
                    TokioIo::new(stream),
                    service_fn(move |request| mock_force_delete(request, all_done, expected_query)),
                )
                .await
                .expect("mock serves the cleanup RPC");
        });

        // Return the unmodified client to preserve its request encoding and response handling.
        let client_config = CarbideClientConfig::default();
        let client = CarbideApiClient::new(
            &ApiConfig::new(&format!("http://{address}"), &client_config),
            carbide_instrument::Instrumentation::noop(),
        );
        (ApiClient(client), server)
    }

    /// Checks explicit Instance consent and returns the requested completion result so tests distinguish an accepted request from deleted resources.
    async fn mock_force_delete(
        request: Request<Incoming>,
        all_done: bool,
        expected_query: &str,
    ) -> Result<Response<Full<Bytes>>, Infallible> {
        // Answer the client's version handshake before checking deletion-specific fields.
        if request.uri().path() == rpc::service_path!("Version") {
            return Ok(grpc_response(BuildInfo::default()));
        }
        assert_eq!(
            request.uri().path(),
            rpc::service_path!("AdminForceDeleteMachine")
        );
        // Decode the real gRPC request to verify the cleanup query and authorization flags.
        let body = request
            .into_body()
            .collect()
            .await
            .expect("cleanup request body is readable")
            .to_bytes();
        let request = AdminForceDeleteMachineRequest::decode(
            body.get(5..).expect("cleanup request has a gRPC frame"),
        )
        .expect("cleanup request decodes");

        assert_eq!(request.host_query, expected_query);
        assert!(!request.wait_for_instance_dpu);
        assert!(!request.allow_delete_with_orphaned_dpf_crds);

        // Require explicit Instance consent without bypassing service cleanup or opting into DPU waits.
        if request.allow_delete_with_instance {
            Ok(grpc_response(AdminForceDeleteMachineResponse {
                all_done,
                managed_host_machine_id: "simulated-machine".to_string(),
                ..Default::default()
            }))
        } else {
            Ok(Response::builder()
                .header("content-type", "application/grpc")
                .header("grpc-status", "9")
                .header(
                    "grpc-message",
                    "attached instance requires explicit deletion authorization",
                )
                .body(Full::new(Bytes::new()))
                .expect("rejection response is valid"))
        }
    }

    /// Encodes one successful gRPC message so the production client decodes the mock response normally.
    fn grpc_response(message: impl Message) -> Response<Full<Bytes>> {
        // Prefix the protobuf payload with the standard uncompressed gRPC frame header.
        let mut data = Vec::with_capacity(5 + message.encoded_len());
        data.push(0);
        data.extend_from_slice(&(message.encoded_len() as u32).to_be_bytes());
        message.encode(&mut data).expect("mock response encodes");
        Response::builder()
            .header("content-type", "application/grpc")
            .header("grpc-status", "0")
            .body(Full::new(Bytes::from(data)))
            .expect("success response is valid")
    }
}

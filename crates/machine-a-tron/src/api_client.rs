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
    MachinesByIdsRequest, SetDynamicConfigRequest,
};
use rpc::protos::forge_api_client::ForgeApiClient;

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
pub struct ApiClient(pub ForgeApiClient);

impl From<ForgeApiClient> for ApiClient {
    fn from(value: ForgeApiClient) -> Self {
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
                Some(BASE64_STANDARD.encode(tpm_ek_certificate.ok_or(
                    ClientApiError::ConfigError("No TPM EK certificate waa supplied".to_string()),
                )?))
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

    pub async fn get_machines(
        &self,
        machine_ids: Vec<MachineId>,
    ) -> ClientApiResult<Vec<rpc::Machine>> {
        let request = MachinesByIdsRequest {
            machine_ids,
            include_history: false,
        };
        let out = self
            .0
            .find_machines_by_ids(request)
            .await
            .map_err(ClientApiError::InvocationError)?;

        Ok(out.machines)
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

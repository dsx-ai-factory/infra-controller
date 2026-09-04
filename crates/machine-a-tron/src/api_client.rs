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

use base64::prelude::*;
use bmc_mock::{DUMMY_FACTORY_PASSWORD, DUMMY_FACTORY_USERNAME, MachineInfo};
use carbide_uuid::instance::InstanceId;
use carbide_uuid::machine::{MachineId, MachineInterfaceId};
use carbide_uuid::machine_validation::MachineValidationId;
use carbide_uuid::power_shelf::PowerShelfId;
use carbide_uuid::rack::{RackId, RackProfileId};
use carbide_uuid::switch::SwitchId;
use mac_address::MacAddress;
use model::expected_machine::HostDpuPolicy;
use rpc::forge::machine_cleanup_info::CleanupStepResult;
use rpc::forge::{
    ConfigSetting, ExpectedInterface, ExpectedMachine, ExpectedPowerShelf, ExpectedRack,
    ExpectedRackRequest, ExpectedSwitch, MachinesByIdsRequest, SetDynamicConfigRequest,
};
use rpc::protos::forge_api_client::ForgeApiClient;

use crate::MachineConfig;

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

const DUMMY_NVOS_USERNAME: &str = "admin";
const DUMMY_NVOS_PASSWORD: &str = "factory_password";

#[derive(Debug, Clone)]
pub struct ApiClient(pub ForgeApiClient);

impl From<ForgeApiClient> for ApiClient {
    fn from(value: ForgeApiClient) -> Self {
        ApiClient(value)
    }
}

pub struct DpuNetworkStatusArgs<'a> {
    pub dpu_machine_id: MachineId,
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
                allow_delete_with_instance_type: false,
                allow_delete_with_instance: false,
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

    /// Registers a mock expected machine. Static BMC (`bmc_ip_address`) is left unset here;
    /// real environments set it through the admin CLI / API when DHCP discovery is not used.
    /// `dpu_policy` is the per-host policy -- pass `Some(Ignore)` for zero-DPU
    /// mock hosts or `Some(Nic)` for DPU-in-NIC-mode mock hosts; `None` for
    /// normal DPU hosts.
    pub async fn add_expected_machine(
        &self,
        bmc_mac_address: String,
        chassis_serial_number: String,
        rack_id: Option<RackId>,
        dpu_policy: Option<HostDpuPolicy>,
        interfaces: Vec<ExpectedInterface>,
    ) -> ClientApiResult<()> {
        self.0
            .add_expected_machine(ExpectedMachine {
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
                dpf_enabled: true,
                is_dpf_enabled: Some(true),
                bmc_ip_address: None,
                bmc_retain_credentials: None,
                dpu_mode: dpu_policy.map(|policy| rpc::forge::DpuMode::from(policy) as i32),
                bmc_ip_allocation: None,
                host_lifecycle_profile: None,
            })
            .await
            .map_err(ClientApiError::InvocationError)
    }

    /// Registers a mock expected power shelf.
    pub async fn add_expected_power_shelf(
        &self,
        bmc_mac_address: String,
        shelf_serial_number: String,
        rack_id: Option<RackId>,
    ) -> ClientApiResult<()> {
        self.0
            .add_expected_power_shelf(ExpectedPowerShelf {
                expected_power_shelf_id: None,
                bmc_mac_address,
                bmc_username: DUMMY_FACTORY_USERNAME.to_string(),
                bmc_password: DUMMY_FACTORY_PASSWORD.to_string(),
                shelf_serial_number,
                bmc_ip_address: String::new(),
                metadata: None,
                rack_id,
                bmc_retain_credentials: Some(true),
            })
            .await
            .map_err(ClientApiError::InvocationError)
    }

    /// Registers a mock expected switch.
    pub async fn add_expected_switch(
        &self,
        bmc_mac_address: String,
        switch_serial_number: String,
        nvos_mac_addresses: Vec<String>,
        rack_id: Option<RackId>,
    ) -> ClientApiResult<()> {
        self.0
            .add_expected_switch(ExpectedSwitch {
                expected_switch_id: None,
                bmc_mac_address,
                nvos_mac_addresses,
                bmc_username: DUMMY_FACTORY_USERNAME.to_string(),
                bmc_password: DUMMY_FACTORY_PASSWORD.to_string(),
                switch_serial_number,
                nvos_username: Some(DUMMY_NVOS_USERNAME.to_string()),
                nvos_password: Some(DUMMY_NVOS_PASSWORD.to_string()),
                bmc_ip_address: String::new(),
                nvos_ip_address: None,
                metadata: None,
                rack_id,
                bmc_retain_credentials: None,
            })
            .await
            .map_err(ClientApiError::InvocationError)
    }

    pub async fn ensure_expected_rack(
        &self,
        rack_id: RackId,
        rack_profile_id: RackProfileId,
    ) -> ClientApiResult<()> {
        let expected_rack = ExpectedRack {
            rack_id: Some(rack_id.clone()),
            rack_profile_id: Some(rack_profile_id.clone()),
            metadata: None,
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
                if existing.rack_profile_id.as_ref() == Some(&rack_profile_id) {
                    Ok(())
                } else {
                    let existing_profile_id = existing
                        .rack_profile_id
                        .as_ref()
                        .map(RackProfileId::as_str)
                        .unwrap_or("<missing>");
                    Err(ClientApiError::ConfigError(format!(
                        "Expected rack {rack_id} already exists with rack_profile_id {existing_profile_id}, not {rack_profile_id}"
                    )))
                }
            }
            Err(status) => Err(ClientApiError::InvocationError(status)),
        }
    }
}

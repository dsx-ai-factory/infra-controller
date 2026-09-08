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
use std::collections::HashMap;
use std::future::Future;
use std::iter;
use std::net::IpAddr;

use carbide_secrets::credentials::{BmcCredentialType, CredentialKey, Credentials};
use carbide_uuid::machine::{DpuMachineId, HostMachineId, MachineId};
use carbide_uuid::machine_validation::MachineValidationId;
use carbide_uuid::rack::{RackId, RackProfileId};
use carbide_uuid::switch::SwitchId;
use db::machine_interface::find_by_mac_address;
use db::{
    DatabaseError, expected_machine as db_expected_machine, rack as db_rack, switch as db_switch,
};
use futures_util::FutureExt;
use health_report::HealthReport;
use model::address_selection_strategy::AddressSelectionStrategy;
use model::expected_machine::ExpectedMachine;
use model::hardware_info::HardwareInfo;
use model::machine::health_override::HARDWARE_HEALTH_OVERRIDE_PREFIX;
use model::machine::{
    BomValidating, BomValidatingContext, CleanupContext, CleanupState, DpfState, DpuInitState,
    FailureCause, FailureDetails, FailureSource, LockdownInfo, LockdownMode, LockdownState,
    MachineState, MachineValidatingState, ManagedHostState, ManagedHostStateSnapshot,
    MeasuringState, SpdmMeasuringState, ValidationState,
};
use model::rack::RackConfig;
use model::site_explorer::{Chassis, EndpointExplorationReport, EndpointType};
use model::switch::{NewSwitch, SwitchConfig};
use model::test_support::ManagedHostConfig;
use rpc::forge::forge_server::Forge;
use rpc::forge::{self, HealthReportEntry, InsertMachineHealthReportRequest};
use rpc::forge_agent_control_response::{Action, LegacyAction};
use rpc::machine_discovery::AttestKeyInfo;
use rpc::{DiscoveryData, DiscoveryInfo};
use sqlx::PgConnection;
use tonic::Request;

use super::dpu::create_machine_inventory;
use super::tpm_attestation::{AK_NAME_SERIALIZED, AK_PUB_SERIALIZED, EK_PUB_SERIALIZED};
use super::{discovery_completed, inject_machine_measurements, network_configured};
use crate::test_support::fixture_config::{FixtureDefault as _, ManagedHostConfigExt as _};
use crate::test_support::mac_address_pool::EXPECTED_SWITCH_NVOS_MAC_ADDRESS_POOL;
use crate::tests::common::api_fixtures::host::host_uefi_setup;
use crate::tests::common::api_fixtures::network_segment::{
    FIXTURE_ADMIN_NETWORK_SEGMENT_GATEWAY, FIXTURE_HOST_INBAND_NETWORK_SEGMENT_GATEWAY,
    FIXTURE_HOST_INBAND_NETWORK_SEGMENT_GATEWAY_2, FIXTURE_UNDERLAY_NETWORK_SEGMENT_GATEWAY,
};
use crate::tests::common::api_fixtures::{
    TestEnv, TestManagedHost, forge_agent_control, get_machine_validation_runs,
    machine_validation_completed, persist_machine_validation_result, update_machine_validation_run,
};
use crate::tests::common::rpc_builder::DhcpDiscovery;

async fn ensure_admin_interface_primary(
    env: &TestEnv,
    mac: mac_address::MacAddress,
) -> eyre::Result<()> {
    let mut txn = env.pool.begin().await?;
    let interfaces = db::machine_interface::find_by_mac_address(txn.as_mut(), mac).await?;
    if let Some(interface) = interfaces.first()
        && !interface.primary_interface
    {
        db::machine_interface::set_primary_interface(&interface.id, true, txn.as_mut()).await?;
    }
    txn.commit().await?;
    Ok(())
}

async fn current_host_state_and_cleanup_needed(
    env: &TestEnv,
    host_machine_id: HostMachineId,
) -> (ManagedHostState, bool) {
    let mut txn = env.db_txn().await;
    let machine = db::machine::find_one(
        txn.as_mut(),
        &host_machine_id,
        model::machine::machine_search_config::MachineSearchConfig::default(),
    )
    .await
    .unwrap()
    .unwrap();

    (
        machine.current_state().clone(),
        machine.status.last_cleanup_time.is_none(),
    )
}

async fn complete_initial_discovery_cleanup_if_needed(
    env: &TestEnv,
    host_machine_id: HostMachineId,
) {
    // Keep the shared fixture usable with both lifecycle shapes: older flows stay in discovery,
    // while newer flows require state-machine-owned cleanup before discovery can complete.
    let mut state = env
        .run_machine_state_controller_iteration_until_state_condition(
            &host_machine_id,
            20,
            |machine| {
                matches!(
                    machine.current_state(),
                    ManagedHostState::HostInit {
                        machine_state: MachineState::WaitingForDiscovery
                    } | ManagedHostState::WaitingForCleanup {
                        cleanup_context: CleanupContext::InitialDiscovery,
                        ..
                    }
                )
            },
        )
        .await;

    if matches!(
        state,
        ManagedHostState::HostInit {
            machine_state: MachineState::WaitingForDiscovery
        }
    ) {
        let (_, needs_initial_cleanup) =
            current_host_state_and_cleanup_needed(env, host_machine_id).await;
        if !needs_initial_cleanup {
            return;
        }

        env.run_machine_state_controller_iteration().await;
        (state, _) = current_host_state_and_cleanup_needed(env, host_machine_id).await;

        if matches!(
            state,
            ManagedHostState::HostInit {
                machine_state: MachineState::WaitingForDiscovery
            }
        ) {
            return;
        }
    }

    if !matches!(
        state,
        ManagedHostState::WaitingForCleanup {
            cleanup_state: CleanupState::HostCleanup { .. },
            cleanup_context: CleanupContext::InitialDiscovery,
        }
    ) {
        env.run_machine_state_controller_iteration_until_state_condition(
            &host_machine_id,
            3,
            |machine| {
                matches!(
                    machine.current_state(),
                    ManagedHostState::WaitingForCleanup {
                        cleanup_state: CleanupState::HostCleanup { .. },
                        cleanup_context: CleanupContext::InitialDiscovery,
                    }
                )
            },
        )
        .await;
    }

    let response = forge_agent_control(env, host_machine_id.into()).await;
    assert!(matches!(response.action, Some(Action::Reset(_))));
    assert_eq!(response.legacy_action, LegacyAction::Reset as i32);

    env.api
        .cleanup_machine_completed(Request::new(rpc::MachineCleanupInfo {
            machine_id: Some(host_machine_id.into()),
            ..Default::default()
        }))
        .await
        .unwrap();

    env.run_machine_state_controller_iteration_until_state_matches(
        &host_machine_id,
        3,
        ManagedHostState::HostInit {
            machine_state: MachineState::WaitingForDiscovery,
        },
    )
    .await;
}

/// MockExploredHost presents a fluent interface for declaring a mock host and running it through
/// the site-explorer ingestion lifecycle. Its methods are intended to be chained together to
/// script together a sequence of expected events to ingest a mock host.
pub(in crate::tests) struct MockExploredHost<'a> {
    pub(in crate::tests) test_env: &'a TestEnv,
    pub(in crate::tests) managed_host: ManagedHostConfig,
    pub(in crate::tests) host_bmc_ip: Option<IpAddr>,
    pub(in crate::tests) dpu_bmc_ips: HashMap<u8, IpAddr>,
    pub(in crate::tests) host_dhcp_response: Option<forge::DhcpRecord>,
    pub(in crate::tests) machine_discovery_response: Option<forge::MachineDiscoveryResult>,
    pub(in crate::tests) dpu_machine_ids: HashMap<u8, DpuMachineId>,
}

impl MockExploredHost<'_> {
    pub(in crate::tests) fn discovered_machine_id(&self) -> Option<MachineId> {
        self.machine_discovery_response
            .as_ref()
            .and_then(|r| r.machine_id)
    }
}

impl<'a> MockExploredHost<'a> {
    pub(in crate::tests) fn new(test_env: &'a TestEnv, managed_host: ManagedHostConfig) -> Self {
        Self {
            test_env,
            managed_host,
            host_bmc_ip: None,
            dpu_bmc_ips: HashMap::new(),
            host_dhcp_response: None,
            machine_discovery_response: None,
            dpu_machine_ids: HashMap::new(),
        }
    }

    /// Simulate the host's BMC interface getting DHCP.
    ///
    /// Yields the result to the passed closure.
    pub(in crate::tests) async fn discover_dhcp_host_bmc<
        F: FnOnce(tonic::Result<tonic::Response<forge::DhcpRecord>>, &mut Self) -> eyre::Result<()>,
    >(
        self,
        f: F,
    ) -> eyre::Result<Self> {
        self.discover_dhcp_host_bmc_from_relay(FIXTURE_UNDERLAY_NETWORK_SEGMENT_GATEWAY.ip(), f)
            .await
    }

    /// Simulate the host's BMC interface getting DHCP through a caller-selected relay.
    ///
    /// Yields the result to the passed closure.
    pub(in crate::tests) async fn discover_dhcp_host_bmc_from_relay<
        F: FnOnce(tonic::Result<tonic::Response<forge::DhcpRecord>>, &mut Self) -> eyre::Result<()>,
    >(
        mut self,
        relay_address: IpAddr,
        f: F,
    ) -> eyre::Result<Self> {
        let result = self
            .test_env
            .api
            .discover_dhcp(
                DhcpDiscovery::builder(self.managed_host.bmc_mac_address, relay_address)
                    .vendor_string("SomeVendor")
                    .tonic_request(),
            )
            .await;

        if let Ok(ref response) = result {
            self.host_bmc_ip = Some(response.get_ref().address.parse()?);
        }

        f(result, &mut self)?;
        Ok(self)
    }

    /// Simulate the given DPU's (indicated by dpu_index) BMC interface getting DHCP. Will panic if
    /// the index is out of range (ie. not part of the ManagedHostConfig.)
    ///
    /// Yields the result to the passed closure.
    pub(in crate::tests) async fn discover_dhcp_dpu_bmc<
        F: FnOnce(tonic::Result<tonic::Response<forge::DhcpRecord>>, &mut Self) -> eyre::Result<()>,
    >(
        mut self,
        dpu_index: u8,
        f: F,
    ) -> eyre::Result<Self> {
        let result = self
            .test_env
            .api
            .discover_dhcp(
                DhcpDiscovery::builder(
                    self.managed_host.dpus[dpu_index as usize].bmc_mac_address,
                    FIXTURE_UNDERLAY_NETWORK_SEGMENT_GATEWAY.ip(),
                )
                .vendor_string("NVIDIA/BF/BMC")
                .tonic_request(),
            )
            .await;

        if let Ok(ref response) = result {
            self.dpu_bmc_ips
                .insert(dpu_index, response.get_ref().address.parse()?);
        }

        f(result, &mut self)?;
        Ok(self)
    }

    // Create EndpointExplorationReports for the host and DPUs, and seed them into the
    // MockEndpointExplorer in this test env. If any of the host BMC or DPU BMC's have not run DHCP
    // yet, they will be skipped (as we won't yet know their IP.)
    pub(in crate::tests) fn insert_site_exploration_results(mut self) -> eyre::Result<Self> {
        let dpu_bmc_ips = self
            .dpu_bmc_ips
            .iter()
            .map(|(idx, ip)| (*idx, *ip))
            .collect::<Vec<_>>();
        let results = self
            .managed_host
            .exploration_results(self.host_bmc_ip, &dpu_bmc_ips)?;
        self.dpu_machine_ids.extend(results.dpu_machine_ids());
        self.test_env
            .endpoint_explorer
            .insert_endpoints(results.into_endpoints());
        Ok(self)
    }

    /// Run DHCP on the host's primary interface. If there are DPU's in the ManagedHostConfig, it
    /// uses the host_nics of the first DPU. If there are no DPUs, it uses the first mac
    /// address in [`ManagedHostConfig#non_dpu_macs`]. If there are none of those, panics.
    ///
    /// Yields the DHCP result to the passed closure
    pub(in crate::tests) async fn discover_dhcp_host_primary_iface<
        F: FnOnce(tonic::Result<tonic::Response<forge::DhcpRecord>>, &mut Self) -> eyre::Result<()>,
    >(
        mut self,
        f: F,
    ) -> eyre::Result<Self> {
        let result = if self.managed_host.admin_dhcp_fallback && !self.managed_host.dpus.is_empty()
        {
            let mac = self.managed_host.dhcp_mac_address();
            let env = self.test_env;
            let dhcp_result = Box::pin(async move {
                let primary_result = env
                    .api
                    .discover_dhcp(
                        DhcpDiscovery::builder(mac, FIXTURE_ADMIN_NETWORK_SEGMENT_GATEWAY.ip())
                            .vendor_string("Bluefield")
                            .tonic_request(),
                    )
                    .await;

                match primary_result {
                    Ok(response) => Ok(response),
                    Err(_) => env
                        .api
                        .discover_dhcp(
                            DhcpDiscovery::builder(
                                mac,
                                FIXTURE_HOST_INBAND_NETWORK_SEGMENT_GATEWAY_2.ip(),
                            )
                            .vendor_string("Bluefield")
                            .tonic_request(),
                        )
                        .await
                        .map_err(|status| {
                            tonic::Status::internal(format!("admin-segment DHCP failed: {status}"))
                        }),
                }
            })
            .await;

            match dhcp_result {
                Ok(response) => {
                    ensure_admin_interface_primary(self.test_env, mac)
                        .await
                        .ok();
                    Ok(response)
                }
                Err(status) => Err(status),
            }
        } else {
            let relay_address = if self.managed_host.dpus.is_empty() {
                FIXTURE_HOST_INBAND_NETWORK_SEGMENT_GATEWAY.ip().to_string()
            } else {
                FIXTURE_ADMIN_NETWORK_SEGMENT_GATEWAY.ip().to_string()
            };

            self.test_env
                .api
                .discover_dhcp(
                    DhcpDiscovery::builder(self.managed_host.dhcp_mac_address(), relay_address)
                        .vendor_string("Bluefield")
                        .tonic_request(),
                )
                .await
        };

        if let Ok(ref response) = result {
            self.host_dhcp_response = Some(response.get_ref().clone());
        }
        f(result, &mut self)?;
        Ok(self)
    }

    pub(in crate::tests) async fn discover_dhcp_dpu_primary_iface(self, dpu_index: u8) -> Self {
        let _ = self
            .test_env
            .api
            .discover_dhcp(
                DhcpDiscovery::builder(
                    self.managed_host.dpus[dpu_index as usize].oob_mac_address,
                    FIXTURE_ADMIN_NETWORK_SEGMENT_GATEWAY.ip(),
                )
                .vendor_string("SomeVendor")
                .tonic_request(),
            )
            .await;

        self
    }

    /// Run DHCP on the specified non-dpu host index ID, if available, from the given relay address.
    pub(in crate::tests) async fn discover_dhcp_host_secondary_iface<
        F: FnOnce(tonic::Result<tonic::Response<forge::DhcpRecord>>, &mut Self) -> eyre::Result<()>,
    >(
        mut self,
        iface_index: u8,
        relay_address: String,
        f: F,
    ) -> eyre::Result<Self> {
        let mac_address = self.managed_host.non_dpu_macs[iface_index as usize].to_string();
        let result = self
            .test_env
            .api
            .discover_dhcp(
                DhcpDiscovery::builder(mac_address, relay_address)
                    .vendor_string("Bluefield")
                    .tonic_request(),
            )
            .await;
        if let Ok(ref response) = result {
            self.host_dhcp_response = Some(response.get_ref().clone());
        }
        f(result, &mut self)?;
        Ok(self)
    }

    /// Simulates scout running machine discovery on the managed host.
    ///
    /// Yields the discovery result to the passed closure.
    pub(in crate::tests) async fn discover_machine<
        F: FnOnce(
            tonic::Result<tonic::Response<forge::MachineDiscoveryResult>>,
            &mut Self,
        ) -> eyre::Result<()>,
    >(
        mut self,
        f: F,
    ) -> eyre::Result<Self> {
        // Run scout discovery from the host

        let mut discovery_info =
            DiscoveryInfo::try_from(HardwareInfo::from(&self.managed_host)).unwrap();

        discovery_info.attest_key_info = Some(AttestKeyInfo {
            ek_pub: EK_PUB_SERIALIZED.to_vec(),
            ak_pub: AK_PUB_SERIALIZED.to_vec(),
            ak_name: AK_NAME_SERIALIZED.to_vec(),
        });

        let result = self
            .test_env
            .api
            .discover_machine(tonic::Request::new(rpc::MachineDiscoveryInfo {
                machine_interface_id: Some(
                    *self
                        .host_dhcp_response
                        .as_ref()
                        .unwrap()
                        .machine_interface_id
                        .as_ref()
                        .unwrap(),
                ),
                create_machine: true,
                discovery_data: Some(DiscoveryData::Info(discovery_info)),
                ..Default::default()
            }))
            .await;

        if let Ok(ref response) = result {
            self.machine_discovery_response = Some(response.get_ref().clone());
        }

        f(result, &mut self)?;
        Ok(self)
    }

    /// Runs one iteration of site explorer in the test env.
    pub(in crate::tests) async fn run_site_explorer_iteration(self) -> Self {
        self.test_env.run_site_explorer_iteration().await;
        self
    }

    /// Runs dpu_state_controller with DPF.
    pub(in crate::tests) async fn dpu_state_controller_iterations_with_dpf(self) -> Self {
        if self.managed_host.dpus.is_empty() {
            return self;
        }

        let mut txn = self.test_env.pool.begin().await.unwrap();

        let host_machine_id =
            db::machine::find_host_by_dpu_machine_id(&mut txn, &self.dpu_machine_ids[&0].clone())
                .await
                .unwrap()
                .unwrap()
                .host_machine_id()
                .unwrap();

        for machine_id in self.dpu_machine_ids.values() {
            create_machine_inventory(self.test_env, machine_id.into()).await;
        }

        self.test_env
            .run_machine_state_controller_iteration_until_state_matches(
                &host_machine_id,
                10 + (10 * self.dpu_machine_ids.len() as u32),
                ManagedHostState::DPUInit {
                    dpu_states: model::machine::DpuInitStates {
                        states: self
                            .dpu_machine_ids
                            .clone()
                            .into_values()
                            .map(|machine_id| {
                                (
                                    machine_id,
                                    DpuInitState::DpfStates {
                                        state: DpfState::WaitingForReady { phase_detail: None },
                                    },
                                )
                            })
                            .collect::<HashMap<DpuMachineId, DpuInitState>>(),
                    },
                },
            )
            .await;

        //run scout discovery for dpu(s)
        for dpu in self.managed_host.dpus.clone() {
            let machine_interfaces = find_by_mac_address(txn.as_mut(), dpu.oob_mac_address)
                .await
                .unwrap();
            let primary_interface = machine_interfaces
                .iter()
                .find(|interface| interface.primary_interface)
                .unwrap();
            let _ = self
                .test_env
                .api
                .discover_machine(tonic::Request::new(rpc::MachineDiscoveryInfo {
                    machine_interface_id: Some(primary_interface.id),
                    create_machine: true,
                    discovery_data: Some(DiscoveryData::Info(
                        DiscoveryInfo::try_from(HardwareInfo::from(&dpu)).unwrap(),
                    )),
                    ..Default::default()
                }))
                .await;
        }

        for machine_id in self.dpu_machine_ids.values() {
            let response = forge_agent_control(self.test_env, machine_id.into()).await;
            assert!(matches!(response.action, Some(Action::Discovery(_))));
            assert_eq!(
                response.legacy_action,
                rpc::forge_agent_control_response::LegacyAction::Discovery as i32
            );

            discovery_completed(self.test_env, machine_id).await;
        }

        txn.commit().await.unwrap();

        self.test_env
            .run_machine_state_controller_iteration_until_state_matches(
                &host_machine_id,
                35,
                ManagedHostState::DPUInit {
                    dpu_states: model::machine::DpuInitStates {
                        states: self
                            .dpu_machine_ids
                            .clone()
                            .into_values()
                            .map(|machine_id| (machine_id, DpuInitState::WaitingForNetworkConfig))
                            .collect::<HashMap<DpuMachineId, DpuInitState>>(),
                    },
                },
            )
            .await;

        self
    }
    /// Runs dpu_state_controller
    pub(in crate::tests) async fn dpu_state_controller_iterations(self) -> Self {
        if self.managed_host.dpus.is_empty() {
            return self;
        }

        let mut txn = self.test_env.pool.begin().await.unwrap();

        let host_machine_id =
            db::machine::find_host_by_dpu_machine_id(&mut txn, &self.dpu_machine_ids[&0].clone())
                .await
                .unwrap()
                .unwrap()
                .host_machine_id()
                .unwrap();

        for machine_id in self.dpu_machine_ids.values() {
            create_machine_inventory(self.test_env, machine_id.into()).await;
        }

        self.test_env
            .run_machine_state_controller_iteration_until_state_matches(
                &host_machine_id,
                10 + (10 * self.dpu_machine_ids.len() as u32),
                ManagedHostState::DPUInit {
                    dpu_states: model::machine::DpuInitStates {
                        states: self
                            .dpu_machine_ids
                            .clone()
                            .into_values()
                            .map(|machine_id| (machine_id, DpuInitState::Init))
                            .collect::<HashMap<DpuMachineId, DpuInitState>>(),
                    },
                },
            )
            .await;

        //run scout discovery for dpu(s)
        for dpu in self.managed_host.dpus.clone() {
            let machine_interfaces = find_by_mac_address(txn.as_mut(), dpu.oob_mac_address)
                .await
                .unwrap();
            let primary_interface = machine_interfaces
                .iter()
                .find(|interface| interface.primary_interface)
                .unwrap();
            let _ = self
                .test_env
                .api
                .discover_machine(tonic::Request::new(rpc::MachineDiscoveryInfo {
                    machine_interface_id: Some(primary_interface.id),
                    create_machine: true,
                    discovery_data: Some(DiscoveryData::Info(
                        DiscoveryInfo::try_from(HardwareInfo::from(&dpu)).unwrap(),
                    )),
                    ..Default::default()
                }))
                .await;
        }

        for machine_id in self.dpu_machine_ids.values() {
            let response = forge_agent_control(self.test_env, machine_id.into()).await;
            assert!(matches!(response.action, Some(Action::Discovery(_)),));
            assert_eq!(
                response.legacy_action,
                rpc::forge_agent_control_response::LegacyAction::Discovery as i32
            );

            discovery_completed(self.test_env, machine_id).await;
        }

        self.test_env
            .run_machine_state_controller_iteration_until_state_matches(
                &host_machine_id,
                35,
                ManagedHostState::DPUInit {
                    dpu_states: model::machine::DpuInitStates {
                        states: self
                            .dpu_machine_ids
                            .clone()
                            .into_values()
                            .map(|machine_id| (machine_id, DpuInitState::WaitingForNetworkConfig))
                            .collect::<HashMap<DpuMachineId, DpuInitState>>(),
                    },
                },
            )
            .await;

        txn.commit().await.unwrap();

        network_configured(
            self.test_env,
            &self.dpu_machine_ids.values().copied().collect(),
        )
        .await;

        // Wait until we exit the DPU states
        self.test_env
            .run_machine_state_controller_iteration_until_state_condition(
                &host_machine_id,
                20,
                |machine| matches!(*machine.current_state(), ManagedHostState::HostInit { .. }),
            )
            .await;

        self
    }

    pub(in crate::tests) async fn dpu_state_controller_iterations_to_network_install(self) -> Self {
        if self.managed_host.dpus.is_empty() {
            return self;
        }

        let mut txn = self.test_env.pool.begin().await.unwrap();

        let host_machine_id =
            db::machine::find_host_by_dpu_machine_id(&mut txn, &self.dpu_machine_ids[&0].clone())
                .await
                .unwrap()
                .unwrap()
                .host_machine_id()
                .unwrap();

        for machine_id in self.dpu_machine_ids.values() {
            create_machine_inventory(self.test_env, machine_id.into()).await;
        }

        self.test_env
            .run_machine_state_controller_iteration_until_state_matches(
                &host_machine_id,
                25,
                ManagedHostState::DPUInit {
                    dpu_states: model::machine::DpuInitStates {
                        states: self
                            .dpu_machine_ids
                            .clone()
                            .into_values()
                            .map(|machine_id| (machine_id, DpuInitState::Init))
                            .collect::<HashMap<DpuMachineId, DpuInitState>>(),
                    },
                },
            )
            .await;

        //run scout discovery for dpu(s)
        for dpu in self.managed_host.dpus.clone() {
            let machine_interfaces = find_by_mac_address(txn.as_mut(), dpu.oob_mac_address)
                .await
                .unwrap();
            let primary_interface = machine_interfaces
                .iter()
                .find(|interface| interface.primary_interface)
                .unwrap();
            let _ = self
                .test_env
                .api
                .discover_machine(tonic::Request::new(rpc::MachineDiscoveryInfo {
                    machine_interface_id: Some(primary_interface.id),
                    create_machine: true,
                    discovery_data: Some(DiscoveryData::Info(
                        DiscoveryInfo::try_from(HardwareInfo::from(&dpu)).unwrap(),
                    )),
                    ..Default::default()
                }))
                .await;
        }

        for machine_id in self.dpu_machine_ids.values() {
            discovery_completed(self.test_env, machine_id).await;
        }

        self.test_env
            .run_machine_state_controller_iteration_until_state_matches(
                &host_machine_id,
                35,
                ManagedHostState::DPUInit {
                    dpu_states: model::machine::DpuInitStates {
                        states: self
                            .dpu_machine_ids
                            .clone()
                            .into_values()
                            .map(|machine_id| (machine_id, DpuInitState::WaitingForNetworkConfig))
                            .collect::<HashMap<DpuMachineId, DpuInitState>>(),
                    },
                },
            )
            .await;

        txn.commit().await.unwrap();

        self
    }

    pub(in crate::tests) async fn host_state_controller_iterations(self) -> Self {
        let host_machine_id: HostMachineId = self
            .machine_discovery_response
            .as_ref()
            .unwrap()
            .machine_id
            .unwrap()
            .try_into()
            .unwrap();

        let expected_state = self.managed_host.expected_state.clone();

        if self.test_env.attestation_enabled {
            let stop_state = self
                .test_env
                .run_machine_state_controller_iteration_until_state_condition(
                    &host_machine_id,
                    10,
                    |machine| {
                        machine.current_state() == &expected_state
                            || matches!(
                                *machine.current_state(),
                                ManagedHostState::HostInit {
                                    machine_state: MachineState::Measuring {
                                        measuring_state: MeasuringState::WaitingForMeasurements,
                                    },
                                }
                            )
                    },
                )
                .await;

            // if we hit the requested state before the measuring state, return early
            if stop_state == expected_state {
                return self;
            }

            inject_machine_measurements(self.test_env, host_machine_id).await;
        }

        // if SPDM attestation is enabled, we need to drive it to completion
        if self.test_env.config.spdm.enabled {
            self.test_env
                .run_machine_state_controller_iteration_until_state_matches(
                    &host_machine_id,
                    10,
                    ManagedHostState::HostInit {
                        machine_state: MachineState::SpdmMeasuring {
                            spdm_measuring_state: SpdmMeasuringState::PollResult,
                        },
                    },
                )
                .await;

            for _ in 0..10 {
                self.test_env.run_spdm_controller_iteration().await;
            }
        }

        complete_initial_discovery_cleanup_if_needed(self.test_env, host_machine_id).await;

        self.test_env
            .api
            .insert_machine_health_report(Request::new(InsertMachineHealthReportRequest {
                health_report_entry: Some(HealthReportEntry {
                    report: Some(
                        HealthReport::empty(format!("{HARDWARE_HEALTH_OVERRIDE_PREFIX}health"))
                            .into(),
                    ),
                    ..Default::default()
                }),
                machine_id: Some(host_machine_id.into()),
            }))
            .await
            .expect("Failed to add hardware health report to newly created machine");

        discovery_completed(self.test_env, &host_machine_id).await;
        self.test_env.run_ib_fabric_monitor_iteration().await;
        host_uefi_setup(self.test_env, host_machine_id).await;

        let stop_state = self
            .test_env
            .run_machine_state_controller_iteration_until_state_condition(
                &host_machine_id,
                15,
                |machine| {
                    machine.current_state() == &expected_state
                        || matches!(
                            *machine.current_state(),
                            ManagedHostState::HostInit {
                                machine_state: MachineState::WaitingForLockdown {
                                    lockdown_info: LockdownInfo {
                                        state: LockdownState::WaitForDPUUp,
                                        mode: LockdownMode::Enable,
                                    },
                                },
                            } | ManagedHostState::BomValidating { .. }
                        )
                },
            )
            .await;

        if stop_state == expected_state {
            return self;
        }

        // Zero-DPU hosts skip the WaitForDPUUp lockdown state and land
        // directly in BomValidating (see `has_managed_dpus` short-circuit in
        // `LockdownState::TimeWaitForDPUDown`). There are no DPUs to
        // signal as configured, so skip the network_configured handshake.
        if !self.dpu_machine_ids.is_empty() {
            // We use carbide-dpu-agent health reporting as a signal that
            // DPU has rebooted.
            super::network_configured(
                self.test_env,
                &self.dpu_machine_ids.values().copied().collect(),
            )
            .await;
        }

        if self.test_env.config.bom_validation.enabled
            && !self
                .test_env
                .config
                .bom_validation
                .ignore_unassigned_machines
        {
            tracing::info!("bom validation enabled");
            let stop_state = self
                .test_env
                .run_machine_state_controller_iteration_until_state_condition(
                    &host_machine_id,
                    20,
                    |machine| {
                        machine.current_state() == &expected_state
                            || machine.config.hw_sku.is_none()
                                && matches!(
                                    *machine.current_state(),
                                    ManagedHostState::BomValidating {
                                        bom_validating_state:
                                            BomValidating::WaitingForSkuAssignment(
                                                BomValidatingContext { .. },
                                            ),
                                    }
                                )
                            || machine.config.hw_sku.is_some()
                    },
                )
                .await;

            // if we hit the requested state before the BomValidating state, return early
            if stop_state == expected_state {
                return self;
            }

            let stop_state = self
                .assign_sku_if_needed(&host_machine_id, stop_state, &expected_state)
                .await;
            if stop_state == expected_state {
                return self;
            }

            // If auto_assign_sku_in_fixture is disabled and machine is stuck in WaitingForSkuAssignment,
            // don't continue waiting - return early to allow tests to inspect this state
            if !self.managed_host.auto_assign_sku_in_fixture
                && matches!(
                    stop_state,
                    ManagedHostState::BomValidating {
                        bom_validating_state: BomValidating::WaitingForSkuAssignment(_)
                    }
                )
            {
                tracing::info!(
                    "auto_assign_sku_in_fixture=false and machine in WaitingForSkuAssignment, returning early"
                );
                return self;
            }
        }
        let stop_state =
            self.test_env
                .run_machine_state_controller_iteration_until_state_condition(
                    &host_machine_id,
                    10,
                    |machine| {
                        machine.current_state() == &expected_state
                            || matches!(
                                *machine.current_state(),
                                ManagedHostState::Validation {
                                    validation_state: ValidationState::MachineValidation {
                                        machine_validation:
                                            MachineValidatingState::MachineValidating { .. },
                                    },
                                } | ManagedHostState::HostInit {
                                    machine_state: MachineState::Discovered {
                                        skip_reboot_wait: true,
                                    },
                                }
                            )
                    },
                )
                .await;

        if stop_state == expected_state {
            return self;
        }

        let stop_state = if matches!(
            stop_state,
            ManagedHostState::Validation {
                validation_state: ValidationState::MachineValidation {
                    machine_validation: MachineValidatingState::MachineValidating { .. },
                },
            }
        ) {
            machine_validation_completed(self.test_env, &host_machine_id, None).await;

            self.test_env
                .run_machine_state_controller_iteration_until_state_condition(
                    &host_machine_id,
                    10,
                    |machine| {
                        machine.current_state() == &expected_state
                            || matches!(
                                *machine.current_state(),
                                ManagedHostState::HostInit {
                                    machine_state: MachineState::Discovered { .. },
                                }
                            )
                    },
                )
                .await
        } else if matches!(
            stop_state,
            ManagedHostState::HostInit {
                machine_state: MachineState::Discovered {
                    skip_reboot_wait: true,
                },
            }
        ) {
            stop_state
        } else {
            panic!("Unexpected state while handling machine validation: {stop_state}");
        };

        if stop_state == expected_state {
            return self;
        }

        let response = forge_agent_control(self.test_env, host_machine_id.into()).await;
        assert!(matches!(response.action, Some(Action::Noop(_))));
        assert_eq!(
            response.legacy_action,
            rpc::forge_agent_control_response::LegacyAction::Noop as i32
        );

        self.test_env
            .run_machine_state_controller_iteration_until_state_condition(
                &host_machine_id,
                1,
                |machine| {
                    let fixed_expected_state = self
                        .test_env
                        .fill_machine_information(&expected_state, machine);
                    machine.current_state() == &fixed_expected_state
                },
            )
            .await;

        self
    }
    /// Marks all BMC IP's as having completed preingestion, manually using the database.
    pub(in crate::tests) async fn mark_preingestion_complete(self) -> eyre::Result<Self> {
        let ips = self
            .dpu_bmc_ips
            .values()
            .copied()
            .chain(iter::once(self.host_bmc_ip.unwrap()))
            .collect::<Vec<_>>();
        let mut txn = self.test_env.pool.begin().await?;
        for ip in ips {
            db::explored_endpoints::set_preingestion_complete(ip, &mut txn).await?;
        }
        txn.commit().await?;
        Ok(self)
    }

    pub(in crate::tests) async fn host_state_controller_iterations_with_machine_validation(
        self,
        machine_validation_result_data: Option<rpc::forge::MachineValidationResult>,
        error: Option<String>,
    ) -> Self {
        let host_machine_id: HostMachineId = self
            .machine_discovery_response
            .as_ref()
            .unwrap()
            .machine_id
            .unwrap()
            .try_into()
            .unwrap();
        let mut machine_validation_result = machine_validation_result_data.unwrap_or_default();
        complete_initial_discovery_cleanup_if_needed(self.test_env, host_machine_id).await;

        self.test_env
            .api
            .insert_machine_health_report(Request::new(InsertMachineHealthReportRequest {
                health_report_entry: Some(HealthReportEntry {
                    report: Some(
                        HealthReport::empty(format!("{HARDWARE_HEALTH_OVERRIDE_PREFIX}health"))
                            .into(),
                    ),
                    ..Default::default()
                }),
                machine_id: Some(host_machine_id.into()),
            }))
            .await
            .expect("Failed to add hardware health report to newly created machine");

        discovery_completed(self.test_env, &host_machine_id).await;
        self.test_env.run_ib_fabric_monitor_iteration().await;
        host_uefi_setup(self.test_env, host_machine_id).await;

        self.test_env
            .run_machine_state_controller_iteration_until_state_matches(
                &host_machine_id,
                5,
                ManagedHostState::HostInit {
                    machine_state: MachineState::WaitingForLockdown {
                        lockdown_info: LockdownInfo {
                            state: LockdownState::WaitForDPUUp,
                            mode: LockdownMode::Enable,
                        },
                    },
                },
            )
            .await;

        // We use forge_dpu_agent's health reporting as a signal that
        // DPU has rebooted.
        super::network_configured(
            self.test_env,
            &self.dpu_machine_ids.values().copied().collect(),
        )
        .await;

        let expected_machine_validating_state = ManagedHostState::Validation {
            validation_state: ValidationState::MachineValidation {
                machine_validation: MachineValidatingState::MachineValidating {
                    context: "Discovery".to_string(),
                    id: MachineValidationId::new(),
                    completed: 1,
                    total: 1,
                    is_enabled: true,
                },
            },
        };
        let stop_state = self
            .test_env
            .run_machine_state_controller_iteration_until_state_condition(
                &host_machine_id,
                10,
                |machine| {
                    let expected_machine_validating_state = self
                        .test_env
                        .fill_machine_information(&expected_machine_validating_state, machine);
                    machine.current_state() == &expected_machine_validating_state
                        || matches!(
                            *machine.current_state(),
                            ManagedHostState::HostInit {
                                machine_state: MachineState::Discovered {
                                    skip_reboot_wait: true,
                                },
                            }
                        )
                },
            )
            .await;

        if matches!(
            stop_state,
            ManagedHostState::Validation {
                validation_state: ValidationState::MachineValidation {
                    machine_validation: MachineValidatingState::MachineValidating { .. },
                },
            }
        ) {
            let response = forge_agent_control(self.test_env, host_machine_id.into()).await;
            let uuid = &response.data.unwrap().pair[1].value;
            let validation_id: MachineValidationId = uuid.parse().unwrap();
            let success = update_machine_validation_run(
                self.test_env,
                Some(validation_id),
                Some(rpc::Duration::from(std::time::Duration::from_secs(1200))),
                1,
            )
            .await;
            assert_eq!(success.message, "Success".to_string());
            let runs = get_machine_validation_runs(self.test_env, &host_machine_id, false).await;
            for run in runs.runs {
                if run.validation_id == Some(validation_id) {
                    assert_eq!(run.status.unwrap_or_default().total, 1);
                    assert_eq!(run.status.unwrap_or_default().completed_tests, 0);
                    assert_eq!(run.duration_to_complete.unwrap_or_default().seconds, 1200);
                }
            }
            machine_validation_result.validation_id = Some(validation_id);
            persist_machine_validation_result(self.test_env, machine_validation_result.clone())
                .await;
            assert_eq!(
                get_machine_validation_runs(self.test_env, &host_machine_id, false)
                    .await
                    .runs[0]
                    .end_time,
                None
            );

            machine_validation_completed(self.test_env, &host_machine_id, error.clone()).await;

            let runs = get_machine_validation_runs(self.test_env, &host_machine_id, false).await;
            for run in runs.runs {
                if run.validation_id == Some(validation_id) {
                    assert_eq!(run.status.unwrap_or_default().total, 1);
                    assert_eq!(
                        run.status.unwrap_or_default().completed_tests,
                        if machine_validation_result.exit_code != 0 {
                            0
                        } else {
                            1
                        }
                    );
                    assert_eq!(run.duration_to_complete.unwrap_or_default().seconds, 1200);
                }
            }

            if error.is_some() {
                self.test_env.run_machine_state_controller_iteration().await;

                let mut txn = self.test_env.pool.begin().await.unwrap();
                let machine = db::machine::find_one(
                    txn.as_mut(),
                    &self.dpu_machine_ids[&0],
                    model::machine::machine_search_config::MachineSearchConfig::default(),
                )
                .await
                .unwrap()
                .unwrap();

                match machine.current_state() {
                    ManagedHostState::Failed { .. } => {}
                    s => {
                        panic!("Incorrect state: {s}");
                    }
                }

                txn.commit().await.unwrap();
            } else if machine_validation_result.exit_code == 0 {
                let _ = forge_agent_control(self.test_env, host_machine_id.into()).await;

                self.test_env
                    .run_machine_state_controller_iteration_until_state_matches(
                        &host_machine_id,
                        10,
                        ManagedHostState::HostInit {
                            machine_state: MachineState::Discovered {
                                skip_reboot_wait: false,
                            },
                        },
                    )
                    .await;

                let response = forge_agent_control(self.test_env, host_machine_id.into()).await;
                assert!(matches!(response.action, Some(Action::Noop(_))));
                assert_eq!(response.legacy_action, LegacyAction::Noop as i32);
                self.test_env
                    .run_machine_state_controller_iteration_until_state_matches(
                        &host_machine_id,
                        1,
                        ManagedHostState::Ready,
                    )
                    .await;
            } else {
                self.test_env
                    .run_machine_state_controller_iteration_until_state_matches(
                        &host_machine_id,
                        1,
                        ManagedHostState::Failed {
                            details: FailureDetails {
                                cause: FailureCause::MachineValidation {
                                    err: format!("{} is failed", machine_validation_result.name),
                                },
                                failed_at: chrono::Utc::now(),
                                source: FailureSource::Scout,
                            },
                            machine_id: host_machine_id.into(),
                            retry_count: 0,
                        },
                    )
                    .await;
            }
        } else if matches!(
            stop_state,
            ManagedHostState::HostInit {
                machine_state: MachineState::Discovered {
                    skip_reboot_wait: true,
                },
            }
        ) {
            self.test_env
                .run_machine_state_controller_iteration_until_state_matches(
                    &host_machine_id,
                    1,
                    ManagedHostState::Ready,
                )
                .await;
        } else {
            panic!("Unexpected state while handling machine validation: {stop_state}");
        }

        self
    }

    #[allow(dead_code)]
    /// Run the passed closure with a mutable referece to self
    pub(in crate::tests) async fn then<F, C: FnOnce(&mut Self) -> F>(
        mut self,
        f: C,
    ) -> eyre::Result<Self>
    where
        F: Future<Output = eyre::Result<()>>,
    {
        f(&mut self).await?;
        Ok(self)
    }

    /// Move self to the passed closure and return the closure's result. Useful as the final step of
    /// a method chain to return a final result.
    pub(in crate::tests) async fn finish<R, F, C: FnOnce(Self) -> F>(self, f: C) -> R
    where
        F: Future<Output = R>,
    {
        f(self).await
    }

    async fn assign_sku_if_needed(
        &self,
        host_machine_id: &HostMachineId,
        state: ManagedHostState,
        expected_state: &ManagedHostState,
    ) -> ManagedHostState {
        if matches!(
            state,
            ManagedHostState::BomValidating {
                bom_validating_state: BomValidating::WaitingForSkuAssignment(
                    BomValidatingContext { .. },
                ),
            }
        ) {
            // Check if auto-assignment is enabled in the fixture config
            if !self.managed_host.auto_assign_sku_in_fixture {
                tracing::info!("Skipping auto SKU assignment (auto_assign_sku_in_fixture=false)");
                return state;
            }

            let mut txn = self.test_env.pool.begin().await.unwrap();
            tracing::info!("generating sku");
            let sku = db::sku::generate_sku_from_machine(txn.as_mut(), host_machine_id)
                .await
                .unwrap();
            tracing::info!(
                sku_id = %sku.id,
                "creating sku",
            );
            db::sku::create(&mut txn, &sku).await.unwrap();

            tracing::info!("assigning sku");
            db::machine::assign_sku(&mut txn, host_machine_id, &sku.id)
                .await
                .unwrap();
            txn.commit().await.unwrap();
            let stop_state = self
                .test_env
                .run_machine_state_controller_iteration_until_state_condition(
                    host_machine_id,
                    3,
                    |machine| {
                        machine.current_state() == expected_state
                            || matches!(
                                *machine.current_state(),
                                ManagedHostState::BomValidating {
                                    bom_validating_state: BomValidating::UpdatingInventory(
                                        BomValidatingContext { .. },
                                    ),
                                }
                            )
                    },
                )
                .await;
            // if we hit the requested state before the BomValidating state, return early
            if &stop_state == expected_state {
                return stop_state;
            }

            tracing::info!("updating inventory");
            // discovery time is based on transaction start time, so this needs a new transaction
            let mut txn = self.test_env.pool.begin().await.unwrap();
            db::machine::update_discovery_time(host_machine_id, &mut txn)
                .await
                .unwrap();

            txn.commit().await.unwrap();
            stop_state
        } else {
            state
        }
    }

    /// Site-explorer ingestion steps shared by [`new_mock_host`].
    ///
    /// Each step is awaited separately so the async state machine does not accumulate the full
    /// chain on the stack (see comment in [`new_mock_host`]).
    async fn finish_standard_ingestion_flow(mut self) -> eyre::Result<Self> {
        self = self.discover_dhcp_host_bmc(|_, _| Ok(())).boxed().await?;
        self = self.insert_site_exploration_results()?;
        self = self.run_site_explorer_iteration().boxed().await;
        self = self.mark_preingestion_complete().boxed().await?;
        self = self.run_site_explorer_iteration().boxed().await;
        self = self
            .discover_dhcp_host_primary_iface(|_, _| Ok(()))
            .boxed()
            .await?;
        self = self.dpu_state_controller_iterations().boxed().await;
        self = self.discover_machine(|_, _| Ok(())).boxed().await?;
        self = self.run_site_explorer_iteration().boxed().await;
        self = self.host_state_controller_iterations().boxed().await;
        Ok(self)
    }
}

#[allow(dead_code)]
fn expected_switch_exploration_report() -> EndpointExplorationReport {
    EndpointExplorationReport {
        endpoint_type: EndpointType::Bmc,
        vendor: Some(bmc_vendor::BMCVendor::Nvidia),
        model: Some("Switch".to_string()),
        chassis: vec![Chassis {
            id: "mgx_nvswitch_0".to_string(),
            manufacturer: Some("NVIDIA".to_string()),
            model: Some("Switch".to_string()),
            ..Default::default()
        }],
        ..Default::default()
    }
}

#[allow(dead_code)]
async fn switch_interface_ip(
    txn: &mut sqlx::PgConnection,
    mac: mac_address::MacAddress,
) -> eyre::Result<Option<IpAddr>> {
    let interfaces = db::machine_interface::find_by_mac_address(&mut *txn, mac).await?;
    let Some(interface) = interfaces.first() else {
        return Ok(None);
    };
    let addresses =
        db::machine_interface_address::find_for_interface(&mut *txn, interface.id).await?;
    Ok(addresses.first().map(|address| address.address))
}

#[allow(dead_code)]
/// Registers mock endpoint-exploration results for every expected switch BMC and NVOS IP.
///
/// Required when switches (and their static interfaces) exist before `new_mock_host` runs,
/// e.g. rack-switch NMX-C simulator tests that create the switch before host discovery.
/// NVOS static IPs live on the `static-assignments` segment, which is typed as underlay,
/// so site-explorer will attempt to explore them too.
pub(in crate::tests) async fn register_expected_switch_exploration_results(
    env: &TestEnv,
) -> eyre::Result<()> {
    let mut txn = env.pool.begin().await?;
    let expected_switches = db::expected_switch::find_all(&mut txn).await?;
    let mut endpoints = Vec::new();
    for expected_switch in expected_switches {
        let report = expected_switch_exploration_report();
        let bmc_ip = if let Some(ip) = expected_switch.bmc_ip_address {
            ip
        } else if let Some(ip) =
            switch_interface_ip(txn.as_mut(), expected_switch.bmc_mac_address).await?
        {
            ip
        } else {
            continue;
        };
        endpoints.push((bmc_ip, report.clone()));

        if let Some(nvos_ip) = expected_switch.nvos_ip_address {
            endpoints.push((nvos_ip, report));
        } else if let [nvos_mac] = expected_switch.nvos_mac_addresses.as_slice()
            && let Some(nvos_ip) = switch_interface_ip(txn.as_mut(), *nvos_mac).await?
        {
            endpoints.push((nvos_ip, report));
        }
    }
    txn.commit().await?;
    if !endpoints.is_empty() {
        env.endpoint_explorer.insert_endpoints(endpoints);
    }
    Ok(())
}

pub(in crate::tests) async fn register_expected_machine(
    env: &'_ TestEnv,
    config: &ManagedHostConfig,
    default_dpf_enabled: Option<bool>,
) {
    // Tests may intentionally pre-create an expected-machine row; avoid inserting duplicates.
    if db_expected_machine::find_by_bmc_mac_address(&env.pool, config.bmc_mac_address)
        .await
        .expect("Expect expected machine lookup by BMC MAC to succeed")
        .is_some()
    {
        return;
    }

    // Always register an expected_machines entry so fixture-created hosts can flow
    // through site-explorer ingestion. Site-explorer now enforces that only hosts listed
    // in `expected_machines` are turned into Managed Hosts.
    let mut data = config.expected_machine_data.clone().unwrap_or_default();
    // Fill data from ManagedHostConfig
    // TODO: Disambiguate chassis and product serial number
    // We seem to set the product serial number here
    data.serial_number = config.serial.clone();
    if data.dpf_enabled.is_none() {
        data.dpf_enabled = default_dpf_enabled;
    }
    // For fixtures that intentionally create zero-DPU hosts (no DpuConfigs),
    // declare them as `Ignore` so site-explorer accepts them. Explicit
    // non-`Manage` policies in `expected_machine_data` are left alone.
    if config.dpus.is_empty() && data.dpu_policy == model::expected_machine::HostDpuPolicy::Manage {
        data.dpu_policy = model::expected_machine::HostDpuPolicy::Ignore;
    }

    let em = ExpectedMachine {
        id: Some(uuid::Uuid::new_v4()),
        bmc_mac_address: config.bmc_mac_address,
        data,
    };

    env.api
        .create_expected_machines(tonic::Request::new(
            rpc::forge::BatchExpectedMachineOperationRequest {
                expected_machines: Some(rpc::forge::ExpectedMachineList {
                    expected_machines: vec![em.into()],
                }),
                accept_partial_results: false,
            },
        ))
        .await
        .expect("Expect expected machine to get registered");
}

/// Seeds the vault with the BMC root credential for the host's BMC and every
/// DPU BMC in the config -- required by anything that reaches a BMC through
/// the real endpoint explorer.
///
/// Site-wide credentials (e.g. the host/DPU UEFI site-default) are seeded
/// centrally in the `TestEnv` builder, not here, since they are not per-device.
pub(in crate::tests) async fn seed_bmc_root_credentials(
    env: &TestEnv,
    config: &ManagedHostConfig,
) -> eyre::Result<()> {
    for bmc_mac_address in
        std::iter::once(config.bmc_mac_address).chain(config.dpus.iter().map(|d| d.bmc_mac_address))
    {
        env.api
            .credential_manager
            .set_credentials(
                &CredentialKey::BmcCredentials {
                    credential_type: BmcCredentialType::BmcRoot { bmc_mac_address },
                },
                &Credentials::UsernamePassword {
                    username: "root".to_string(),
                    password: "notforprod".to_string(),
                },
            )
            .await?;
    }
    Ok(())
}

/// Ingests a zero-DPU host via site-explorer and stops BEFORE its in-band
/// NIC's first DHCP lease: the machine exists with predicted interfaces only,
/// no real `machine_interfaces` rows. Registers the expected machine and
/// seeds BMC credentials; the caller drives anything past this window.
pub(in crate::tests) async fn ingest_zero_dpu_host_awaiting_first_lease<'a>(
    env: &'a TestEnv,
    config: ManagedHostConfig,
) -> eyre::Result<MockExploredHost<'a>> {
    register_expected_machine(env, &config, None).await;
    seed_bmc_root_credentials(env, &config).await?;

    Ok(MockExploredHost::new(env, config)
        .discover_dhcp_host_bmc(|result, _| {
            assert!(result.is_ok());
            Ok(())
        })
        .await?
        .insert_site_exploration_results()?
        .run_site_explorer_iteration()
        .await
        .mark_preingestion_complete()
        .await?
        .run_site_explorer_iteration()
        .await)
}

/// Use this function to make a new managed host with a given number of DPUs, using site-explorer
/// to ingest it into the database. Returns a MockExploredHost that you can call more methods on
/// before finishing.
pub(in crate::tests) async fn new_mock_host(
    env: &'_ TestEnv,
    config: ManagedHostConfig,
) -> eyre::Result<MockExploredHost<'_>> {
    // Make the IB ports visible in Mock-UFM
    let mock_ib_fabric = env.ib_fabric_manager.get_mock_manager();
    for ib_guid in config.ib_guids.iter() {
        mock_ib_fabric.register_port(ib_guid.clone());
    }

    // Create an expected-machine record for the new machine
    register_expected_machine(env, &config, None).await;

    // Set BMC credentials in vault
    seed_bmc_root_credentials(env, &config).await?;

    let dpu_count = config.dpus.len() as u8;
    let mut mock_explored_host = MockExploredHost::new(env, config);

    // Run BMC DHCP. DPUs first...
    for dpu_index in 0..dpu_count {
        mock_explored_host = mock_explored_host
            .discover_dhcp_dpu_bmc(dpu_index, |_, _| Ok(()))
            .await?;
    }

    // Run DHCP for DPU primary iface
    for dpu_index in 0..dpu_count {
        mock_explored_host = mock_explored_host
            .discover_dhcp_dpu_primary_iface(dpu_index)
            .await;
    }

    // NOTE: Calling `.boxed()` on each future in `finish_standard_ingestion_flow` decreases the
    // amount of stack space used by these futures. Prior to this we were hitting stack space
    // limits (going over 2MB in stack) in unit tests.
    mock_explored_host.finish_standard_ingestion_flow().await
}

/// Use this function to make a new managed host with a given number of DPUs, using site-explorer
/// to ingest it into the database. Returns the ManagedHostStateSnapshot of what was created
pub(in crate::tests) async fn new_host(
    env: &TestEnv,
    config: ManagedHostConfig,
) -> eyre::Result<ManagedHostStateSnapshot> {
    new_mock_host(env, config)
        .await?
        .finish(|mock| async move {
            let machine_id = mock.discovered_machine_id().unwrap();
            Ok(db::managed_host::load_snapshot(
                &mut env.db_reader(),
                &machine_id,
                Default::default(),
            )
            .await
            .transpose()
            .unwrap()?)
        })
        .await
}

pub(in crate::tests) async fn new_host_with_machine_validation(
    env: &TestEnv,
    dpu_count: u8,
    machine_validation_result_data: Option<rpc::forge::MachineValidationResult>,
    error: Option<String>,
) -> eyre::Result<ManagedHostStateSnapshot> {
    let managed_host = ManagedHostConfig::default().with_dpu_count(dpu_count.into());
    register_expected_machine(env, &managed_host, None).await;
    let mut mock_explored_host = MockExploredHost::new(env, managed_host);

    // Run BMC DHCP. DPUs first...
    for dpu_index in 0..dpu_count {
        mock_explored_host = mock_explored_host
            .discover_dhcp_dpu_bmc(dpu_index, |_, _| Ok(()))
            .await?;
    }

    // Run DHCP for DPU primary iface
    for dpu_index in 0..dpu_count {
        mock_explored_host = mock_explored_host
            .discover_dhcp_dpu_primary_iface(dpu_index)
            .await;
    }

    mock_explored_host
        // ...Then run host BMC's DHCP
        .discover_dhcp_host_bmc(|_, _| Ok(()))
        .boxed()
        .await?
        .insert_site_exploration_results()?
        .run_site_explorer_iteration()
        .boxed()
        .await
        .mark_preingestion_complete()
        .boxed()
        .await?
        .run_site_explorer_iteration()
        .boxed()
        .await
        .discover_dhcp_host_primary_iface(|_, _| Ok(()))
        .boxed()
        .await?
        .dpu_state_controller_iterations()
        .boxed()
        .await
        .discover_machine(|_, _| Ok(()))
        .boxed()
        .await?
        .run_site_explorer_iteration()
        .boxed()
        .await
        .host_state_controller_iterations_with_machine_validation(
            machine_validation_result_data,
            error,
        )
        .boxed()
        .await
        .finish(|mock| async move {
            let machine_id = mock.machine_discovery_response.unwrap().machine_id.unwrap();
            Ok(db::managed_host::load_snapshot(
                &mut env.db_reader(),
                &machine_id,
                Default::default(),
            )
            .await
            .transpose()
            .unwrap()?)
        })
        .boxed()
        .await
}

pub(in crate::tests) async fn new_dpu(
    env: &TestEnv,
    config: ManagedHostConfig,
) -> eyre::Result<DpuMachineId> {
    register_expected_machine(env, &config, None).await;
    let mut mock_explored_host = MockExploredHost::new(env, config);

    mock_explored_host = mock_explored_host
        .discover_dhcp_dpu_bmc(0, |_, _| Ok(()))
        .await?;

    mock_explored_host = mock_explored_host.discover_dhcp_dpu_primary_iface(0).await;

    mock_explored_host = mock_explored_host
        // ...Then run host BMC's DHCP
        .discover_dhcp_host_bmc(|_, _| Ok(()))
        .boxed()
        .await?
        .insert_site_exploration_results()?
        .run_site_explorer_iteration()
        .boxed()
        .await
        .mark_preingestion_complete()
        .boxed()
        .await?
        .run_site_explorer_iteration()
        .boxed()
        .await
        .dpu_state_controller_iterations()
        .boxed()
        .await;

    Ok(mock_explored_host.dpu_machine_ids[&0])
}

pub(in crate::tests) async fn new_dpu_in_network_install(
    env: &TestEnv,
    config: ManagedHostConfig,
) -> eyre::Result<TestManagedHost> {
    register_expected_machine(env, &config, None).await;
    let mut mock_explored_host = MockExploredHost::new(env, config);

    mock_explored_host = mock_explored_host
        .discover_dhcp_dpu_bmc(0, |_, _| Ok(()))
        .await?;

    mock_explored_host = mock_explored_host.discover_dhcp_dpu_primary_iface(0).await;

    mock_explored_host = mock_explored_host
        // ...Then run host BMC's DHCP
        .discover_dhcp_host_bmc(|_, _| Ok(()))
        .boxed()
        .await?
        .insert_site_exploration_results()?
        .run_site_explorer_iteration()
        .boxed()
        .await
        .mark_preingestion_complete()
        .boxed()
        .await?
        .run_site_explorer_iteration()
        .boxed()
        .await
        .dpu_state_controller_iterations_to_network_install()
        .boxed()
        .await;

    let mut txn = env.pool.begin().await.unwrap();
    let dpu_machine_id = mock_explored_host.dpu_machine_ids[&0];
    let host_machine_id = db::machine::find_host_by_dpu_machine_id(&mut txn, &dpu_machine_id)
        .await
        .unwrap()
        .unwrap()
        .id;

    Ok(TestManagedHost {
        id: host_machine_id
            .try_into()
            .expect("discovered host ID should be a valid HostMachineId"),
        dpu_ids: vec![dpu_machine_id],
        api: env.api.clone(),
    })
}

/*
Builder for a test rack. For now, we only have tests that use expected
computer trays, switches, and power shelves for testing rack state
histories. So that is all that is currently represented here. The intent
of this builder is to create test racks that may have various combinations
of expected and discovered resources to allow for testing a simulated rack
in various states, and abstract the way in which they are configured and
stored in the database, as the case may be.

The expectation is we will add members to this struct such as:

```
compute_trays: Vec<MachineId>,
```

and fns to its impl such as:

```
pub(in crate::tests) fn _with_compute_trays(mut self, compute_trays: Vec<MachineId>) -> Self {
    self.compute_trays = compute_trays;
    self
}
```

In short, what belongs here is anything that might aid a test writer to
create a rack, represented in the database, representing state of a rack
for both happy and non-happy path positive and negative testing. And
do it without regard to the underlying impl and in a way that makes it
clear looking at the test what the intent of the configuration is.
*/
pub(in crate::tests) struct TestRackDbBuilder {
    rack_id: RackId,
    rack_profile_id: Option<RackProfileId>,
}

impl Default for TestRackDbBuilder {
    fn default() -> Self {
        TestRackDbBuilder {
            rack_id: RackId::new(uuid::Uuid::new_v4().to_string()),
            rack_profile_id: Some(RackProfileId::new(super::TEST_RMS_RACK_PROFILE_ID)),
        }
    }
}

impl TestRackDbBuilder {
    pub(in crate::tests) fn new() -> TestRackDbBuilder {
        TestRackDbBuilder {
            ..Default::default()
        }
    }

    pub(in crate::tests) fn with_rack_id(mut self, id: RackId) -> Self {
        self.rack_id = id;
        self
    }

    pub(in crate::tests) fn with_rack_profile_id(
        mut self,
        rack_profile_id: impl Into<String>,
    ) -> Self {
        self.rack_profile_id = Some(RackProfileId::new(rack_profile_id));
        self
    }

    pub(in crate::tests) async fn persist(
        &self,
        txn: &mut PgConnection,
    ) -> Result<RackId, DatabaseError> {
        let rack_config = RackConfig::default();
        db_rack::create(
            txn,
            &self.rack_id,
            self.rack_profile_id.as_ref(),
            &rack_config,
            None,
        )
        .await?;

        Ok(self.rack_id.clone())
    }
}

/// Creates a new switch for testing purposes.
///
/// When `bmc_mac_address` is provided, an `ExpectedSwitch` record is also
/// created so the switch state controller can look it up during initialisation.
pub(in crate::tests) async fn new_switch(
    env: &TestEnv,
    name: Option<String>,
    _location: Option<String>,
) -> eyre::Result<SwitchId> {
    let mut txn = env.pool.begin().await.unwrap();

    let expected_switches = create_expected_switches(&mut txn).await;
    let expected_switch = match name {
        Some(n) => expected_switches
            .iter()
            .find(|s| s.metadata.name == n)
            .ok_or(eyre::eyre!("no expected switch found"))?,
        None => expected_switches.first().unwrap(),
    };

    let switch_id = model::switch::switch_id::from_hardware_info(
        &expected_switch.serial_number,
        "NVIDIA",
        "Switch",
        carbide_uuid::switch::SwitchIdSource::ProductBoardChassisSerial,
        carbide_uuid::switch::SwitchType::NvLink,
    )
    .map_err(|e| eyre::eyre!("failed to create switch ID: {:?}", e))
    .unwrap();

    let config = SwitchConfig {
        name: expected_switch.metadata.name.clone(),
        enable_nmxc: false,
        fabric_manager_config: None,
    };

    let new_switch = NewSwitch {
        id: switch_id,
        config,
        bmc_mac_address: Some(expected_switch.bmc_mac_address),
        metadata: None,
        rack_id: None,
        slot_number: Some(0),
        tray_index: Some(0),
    };

    let _switch = db_switch::create(&mut txn, &new_switch)
        .await
        .map_err(|e| eyre::eyre!("failed to create switch: {:?}", e))?;

    // Mirror site-explorer ingestion (switch_creator): link the switch's BMC
    // machine_interface back to the switch and annotate it `Bmc`, so that
    // `bmc_info` resolves via the interface link.
    let bmc_interfaces =
        db::machine_interface::find_by_mac_address(&mut *txn, expected_switch.bmc_mac_address)
            .await
            .map_err(|e| eyre::eyre!("failed to find BMC machine interface: {:?}", e))?;
    if let Some(interface) = bmc_interfaces.first() {
        db::machine_interface::associate_bmc_interface(
            &interface.id,
            model::machine_interface_address::MachineInterfaceAssociation::Switch(switch_id),
            &mut txn,
        )
        .await
        .map_err(|e| eyre::eyre!("failed to link BMC machine interface: {:?}", e))?;
    }

    txn.commit().await.unwrap();

    Ok(switch_id)
}

/// It is neccesary to start a tower_test server to simulate the kube environment and handle the DPF requests.
pub(in crate::tests) async fn new_mock_host_with_dpf(
    env: &'_ TestEnv,
    config: ManagedHostConfig,
) -> eyre::Result<ManagedHostStateSnapshot> {
    // Make the IB ports visible in Mock-UFM
    let mock_ib_fabric = env.ib_fabric_manager.get_mock_manager();
    for ib_guid in config.ib_guids.iter() {
        mock_ib_fabric.register_port(ib_guid.clone());
    }

    // Create an expected-machine record for the new machine
    register_expected_machine(env, &config, Some(true)).await;

    // Set BMC credentials in vault
    seed_bmc_root_credentials(env, &config).await?;

    let dpu_count = config.dpus.len() as u8;
    let mut mock_explored_host = MockExploredHost::new(env, config);

    // Run BMC DHCP. DPUs first...
    for dpu_index in 0..dpu_count {
        mock_explored_host = mock_explored_host
            .discover_dhcp_dpu_bmc(dpu_index, |_, _| Ok(()))
            .await?;
    }

    // Run DHCP for DPU primary iface
    for dpu_index in 0..dpu_count {
        mock_explored_host = mock_explored_host
            .discover_dhcp_dpu_primary_iface(dpu_index)
            .await;
    }

    mock_explored_host = mock_explored_host
        .discover_dhcp_host_bmc(|_, _| Ok(()))
        .boxed()
        .await?;
    mock_explored_host = mock_explored_host.insert_site_exploration_results()?;
    mock_explored_host = mock_explored_host
        .run_site_explorer_iteration()
        .boxed()
        .await;
    mock_explored_host = mock_explored_host
        .mark_preingestion_complete()
        .boxed()
        .await?;
    mock_explored_host = mock_explored_host
        .run_site_explorer_iteration()
        .boxed()
        .await;
    mock_explored_host = mock_explored_host
        .discover_dhcp_host_primary_iface(|_, _| Ok(()))
        .boxed()
        .await?;
    mock_explored_host = mock_explored_host
        .dpu_state_controller_iterations_with_dpf()
        .boxed()
        .await;

    let dpu_ids: Vec<DpuMachineId> = mock_explored_host
        .dpu_machine_ids
        .values()
        .copied()
        .collect();
    network_configured(env, &dpu_ids).await;

    mock_explored_host = mock_explored_host
        .discover_machine(|_, _| Ok(()))
        .boxed()
        .await?;
    mock_explored_host = mock_explored_host
        .run_site_explorer_iteration()
        .boxed()
        .await;
    mock_explored_host = mock_explored_host
        .host_state_controller_iterations()
        .boxed()
        .await;

    mock_explored_host
        .finish(|mock| async move {
            let machine_id = mock.machine_discovery_response.unwrap().machine_id.unwrap();
            Ok(db::managed_host::load_snapshot(
                &mut env.db_reader(),
                &machine_id,
                Default::default(),
            )
            .await
            .transpose()
            .unwrap()
            .unwrap())
        })
        .boxed()
        .await
}

/// Seeds one expected switch (plus BMC/NVOS machine interfaces) into the database.
pub(in crate::tests) async fn create_expected_switch(
    txn: &mut sqlx::PgConnection,
    index: u32,
) -> model::expected_switch::ExpectedSwitch {
    use model::expected_switch::ExpectedSwitch;
    use model::metadata::Metadata;

    use crate::test_support::mac_address_pool::EXPECTED_SWITCH_BMC_MAC_ADDRESS_POOL;

    let i = index as usize;
    let switch = ExpectedSwitch {
        expected_switch_id: None,
        bmc_mac_address: EXPECTED_SWITCH_BMC_MAC_ADDRESS_POOL.allocate(),
        nvos_mac_addresses: vec![EXPECTED_SWITCH_NVOS_MAC_ADDRESS_POOL.allocate()],
        serial_number: format!("SW-SN-{:03}", index + 1),
        bmc_username: "ADMIN".into(),
        bmc_password: "Pwd2023x0x0x0x7".into(),
        nvos_username: if (3..=4).contains(&i) {
            Some(format!("nvos_admin{}", i - 2))
        } else {
            None
        },
        nvos_password: if (3..=4).contains(&i) {
            Some(format!("nvos_pass{}", i - 2))
        } else {
            None
        },
        bmc_ip_address: None,
        nvos_ip_address: None,
        metadata: Metadata {
            name: format!("Switch{}", index + 1),
            description: format!("Test Switch {}", index + 1),
            labels: HashMap::new(),
        },
        rack_id: None,
        bmc_retain_credentials: None,
    };
    let result = db::expected_switch::create(txn, switch)
        .await
        .expect("unable to create expected switch");

    let network_segments = db::network_segment::admin(txn)
        .await
        .map_err(|e| eyre::eyre!("failed to get admin network segment: {:?}", e))
        .unwrap();

    for nvos_mac in &result.nvos_mac_addresses.clone() {
        db::machine_interface::create(
            txn,
            &network_segments,
            nvos_mac,
            false,
            AddressSelectionStrategy::NextAvailableIp,
            None,
        )
        .await
        .map_err(|e| eyre::eyre!("failed to create NVOS machine interface: {:?}", e))
        .unwrap();
    }
    let overlay_network_segment = db::network_segment::find_by_name(txn, "UNDERLAY")
        .await
        .map_err(|e| eyre::eyre!("failed to get overlay network segment: {:?}", e))
        .unwrap();

    db::machine_interface::create(
        txn,
        std::slice::from_ref(&overlay_network_segment),
        &result.bmc_mac_address.clone(),
        false,
        AddressSelectionStrategy::NextAvailableIp,
        None,
    )
    .await
    .map_err(|e| eyre::eyre!("failed to create BMC machine interface: {:?}", e))
    .unwrap();

    result
}

/// create_expected_switches seeds 6 expected switches into the database,
/// replacing the create_expected_switch.sql fixture.
pub(in crate::tests) async fn create_expected_switches(
    txn: &mut sqlx::PgConnection,
) -> Vec<model::expected_switch::ExpectedSwitch> {
    let mut created = Vec::new();
    for i in 0..6 {
        created.push(create_expected_switch(txn, i).await);
    }
    created
}

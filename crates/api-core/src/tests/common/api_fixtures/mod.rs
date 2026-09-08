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

//! Contains fixtures that use the Carbide API for setting up

use std::collections::HashMap;
use std::default::Default;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use carbide_extension_service_controller::context::ExtensionServiceStateHandlerServices;
use carbide_extension_service_controller::handler::ExtensionServiceStateHandler;
use carbide_extension_service_controller::io::ExtensionServiceStateControllerIO;
use carbide_ib_fabric::IbFabricMonitor;
use carbide_ib_fabric::ib::IBFabricManagerImpl;
use carbide_ib_partition_controller::context::IBPartitionStateHandlerServices;
use carbide_ib_partition_controller::handler::IBPartitionStateHandler;
use carbide_ib_partition_controller::io::IBPartitionStateControllerIO;
use carbide_ipmi::IPMITool;
use carbide_machine_controller::config::machine_validation::MachineValidationConfig;
use carbide_machine_controller::context::MachineStateHandlerServices;
use carbide_machine_controller::dpf::DpfOperations;
use carbide_machine_controller::handler::{
    MachineStateHandler, MachineStateHandlerBuilder, PowerOptionConfig, ReachabilityParams,
};
use carbide_machine_controller::io::MachineStateControllerIO;
use carbide_network::virtualization::build_dual_stack_list;
use carbide_network_segment_controller::context::NetworkSegmentStateHandlerServices;
use carbide_network_segment_controller::handler::NetworkSegmentStateHandler;
use carbide_network_segment_controller::io::NetworkSegmentStateControllerIO;
use carbide_nvlink_manager::nvlink::test_support::NmxcSimClient;
use carbide_nvlink_manager::{
    NvlPartitionMonitor, SwitchCertificateMonitor, SwitchCertificateMonitorIterationResult,
};
use carbide_rack::test_support::RmsSim;
use carbide_rack_controller::config::RackConfig;
use carbide_rack_controller::context::RackStateHandlerServices;
use carbide_rack_controller::firmware_object::FirmwareObjectFetcher;
use carbide_rack_controller::handler::RackStateHandler;
use carbide_rack_controller::io::RackStateControllerIO;
use carbide_redfish::libredfish::test_support::RedfishSim;
use carbide_secrets::credentials::{
    CompositeCredentialManager, CredentialKey, CredentialManager, CredentialReader, CredentialType,
    Credentials,
};
use carbide_secrets::test_support::credentials::TestCredentialManager;
use carbide_secrets::{ChainedCredentialReader, CredentialSnapshot, UsernamePassword};
use carbide_site_explorer::SiteExplorer;
use carbide_site_explorer::config::{SiteExplorerConfig, SiteExplorerExploreMode};
use carbide_site_explorer::test_support::MockEndpointExplorer;
use carbide_spdm_controller::context::SpdmStateHandlerServices;
use carbide_spdm_controller::handler::SpdmAttestationStateHandler;
use carbide_spdm_controller::io::SpdmStateControllerIO;
use carbide_utils::test_support::test_meter::TestMeter;
use carbide_uuid::instance::InstanceId;
use carbide_uuid::instance_type::InstanceTypeId;
use carbide_uuid::machine::{DpuMachineId, HostMachineId, MachineId};
use carbide_uuid::machine_validation::MachineValidationId;
use carbide_uuid::network::NetworkSegmentId;
use carbide_uuid::vpc::VpcId;
use carbide_vpc_prefix_controller::context::VpcPrefixStateHandlerServices;
use carbide_vpc_prefix_controller::handler::VpcPrefixStateHandler;
use carbide_vpc_prefix_controller::io::VpcPrefixStateControllerIO;
use chrono::{DateTime, Duration, Utc};
use db::db_read::PgPoolReader;
use db::instance_type::create as create_instance_type;
use db::network_security_group::create as create_network_security_group;
use db::work_lock_manager;
use futures::FutureExt as _;
use health_report::{HealthReport, HealthReportApplyMode};
use ipnetwork::IpNetwork;
use libnmxc::NmxcPool;
use measured_boot::pcr::PcrRegisterValue;
use model::attestation::spdm::Verifier;
use model::hardware_info::{HardwareInfo, TpmEkCertificate};
use model::instance_type::InstanceTypeMachineCapabilityFilter;
use model::machine::capabilities::MachineCapabilityType;
use model::machine::{
    FailureDetails, Machine, MachineLastRebootRequested, MachineValidatingState, ManagedHostState,
    ValidationState,
};
use model::metadata::Metadata;
use model::network_security_group;
use model::rack_type::{RackProfile, RackProfileConfig};
use model::resource_pool::common::CommonPools;
use model::resource_pool::{self};
use model::tenant::TenantOrganizationId;
use model::test_support::dpu::{DPU_BF3_INFO_JSON, DPU_BF4_INFO_JSON};
use model::test_support::{DpuConfig, HardwareInfoTemplate, ManagedHostConfig, rms_rack_profiles};
use nras::{
    DeviceAttestationInfo, NrasError, ProcessedAttestationOutcome, RawAttestationOutcome,
    VerifierClient,
};
use rpc::forge::forge_server::Forge;
use rpc::forge::{
    HealthReportEntry, InsertMachineHealthReportRequest, RemoveMachineHealthReportRequest,
    VpcVirtualizationType,
};
use rpc_instance::RpcInstance;
use site_explorer::new_host_with_machine_validation;
use sqlx::PgPool;
use sqlx::postgres::PgConnectOptions;
use state_controller::controller::StateController;
use state_controller::state_handler::{
    StateHandler, StateHandlerContext, StateHandlerError, StateHandlerOutcome,
};
use tokio::sync::Mutex;
use tokio::task::JoinSet;
use tokio_util::sync::{CancellationToken, DropGuard};
use tonic::Request;

use crate::api::Api;
use crate::api::metrics::ApiMetricsEmitter;
use crate::cfg::file::{CarbideConfig, ComputeAllocationEnforcement, FnnConfig};
use crate::ethernet_virtualization::{EthVirtData, SiteFabricPrefixList};
use crate::measured_boot::convert_vec;
use crate::test_support::builder::TestApiBuilder;
use crate::test_support::default_config;
use crate::test_support::fixture_config::{
    DpuConfigExt as _, FixtureDefault as _, ManagedHostConfigExt as _,
};
use crate::test_support::ib_fabric::ib_fabric_test_manager;
pub(in crate::tests) use crate::test_support::network::{
    FIXTURE_DHCP_RELAY_ADDRESS, TEST_SITE_PREFIXES,
};
pub(in crate::tests) use crate::test_support::network_segment;
use crate::test_support::network_segment::{
    FIXTURE_TENANT_NETWORK_SEGMENT_GATEWAYS, FIXTURE_TENANT_ORG_ID, create_admin_network_segment,
    create_static_assignments_segment, create_tenant_network_segment,
    create_underlay_network_segment,
};
use crate::tests::common::rpc_builder::VpcCreationRequest;

fn test_nvos_update_manager(
    rms_sim: &RmsSim,
) -> Option<Arc<dyn component_manager::NvosUpdateManager>> {
    rms_sim.as_rms_client().map(|client| {
        Arc::new(component_manager::rms::rms_nvos_update_manager(client))
            as Arc<dyn component_manager::NvosUpdateManager>
    })
}

pub(in crate::tests) mod dpu;
pub(in crate::tests) mod host;
pub(in crate::tests) mod ib_partition;
pub(in crate::tests) mod instance;
pub(in crate::tests) mod nvl_logical_partition;
pub(in crate::tests) mod rpc_instance;
pub(in crate::tests) mod site_explorer;
pub(in crate::tests) mod tenant;
pub(in crate::tests) mod test_machine;
pub(in crate::tests) mod test_managed_host;
pub(in crate::tests) mod tpm_attestation;
pub(in crate::tests) mod vpc;

pub(in crate::tests) type TestMachine<ID> = test_machine::TestMachine<ID>;
pub(in crate::tests) type TestManagedHost = test_managed_host::TestManagedHost;

#[derive(Clone, Debug, Default)]
pub(in crate::tests) struct TestEnvOverrides {
    pub(in crate::tests) site_prefixes: Option<Vec<IpNetwork>>,
    pub(in crate::tests) config: Option<CarbideConfig>,
    pub(in crate::tests) create_network_segments: Option<bool>,
    pub(in crate::tests) dpu_agent_version_staleness_threshold: Option<chrono::Duration>,
    pub(in crate::tests) prevent_allocations_on_stale_dpu_agent_version: Option<bool>,
    pub(in crate::tests) network_segments_drain_period: Option<chrono::Duration>,
    pub(in crate::tests) power_manager_enabled: Option<bool>,
    pub(in crate::tests) dpf_sdk: Option<Arc<dyn DpfOperations>>,
    pub(in crate::tests) fnn_config: Option<FnnConfig>,
    pub(in crate::tests) nmxc_default_partition: Option<bool>,
    pub(in crate::tests) nmxc_unknown_partition: Option<bool>,
    // After n create_requests succeed, they will start failing.
    pub(in crate::tests) nmxc_fail_after_n_creates: Option<usize>,
    pub(in crate::tests) compute_allocation_enforcement: Option<ComputeAllocationEnforcement>,
    pub(in crate::tests) nmxc_simulator: Option<bool>,

    /// Optional compute-tray backend injected into the component manager.
    pub(in crate::tests) compute_tray_manager:
        Option<Arc<dyn component_manager::compute_tray_manager::ComputeTrayManager>>,

    /// Optional firmware-object fetcher injected into the rack state handler.
    pub(in crate::tests) firmware_object_fetcher: Option<Arc<dyn FirmwareObjectFetcher>>,

    pub(in crate::tests) nras_should_fail_parsing: Option<Arc<AtomicBool>>,
    pub(in crate::tests) vpc_prefixes_drain_period: Option<chrono::Duration>,
    pub(in crate::tests) dhcp_lease_expiry_handling: Option<bool>,
}

impl TestEnvOverrides {
    pub(in crate::tests) fn with_config(config: CarbideConfig) -> Self {
        Self {
            config: Some(config),
            ..Default::default()
        }
    }

    pub(in crate::tests) fn with_dpf_sdk(mut self, dpf_sdk: Arc<dyn DpfOperations>) -> Self {
        self.dpf_sdk = Some(dpf_sdk);
        self
    }

    pub(in crate::tests) fn with_fnn_config(mut self, fnn_config: Option<FnnConfig>) -> Self {
        self.fnn_config = fnn_config.or_else(|| {
            Some(FnnConfig {
                admin_vpc: None,
                common_internal_route_target: None,
                additional_route_target_imports: vec![],
                routing_profiles: HashMap::from([
                    (
                        "EXTERNAL".to_string(),
                        crate::cfg::file::FnnRoutingProfileConfig {
                            access_tier: Some(2),
                            internal: Some(false),
                            ..Default::default()
                        },
                    ),
                    (
                        "INTERNAL".to_string(),
                        crate::cfg::file::FnnRoutingProfileConfig {
                            access_tier: Some(1),
                            internal: Some(true),
                            ..Default::default()
                        },
                    ),
                ]),
                use_vpc_vrf_loopback: false,
            })
        });

        self
    }

    pub(in crate::tests) fn with_compute_allocation_enforcement(
        mut self,
        enforcement: ComputeAllocationEnforcement,
    ) -> Self {
        self.compute_allocation_enforcement = Some(enforcement);
        self
    }

    pub(in crate::tests) fn no_network_segments() -> Self {
        Self {
            create_network_segments: Some(false),
            ..Default::default()
        }
    }
}

#[allow(dead_code)]
pub(crate) struct TestEnv {
    pub(crate) api: Arc<Api>,
    pub(in crate::tests) config: Arc<CarbideConfig>,
    pub(in crate::tests) common_pools: Arc<CommonPools>,
    pub(in crate::tests) pool: PgPool,
    pub(in crate::tests) redfish_sim: Arc<RedfishSim>,
    pub(in crate::tests) ib_fabric_monitor: Arc<IbFabricMonitor>,
    pub(in crate::tests) ib_fabric_manager: Arc<IBFabricManagerImpl>,
    pub(in crate::tests) ipmi_tool: Arc<dyn IPMITool>,
    machine_state_controller: Arc<Mutex<StateController<MachineStateControllerIO>>>,
    spdm_state_controller: Arc<Mutex<StateController<SpdmStateControllerIO>>>,
    pub(in crate::tests) machine_state_handler: SwapHandler<MachineStateHandler>,
    network_segment_controller: Arc<Mutex<StateController<NetworkSegmentStateControllerIO>>>,
    vpc_prefix_controller: Arc<Mutex<StateController<VpcPrefixStateControllerIO>>>,
    extension_service_controller: Arc<Mutex<StateController<ExtensionServiceStateControllerIO>>>,
    ib_partition_controller: Arc<Mutex<StateController<IBPartitionStateControllerIO>>>,
    rack_controller: Arc<Mutex<StateController<RackStateControllerIO>>>,
    pub(in crate::tests) reachability_params: ReachabilityParams,
    pub(in crate::tests) test_meter: TestMeter,
    pub(in crate::tests) attestation_enabled: bool,
    pub(in crate::tests) site_explorer: SiteExplorer,
    pub(in crate::tests) nmxc_sim: Arc<dyn NmxcPool>,
    /// True when the test env uses [`NmxcSimClient::simulator_for_nvlink_config`] (gRPC proxy to a local simulator).
    pub(in crate::tests) nmxc_grpc_simulator: bool,
    pub(in crate::tests) endpoint_explorer: MockEndpointExplorer,
    pub(in crate::tests) admin_segments: Vec<NetworkSegmentId>,
    pub(in crate::tests) underlay_segment: Option<NetworkSegmentId>,
    pub(in crate::tests) domain: uuid::Uuid,
    pub(in crate::tests) nvl_partition_monitor: Arc<Mutex<NvlPartitionMonitor>>,
    pub(in crate::tests) switch_cert_monitor: Arc<Mutex<SwitchCertificateMonitor>>,
    pub(in crate::tests) test_credential_manager: Arc<TestCredentialManager>,
    pub(in crate::tests) rms_sim: Arc<RmsSim>,
    pub(in crate::tests) test_component_manager:
        Option<Arc<component_manager::component_manager::ComponentManager>>,

    /// Firmware-object fetcher used by this environment's rack state handler.
    pub(in crate::tests) firmware_object_fetcher: Arc<dyn FirmwareObjectFetcher>,

    _drop_guard: DropGuard,
    // Background tasks are spawned here, hold it so they don't get dropped.
    _join_set: JoinSet<()>,
}

impl TestEnv {
    /// Returns the default admin network segment used by most tests.
    pub(in crate::tests) fn admin_segment(&self) -> NetworkSegmentId {
        *self
            .admin_segments
            .first()
            .expect("test env should have an admin segment")
    }

    /// Returns a reference to the default admin network segment used by most tests.
    pub(in crate::tests) fn admin_segment_ref(&self) -> &NetworkSegmentId {
        self.admin_segments
            .first()
            .expect("test env should have an admin segment")
    }

    #[allow(dead_code)]
    /// Creates an instance of MachineStateHandlerServices that are suitable for this
    /// test environment
    pub(in crate::tests) fn machine_state_handler_services(&self) -> MachineStateHandlerServices {
        MachineStateHandlerServices {
            db_pool: self.pool.clone(),
            db_reader: self.pool.clone().into(),
            redfish_client_pool: self.redfish_sim.clone(),
            bmc_credential_ops: self.redfish_sim.clone(),
            ipmi_tool: self.ipmi_tool.clone(),
            site_config: self.config.machine_state_handler_site_config().into(),
            component_manager: self.test_component_manager.clone(),
            credential_manager: self.test_credential_manager.clone(),
            bmc_rotation_gate: carbide_credential_rotation::RotationGate::new_for_family(
                db::credential_rotation::CredentialRotationType::Bmc,
            ),
            host_uefi_rotation_gate: carbide_credential_rotation::RotationGate::new_for_family(
                db::credential_rotation::CredentialRotationType::HostUefi,
            ),
            dpu_uefi_rotation_gate: carbide_credential_rotation::RotationGate::new_for_family(
                db::credential_rotation::CredentialRotationType::DpuUefi,
            ),
            dpu_bmc_service_rotation_gate:
                carbide_credential_rotation::RotationGate::new_for_family(
                    db::credential_rotation::CredentialRotationType::DpuBmcService,
                ),
            nic_lockdown_rotation_gate: carbide_credential_rotation::RotationGate::new_for_family(
                db::credential_rotation::CredentialRotationType::LockdownIkm,
            ),
            per_object_metrics_registry: self.per_object_metrics_registry(),
            per_object_info: None,
        }
    }

    /// Creates a per-object metrics registry from this test environment's
    /// observability config (disabled unless the config opts in).
    pub(in crate::tests) fn per_object_metrics_registry(
        &self,
    ) -> std::sync::Arc<carbide_health_metrics::PerObjectMetricsRegistry> {
        carbide_health_metrics::PerObjectMetricsRegistry::new(
            self.config
                .observability
                .per_object_metrics_for_classifications
                .clone(),
            std::time::Duration::from_secs(60),
        )
    }

    /// Creates an instance of RackStateHandlerServices that are suitable for this
    /// test environment
    pub(in crate::tests) fn rack_state_handler_services(&self) -> RackStateHandlerServices {
        RackStateHandlerServices {
            db_pool: self.pool.clone(),
            rms_client: self.rms_sim.as_rms_client(),
            site_config: RackConfig {
                rms: self.config.rms.clone(),
                rack_validation_config: self.config.rack_validation_config.clone(),
                rack_profiles: self.config.rack_profiles.clone(),
            }
            .into(),
            nvos_update_manager: test_nvos_update_manager(&self.rms_sim),
            credential_manager: self.test_credential_manager.clone(),
            component_manager: self.test_component_manager.clone(),
            nmx_cluster_switch_mtls_services:
                component_manager::config::switch_mtls_services_as_i32(
                    &component_manager::config::effective_nmx_cluster_switch_mtls_services(&[]),
                ),
            firmware_object_fetcher: self.firmware_object_fetcher.clone(),
            per_object_metrics_registry: self.per_object_metrics_registry(),
        }
    }

    /// Generates a config for Host+DPU pair
    pub(in crate::tests) fn managed_host_config(&self) -> ManagedHostConfig {
        ManagedHostConfig::default()
    }

    /// Create database transaction for tests.
    pub(in crate::tests) async fn db_txn(&self) -> sqlx::Transaction<'_, sqlx::Postgres> {
        self.pool
            .begin()
            .await
            .expect("Unable to create transaction on database pool")
    }

    fn fill_machine_information(
        &self,
        state: &ManagedHostState,
        machine: &Machine,
    ) -> ManagedHostState {
        //This block is to fill data that is populated within statemachine
        match state.clone() {
            ManagedHostState::DpuDiscoveringState { .. } => state.clone(),
            ManagedHostState::DPUInit { .. } => state.clone(),
            ManagedHostState::HostInit { machine_state } => {
                let mc = match machine_state {
                    model::machine::MachineState::Init => machine_state,
                    model::machine::MachineState::WaitingForPlatformConfiguration { .. } => {
                        machine_state
                    }
                    model::machine::MachineState::PollingBiosSetup { .. } => machine_state,
                    model::machine::MachineState::SetBootOrder { .. } => machine_state,
                    model::machine::MachineState::UefiSetup { .. } => machine_state,
                    model::machine::MachineState::WaitingForDiscovery => machine_state,
                    model::machine::MachineState::Discovered { .. } => machine_state,
                    model::machine::MachineState::WaitingForLockdown { .. } => machine_state,
                    model::machine::MachineState::Measuring { .. } => machine_state,
                    model::machine::MachineState::SpdmMeasuring { .. } => machine_state,

                    model::machine::MachineState::EnableIpmiOverLan => machine_state,
                    model::machine::MachineState::WaitingForBiosJob { .. } => machine_state,
                };
                ManagedHostState::HostInit { machine_state: mc }
            }
            ManagedHostState::Ready => state.clone(),
            ManagedHostState::BootConfiguring { .. } => state.clone(),
            ManagedHostState::Maintenance { .. } => state.clone(),
            ManagedHostState::Assigned { .. } => state.clone(),
            ManagedHostState::WaitingForCleanup { .. } => state.clone(),
            ManagedHostState::Created => state.clone(),
            ManagedHostState::ForceDeletion => state.clone(),
            ManagedHostState::Failed {
                details,
                machine_id,
                retry_count,
            } => ManagedHostState::Failed {
                details: FailureDetails {
                    cause: details.cause,
                    failed_at: machine.status.failure_details.failed_at,
                    source: details.source,
                },
                machine_id,
                retry_count,
            },
            ManagedHostState::DPUReprovision { .. } => state.clone(),
            ManagedHostState::ConfigureAstra { .. } => state.clone(),
            ManagedHostState::Measuring { .. } => state.clone(),
            ManagedHostState::PostAssignedMeasuring { .. } => state.clone(),
            ManagedHostState::PreAssignedMeasuring { .. } => state.clone(),
            ManagedHostState::StartAssignmentCycle => state.clone(),
            ManagedHostState::HostReprovision { .. } => state.clone(),
            ManagedHostState::RotatingBmc { .. } => state.clone(),
            ManagedHostState::RotatingHostUefi { .. } => state.clone(),
            ManagedHostState::Decommissioning { .. } => state.clone(),
            ManagedHostState::RotatingDpuUefi { .. } => state.clone(),
            ManagedHostState::RotatingNicLockdown => state.clone(),
            ManagedHostState::BomValidating { .. } => state.clone(),
            ManagedHostState::Validation { validation_state } => match validation_state {
                ValidationState::MachineValidation { machine_validation } => {
                    match machine_validation {
                        MachineValidatingState::MachineValidating {
                            context,
                            id: _,
                            completed,
                            total,
                            is_enabled,
                        } => {
                            let mut id =
                                machine.discovery_machine_validation_id.unwrap_or_default();
                            if context == "Cleanup" {
                                id = machine.cleanup_machine_validation_id.unwrap_or_default();
                            } else if context == "OnDemand" {
                                id = machine.on_demand_machine_validation_id.unwrap_or_default();
                            }
                            model::machine::ManagedHostState::Validation {
                                validation_state: ValidationState::MachineValidation {
                                    machine_validation: MachineValidatingState::MachineValidating {
                                        context,
                                        id,
                                        completed,
                                        total,
                                        is_enabled,
                                    },
                                },
                            }
                        }
                        MachineValidatingState::RebootHost { .. } => state.clone(),
                        MachineValidatingState::PrepareBootRepair { .. }
                        | MachineValidatingState::UnlockForBootRepair { .. }
                        | MachineValidatingState::CheckBootConfigForRepair { .. }
                        | MachineValidatingState::ConfigureBootBios { .. }
                        | MachineValidatingState::WaitingForBootBiosJob { .. }
                        | MachineValidatingState::PollingBootBiosSetup { .. }
                        | MachineValidatingState::RepairBootConfig { .. }
                        | MachineValidatingState::LockAfterBootRepair { .. } => state.clone(),
                    }
                }
            },
        }
    }

    pub(in crate::tests) async fn run_machine_state_controller_iteration_until_state_matches(
        &self,
        host_machine_id: &HostMachineId,
        max_iterations: u32,
        expected_state: ManagedHostState,
    ) {
        self.run_machine_state_controller_iteration_until_state_condition(
            host_machine_id,
            max_iterations,
            |machine| {
                let fixed_expected_state = self.fill_machine_information(&expected_state, machine);
                machine.current_state() == &fixed_expected_state
            },
        )
        .await;
    }

    /// Runs iterations of the machine state controller handler with the services
    /// in this test environment until the condition is met.  using a callback function
    /// allows the caller to use "matches!" to compare patterns instead of concrete values.
    pub(in crate::tests) async fn run_machine_state_controller_iteration_until_state_condition(
        &self,
        host_machine_id: &HostMachineId,
        max_iterations: u32,
        state_check: impl Fn(&Machine) -> bool,
    ) -> ManagedHostState {
        for _ in 0..max_iterations {
            self.machine_state_controller
                .lock()
                .await
                .run_single_iteration()
                .boxed()
                .await;

            let mut txn: sqlx::Transaction<'static, sqlx::Postgres> =
                self.pool.begin().await.unwrap();
            let machine = db::machine::find_one(
                txn.as_mut(),
                host_machine_id,
                model::machine::machine_search_config::MachineSearchConfig::default(),
            )
            .await
            .unwrap()
            .unwrap();

            if state_check(&machine) {
                return machine.state.value;
            }
        }
        let mut txn = self.pool.begin().await.unwrap();
        let machine = db::machine::find_one(
            txn.as_mut(),
            host_machine_id,
            model::machine::machine_search_config::MachineSearchConfig::default(),
        )
        .await
        .unwrap()
        .unwrap();
        panic!(
            "Expected Machine state condition not hit after {max_iterations} iterations; state is {:?}",
            machine.current_state()
        );
    }

    /// Runs one iteration of the machine state controller handler
    //// with the services in this test environment
    pub(in crate::tests) async fn run_machine_state_controller_iteration(&self) {
        self.machine_state_controller
            .lock()
            .await
            .run_single_iteration()
            .boxed()
            .await;
    }

    /// Runs one iteration of the network state controller handler with the services
    /// in this test environment
    pub(in crate::tests) async fn run_network_segment_controller_iteration(&self) {
        self.network_segment_controller
            .lock()
            .await
            .run_single_iteration()
            .boxed()
            .await;
    }

    /// Runs one iteration of the VPC prefix state controller handler with the services
    /// in this test environment.
    pub(in crate::tests) async fn run_vpc_prefix_controller_iteration(&self) {
        self.vpc_prefix_controller
            .lock()
            .await
            .run_single_iteration()
            .boxed()
            .await;
    }

    /// Runs one periodic-scan and processor iteration of the extension-service
    /// state controller with the services in this test environment.
    pub(in crate::tests) async fn run_extension_service_controller_iteration(&self) {
        self.extension_service_controller
            .lock()
            .await
            .run_single_iteration()
            .boxed()
            .await;
    }

    /// Runs one iteration of the SPDM state controller handler with the services
    /// in this test environment
    pub(in crate::tests) async fn run_spdm_controller_iteration(&self) {
        self.spdm_state_controller
            .lock()
            .await
            .run_single_iteration()
            .boxed()
            .await;
    }

    /// Runs one iteration of the SPDM state controller handler with the services
    /// in this test environment
    /// No requeuing of tasks is allowed
    pub(in crate::tests) async fn run_spdm_controller_iteration_no_requeue(&self) {
        self.spdm_state_controller
            .lock()
            .await
            .run_single_iteration_ext(false)
            .boxed()
            .await;
    }

    /// Runs one iteration of the IB partition state controller handler with the services
    /// in this test environment
    pub(in crate::tests) async fn run_ib_partition_controller_iteration(&self) {
        self.ib_partition_controller
            .lock()
            .await
            .run_single_iteration()
            .boxed()
            .await;
    }

    /// Runs one iteration of the rack state controller handler with the services
    /// in this test environment
    #[allow(clippy::await_holding_refcell_ref)]
    pub(in crate::tests) async fn run_rack_controller_iteration(&self) {
        self.rack_controller
            .lock()
            .await
            .run_single_iteration()
            .await;
    }

    pub(in crate::tests) async fn run_site_explorer_iteration(&self) {
        self.site_explorer
            .run_single_iteration()
            .boxed()
            .await
            .unwrap();
    }

    pub(in crate::tests) async fn run_ib_fabric_monitor_iteration(&self) {
        let _num_changes = self
            .ib_fabric_monitor
            .run_single_iteration()
            .boxed()
            .await
            .unwrap();
    }

    /// Runs the necessary iterations to return an instance back to an Assigned/Ready
    /// state after a network config update has added/removed an interface.
    pub(in crate::tests) async fn run_machine_state_controller_iteration_network_config_return_to_ready(
        &self,
        mh: &TestManagedHost,
        interfaces_added: bool,
    ) {
        if interfaces_added {
            // Move the network segment along
            self.run_network_segment_controller_iteration().await;
        }

        // Ticks for WaitingForConfigSynced
        self.run_machine_state_controller_iteration_until_state_matches(
            &mh.host().id,
            10,
            ManagedHostState::Assigned {
                instance_state: model::machine::InstanceState::NetworkConfigUpdate {
                    network_config_update_state:
                        model::machine::NetworkConfigUpdateState::WaitingForConfigSynced,
                },
            },
        )
        .await;

        // Simulate the DPU calling in, getting a response,
        // configuring itself, and reporting back.
        mh.network_configured(self).await;

        // Ticks to get us back to assigned ready after releasing old resources
        self.run_machine_state_controller_iteration_until_state_matches(
            &mh.host().id,
            10,
            ManagedHostState::Assigned {
                instance_state: model::machine::InstanceState::Ready,
            },
        )
        .await;
    }

    pub(in crate::tests) async fn override_machine_state_controller_handler(
        &self,
        handler: MachineStateHandler,
    ) {
        *self.machine_state_handler.inner.lock().await = handler;
    }

    // Returns all machines using FindMachinesByIds call.
    pub(in crate::tests) async fn find_machine(
        &self,
        id: &carbide_uuid::machine::MachineId,
    ) -> Vec<rpc::forge::Machine> {
        self.api
            .find_machines_by_ids(tonic::Request::new(rpc::forge::MachinesByIdsRequest {
                machine_ids: vec![*id],
                include_history: true,
            }))
            .await
            .unwrap()
            .into_inner()
            .machines
    }

    // Returns all instances using FindInstancesByIds call.
    pub(in crate::tests) async fn find_instances(
        &self,
        ids: Vec<InstanceId>,
    ) -> rpc::forge::InstanceList {
        self.api
            .find_instances_by_ids(tonic::Request::new(rpc::forge::InstancesByIdsRequest {
                instance_ids: ids,
            }))
            .await
            .unwrap()
            .into_inner()
    }

    pub(in crate::tests) async fn one_instance(&self, id: InstanceId) -> RpcInstance {
        let mut result = self
            .api
            .find_instances_by_ids(tonic::Request::new(rpc::forge::InstancesByIdsRequest {
                instance_ids: vec![id],
            }))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(result.instances.len(), 1);
        RpcInstance::new(result.instances.remove(0))
    }

    pub(in crate::tests) async fn create_vpc_and_tenant_segment_with_vpc_details(
        &self,
        vpc_details: rpc::forge::VpcCreationRequest,
    ) -> NetworkSegmentId {
        let vpc = self
            .api
            .create_vpc(tonic::Request::new(vpc_details))
            .await
            .unwrap()
            .into_inner();

        let tenant_network_id = create_tenant_network_segment(
            &self.api,
            vpc.id,
            FIXTURE_TENANT_NETWORK_SEGMENT_GATEWAYS[0],
            "TENANT",
            true,
        )
        .await;

        // Get the tenant segment into ready state
        self.run_network_segment_controller_iteration().await;
        self.run_network_segment_controller_iteration().await;

        tenant_network_id
    }

    pub(in crate::tests) async fn create_vpc_and_tenant_segments_with_vpc_details(
        &self,
        vpc_details: rpc::forge::VpcCreationRequest,
        segment_count: usize,
    ) -> Vec<NetworkSegmentId> {
        let vpc = self
            .api
            .create_vpc(tonic::Request::new(vpc_details))
            .await
            .unwrap()
            .into_inner();

        let mut segment_ids = Vec::default();
        for segment_index in 0..segment_count {
            segment_ids.push(
                create_tenant_network_segment(
                    &self.api,
                    vpc.id,
                    FIXTURE_TENANT_NETWORK_SEGMENT_GATEWAYS[segment_index],
                    "TENANT",
                    true,
                )
                .await,
            );

            // Get the tenant segment into ready state
            self.run_network_segment_controller_iteration().await;
            self.run_network_segment_controller_iteration().await;
        }
        segment_ids
    }

    pub(in crate::tests) async fn create_vpc_and_peer_vpc_with_tenant_segments(
        &self,
        vtype1: VpcVirtualizationType,
        vtype2: VpcVirtualizationType,
    ) -> (
        Option<VpcId>,
        Option<u32>,
        NetworkSegmentId,
        Option<VpcId>,
        Option<u32>,
        NetworkSegmentId,
    ) {
        self.create_vpc_and_peer_vpc_with_tenant_segments_for_tenants(
            FIXTURE_TENANT_ORG_ID,
            vtype1,
            "e65a9d69-39d2-4872-a53e-e5cb87c84e75",
            vtype2,
        )
        .await
    }

    /// Creates two VPCs for the provided tenants and attaches one tenant segment to each.
    pub(in crate::tests) async fn create_vpc_and_peer_vpc_with_tenant_segments_for_tenants(
        &self,
        tenant_organization_id: &str,
        vtype1: VpcVirtualizationType,
        peer_tenant_organization_id: &str,
        vtype2: VpcVirtualizationType,
    ) -> (
        Option<VpcId>,
        Option<u32>,
        NetworkSegmentId,
        Option<VpcId>,
        Option<u32>,
        NetworkSegmentId,
    ) {
        // Create the primary VPC and tenant segment.
        let vpc_details = VpcCreationRequest::builder(tenant_organization_id)
            .metadata(Metadata {
                name: "test vpc".to_string(),
                description: "".to_string(),
                labels: Default::default(),
            })
            .network_virtualization_type(vtype1)
            .tonic_request();

        let vpc = self.api.create_vpc(vpc_details).await.unwrap().into_inner();

        let tenant_network_id = create_tenant_network_segment(
            &self.api,
            vpc.id,
            FIXTURE_TENANT_NETWORK_SEGMENT_GATEWAYS[0],
            "TENANT1",
            true,
        )
        .await;

        // Drive the primary tenant segment to ready state.
        self.run_network_segment_controller_iteration().await;
        self.run_network_segment_controller_iteration().await;

        // Create the peer VPC and tenant segment.
        let peer_vpc_details = VpcCreationRequest::builder(peer_tenant_organization_id)
            .metadata(Metadata {
                name: "test peer vpc".to_string(),
                ..Default::default()
            })
            .network_virtualization_type(vtype2)
            .tonic_request();

        let peer_vpc = self
            .api
            .create_vpc(peer_vpc_details)
            .await
            .unwrap()
            .into_inner();

        let peer_tenant_network_id = create_tenant_network_segment(
            &self.api,
            peer_vpc.id,
            FIXTURE_TENANT_NETWORK_SEGMENT_GATEWAYS[1],
            "TENANT2",
            true,
        )
        .await;

        // Drive the peer tenant segment to ready state.
        self.run_network_segment_controller_iteration().await;
        self.run_network_segment_controller_iteration().await;

        (
            vpc.id,
            vpc.status.as_ref().and_then(|s| s.vni),
            tenant_network_id,
            peer_vpc.id,
            peer_vpc.status.as_ref().and_then(|s| s.vni),
            peer_tenant_network_id,
        )
    }

    pub(in crate::tests) async fn create_vpc_and_tenant_segment(&self) -> NetworkSegmentId {
        self.create_vpc_and_tenant_segment_with_vpc_details(
            VpcCreationRequest::builder(FIXTURE_TENANT_ORG_ID)
                .metadata(Metadata {
                    name: "test vpc 1".to_string(),
                    ..Default::default()
                })
                .rpc(),
        )
        .await
    }

    pub(in crate::tests) async fn create_vpc_and_tenant_segments(
        &self,
        segment_count: usize,
    ) -> Vec<NetworkSegmentId> {
        self.create_vpc_and_tenant_segments_with_vpc_details(
            VpcCreationRequest::builder(FIXTURE_TENANT_ORG_ID)
                .metadata(Metadata {
                    name: "test vpc 1".to_string(),
                    ..Default::default()
                })
                .rpc(),
            segment_count,
        )
        .await
    }

    pub(in crate::tests) async fn create_vpc_and_dual_tenant_segment(
        &self,
    ) -> (NetworkSegmentId, NetworkSegmentId) {
        let vpc = self
            .api
            .create_vpc(
                VpcCreationRequest::builder(FIXTURE_TENANT_ORG_ID)
                    .metadata(Metadata {
                        name: "test vpc 1".to_string(),
                        ..Default::default()
                    })
                    .tonic_request(),
            )
            .await
            .unwrap()
            .into_inner();

        let tenant_network_id_1 = create_tenant_network_segment(
            &self.api,
            vpc.id,
            FIXTURE_TENANT_NETWORK_SEGMENT_GATEWAYS[0],
            "TENANT",
            true,
        )
        .await;
        self.run_network_segment_controller_iteration().await;
        self.run_network_segment_controller_iteration().await;

        let tenant_network_id_2 = create_tenant_network_segment(
            &self.api,
            vpc.id,
            FIXTURE_TENANT_NETWORK_SEGMENT_GATEWAYS[1],
            "TENANT2",
            false,
        )
        .await;
        self.run_network_segment_controller_iteration().await;
        self.run_network_segment_controller_iteration().await;

        (tenant_network_id_1, tenant_network_id_2)
    }

    pub(in crate::tests) async fn run_nvl_partition_monitor_iteration(&self) {
        self.nvl_partition_monitor
            .lock()
            .await
            .run_single_iteration()
            .boxed()
            .await
            .unwrap();
    }

    #[allow(dead_code)]
    pub(in crate::tests) async fn run_switch_cert_monitor_iteration(
        &self,
    ) -> SwitchCertificateMonitorIterationResult {
        let cancel_token = CancellationToken::new();
        self.switch_cert_monitor
            .lock()
            .await
            .run_single_iteration(&cancel_token)
            .boxed()
            .await
            .unwrap()
    }

    pub(in crate::tests) fn db_reader(&self) -> PgPoolReader {
        self.pool.clone().into()
    }
}

pub(in crate::tests) fn get_config() -> CarbideConfig {
    default_config::get()
}

pub(in crate::tests) use model::test_support::TEST_RMS_RACK_PROFILE_ID;

/// Returns the default test config plus an RMS-ready NVL72 rack profile.
pub(in crate::tests) fn get_config_with_rack_profiles() -> CarbideConfig {
    let mut config = get_config();
    config.rack_profiles = rms_rack_profiles();

    config
}

fn extend_component_manager_rms_profiles(
    target: &mut RackProfileConfig,
    source: &RackProfileConfig,
) {
    // This fixture builds component-manager with RMS switch and power shelf
    // backends. Include custom RMS-ready profiles so tests exercise the profile
    // IDs used by API paths, but keep intentionally incomplete profiles for
    // call-time negative tests out of startup validation.
    target.rack_profiles.extend(
        source
            .rack_profiles
            .iter()
            .filter(|(_, profile)| is_component_manager_rms_ready_profile(profile))
            .map(|(profile_id, profile)| (profile_id.clone(), profile.clone())),
    );
}

fn is_component_manager_rms_ready_profile(profile: &RackProfile) -> bool {
    profile.product_family.is_some()
        && profile
            .rack_capabilities
            .switch
            .vendor
            .as_deref()
            .is_some_and(|vendor| !vendor.trim().is_empty())
        && profile
            .rack_capabilities
            .power_shelf
            .vendor
            .as_deref()
            .is_some_and(|vendor| !vendor.trim().is_empty())
}

/// crate::sqlx_test shares the pool with all testcases in a file. If there are many testcases in a file,
/// test cases will start getting PoolTimedOut error. To avoid it, each test case will be assigned
/// its own pool.
async fn create_pool(current_pool: sqlx::PgPool) -> sqlx::PgPool {
    let db_url = std::env::var("DATABASE_URL").expect("DATABASE_URL is not set.");
    let db_options = current_pool.connect_options();
    let db: &str = db_options
        .get_database()
        .expect("No database is set initially.");

    use sqlx::ConnectOptions;
    let connect_options = PgConnectOptions::from_str(&db_url)
        .unwrap()
        .database(db)
        .log_statements("INFO".parse().unwrap());

    sqlx::postgres::PgPoolOptions::new()
        .max_connections(15)
        .acquire_timeout(std::time::Duration::from_secs(15))
        .connect_with(connect_options)
        .await
        .expect("Pool creation failed.")
}

/// Creates an environment for unit-testing
///
/// This returns the `Api` object instance which can be used to simulate calls against
/// the Forge site controller, as well as mocks for dependent services that
/// can be inspected and passed to other systems.
pub(crate) async fn create_test_env(db_pool: sqlx::PgPool) -> TestEnv {
    create_test_env_with_overrides(db_pool, Default::default()).await
}

/// `create_test_env` with the fixture admin + host-inband site prefixes
/// routable and the host-inband network segment created -- the standard
/// setup for zero-DPU / `Nic`-policy ingestion tests.
pub(in crate::tests) async fn create_test_env_with_host_inband(db_pool: sqlx::PgPool) -> TestEnv {
    let env = create_test_env_with_overrides(
        db_pool,
        TestEnvOverrides {
            site_prefixes: Some(vec![
                IpNetwork::new(
                    network_segment::FIXTURE_ADMIN_NETWORK_SEGMENT_GATEWAY.network(),
                    network_segment::FIXTURE_ADMIN_NETWORK_SEGMENT_GATEWAY.prefix(),
                )
                .unwrap(),
                IpNetwork::new(
                    network_segment::FIXTURE_HOST_INBAND_NETWORK_SEGMENT_GATEWAY.network(),
                    network_segment::FIXTURE_HOST_INBAND_NETWORK_SEGMENT_GATEWAY.prefix(),
                )
                .unwrap(),
            ]),
            ..Default::default()
        },
    )
    .await;
    network_segment::create_host_inband_network_segment(&env.api, None).await;
    env
}

#[derive(Debug, Default)]
pub(in crate::tests) struct VerifierSimImpl {
    should_fail_parsing: Arc<AtomicBool>,
}

#[async_trait::async_trait]
impl Verifier for VerifierSimImpl {
    fn client(&self, _nras_config: nras::Config) -> Box<dyn nras::VerifierClient> {
        Box::new(VerifierClientSim::default())
    }
    async fn parse_attestation_outcome(
        &self,
        _nras_config: &nras::Config,
        _state: &RawAttestationOutcome,
    ) -> Result<ProcessedAttestationOutcome, NrasError> {
        if self.should_fail_parsing.load(Ordering::Relaxed) {
            Ok(ProcessedAttestationOutcome {
                attestation_passed: false,
                devices: HashMap::new(),
            })
        } else {
            Ok(ProcessedAttestationOutcome {
                attestation_passed: true,
                devices: HashMap::new(),
            })
        }
    }
}

#[derive(Debug, Default)]
pub(in crate::tests) struct VerifierClientSim {}

#[async_trait]
impl VerifierClient for VerifierClientSim {
    async fn attest_gpu(
        &self,
        _device_attestation_info: &DeviceAttestationInfo,
    ) -> Result<RawAttestationOutcome, NrasError> {
        let verifier_response = RawAttestationOutcome {
            overall_outcome: ("JWT".to_string(), "All_good".to_string()),
            devices_outcome: HashMap::new(),
        };
        Ok(verifier_response)
    }

    async fn attest_dpu(
        &self,
        _device_attestation_info: &DeviceAttestationInfo,
    ) -> Result<RawAttestationOutcome, NrasError> {
        Err(NrasError::NotImplemented)
    }
    async fn attest_cx7(
        &self,
        _device_attestation_info: &DeviceAttestationInfo,
    ) -> Result<RawAttestationOutcome, NrasError> {
        Err(NrasError::NotImplemented)
    }
}

pub(in crate::tests) async fn create_test_env_with_overrides(
    db_pool: sqlx::PgPool,
    overrides: TestEnvOverrides,
) -> TestEnv {
    let db_pool = create_pool(db_pool).await;
    let cancel_token = CancellationToken::new();
    let mut join_set = JoinSet::new();

    let work_lock_manager_handle = work_lock_manager::start(
        &mut join_set,
        db_pool.clone(),
        work_lock_manager::KeepaliveConfig::default(),
    )
    .await
    .expect("work_lock_manager failed to start: no availble connections?");

    let test_meter = TestMeter::default();
    let credential_manager = Arc::new(TestCredentialManager::default());

    let firmware_object_fetcher = overrides
        .firmware_object_fetcher
        .clone()
        .unwrap_or_else(|| Arc::new(reqwest::Client::new()));

    let chained_reader = ChainedCredentialReader::from(vec![
        Box::new(test_static_credential_snapshot()) as Box<dyn CredentialReader>,
        Box::new(credential_manager.clone()),
    ]);
    let composite_manager: Arc<dyn CredentialManager> = Arc::new(CompositeCredentialManager::new(
        chained_reader,
        credential_manager.clone(),
    ));

    let redfish_sim = Arc::new(RedfishSim::default());

    // Seed the site-wide host and DPU UEFI site-default credentials (version 0).
    // These are written during site setup in production; tests don't run that.
    // UEFI setup resolves and reads the site-wide credential in the controller
    // (`resolve_site_uefi_credentials`) through `redfish_client_pool`'s reader --
    // which in tests is the `RedfishSim`'s own store -- before calling the
    // (mocked) `uefi_setup`, so a missing credential surfaces as a hard error.
    // Seed centrally so every machine-driving test has them regardless of fixture.
    for key in [
        CredentialKey::HostUefi {
            credential_type: CredentialType::SiteDefault,
        },
        CredentialKey::DpuUefi {
            credential_type: CredentialType::SiteDefault,
        },
    ] {
        redfish_sim
            .seed_credential(
                &key,
                &Credentials::UsernamePassword {
                    username: "".to_string(),
                    password: "notforprod".to_string(),
                },
            )
            .await;
    }

    let nvlink_for_nmxc_sim = overrides
        .config
        .as_ref()
        .and_then(|c| c.nvlink_config.as_ref())
        .cloned()
        .unwrap_or_default();

    let nmxc_grpc_simulator = overrides.nmxc_simulator == Some(true);
    let nmxc_sim: Arc<dyn NmxcPool> = if nmxc_grpc_simulator {
        Arc::new(NmxcSimClient::simulator_for_nvlink_config(
            &nvlink_for_nmxc_sim,
        ))
    } else if let Some(n) = overrides.nmxc_fail_after_n_creates {
        Arc::new(NmxcSimClient::with_fail_after_n_creates(n))
    } else if overrides.nmxc_default_partition == Some(true) {
        Arc::new(NmxcSimClient::with_default_partition())
    } else if overrides.nmxc_unknown_partition == Some(true) {
        Arc::new(NmxcSimClient::with_unknown_partition())
    } else {
        Arc::new(NmxcSimClient::default())
    };

    let mut config = overrides.config.unwrap_or(get_config());
    if let Some(threshold) = overrides.dpu_agent_version_staleness_threshold {
        config.host_health.dpu_agent_version_staleness_threshold = threshold;
    }
    if let Some(prevent) = overrides.prevent_allocations_on_stale_dpu_agent_version {
        config
            .host_health
            .prevent_allocations_on_stale_dpu_agent_version = prevent;
    }

    config.fnn = if let Some(override_fnn_config) = overrides.fnn_config {
        Some(override_fnn_config)
    } else {
        Default::default()
    };

    config.compute_allocation_enforcement =
        overrides.compute_allocation_enforcement.unwrap_or_default();

    if let Some(val) = overrides.dhcp_lease_expiry_handling {
        config.dhcp_lease_expiry_handling = val;
    }

    let config = Arc::new(config);

    let site_fabric_networks = overrides
        .site_prefixes
        .as_ref()
        .unwrap_or(&TEST_SITE_PREFIXES)
        .to_vec();
    let site_fabric_count = site_fabric_networks.len() as u8;
    let site_fabric_prefixes = { SiteFabricPrefixList::from_ipnetwork_vec(site_fabric_networks) };

    let eth_virt_data = EthVirtData {
        asn: 65535,
        dhcp_servers: vec![FIXTURE_DHCP_RELAY_ADDRESS.parse().unwrap()],
        deny_prefixes: vec![],
        site_fabric_prefixes,
    };

    // Populate resource pools, leaving room for at least 5 networks, more if there are lots of
    // configured site prefixes
    let pool_size = site_fabric_count.max(5);
    let mut txn = db_pool.begin().await.unwrap();
    db::resource_pool::define_all_from(&mut txn, &pool_defs(pool_size))
        .await
        .unwrap();
    txn.commit().await.unwrap();

    let common_pools =
        db::resource_pool::create_common_pools(db_pool.clone(), ["default".to_string()].into())
            .await
            .expect("Creating pools should work");

    let ib_fabric_manager = ib_fabric_test_manager(&config, composite_manager.clone());

    let rms_sim = Arc::new(RmsSim::default());
    let mut component_manager_rack_profiles = get_config_with_rack_profiles().rack_profiles;
    extend_component_manager_rms_profiles(
        &mut component_manager_rack_profiles,
        &config.rack_profiles,
    );

    let mut site_explorer_rack_profiles = component_manager_rack_profiles.clone();
    site_explorer_rack_profiles
        .rack_profiles
        .extend(config.rack_profiles.rack_profiles.clone());

    let mut test_component_manager = component_manager::component_manager::build_component_manager(
        &component_manager::config::ComponentManagerConfig {
            nv_switch_backend: component_manager::nv_switch_manager::Backend::Rms,
            power_shelf_backend: component_manager::power_shelf_manager::Backend::Rms,
            compute_tray_backend: component_manager::compute_tray_manager::Backend::Mock,
            nv_switch_use_state_controller: true,
            power_shelf_use_state_controller: true,
            ..Default::default()
        },
        component_manager_rack_profiles,
        rms_sim.as_rms_client(),
        None,
        Some(db_pool.clone()),
        None,
    )
    .await
    .expect("test component manager should build");
    if let Some(compute_tray_manager) = overrides.compute_tray_manager.clone() {
        test_component_manager.compute_tray = compute_tray_manager;
    }
    let test_component_manager = Some(Arc::new(test_component_manager));
    let fake_endpoint_explorer = MockEndpointExplorer::default();

    let mut api_builder = TestApiBuilder::new(
        db_pool.clone(),
        common_pools.clone(),
        work_lock_manager_handle,
    )
    .with_credential_manager(composite_manager.clone())
    .with_runtime_config(config.clone())
    .with_eth_data(eth_virt_data)
    .with_nmxc_client_pool(nmxc_sim.clone())
    .with_metric_emitter(ApiMetricsEmitter::new(&test_meter.meter()))
    .with_redfish_pool(redfish_sim.clone())
    .with_ib_fabric_manager(ib_fabric_manager.clone())
    .with_endpoint_explorer(fake_endpoint_explorer.clone());

    if let Some(rms_client) = rms_sim.as_rms_client() {
        api_builder = api_builder.with_rms_client(rms_client);
    }

    if let Some(component_manager) = test_component_manager.clone() {
        api_builder = api_builder.with_component_manager(component_manager);
    }

    if let Some(dpf_sdk) = overrides.dpf_sdk.clone() {
        api_builder = api_builder.with_dpf_sdk(dpf_sdk);
    }

    let api = Arc::new(api_builder.build());

    let ib_fabric_monitor = IbFabricMonitor::new(
        db_pool.clone(),
        config.ib_fabrics.clone(),
        test_meter.meter(),
        api.ib_fabric_manager.clone(),
        config.host_health,
        api.work_lock_manager_handle.clone(),
    );

    let nvl_partition_monitor = NvlPartitionMonitor::new(
        db_pool.clone(),
        nmxc_sim.clone(),
        test_meter.meter(),
        config.nvlink_config.clone().unwrap(),
        config.host_health,
        api.work_lock_manager_handle.clone(),
    );

    let switch_cert_monitor = SwitchCertificateMonitor::new(
        db_pool.clone(),
        test_meter.meter(),
        config.nvlink_config.clone().unwrap(),
        test_component_manager.clone(),
        api.work_lock_manager_handle.clone(),
    );

    let attestation_enabled = config.attestation_enabled;
    let ipmi_tool = carbide_ipmi::test_support();
    let mut power_options: PowerOptionConfig = config.power_manager_options.clone().into();
    if let Some(v) = overrides.power_manager_enabled {
        power_options.enabled = v;
    }

    let reachability_params = ReachabilityParams {
        dpu_wait_time: Duration::seconds(0),
        power_down_wait: Duration::seconds(0),
        failure_retry_time: Duration::seconds(0),
        scout_reporting_timeout: config.machine_state_controller.scout_reporting_timeout,
        waiting_for_measurements_timeout: config
            .machine_state_controller
            .waiting_for_measurements_timeout,
        uefi_boot_wait: Duration::seconds(0),
    };

    let machine_swap = SwapHandler {
        inner: Arc::new(Mutex::new(
            MachineStateHandlerBuilder::builder()
                .hardware_models(config.get_firmware_config())
                .reachability_params(reachability_params)
                .attestation_enabled(attestation_enabled)
                .common_pools(common_pools.clone())
                .dpu_enable_secure_boot(config.dpu_config.dpu_enable_secure_boot)
                .machine_validation_config(MachineValidationConfig {
                    enabled: config.machine_validation_config.enabled,
                    run_interval: config.machine_validation_config.run_interval,
                    stale_run_timeout: config.machine_validation_config.stale_run_timeout,
                    tests: config.machine_validation_config.tests.clone(),
                    test_selection_mode: config.machine_validation_config.test_selection_mode,
                    approved_plugin_registries: config
                        .machine_validation_config
                        .approved_plugin_registries
                        .clone(),
                    allow_privileged_plugins: config
                        .machine_validation_config
                        .allow_privileged_plugins,
                    allow_full_host_plugins: config
                        .machine_validation_config
                        .allow_full_host_plugins,
                })
                .bom_validation(config.bom_validation)
                .instance_autoreboot_period(
                    config.machine_updater.instance_autoreboot_period.clone(),
                )
                .power_options_config(power_options)
                .dpf_sdk(overrides.dpf_sdk)
                .build(),
        )),
    };

    let verifier = VerifierSimImpl {
        should_fail_parsing: overrides
            .nras_should_fail_parsing
            .unwrap_or(Arc::new(AtomicBool::new(false))),
    };

    let spdm_swap = SwapHandler {
        inner: Arc::new(Mutex::new(SpdmAttestationStateHandler::new(
            Arc::new(verifier),
            nras::Config::default(),
        ))),
    };

    let state_controller_id = uuid::Uuid::new_v4().to_string();

    let per_object_metrics_registry = carbide_health_metrics::PerObjectMetricsRegistry::new(
        config
            .observability
            .per_object_metrics_for_classifications
            .clone(),
        std::time::Duration::from_secs(60),
    );

    let machine_controller = StateController::<MachineStateControllerIO>::builder()
        .database(db_pool.clone(), api.work_lock_manager_handle.clone())
        .meter("carbide_machines", test_meter.meter())
        .processor_id(state_controller_id.clone())
        .services(
            MachineStateHandlerServices {
                db_pool: db_pool.clone(),
                db_reader: db_pool.clone().into(),
                redfish_client_pool: redfish_sim.clone(),
                bmc_credential_ops: redfish_sim.clone(),
                ipmi_tool: ipmi_tool.clone(),
                site_config: config.machine_state_handler_site_config().into(),
                component_manager: test_component_manager.clone(),
                credential_manager: credential_manager.clone(),
                bmc_rotation_gate: carbide_credential_rotation::RotationGate::new_for_family(
                    db::credential_rotation::CredentialRotationType::Bmc,
                ),
                host_uefi_rotation_gate: carbide_credential_rotation::RotationGate::new_for_family(
                    db::credential_rotation::CredentialRotationType::HostUefi,
                ),
                dpu_uefi_rotation_gate: carbide_credential_rotation::RotationGate::new_for_family(
                    db::credential_rotation::CredentialRotationType::DpuUefi,
                ),
                dpu_bmc_service_rotation_gate:
                    carbide_credential_rotation::RotationGate::new_for_family(
                        db::credential_rotation::CredentialRotationType::DpuBmcService,
                    ),
                nic_lockdown_rotation_gate:
                    carbide_credential_rotation::RotationGate::new_for_family(
                        db::credential_rotation::CredentialRotationType::LockdownIkm,
                    ),
                per_object_metrics_registry: per_object_metrics_registry.clone(),
                per_object_info: None,
            }
            .into(),
        )
        .state_handler(Arc::new(machine_swap.clone()))
        .io(Arc::new(MachineStateControllerIO {
            host_health: config.host_health,
            sla_config: model::machine::slas::MachineSlaConfig::new(
                config.machine_state_controller.failure_retry_time,
            ),
        }))
        .build_for_manual_iterations(cancel_token.clone())
        .expect("Unable to build state controller");

    let spdm_controller = StateController::<SpdmStateControllerIO>::builder()
        .database(db_pool.clone(), api.work_lock_manager_handle.clone())
        .meter("spdm", test_meter.meter())
        .processor_id(state_controller_id.clone())
        .services(
            SpdmStateHandlerServices {
                db_pool: db_pool.clone(),
                redfish_client_pool: redfish_sim.clone(),
            }
            .into(),
        )
        .state_handler(Arc::new(spdm_swap.clone()))
        .io(Arc::new(SpdmStateControllerIO {}))
        .build_for_manual_iterations(cancel_token.clone())
        .expect("Unable to build spdm state controller");

    let ib_swap = SwapHandler {
        inner: Arc::new(Mutex::new(IBPartitionStateHandler::default())),
    };

    let ib_controller = StateController::builder()
        .database(db_pool.clone(), api.work_lock_manager_handle.clone())
        .meter("carbide_machines", test_meter.meter())
        .processor_id(state_controller_id.clone())
        .services(
            IBPartitionStateHandlerServices {
                db_pool: db_pool.clone(),
                ib_fabric_manager: api.ib_fabric_manager.clone(),
                ib_pools: common_pools.infiniband.clone(),
            }
            .into(),
        )
        .state_handler(Arc::new(ib_swap.clone()))
        .build_for_manual_iterations(cancel_token.clone())
        .expect("Unable to build state controller");

    let network_swap = SwapHandler {
        inner: Arc::new(Mutex::new(NetworkSegmentStateHandler::new(
            overrides
                .network_segments_drain_period
                .unwrap_or(chrono::Duration::milliseconds(500)),
            common_pools.ethernet.pool_vlan_id.clone(),
            common_pools.ethernet.pool_vni.clone(),
        ))),
    };

    let mut network_controller = StateController::builder()
        .database(db_pool.clone(), api.work_lock_manager_handle.clone())
        .meter("carbide_machines", test_meter.meter())
        .processor_id(state_controller_id.clone())
        .services(
            NetworkSegmentStateHandlerServices {
                db_pool: db_pool.clone(),
            }
            .into(),
        )
        .state_handler(Arc::new(network_swap.clone()))
        .build_for_manual_iterations(cancel_token.clone())
        .expect("Unable to build state controller");

    let vpc_prefix_swap = SwapHandler {
        inner: Arc::new(Mutex::new(VpcPrefixStateHandler::new(
            overrides
                .vpc_prefixes_drain_period
                .unwrap_or(chrono::Duration::milliseconds(500)),
        ))),
    };

    let vpc_prefix_controller = StateController::builder()
        .database(db_pool.clone(), api.work_lock_manager_handle.clone())
        .meter("carbide_vpc_prefixes", test_meter.meter())
        .processor_id(state_controller_id.clone())
        .services(
            VpcPrefixStateHandlerServices {
                db_pool: db_pool.clone(),
            }
            .into(),
        )
        .state_handler(Arc::new(vpc_prefix_swap.clone()))
        .build_for_manual_iterations(cancel_token.clone())
        .expect("Unable to build VpcPrefixStateController");

    let extension_service_controller = StateController::builder()
        .database(db_pool.clone(), api.work_lock_manager_handle.clone())
        .meter("carbide_extension_services", test_meter.meter())
        .processor_id(state_controller_id.clone())
        .services(
            ExtensionServiceStateHandlerServices {
                db_pool: db_pool.clone(),
                dpf_sdk: api.dpf_sdk.clone(),
            }
            .into(),
        )
        .state_handler(Arc::new(ExtensionServiceStateHandler))
        .build_for_manual_iterations(cancel_token.clone())
        .expect("Unable to build ExtensionServiceStateController");

    let rack_controller = StateController::builder()
        .database(db_pool.clone(), api.work_lock_manager_handle.clone())
        .meter("carbide_racks", test_meter.meter())
        .processor_id(state_controller_id.clone())
        .services(
            RackStateHandlerServices {
                db_pool: db_pool.clone(),
                rms_client: rms_sim.as_rms_client(),
                site_config: RackConfig {
                    rms: config.rms.clone(),
                    rack_validation_config: config.rack_validation_config.clone(),
                    rack_profiles: config.rack_profiles.clone(),
                }
                .into(),
                nvos_update_manager: test_nvos_update_manager(&rms_sim),
                credential_manager: credential_manager.clone(),
                component_manager: test_component_manager.clone(),
                nmx_cluster_switch_mtls_services:
                    component_manager::config::switch_mtls_services_as_i32(
                        &component_manager::config::effective_nmx_cluster_switch_mtls_services(&[]),
                    ),
                firmware_object_fetcher: firmware_object_fetcher.clone(),
                per_object_metrics_registry: per_object_metrics_registry.clone(),
            }
            .into(),
        )
        .state_handler(Arc::new(RackStateHandler::default()))
        .build_for_manual_iterations(cancel_token.clone())
        .expect("Unable to build RackStateController");

    // The API server is launched with a disabled site-explorer config so that it doesn't launch one
    // on its own. TestEnv's site_explorer is a separate instance talking to the same database that
    // *is* enabled, so it gets a different config. The purpose is so that tests can manually run
    // site explorer iterations to seed data/etc.
    let site_explorer = SiteExplorer::new(
        db_pool.clone(),
        SiteExplorerConfig {
            enabled: Arc::new(true.into()),
            retained_boot_interface_window: None,
            // run_interval shouldn't matter, this should not be run(), we only trigger intervals manually.
            run_interval: Duration::seconds(0).to_std().unwrap(),
            concurrent_explorations: 100,
            explorations_per_run: 100,
            create_machines: Arc::new(true.into()),
            machines_created_per_run: 1,
            override_target_ip: None,
            override_target_port: None,
            bmc_proxy: Arc::new(Default::default()),
            allow_changing_bmc_proxy: None,
            reset_rate_limit: Duration::hours(1),
            admin_segment_type_non_dpu: Arc::new(false.into()),
            create_power_shelves: Arc::new(true.into()),
            power_shelves_created_per_run: 1,
            create_switches: Arc::new(true.into()),
            switches_created_per_run: 1,
            rotate_switch_nvos_credentials: Arc::new(false.into()),
            dpu_policy: None,
            deprecated_force_dpu_nic_mode: None,
            // Tests use MockEndpointExplorer. So this doesn't affect anything.
            explore_mode: SiteExplorerExploreMode::NvRedfish,
        },
        test_meter.meter(),
        api.endpoint_exploration_service.clone(),
        api.bmc_client.clone(),
        common_pools.clone(),
        api.work_lock_manager_handle.clone(),
        site_explorer_rack_profiles,
        rms_sim.as_rms_client(),
        credential_manager.clone(),
    );

    // Create some instance types
    let mut txn = api.txn_begin().await.unwrap();

    for _ in 0..3 {
        let uid = uuid::Uuid::new_v4();

        // Prepare some attributes for creation and comparison later
        let desired_capabilities = vec![InstanceTypeMachineCapabilityFilter {
            capability_type: MachineCapabilityType::Cpu,
            ..Default::default()
        }];

        let metadata = Metadata {
            name: format!("the best type {uid}"),
            description: "".to_string(),
            labels: HashMap::new(),
        };

        let id = InstanceTypeId::from(uid);

        let _it = create_instance_type(&mut txn, &id, &metadata, &desired_capabilities)
            .await
            .unwrap();
    }

    txn.commit().await.unwrap();

    // Create domain
    let domain: carbide_uuid::domain::DomainId = api
        .create_domain(Request::new(rpc::protos::dns::CreateDomainRequest {
            name: "dwrt1.com".to_string(),
        }))
        .await
        .unwrap()
        .into_inner()
        .id
        .map(::carbide_uuid::domain::DomainId::try_from)
        .unwrap()
        .unwrap();

    let (admin_segments, underlay_segment) = if overrides.create_network_segments.unwrap_or(true) {
        // Create admin network
        let admin_segments = vec![create_admin_network_segment(&api).await];
        network_controller.run_single_iteration().await;
        network_controller.run_single_iteration().await;

        // Create underlay network
        let underlay = Some(create_underlay_network_segment(&api).await);
        network_controller.run_single_iteration().await;
        network_controller.run_single_iteration().await;

        // Synthetic segment for operator static IPs outside Carbide-managed prefixes (expected
        // machine / switch / shelf BMC pre-allocation). Required for static-BMC integration tests.
        // Pass the domain to match production behavior (db_init passes Some(domain_id)).
        create_static_assignments_segment(&api, Some(domain)).await;
        network_controller.run_single_iteration().await;
        network_controller.run_single_iteration().await;

        (admin_segments, underlay)
    } else {
        (Vec::new(), None)
    };

    TestEnv {
        api,
        common_pools,
        config,
        pool: db_pool,
        redfish_sim,
        ib_fabric_manager,
        ipmi_tool,
        machine_state_controller: Arc::new(Mutex::new(machine_controller)),
        spdm_state_controller: Arc::new(Mutex::new(spdm_controller)),
        machine_state_handler: machine_swap,
        ib_fabric_monitor: Arc::new(ib_fabric_monitor),
        ib_partition_controller: Arc::new(Mutex::new(ib_controller)),
        network_segment_controller: Arc::new(Mutex::new(network_controller)),
        vpc_prefix_controller: Arc::new(Mutex::new(vpc_prefix_controller)),
        extension_service_controller: Arc::new(Mutex::new(extension_service_controller)),
        rack_controller: Arc::new(Mutex::new(rack_controller)),
        reachability_params,
        attestation_enabled,
        test_meter,
        site_explorer,
        nmxc_sim,
        nmxc_grpc_simulator,
        endpoint_explorer: fake_endpoint_explorer,
        admin_segments,
        underlay_segment,
        domain: domain.into(),
        nvl_partition_monitor: Arc::new(Mutex::new(nvl_partition_monitor)),
        switch_cert_monitor: Arc::new(Mutex::new(switch_cert_monitor)),
        test_credential_manager: credential_manager.clone(),
        rms_sim,
        test_component_manager,
        firmware_object_fetcher,
        _drop_guard: cancel_token.drop_guard(),
        _join_set: join_set,
    }
}

pub(in crate::tests) async fn get_instance_type_fixture_id(env: &TestEnv) -> String {
    // Find the existing instance types in the test env
    let existing_instance_type_ids = env
        .api
        .find_instance_type_ids(tonic::Request::new(
            rpc::forge::FindInstanceTypeIdsRequest {},
        ))
        .await
        .unwrap()
        .into_inner()
        .instance_type_ids;

    env.api
        .find_instance_types_by_ids(tonic::Request::new(
            rpc::forge::FindInstanceTypesByIdsRequest {
                instance_type_ids: existing_instance_type_ids,
                include_allocation_stats: false,
                tenant_organization_id: None,
            },
        ))
        .await
        .unwrap()
        .into_inner()
        .instance_types
        .pop()
        .unwrap()
        .id
}

pub(in crate::tests) async fn populate_network_security_groups(api: Arc<Api>) {
    // Create tenant orgs
    let default_tenant_org = "Tenant1";
    let _ = api
        .create_tenant(tonic::Request::new(rpc::forge::CreateTenantRequest {
            organization_id: default_tenant_org.to_string(),
            routing_profile_type: None,
            metadata: Some(rpc::forge::Metadata {
                name: default_tenant_org.to_string(),
                description: "".to_string(),
                labels: vec![],
            }),
        }))
        .await
        .unwrap();

    let tenant_org2 = "Tenant2";
    let _ = api
        .create_tenant(tonic::Request::new(rpc::forge::CreateTenantRequest {
            organization_id: tenant_org2.to_string(),
            routing_profile_type: None,
            metadata: Some(rpc::forge::Metadata {
                name: tenant_org2.to_string(),
                description: "".to_string(),
                labels: vec![],
            }),
        }))
        .await
        .unwrap();

    // Create default network security groups.
    let mut txn = api.txn_begin().await.unwrap();

    // Just a default ID for group and single rule.
    let uid = "fd3ab096-d811-11ef-8fe9-7be4b2483448";

    let rules = vec![network_security_group::NetworkSecurityGroupRule {
        id: Some(uid.to_string()),
        direction: network_security_group::NetworkSecurityGroupRuleDirection::Ingress,
        ipv6: false,
        src_port_start: Some(80),
        src_port_end: Some(32768),
        dst_port_start: Some(80),
        dst_port_end: Some(32768),
        protocol: network_security_group::NetworkSecurityGroupRuleProtocol::Any,
        action: network_security_group::NetworkSecurityGroupRuleAction::Deny,
        priority: 9001,
        src_net: network_security_group::NetworkSecurityGroupRuleNet::Prefix(
            "0.0.0.0/0".parse().unwrap(),
        ),
        dst_net: network_security_group::NetworkSecurityGroupRuleNet::Prefix(
            "0.0.0.0/0".parse().unwrap(),
        ),
    }];

    let metadata = Metadata {
        name: "default_network_security_group_1".to_string(),
        description: "".to_string(),
        labels: HashMap::new(),
    };

    let id = uid.parse().unwrap();

    let tenant_org = default_tenant_org.parse::<TenantOrganizationId>().unwrap();

    let _it =
        create_network_security_group(&mut txn, &id, &tenant_org, None, &metadata, false, &rules)
            .await
            .unwrap();

    // Create one more NSG with a different name.
    // The rules can be the same.
    // Just another default ID for group and single rule.
    let uid = "b65b13d6-d81c-11ef-9252-b346dc360bd4";
    let metadata = Metadata {
        name: "default_network_security_group_2".to_string(),
        description: "".to_string(),
        labels: HashMap::new(),
    };
    let id = uid.parse().unwrap();

    let _it =
        create_network_security_group(&mut txn, &id, &tenant_org, None, &metadata, false, &rules)
            .await
            .unwrap();

    // One more for the second tenant
    let uid = "ddfcabc4-92dc-41e2-874e-2c7eeb9fa156";
    let metadata = Metadata {
        name: "default_network_security_group_3".to_string(),
        description: "".to_string(),
        labels: HashMap::new(),
    };
    let id = uid.parse().unwrap();

    let _it = create_network_security_group(
        &mut txn,
        &id,
        &tenant_org2.parse::<TenantOrganizationId>().unwrap(),
        None,
        &metadata,
        false,
        &rules,
    )
    .await
    .unwrap();

    txn.commit().await.unwrap();
}

fn test_static_credential_snapshot() -> CredentialSnapshot {
    use std::collections::HashMap;

    use base64::Engine;

    let test_key_b64 = base64::engine::general_purpose::STANDARD.encode([0u8; 32]);
    let mut encryption_keys = HashMap::new();
    encryption_keys.insert("test".to_string(), test_key_b64);
    CredentialSnapshot {
        dpu_redfish_factory_default: Some(UsernamePassword {
            username: "root".to_string(),
            password: "dpuredfish_dpuhardwaredefault".to_string(),
        }),
        dpu_redfish_site_default: Some(UsernamePassword {
            username: "root".to_string(),
            password: "dpuredfish_sitedefault".to_string(),
        }),
        host_redfish_site_default: Some(UsernamePassword {
            username: "root".to_string(),
            password: "hostredfish_sitedefault".to_string(),
        }),
        machine_identity: Some(carbide_secrets::MachineIdentityConfig { encryption_keys }),
        ..Default::default()
    }
}

fn pool_defs(fabric_len: u8) -> HashMap<String, resource_pool::ResourcePoolDef> {
    let mut defs = HashMap::new();
    defs.insert(
        "ib_fabrics.default.pkey".to_string(),
        resource_pool::ResourcePoolDef {
            pool_type: resource_pool::ResourcePoolType::Integer,
            ranges: vec![
                resource_pool::Range {
                    start: "1".to_string(),
                    end: "100".to_string(),
                    auto_assign: true,
                },
                resource_pool::Range {
                    start: "101".to_string(),
                    end: "200".to_string(),
                    auto_assign: false,
                },
            ],
            prefix: None,
            delegate_prefix_len: None,
        },
    );
    defs.insert(
        model::resource_pool::common::VPC_DPU_LOOPBACK.to_string(),
        resource_pool::ResourcePoolDef {
            pool_type: resource_pool::ResourcePoolType::Ipv4,
            // Must match a network_prefix in fixtures/create_network_segment.sql
            prefix: None,
            ranges: vec![resource_pool::Range {
                start: "10.255.255.0".to_string(),
                end: "10.255.255.127".to_string(),
                auto_assign: true,
            }],
            delegate_prefix_len: None,
        },
    );
    defs.insert(
        model::resource_pool::common::LOOPBACK_IP.to_string(),
        resource_pool::ResourcePoolDef {
            pool_type: resource_pool::ResourcePoolType::Ipv4,
            // Must match a network_prefix in fixtures/create_network_segment.sql
            prefix: Some("172.20.0.0/24".to_string()),
            ranges: vec![],
            delegate_prefix_len: None,
        },
    );
    defs.insert(
        model::resource_pool::common::VNI.to_string(),
        resource_pool::ResourcePoolDef {
            pool_type: resource_pool::ResourcePoolType::Integer,
            ranges: vec![resource_pool::Range {
                start: 10_001.to_string(),
                end: (10_001 + fabric_len as u16 - 1).to_string(),
                auto_assign: true,
            }],
            prefix: None,
            delegate_prefix_len: None,
        },
    );
    defs.insert(
        model::resource_pool::common::DPA_VNI.to_string(),
        resource_pool::ResourcePoolDef {
            pool_type: resource_pool::ResourcePoolType::Integer,
            ranges: vec![resource_pool::Range {
                start: 40001.to_string(),
                end: (40001 + fabric_len as u16 - 1).to_string(),
                auto_assign: true,
            }],
            prefix: None,
            delegate_prefix_len: None,
        },
    );
    defs.insert(
        model::resource_pool::common::VLANID.to_string(),
        resource_pool::ResourcePoolDef {
            pool_type: resource_pool::ResourcePoolType::Integer,
            ranges: vec![resource_pool::Range {
                start: 1.to_string(),
                end: (1 + fabric_len as u16 - 1).to_string(),
                auto_assign: true,
            }],
            prefix: None,
            delegate_prefix_len: None,
        },
    );
    defs.insert(
        model::resource_pool::common::VPC_VNI.to_string(),
        resource_pool::ResourcePoolDef {
            pool_type: resource_pool::ResourcePoolType::Integer,
            ranges: vec![
                resource_pool::Range {
                    start: 20001.to_string(),
                    end: (20001 + fabric_len as u16 - 1).to_string(),
                    auto_assign: true,
                },
                resource_pool::Range {
                    start: 60001.to_string(),
                    end: (60001 + fabric_len as u16 - 1).to_string(),
                    auto_assign: false,
                },
            ],
            prefix: None,
            delegate_prefix_len: None,
        },
    );

    defs.insert(
        model::resource_pool::common::EXTERNAL_VPC_VNI.to_string(),
        resource_pool::ResourcePoolDef {
            pool_type: resource_pool::ResourcePoolType::Integer,
            ranges: vec![resource_pool::Range {
                start: 50001.to_string(),
                end: (50001 + fabric_len as u16 - 1).to_string(),
                auto_assign: true,
            }],
            prefix: None,
            delegate_prefix_len: None,
        },
    );
    defs.insert(
        model::resource_pool::common::FNN_ASN.to_string(),
        resource_pool::ResourcePoolDef {
            pool_type: resource_pool::ResourcePoolType::Integer,
            ranges: vec![resource_pool::Range {
                start: "30001".to_string(),
                end: "30035".to_string(),
                auto_assign: true,
            }],
            prefix: None,
            delegate_prefix_len: None,
        },
    );
    defs
}

/// Emulates the `DiscoveryCompleted` request of a DPU/Host
pub(in crate::tests) async fn discovery_completed(env: &TestEnv, machine_id: &MachineId) {
    let _response = env
        .api
        .discovery_completed(Request::new(rpc::forge::MachineDiscoveryCompletedRequest {
            machine_id: Some(*machine_id),
        }))
        .await
        .unwrap()
        .into_inner();
}

/// Fake an iteration of forge-dpu-agent requesting network config, applying it, and reporting back
pub(in crate::tests) async fn network_configured(
    env: &TestEnv,
    dpu_machine_ids: &Vec<DpuMachineId>,
) {
    for dpu_machine_id in dpu_machine_ids {
        network_configured_with_health(env, dpu_machine_id, None).await
    }
}

/// Fake an iteration of forge-dpu-agent requesting network config, applying it, and reporting back.
/// When reporting back, the health reported by the DPU can be overrridden
pub(in crate::tests) async fn network_configured_with_health(
    env: &TestEnv,
    dpu_machine_id: &DpuMachineId,
    dpu_health: Option<rpc::health::HealthReport>,
) {
    network_configured_with_health_and_ext_services(env, dpu_machine_id, dpu_health, None).await
}

/// Fake an iteration of forge-dpu-agent requesting network config, applying it, and reporting back.
/// When reporting back, the health and extension services statuses reported by the DPU can be overrridden
// This fixture reports the compatibility fields populated for older agents.
#[allow(deprecated)]
pub(in crate::tests) async fn network_configured_with_health_and_ext_services(
    env: &TestEnv,
    dpu_machine_id: &DpuMachineId,
    dpu_health: Option<rpc::health::HealthReport>,
    extension_services_state: Option<rpc::forge::DpuExtensionServiceDeploymentStatus>,
) {
    let network_config = env
        .api
        .get_managed_host_network_config(Request::new(
            rpc::forge::ManagedHostNetworkConfigRequest {
                dpu_machine_id: Some((*dpu_machine_id).into()),
            },
        ))
        .await
        .unwrap()
        .into_inner();

    let instance_network_config_version =
        if network_config.instance_network_config_version.is_empty() {
            None
        } else {
            Some(network_config.instance_network_config_version.clone())
        };
    let instance: Option<rpc::Instance> = env
        .api
        .find_instance_by_machine_id(Request::new(dpu_machine_id.into()))
        .await
        .unwrap()
        .into_inner()
        .instances
        .pop();
    let instance_config_version = if let Some(instance) = instance {
        // If an instance is reported via this API, the version should match what we
        // get via the GetManagedHostNetworkConfig API
        if !network_config.use_admin_network {
            assert_eq!(
                instance_network_config_version.as_ref().unwrap().as_str(),
                instance.network_config_version,
                "Different network config versions reported via FindInstanceByMachineId and GetManagedHostNetworkConfig"
            );
        }
        Some(instance.config_version)
    } else {
        None
    };

    let interfaces = if network_config.use_admin_network {
        let iface = network_config
            .admin_interface
            .as_ref()
            .expect("use_admin_network true so admin_interface should be Some");
        vec![rpc::forge::InstanceInterfaceStatusObservation {
            function_type: iface.function_type,
            virtual_function_id: None,
            mac_address: None,
            addresses: build_dual_stack_list(
                iface.ip.clone(),
                iface.ipv6_interface_config.as_ref().map(|v6| v6.ip.clone()),
            ),
            prefixes: build_dual_stack_list(
                iface.interface_prefix.clone(),
                iface
                    .ipv6_interface_config
                    .as_ref()
                    .map(|v6| v6.interface_prefix.clone()),
            ),
            gateways: build_dual_stack_list(iface.gateway.clone(), None),
            network_security_group: None,
            internal_uuid: iface.internal_uuid.clone(),
        }]
    } else {
        let mut interfaces = vec![];
        for iface in network_config.tenant_interfaces.iter() {
            interfaces.push(rpc::forge::InstanceInterfaceStatusObservation {
                function_type: iface.function_type,
                virtual_function_id: iface.virtual_function_id,
                mac_address: None,
                addresses: build_dual_stack_list(
                    iface.ip.clone(),
                    iface.ipv6_interface_config.as_ref().map(|v6| v6.ip.clone()),
                ),
                prefixes: build_dual_stack_list(
                    iface.interface_prefix.clone(),
                    iface
                        .ipv6_interface_config
                        .as_ref()
                        .map(|v6| v6.interface_prefix.clone()),
                ),
                gateways: build_dual_stack_list(iface.gateway.clone(), None),
                network_security_group: None,
                internal_uuid: iface.internal_uuid.clone(),
            });
        }
        interfaces
    };

    let dpu_health = dpu_health.unwrap_or_else(|| rpc::health::HealthReport {
        source: "forge-dpu-agent".to_string(),
        triggered_by: None,
        observed_at: None,
        successes: vec![],
        alerts: vec![],
    });

    let dpu_extension_services: Vec<rpc::forge::DpuExtensionServiceStatusObservation> =
        network_config
            .dpu_extension_services
            .iter()
            .map(
                |extension_service| rpc::forge::DpuExtensionServiceStatusObservation {
                    service_id: extension_service.service_id.clone(),
                    service_type: extension_service.service_type,
                    service_name: "".to_string(),
                    version: extension_service.version.to_string(),
                    state: extension_services_state.unwrap_or(
                        rpc::forge::DpuExtensionServiceDeploymentStatus::DpuExtensionServiceRunning,
                    ) as i32,
                    components: vec![],
                    message: "".to_string(),
                    removed: extension_service.removed.clone(),
                },
            )
            .collect();

    let status = rpc::forge::DpuNetworkStatus {
        dpu_machine_id: Some((*dpu_machine_id).into()),
        dpu_agent_version: Some(dpu::TEST_DPU_AGENT_VERSION.to_string()),
        observed_at: None,
        dpu_health: Some(dpu_health),
        network_config_version: Some(network_config.managed_host_config_version.clone()),
        instance_id: network_config.instance_id,
        instance_config_version: instance_config_version.clone(),
        instance_network_config_version: instance_network_config_version.clone(),
        interfaces,
        network_config_error: None,
        client_certificate_expiry_unix_epoch_secs: None,
        fabric_interfaces: vec![],
        last_dhcp_requests: vec![],
        dpu_extension_service_version: network_config
            .instance
            .map(|instance| instance.dpu_extension_service_version),
        dpu_extension_services,
        astra_config_status: None,
    };
    tracing::trace!(
        network_config_version = %status.network_config_version.as_ref().unwrap(),
        instance_network_config_version = ?instance_network_config_version,
        instance_config_version = ?instance_config_version,
        "machine network configured",
    );
    let _ = env
        .api
        .record_dpu_network_status(Request::new(status))
        .await
        .unwrap();
}

/// Fake hardware health service reporting health
pub(in crate::tests) async fn simulate_hardware_health_report(
    env: &TestEnv,
    host_machine_id: &MachineId,
    health_report: health_report::HealthReport,
) {
    use rpc::forge::forge_server::Forge;
    use rpc::forge::{HealthReportEntry, InsertMachineHealthReportRequest};
    use tonic::Request;

    let _ = env
        .api
        .insert_machine_health_report(Request::new(InsertMachineHealthReportRequest {
            machine_id: Some(*host_machine_id),
            health_report_entry: Some(HealthReportEntry {
                report: Some(health_report.into()),
                ..Default::default()
            }),
        }))
        .await
        .unwrap();
}

/// Send a health report entry
pub(in crate::tests) async fn send_health_report_entry(
    env: &TestEnv,
    machine_id: &MachineId,
    entry: (HealthReport, HealthReportApplyMode),
) {
    use rpc::forge::forge_server::Forge;
    use tonic::Request;
    let _ = env
        .api
        .insert_machine_health_report(Request::new(InsertMachineHealthReportRequest {
            machine_id: Some(*machine_id),
            health_report_entry: Some(HealthReportEntry {
                report: Some(entry.0.into()),
                mode: entry.1 as i32,
            }),
        }))
        .await
        .unwrap();
}

/// Remove a health report entry
pub(in crate::tests) async fn remove_health_report_entry(
    env: &TestEnv,
    machine_id: &MachineId,
    source: String,
) {
    use rpc::forge::forge_server::Forge;
    use tonic::Request;
    let _ = env
        .api
        .remove_machine_health_report(Request::new(RemoveMachineHealthReportRequest {
            machine_id: Some(*machine_id),
            source,
        }))
        .await
        .unwrap();
}

pub(in crate::tests) async fn forge_agent_control(
    env: &TestEnv,
    machine_id: carbide_uuid::machine::MachineId,
) -> rpc::forge::ForgeAgentControlResponse {
    let _ = reboot_completed(env, machine_id).await;

    env.api
        .forge_agent_control(Request::new(rpc::forge::ForgeAgentControlRequest {
            machine_id: Some(machine_id),
        }))
        .await
        .unwrap()
        .into_inner()
}

/// Create a managed host with 1 DPU (default config)
pub(in crate::tests) async fn create_managed_host(env: &TestEnv) -> TestManagedHost {
    create_managed_host_with_config(env, ManagedHostConfig::default()).await
}

/// Create a managed host with 1 DPU (default config)
pub(in crate::tests) async fn create_managed_host_with_dpf(env: &TestEnv) -> TestManagedHost {
    create_managed_host_with_dpf_multi(env, 1).await
}

/// Create a managed host with `dpu_count` DPUs using the DPF path.
pub(in crate::tests) async fn create_managed_host_with_dpf_multi(
    env: &TestEnv,
    dpu_count: usize,
) -> TestManagedHost {
    create_managed_host_with_dpf_multi_hw(env, dpu_count, DPU_BF3_INFO_JSON).await
}

/// Create a managed host with a single BF4 DPU using the DPF path. The DPU's
/// `dmi_data.product_name` identifies it as BlueField-4, which the DPU platform
/// handler uses to skip the BF3-only vendor `machine_setup`/platform steps.
pub(in crate::tests) async fn create_managed_host_with_dpf_bf4(env: &TestEnv) -> TestManagedHost {
    create_managed_host_with_dpf_multi_hw(env, 1, DPU_BF4_INFO_JSON).await
}

/// Create a managed host with `dpu_count` DPUs using the DPF path, each DPU built
/// from the given hardware-info template (BF3 vs BF4 differ by `product_name`).
pub(in crate::tests) async fn create_managed_host_with_dpf_multi_hw(
    env: &TestEnv,
    dpu_count: usize,
    hardware_info_json: &'static [u8],
) -> TestManagedHost {
    assert!(dpu_count >= 1, "need to specify at least 1 dpu");
    let dpu_configs: Vec<DpuConfig> = (0..dpu_count)
        .map(|_| {
            DpuConfig::with_hardware_info_template(HardwareInfoTemplate::Custom(hardware_info_json))
        })
        .collect();
    let mh_config = ManagedHostConfig::default().with_dpus(dpu_configs);
    let mh = site_explorer::new_mock_host_with_dpf(env, mh_config)
        .await
        .expect("Failed to create a new host");
    TestManagedHost {
        id: mh
            .host_snapshot
            .id
            .try_into()
            .expect("managed host snapshot ID should be a valid HostMachineId"),
        dpu_ids: mh
            .dpu_snapshots
            .iter()
            .map(|dpu| {
                DpuMachineId::try_from(dpu.id)
                    .expect("DPU snapshot ID should be a valid DpuMachineId")
            })
            .collect(),
        api: env.api.clone(),
    }
}

pub(in crate::tests) async fn create_managed_host_with_ek(
    env: &TestEnv,
    ek_cert: &[u8],
) -> TestManagedHost {
    let host_config = ManagedHostConfig {
        tpm_ek_cert: TpmEkCertificate::from(ek_cert.to_vec()),
        ..ManagedHostConfig::default()
    };

    create_managed_host_with_config(env, host_config.clone()).await
}

/// Create a managed host with `dpu_count` DPUs (default config)
pub(in crate::tests) async fn create_managed_host_multi_dpu(
    env: &TestEnv,
    dpu_count: usize,
) -> TestManagedHost {
    assert!(dpu_count >= 1, "need to specify at least 1 dpu");
    let config = ManagedHostConfig::default().with_dpu_count(dpu_count);
    create_managed_host_with_config(env, config).await
}

/// Create a managed host with full config control
pub(in crate::tests) async fn create_managed_host_with_config(
    env: &TestEnv,
    config: ManagedHostConfig,
) -> TestManagedHost {
    let mh = site_explorer::new_host(env, config)
        .await
        .expect("Failed to create a new host");

    let dpu_ids = mh
        .dpu_snapshots
        .iter()
        .map(|snapshot| {
            DpuMachineId::try_from(snapshot.id)
                .expect("dpu_snapshot ID should be valid DpuMachineId")
        })
        .collect();

    TestManagedHost {
        id: mh
            .host_snapshot
            .id
            .try_into()
            .expect("managed host snapshot ID should be a valid HostMachineId"),
        dpu_ids,
        api: env.api.clone(),
    }
}

pub(in crate::tests) async fn create_host_with_machine_validation(
    env: &TestEnv,
    machine_validation_result_data: Option<rpc::forge::MachineValidationResult>,
    error: Option<String>,
) -> TestManagedHost {
    let mh = new_host_with_machine_validation(env, 1, machine_validation_result_data, error)
        .await
        .unwrap();
    TestManagedHost {
        id: mh
            .host_snapshot
            .id
            .try_into()
            .expect("managed host ID should be valid HostMachineId"),
        dpu_ids: mh
            .dpu_snapshots
            .into_iter()
            .map(|snapshot| {
                DpuMachineId::try_from(snapshot.id)
                    .expect("DPU snapshot ID should be a valid DpuMachineId")
            })
            .collect(),
        api: env.api.clone(),
    }
}

pub(in crate::tests) async fn create_managed_host_with_hardware_info_template(
    env: &TestEnv,
    hardware_info_template: HardwareInfoTemplate,
) -> TestManagedHost {
    insert_nvlink_nmxc_endpoint_from_managed_host(env, &hardware_info_template).await;
    let config = ManagedHostConfig::default().with_hardware_info_template(hardware_info_template);
    let mh = site_explorer::new_host(env, config).await.unwrap();
    TestManagedHost {
        id: mh
            .host_snapshot
            .id
            .try_into()
            .expect("managed host snapshot ID should be a valid HostMachineId"),
        dpu_ids: mh
            .dpu_snapshots
            .into_iter()
            .map(|s| {
                DpuMachineId::try_from(s.id)
                    .expect("DPU snapshot ID should be a valid DpuMachineId")
            })
            .collect(),
        api: env.api.clone(),
    }
}

fn hardware_info_from_hardware_info_template(
    template: &HardwareInfoTemplate,
) -> Option<HardwareInfo> {
    let json_bytes: &[u8] = match template {
        HardwareInfoTemplate::Default => model::test_support::managed_host::X86_INFO_JSON,
        HardwareInfoTemplate::Custom(data) => data,
    };
    serde_json::from_slice::<HardwareInfo>(json_bytes).ok()
}

/// Inserts `nvlink_nmxc_endpoints` with the NMX-C gRPC simulator default URL when the test env
/// uses the gRPC simulator, otherwise a random `http://<ipv4>:<port>` endpoint, when the
/// template's `dmi_data.product_name` contains `"GB200"` and a non-empty
/// `gpus[].platform_info.chassis_serial` exists. Skips if the row already exists or on DB errors.
pub(in crate::tests) async fn insert_nvlink_nmxc_endpoint_from_managed_host(
    env: &TestEnv,
    hardware_info_template: &HardwareInfoTemplate,
) {
    let endpoint = if env.nmxc_grpc_simulator {
        NmxcSimClient::SIMULATOR_URL.to_string()
    } else {
        format!(
            "http://{}.{}.{}.{}:{}",
            rand::random::<u8>(),
            rand::random::<u8>(),
            rand::random::<u8>(),
            rand::random::<u8>(),
            rand::random::<u16>() % 40_000 + 10_000,
        )
    };
    let Some(hi) = hardware_info_from_hardware_info_template(hardware_info_template) else {
        return;
    };
    if !hi
        .dmi_data
        .as_ref()
        .is_some_and(|d| d.product_name.contains("GB200"))
    {
        return;
    }
    let Some(chassis_serial_owned) = hi.gpus.iter().find_map(|g| {
        g.platform_info.as_ref().and_then(|p| {
            let s = p.chassis_serial.trim();
            if s.is_empty() {
                None
            } else {
                Some(s.to_string())
            }
        })
    }) else {
        return;
    };
    let chassis_serial = chassis_serial_owned.trim();
    if chassis_serial.is_empty() {
        return;
    }
    let Ok(mut txn) = db::Transaction::begin(&env.pool).await else {
        return;
    };
    let Ok(existing) =
        db::nvlink_nmxc_endpoints::find_by_chassis_serial(&mut txn, chassis_serial).await
    else {
        txn.rollback().await.ok();
        return;
    };
    if existing.is_some() {
        txn.commit().await.ok();
        return;
    }
    if db::nvlink_nmxc_endpoints::create(&mut txn, chassis_serial, endpoint.as_str())
        .await
        .is_err()
    {
        txn.rollback().await.ok();
        return;
    }
    txn.commit().await.ok();
}

#[allow(dead_code)]
pub(in crate::tests) async fn set_nvlink_nmxc_endpoint(
    env: &TestEnv,
    chassis_serial: &str,
    endpoint: &str,
) {
    let mut txn = db::Transaction::begin(&env.pool)
        .await
        .expect("begin txn for nvlink_nmxc_endpoint");
    match db::nvlink_nmxc_endpoints::find_by_chassis_serial(&mut txn, chassis_serial).await {
        Ok(None) => {
            db::nvlink_nmxc_endpoints::create(&mut txn, chassis_serial, endpoint)
                .await
                .expect("create nvlink_nmxc_endpoint");
        }
        Ok(Some(_)) => {
            db::nvlink_nmxc_endpoints::update(&mut txn, chassis_serial, endpoint)
                .await
                .expect("update nvlink_nmxc_endpoint")
                .expect("nvlink_nmxc_endpoint row missing after find");
        }
        Err(e) => panic!("find nvlink_nmxc_endpoint: {e}"),
    }
    txn.commit().await.expect("commit nvlink_nmxc_endpoint");
}

pub(in crate::tests) async fn update_time_params(
    pool: &sqlx::PgPool,
    machine: &Machine,
    retry_count: i64,
    last_reboot_requested: Option<DateTime<Utc>>,
) {
    let mut txn = pool.begin().await.unwrap();
    let data = MachineLastRebootRequested {
        time: if let Some(last_reboot_requested) = last_reboot_requested {
            last_reboot_requested
        } else {
            machine.status.last_reboot_requested.as_ref().unwrap().time - Duration::minutes(1)
        },
        mode: machine.status.last_reboot_requested.as_ref().unwrap().mode,
        restart_verified: None,
        verification_attempts: None,
    };

    let last_reboot_time = machine.status.last_reboot_time.unwrap() - Duration::minutes(2i64);

    let ts = machine.status.last_reboot_requested.as_ref().unwrap().time
        - Duration::minutes(retry_count);
    let last_discovery_time = ts - Duration::minutes(1);

    let version = format!(
        "V{}-T{}",
        machine.current_version().version_nr(),
        ts.timestamp_micros()
    );

    let query = "UPDATE machines SET last_reboot_requested=$1, controller_state_version=$3, last_reboot_time=$4, last_discovery_time=$5 WHERE id=$2 RETURNING *";
    sqlx::query(query)
        .bind(sqlx::types::Json(&data))
        .bind(machine.id.to_string())
        .bind(version)
        .bind(last_reboot_time)
        .bind(last_discovery_time)
        .execute(&mut *txn)
        .await
        .unwrap();
    txn.commit().await.unwrap();
}

pub(in crate::tests) async fn reboot_completed(
    env: &TestEnv,
    machine_id: carbide_uuid::machine::MachineId,
) -> rpc::forge::MachineRebootCompletedResponse {
    tracing::info!(
        machine_id = %machine_id,
        "Machine rebooted",
    );
    env.api
        .reboot_completed(Request::new(rpc::forge::MachineRebootCompletedRequest {
            machine_id: Some(machine_id),
        }))
        .await
        .unwrap()
        .into_inner()
}

// Emulates the `MachineValidationComplete` request of a Host
pub(in crate::tests) async fn machine_validation_completed(
    env: &TestEnv,
    machine_id: &MachineId,
    machine_validation_error: Option<String>,
) {
    let response = forge_agent_control(env, *machine_id).await;
    let uuid = &response.data.unwrap().pair[1].value;
    let validation_id: MachineValidationId = uuid.parse().unwrap();

    let _response = env
        .api
        .machine_validation_completed(Request::new(
            rpc::forge::MachineValidationCompletedRequest {
                machine_id: Some(*machine_id),
                machine_validation_error,
                validation_id: Some(validation_id),
            },
        ))
        .await
        .unwrap()
        .into_inner();
}

/// inject_machine_measurements injects auto-approved measurements
/// for a machine. This also will create a new profile and bundle,
/// if needed, as part of the auto-approval process.
pub(in crate::tests) async fn inject_machine_measurements(
    env: &TestEnv,
    machine_id: HostMachineId,
) {
    let _response = env
        .api
        .add_measurement_trusted_machine(Request::new(
            rpc::protos::measured_boot::AddMeasurementTrustedMachineRequest {
                machine_id: machine_id.to_string(),
                approval_type: rpc::protos::measured_boot::MeasurementApprovedTypePb::Oneshot
                    as i32,
                pcr_registers: "0-1".to_string(),
                comments: "".to_string(),
            },
        ))
        .await
        .unwrap()
        .into_inner();

    let pcr_values: Vec<PcrRegisterValue> = vec![
        PcrRegisterValue {
            pcr_register: 0,
            sha_any: "aa".to_string(),
        },
        PcrRegisterValue {
            pcr_register: 1,
            sha_any: "bb".to_string(),
        },
    ];

    let _response = env
        .api
        .attest_candidate_machine(Request::new(
            rpc::protos::measured_boot::AttestCandidateMachineRequest {
                machine_id: machine_id.to_string(),
                pcr_values: convert_vec(pcr_values),
            },
        ))
        .await
        .unwrap()
        .into_inner();
}

/// Emulates the `MachineValidationComplete` request of a Host
pub(in crate::tests) async fn persist_machine_validation_result(
    env: &TestEnv,
    machine_validation_result: rpc::forge::MachineValidationResult,
) {
    env.api
        .persist_validation_result(Request::new(
            rpc::forge::MachineValidationResultPostRequest {
                result: Some(machine_validation_result),
            },
        ))
        .await
        .unwrap()
        .into_inner();
}

/// Emulates the `get_machine_validation_results` request of a Host
pub(in crate::tests) async fn get_machine_validation_results(
    env: &TestEnv,
    machine_id: Option<&MachineId>,
    include_history: bool,
    validation_id: Option<MachineValidationId>,
) -> rpc::forge::MachineValidationResultList {
    env.api
        .get_machine_validation_results(Request::new(rpc::forge::MachineValidationGetRequest {
            machine_id: machine_id.copied(),
            include_history,
            validation_id,
        }))
        .await
        .unwrap()
        .into_inner()
}

/// Emulates the `get_machine_validation_runs` request of a Host
pub(in crate::tests) async fn get_machine_validation_runs(
    env: &TestEnv,
    machine_id: &MachineId,
    include_history: bool,
) -> rpc::forge::MachineValidationRunList {
    env.api
        .get_machine_validation_runs(Request::new(
            rpc::forge::MachineValidationRunListGetRequest {
                machine_id: Some(*machine_id),
                include_history,
            },
        ))
        .await
        .unwrap()
        .into_inner()
}

// Emulates the `OnDemandMachineValidation` request of a Host
pub(in crate::tests) async fn on_demand_machine_validation(
    env: &TestEnv,
    machine_id: carbide_uuid::machine::MachineId,
    tags: Vec<String>,
    allowed_tests: Vec<String>,
    run_unverfied_tests: bool,
    contexts: Vec<String>,
) -> rpc::forge::MachineValidationOnDemandResponse {
    env.api
        .on_demand_machine_validation(Request::new(rpc::forge::MachineValidationOnDemandRequest {
            machine_id: Some(machine_id),
            action: rpc::forge::machine_validation_on_demand_request::Action::Start.into(),
            tags,
            allowed_tests,
            run_unverfied_tests,
            contexts,
        }))
        .await
        .unwrap()
        .into_inner()
}

pub(in crate::tests) async fn update_machine_validation_run(
    env: &TestEnv,
    validation_id: Option<MachineValidationId>,
    duration_to_complete: Option<rpc::Duration>,
    total: u32,
) -> rpc::forge::MachineValidationRunResponse {
    env.api
        .update_machine_validation_run(Request::new(rpc::forge::MachineValidationRunRequest {
            validation_id,
            duration_to_complete,
            total,
            selected_tests: Vec::new(),
        }))
        .await
        .unwrap()
        .into_inner()
}

pub(in crate::tests) async fn get_vpc_fixture_id(env: &TestEnv) -> VpcId {
    db::vpc::find_by_name(&env.pool, "test vpc 1")
        .await
        .unwrap()
        .into_iter()
        .next()
        .unwrap()
        .id
}

/// A hot swappable machine state handler.
/// Allows modifying the handler behavior without reconstructing the machine
/// state controller (which leads to stale metrics being saved).
#[derive(Debug)]
pub(in crate::tests) struct SwapHandler<H: StateHandler> {
    pub(in crate::tests) inner: Arc<Mutex<H>>,
}

impl<H: StateHandler> Clone for SwapHandler<H> {
    fn clone(&self) -> Self {
        SwapHandler {
            inner: self.inner.clone(),
        }
    }
}

#[async_trait::async_trait]
impl<H: StateHandler> StateHandler for SwapHandler<H>
where
    H::ObjectId: Send + Sync,
    H::State: Send + Sync,
    H::ControllerState: Send + Sync,
    H::ContextObjects: Send + Sync,
{
    type ObjectId = H::ObjectId;
    type State = H::State;
    type ControllerState = H::ControllerState;
    type ContextObjects = H::ContextObjects;

    async fn handle_object_state(
        &self,
        object_id: &Self::ObjectId,
        state: &mut Self::State,
        controller_state: &Self::ControllerState,
        ctx: &mut StateHandlerContext<Self::ContextObjects>,
    ) -> Result<StateHandlerOutcome<Self::ControllerState>, StateHandlerError> {
        self.inner
            .lock()
            .await
            .handle_object_state(object_id, state, controller_state, ctx)
            .await
    }
}

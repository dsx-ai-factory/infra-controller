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

pub(crate) mod metrics;

use std::panic::Location;
use std::pin::Pin;
use std::sync::{Arc, OnceLock};

pub(crate) use ::rpc::forge as rpc;
use ::rpc::forge::{RemoveSkuRequest, SkuIdList};
use ::rpc::protos::dns::{
    CreateDomainRequest, DnsResourceRecordLookupRequest, DnsResourceRecordLookupResponse, Domain,
    DomainDeletionRequest, DomainDeletionResult, DomainList, DomainMetadataRequest,
    DomainMetadataResponse, DomainSearchQuery, GetAllDomainsRequest, GetAllDomainsResponse,
    UpdateDomainRequest,
};
use ::rpc::protos::{measured_boot as measured_boot_pb, mlx_device as mlx_device_pb};
use carbide_ib_fabric::ib::IBFabricManager;
use carbide_machine_controller::dpf::DpfOperations;
use carbide_machine_controller::io::MachineStateControllerIO;
use carbide_rack::bms_client::BmsDsxExchangeHandle;
use carbide_redfish::libredfish::{BmcCredentialOps, RedfishClientPool};
use carbide_secrets::certificates::CertificateProvider;
use carbide_secrets::credentials::{
    BmcCredentialType, CredentialKey, CredentialManager, CredentialType, Credentials,
};
use carbide_site_explorer::{AuthenticatedBmc, EndpointExplorationService, EndpointExplorer};
use carbide_uuid::machine::{MachineId, MachineIdSubtypeTrait, MachineInterfaceId};
use db::db_read::PgPoolReader;
use db::work_lock_manager::WorkLockManagerHandle;
use db::{DatabaseError, DatabaseResult, WithTransaction};
use libnmxc::NmxcPool;
use librms::RmsApi;
use model::machine::Machine;
use model::machine::machine_search_config::MachineSearchConfig;
use model::resource_pool::common::CommonPools;
use sqlx::PgTransaction;
use state_controller::controller::Enqueuer;
use tokio_stream::Stream;
use tonic::{Request, Response, Status, Streaming};

use self::metrics::ApiMetricsEmitter;
use self::rpc::forge_server::Forge;
use crate::cfg::file::CarbideConfig;
use crate::dynamic_settings::DynamicSettings;
use crate::ethernet_virtualization::EthVirtData;
use crate::logging::log_limiter::LogLimiter;
use crate::scout_stream::ConnectionRegistry;
use crate::{CarbideError, CarbideResult};

pub struct Api {
    // `pub` (not `pub(crate)`): read by the `carbide-api-web` admin UI (e.g. IPAM/DNS pages).
    pub database_connection: sqlx::PgPool,
    pub(crate) credential_manager: Arc<dyn CredentialManager>,
    pub(crate) certificate_provider: Arc<dyn CertificateProvider>,
    pub(crate) redfish_pool: Arc<dyn RedfishClientPool>,
    /// Credential-lifecycle operations (password set/rotate/clear, candidate
    /// validation). A sealed trait implemented only by the direct pool, so
    /// handing these to a wrapper pool is a compile error (a wrong-pool
    /// guard, not a wire-path guarantee -- see [`BmcCredentialOps`]).
    pub(crate) bmc_credential_ops: Arc<dyn BmcCredentialOps>,
    pub(crate) bmc_session_manager: Arc<crate::credentials::BmcSessionManager>,
    pub(crate) eth_data: EthVirtData,
    pub(crate) common_pools: Arc<CommonPools>,
    pub(crate) ib_fabric_manager: Arc<dyn IBFabricManager>,
    // `pub` (not `pub(crate)`): read by the `carbide-api-web` admin UI for config-derived display.
    pub runtime_config: Arc<CarbideConfig>,
    pub(crate) dpu_health_log_limiter: LogLimiter<MachineId>,
    pub dynamic_settings: DynamicSettings,
    pub(crate) endpoint_explorer: Arc<dyn EndpointExplorer>,
    /// Authenticated BMC client for admin operations, supplied independently of
    /// endpoint exploration.
    pub(crate) bmc_client: Arc<dyn AuthenticatedBmc>,
    pub(crate) endpoint_exploration_service: Arc<EndpointExplorationService>,
    pub(crate) scout_stream_registry: ConnectionRegistry,
    #[allow(unused)]
    pub(crate) rms_client: Option<Arc<dyn RmsApi>>,
    pub(crate) nmxc_client_pool: Arc<dyn NmxcPool>,
    pub(crate) work_lock_manager_handle: WorkLockManagerHandle,
    pub(crate) dpf_sdk: Option<Arc<dyn DpfOperations>>,
    pub(crate) machine_state_handler_enqueuer: Enqueuer<MachineStateControllerIO>,
    pub(crate) metric_emitter: ApiMetricsEmitter,
    pub(crate) component_manager: Option<component_manager::component_manager::ComponentManager>,
    pub(crate) bms_client: OnceLock<Arc<BmsDsxExchangeHandle>>,
    pub(crate) secrets_context: Option<crate::secrets::SecretsContext>,
    /// Validator for node-auth bearer JWTs (issue #355). `Some` only when
    /// `[node_auth] enabled`; installed into the authn middleware by the
    /// listener.
    pub(crate) node_jwt_validator: Option<Arc<crate::node_auth::NodeJwtValidator>>,
}

pub(crate) type ScoutStreamType =
    Pin<Box<dyn Stream<Item = Result<rpc::ScoutStreamScoutBoundMessage, Status>> + Send>>;

#[tonic::async_trait]
impl Forge for Api {
    type ScoutStreamStream = ScoutStreamType;

    async fn version(
        &self,
        request: Request<rpc::VersionRequest>,
    ) -> Result<Response<rpc::BuildInfo>, Status> {
        crate::handlers::api::version(self, request)
    }

    async fn create_domain(
        &self,
        request: Request<CreateDomainRequest>,
    ) -> Result<Response<Domain>, Status> {
        crate::handlers::domain::create(self, request).await
    }

    async fn update_domain(
        &self,
        request: Request<UpdateDomainRequest>,
    ) -> Result<Response<Domain>, Status> {
        crate::handlers::domain::update(self, request).await
    }

    async fn delete_domain(
        &self,
        request: Request<DomainDeletionRequest>,
    ) -> Result<Response<DomainDeletionResult>, Status> {
        crate::handlers::domain::delete(self, request).await
    }

    async fn find_domain(
        &self,
        request: Request<DomainSearchQuery>,
    ) -> Result<Response<DomainList>, Status> {
        crate::handlers::domain::find(self, request).await
    }

    // Legacy domain methods for backward compatibility
    // TODO: Remove this after clients have migrated
    async fn create_domain_legacy(
        &self,
        request: Request<rpc::DomainLegacy>,
    ) -> Result<Response<rpc::DomainLegacy>, Status> {
        crate::handlers::domain::create_legacy_compat(self, request).await
    }

    async fn update_domain_legacy(
        &self,
        request: Request<rpc::DomainLegacy>,
    ) -> Result<Response<rpc::DomainLegacy>, Status> {
        crate::handlers::domain::update_legacy_compat(self, request).await
    }

    async fn delete_domain_legacy(
        &self,
        request: Request<rpc::DomainDeletionLegacy>,
    ) -> Result<Response<rpc::DomainDeletionResultLegacy>, Status> {
        crate::handlers::domain::delete_legacy_compat(self, request).await
    }

    async fn find_domain_legacy(
        &self,
        request: Request<rpc::DomainSearchQueryLegacy>,
    ) -> Result<Response<rpc::DomainListLegacy>, Status> {
        crate::handlers::domain::find_legacy_compat(self, request).await
    }

    async fn create_vpc(
        &self,
        request: Request<rpc::VpcCreationRequest>,
    ) -> Result<Response<rpc::Vpc>, Status> {
        crate::handlers::vpc::create(self, request).await
    }

    async fn update_vpc(
        &self,
        request: Request<rpc::VpcUpdateRequest>,
    ) -> Result<Response<rpc::VpcUpdateResult>, Status> {
        crate::handlers::vpc::update(self, request).await
    }

    async fn update_vpc_virtualization(
        &self,
        request: Request<rpc::VpcUpdateVirtualizationRequest>,
    ) -> Result<Response<rpc::VpcUpdateVirtualizationResult>, Status> {
        crate::handlers::vpc::update_virtualization(self, request).await
    }

    async fn delete_vpc(
        &self,
        request: Request<rpc::VpcDeletionRequest>,
    ) -> Result<Response<rpc::VpcDeletionResult>, Status> {
        crate::handlers::vpc::delete(self, request).await
    }

    async fn find_vpc_ids(
        &self,
        request: Request<rpc::VpcSearchFilter>,
    ) -> Result<Response<rpc::VpcIdList>, Status> {
        crate::handlers::vpc::find_ids(self, request).await
    }

    async fn find_vpcs_by_ids(
        &self,
        request: Request<rpc::VpcsByIdsRequest>,
    ) -> Result<Response<rpc::VpcList>, Status> {
        crate::handlers::vpc::find_by_ids(self, request).await
    }

    async fn find_site_prefix_ids(
        &self,
        request: Request<rpc::SitePrefixSearchFilter>,
    ) -> Result<Response<rpc::SitePrefixIdList>, Status> {
        crate::handlers::site_prefix::find_ids(self, request).await
    }

    async fn find_site_prefixes_by_ids(
        &self,
        request: Request<rpc::SitePrefixesByIdsRequest>,
    ) -> Result<Response<rpc::SitePrefixList>, Status> {
        crate::handlers::site_prefix::find_by_ids(self, request).await
    }

    async fn create_site_prefix(
        &self,
        request: Request<rpc::SitePrefixCreationRequest>,
    ) -> Result<Response<rpc::SitePrefix>, Status> {
        crate::handlers::site_prefix::create(self, request).await
    }

    async fn update_site_prefix(
        &self,
        request: Request<rpc::SitePrefixUpdateRequest>,
    ) -> Result<Response<rpc::SitePrefix>, Status> {
        crate::handlers::site_prefix::update(self, request).await
    }

    async fn delete_site_prefix(
        &self,
        request: Request<rpc::SitePrefixDeletionRequest>,
    ) -> Result<Response<rpc::SitePrefixDeletionResult>, Status> {
        crate::handlers::site_prefix::delete(self, request).await
    }

    async fn find_site_prefix_state_histories(
        &self,
        request: Request<rpc::SitePrefixStateHistoriesRequest>,
    ) -> Result<Response<rpc::StateHistories>, Status> {
        crate::handlers::site_prefix::find_state_histories(self, request).await
    }

    async fn create_vpc_prefix(
        &self,
        request: Request<rpc::VpcPrefixCreationRequest>,
    ) -> Result<Response<rpc::VpcPrefix>, Status> {
        crate::handlers::vpc_prefix::create(self, request).await
    }

    async fn search_vpc_prefixes(
        &self,
        request: Request<rpc::VpcPrefixSearchQuery>,
    ) -> Result<Response<rpc::VpcPrefixIdList>, Status> {
        crate::handlers::vpc_prefix::search(self, request).await
    }

    async fn get_vpc_prefixes(
        &self,
        request: Request<rpc::VpcPrefixGetRequest>,
    ) -> Result<Response<rpc::VpcPrefixList>, Status> {
        crate::handlers::vpc_prefix::get(self, request).await
    }

    async fn update_vpc_prefix(
        &self,
        request: Request<rpc::VpcPrefixUpdateRequest>,
    ) -> Result<Response<rpc::VpcPrefix>, Status> {
        crate::handlers::vpc_prefix::update(self, request).await
    }
    async fn delete_vpc_prefix(
        &self,
        request: Request<rpc::VpcPrefixDeletionRequest>,
    ) -> Result<Response<rpc::VpcPrefixDeletionResult>, Status> {
        crate::handlers::vpc_prefix::delete(self, request).await
    }

    async fn find_vpc_prefix_state_histories(
        &self,
        request: Request<rpc::VpcPrefixStateHistoriesRequest>,
    ) -> Result<Response<rpc::StateHistories>, Status> {
        crate::handlers::vpc_prefix::find_state_histories(self, request).await
    }

    async fn create_vpc_peering(
        &self,
        request: Request<rpc::VpcPeeringCreationRequest>,
    ) -> Result<Response<rpc::VpcPeering>, Status> {
        crate::handlers::vpc_peering::create(self, request).await
    }

    async fn find_vpc_peering_ids(
        &self,
        request: Request<rpc::VpcPeeringSearchFilter>,
    ) -> Result<Response<rpc::VpcPeeringIdList>, Status> {
        crate::handlers::vpc_peering::find_ids(self, request).await
    }

    async fn find_vpc_peerings_by_ids(
        &self,
        request: Request<rpc::VpcPeeringsByIdsRequest>,
    ) -> Result<Response<rpc::VpcPeeringList>, Status> {
        crate::handlers::vpc_peering::find_by_ids(self, request).await
    }

    async fn delete_vpc_peering(
        &self,
        request: Request<rpc::VpcPeeringDeletionRequest>,
    ) -> Result<Response<rpc::VpcPeeringDeletionResult>, Status> {
        crate::handlers::vpc_peering::delete(self, request).await
    }

    async fn create_spx_partition(
        &self,
        request: Request<rpc::SpxPartitionCreationRequest>,
    ) -> Result<Response<rpc::SpxPartition>, Status> {
        crate::handlers::spx_partition::create(self, request).await
    }

    async fn delete_spx_partition(
        &self,
        request: Request<rpc::SpxPartitionDeletionRequest>,
    ) -> Result<Response<rpc::SpxPartitionDeletionResult>, Status> {
        crate::handlers::spx_partition::delete(self, request).await
    }

    async fn find_spx_partition_ids(
        &self,
        request: Request<rpc::SpxPartitionSearchFilter>,
    ) -> Result<Response<rpc::SpxPartitionIdList>, Status> {
        crate::handlers::spx_partition::find_ids(self, request).await
    }

    async fn find_spx_partitions_by_ids(
        &self,
        request: Request<rpc::SpxPartitionsByIdsRequest>,
    ) -> Result<Response<rpc::SpxPartitionList>, Status> {
        crate::handlers::spx_partition::find_by_ids(self, request).await
    }

    async fn find_ib_partition_ids(
        &self,
        request: Request<rpc::IbPartitionSearchFilter>,
    ) -> Result<Response<rpc::IbPartitionIdList>, Status> {
        crate::handlers::ib_partition::find_ids(self, request).await
    }

    async fn find_ib_partitions_by_ids(
        &self,
        request: Request<rpc::IbPartitionsByIdsRequest>,
    ) -> Result<Response<rpc::IbPartitionList>, Status> {
        crate::handlers::ib_partition::find_by_ids(self, request).await
    }

    async fn create_ib_partition(
        &self,
        request: Request<rpc::IbPartitionCreationRequest>,
    ) -> Result<Response<rpc::IbPartition>, Status> {
        crate::handlers::ib_partition::create(self, request).await
    }

    async fn delete_ib_partition(
        &self,
        request: Request<rpc::IbPartitionDeletionRequest>,
    ) -> Result<Response<rpc::IbPartitionDeletionResult>, Status> {
        crate::handlers::ib_partition::delete(self, request).await
    }

    async fn update_ib_partition(
        &self,
        request: Request<rpc::IbPartitionUpdateRequest>,
    ) -> Result<Response<rpc::IbPartition>, Status> {
        crate::handlers::ib_partition::update(self, request).await
    }

    async fn ib_partitions_for_tenant(
        &self,
        request: Request<rpc::TenantSearchQuery>,
    ) -> Result<Response<rpc::IbPartitionList>, Status> {
        crate::handlers::ib_partition::for_tenant(self, request).await
    }

    async fn find_power_shelves(
        &self,
        request: Request<rpc::PowerShelfQuery>,
    ) -> Result<Response<rpc::PowerShelfList>, Status> {
        crate::handlers::power_shelf::find_power_shelf(self, request).await
    }

    async fn find_power_shelf_ids(
        &self,
        request: Request<rpc::PowerShelfSearchFilter>,
    ) -> Result<Response<rpc::PowerShelfIdList>, Status> {
        crate::handlers::power_shelf::find_ids(self, request).await
    }

    async fn find_power_shelves_by_ids(
        &self,
        request: Request<rpc::PowerShelvesByIdsRequest>,
    ) -> Result<Response<rpc::PowerShelfList>, Status> {
        crate::handlers::power_shelf::find_by_ids(self, request).await
    }

    async fn decommission_power_shelf(
        &self,
        request: Request<rpc::DecommissionPowerShelfRequest>,
    ) -> Result<Response<rpc::DecommissionPowerShelfResponse>, Status> {
        crate::handlers::power_shelf::decommission_power_shelf(self, request).await
    }

    async fn delete_power_shelf(
        &self,
        request: Request<rpc::PowerShelfDeletionRequest>,
    ) -> Result<Response<rpc::PowerShelfDeletionResult>, Status> {
        crate::handlers::power_shelf::delete_power_shelf(self, request).await
    }

    async fn admin_force_delete_power_shelf(
        &self,
        request: Request<rpc::AdminForceDeletePowerShelfRequest>,
    ) -> Result<Response<rpc::AdminForceDeletePowerShelfResponse>, Status> {
        crate::handlers::power_shelf::admin_force_delete_power_shelf(self, request).await
    }

    async fn set_power_shelf_maintenance(
        &self,
        request: Request<rpc::PowerShelfMaintenanceRequest>,
    ) -> Result<Response<()>, Status> {
        crate::handlers::power_shelf::set_power_shelf_maintenance(self, request).await
    }

    async fn find_switches(
        &self,
        request: Request<rpc::SwitchQuery>,
    ) -> Result<Response<rpc::SwitchList>, Status> {
        crate::handlers::switch::find_switch(self, request).await
    }

    async fn find_switch_ids(
        &self,
        request: Request<rpc::SwitchSearchFilter>,
    ) -> Result<Response<rpc::SwitchIdList>, Status> {
        crate::handlers::switch::find_ids(self, request).await
    }

    async fn find_switches_by_ids(
        &self,
        request: Request<rpc::SwitchesByIdsRequest>,
    ) -> Result<Response<rpc::SwitchList>, Status> {
        crate::handlers::switch::find_by_ids(self, request).await
    }

    async fn delete_switch(
        &self,
        request: Request<rpc::SwitchDeletionRequest>,
    ) -> Result<Response<rpc::SwitchDeletionResult>, Status> {
        crate::handlers::switch::delete_switch(self, request).await
    }

    async fn decommission_switch(
        &self,
        request: Request<rpc::DecommissionSwitchRequest>,
    ) -> Result<Response<rpc::DecommissionSwitchResponse>, Status> {
        crate::handlers::switch::decommission_switch(self, request).await
    }

    async fn admin_force_delete_switch(
        &self,
        request: Request<rpc::AdminForceDeleteSwitchRequest>,
    ) -> Result<Response<rpc::AdminForceDeleteSwitchResponse>, Status> {
        crate::handlers::switch::admin_force_delete_switch(self, request).await
    }

    async fn find_ib_fabric_ids(
        &self,
        request: Request<rpc::IbFabricSearchFilter>,
    ) -> Result<Response<rpc::IbFabricIdList>, Status> {
        crate::handlers::ib_fabric::find_ids(self, request)
    }

    async fn find_network_segment_ids(
        &self,
        request: Request<rpc::NetworkSegmentSearchFilter>,
    ) -> Result<Response<rpc::NetworkSegmentIdList>, Status> {
        crate::handlers::network_segment::find_ids(self, request).await
    }

    async fn find_network_segments_by_ids(
        &self,
        request: Request<rpc::NetworkSegmentsByIdsRequest>,
    ) -> Result<Response<rpc::NetworkSegmentList>, Status> {
        crate::handlers::network_segment::find_by_ids(self, request).await
    }

    async fn create_network_segment(
        &self,
        request: Request<rpc::NetworkSegmentCreationRequest>,
    ) -> Result<Response<rpc::NetworkSegment>, Status> {
        crate::handlers::network_segment::create(self, request).await
    }

    async fn attach_network_segment_to_vpc(
        &self,
        request: Request<rpc::AttachNetworkSegmentToVpcRequest>,
    ) -> Result<Response<rpc::NetworkSegment>, Status> {
        crate::handlers::network_segment::attach_to_vpc(self, request).await
    }

    async fn delete_network_segment(
        &self,
        request: Request<rpc::NetworkSegmentDeletionRequest>,
    ) -> Result<Response<rpc::NetworkSegmentDeletionResult>, Status> {
        crate::handlers::network_segment::delete(self, request).await
    }

    async fn network_segments_for_vpc(
        &self,
        request: Request<rpc::VpcSearchQuery>,
    ) -> Result<Response<rpc::NetworkSegmentList>, Status> {
        crate::handlers::network_segment::for_vpc(self, request).await
    }

    async fn find_network_segment_state_histories(
        &self,
        request: Request<rpc::NetworkSegmentStateHistoriesRequest>,
    ) -> Result<Response<rpc::StateHistories>, Status> {
        crate::handlers::network_segment::find_state_histories(self, request).await
    }

    async fn allocate_instance(
        &self,
        request: Request<rpc::InstanceAllocationRequest>,
    ) -> Result<Response<rpc::Instance>, Status> {
        crate::handlers::instance::allocate(self, request).await
    }

    async fn allocate_instances(
        &self,
        request: Request<rpc::BatchInstanceAllocationRequest>,
    ) -> Result<Response<rpc::BatchInstanceAllocationResponse>, Status> {
        crate::handlers::instance::batch_allocate(self, request).await
    }

    async fn find_instance_ids(
        &self,
        request: Request<rpc::InstanceSearchFilter>,
    ) -> Result<Response<rpc::InstanceIdList>, Status> {
        crate::handlers::instance::find_ids(self, request).await
    }

    async fn find_instances_by_ids(
        &self,
        request: Request<rpc::InstancesByIdsRequest>,
    ) -> Result<Response<rpc::InstanceList>, Status> {
        crate::handlers::instance::find_by_ids(self, request).await
    }

    async fn find_instance_by_machine_id(
        &self,
        request: Request<MachineId>,
    ) -> Result<Response<rpc::InstanceList>, Status> {
        crate::handlers::instance::find_by_machine_id(self, request).await
    }

    async fn release_instance(
        &self,
        request: Request<rpc::InstanceReleaseRequest>,
    ) -> Result<Response<rpc::InstanceReleaseResult>, Status> {
        crate::handlers::instance::release(self, request).await
    }

    async fn release_instances(
        &self,
        request: Request<rpc::BatchInstanceReleaseRequest>,
    ) -> Result<Response<rpc::BatchInstanceReleaseResponse>, Status> {
        crate::handlers::instance::batch_release(self, request).await
    }

    async fn update_instance_phone_home_last_contact(
        &self,
        request: Request<rpc::InstancePhoneHomeLastContactRequest>,
    ) -> Result<Response<rpc::InstancePhoneHomeLastContactResponse>, Status> {
        crate::handlers::instance::update_phone_home_last_contact(self, request).await
    }

    async fn update_instance_operating_system(
        &self,
        request: Request<rpc::InstanceOperatingSystemUpdateRequest>,
    ) -> Result<Response<rpc::Instance>, Status> {
        crate::handlers::instance::update_operating_system(self, request).await
    }

    async fn update_instance_config(
        &self,
        request: Request<rpc::InstanceConfigUpdateRequest>,
    ) -> Result<Response<rpc::Instance>, Status> {
        crate::handlers::instance::update_instance_config(self, request).await
    }

    async fn get_managed_host_network_config(
        &self,
        request: Request<rpc::ManagedHostNetworkConfigRequest>,
    ) -> Result<Response<rpc::ManagedHostNetworkConfigResponse>, Status> {
        crate::handlers::dpu::get_managed_host_network_config(self, request).await
    }

    async fn update_agent_reported_inventory(
        &self,
        request: Request<rpc::DpuAgentInventoryReport>,
    ) -> Result<Response<()>, Status> {
        crate::handlers::dpu::update_agent_reported_inventory(self, request).await
    }

    async fn record_dpu_network_status(
        &self,
        request: Request<rpc::DpuNetworkStatus>,
    ) -> Result<Response<()>, Status> {
        crate::handlers::dpu::record_dpu_network_status(self, request).await
    }

    async fn list_machine_health_reports(
        &self,
        request: Request<MachineId>,
    ) -> Result<Response<rpc::ListHealthReportResponse>, Status> {
        crate::handlers::health::list_machine_health_reports(self, request).await
    }

    async fn insert_machine_health_report(
        &self,
        request: Request<rpc::InsertMachineHealthReportRequest>,
    ) -> Result<Response<()>, Status> {
        crate::handlers::health::insert_machine_health_report(self, request).await
    }

    async fn remove_machine_health_report(
        &self,
        request: Request<rpc::RemoveMachineHealthReportRequest>,
    ) -> Result<Response<()>, Status> {
        crate::handlers::health::remove_machine_health_report(self, request).await
    }

    async fn list_rack_health_reports(
        &self,
        request: Request<rpc::ListRackHealthReportsRequest>,
    ) -> Result<Response<rpc::ListHealthReportResponse>, Status> {
        crate::handlers::rack::list_rack_health_reports(self, request).await
    }

    async fn insert_rack_health_report(
        &self,
        request: Request<rpc::InsertRackHealthReportRequest>,
    ) -> Result<Response<()>, Status> {
        crate::handlers::rack::insert_rack_health_report(self, request).await
    }

    async fn remove_rack_health_report(
        &self,
        request: Request<rpc::RemoveRackHealthReportRequest>,
    ) -> Result<Response<()>, Status> {
        crate::handlers::rack::remove_rack_health_report(self, request).await
    }

    // Deprecated aliases. Delegate to the canonical handlers above.
    #[allow(deprecated)]
    async fn list_health_report_overrides(
        &self,
        request: Request<MachineId>,
    ) -> Result<Response<rpc::ListHealthReportResponse>, Status> {
        self.list_machine_health_reports(request).await
    }

    #[allow(deprecated)]
    async fn insert_health_report_override(
        &self,
        request: Request<rpc::InsertMachineHealthReportRequest>,
    ) -> Result<Response<()>, Status> {
        self.insert_machine_health_report(request).await
    }

    #[allow(deprecated)]
    async fn remove_health_report_override(
        &self,
        request: Request<rpc::RemoveMachineHealthReportRequest>,
    ) -> Result<Response<()>, Status> {
        self.remove_machine_health_report(request).await
    }

    async fn list_switch_health_reports(
        &self,
        request: Request<rpc::ListSwitchHealthReportsRequest>,
    ) -> Result<Response<rpc::ListHealthReportResponse>, Status> {
        crate::handlers::switch::list_switch_health_reports(self, request).await
    }

    async fn insert_switch_health_report(
        &self,
        request: Request<rpc::InsertSwitchHealthReportRequest>,
    ) -> Result<Response<()>, Status> {
        crate::handlers::switch::insert_switch_health_report(self, request).await
    }

    async fn remove_switch_health_report(
        &self,
        request: Request<rpc::RemoveSwitchHealthReportRequest>,
    ) -> Result<Response<()>, Status> {
        crate::handlers::switch::remove_switch_health_report(self, request).await
    }

    async fn list_power_shelf_health_reports(
        &self,
        request: Request<rpc::ListPowerShelfHealthReportsRequest>,
    ) -> Result<Response<rpc::ListHealthReportResponse>, Status> {
        crate::handlers::power_shelf::list_power_shelf_health_reports(self, request).await
    }

    async fn insert_power_shelf_health_report(
        &self,
        request: Request<rpc::InsertPowerShelfHealthReportRequest>,
    ) -> Result<Response<()>, Status> {
        crate::handlers::power_shelf::insert_power_shelf_health_report(self, request).await
    }

    async fn remove_power_shelf_health_report(
        &self,
        request: Request<rpc::RemovePowerShelfHealthReportRequest>,
    ) -> Result<Response<()>, Status> {
        crate::handlers::power_shelf::remove_power_shelf_health_report(self, request).await
    }

    async fn list_nv_link_domain_health_reports(
        &self,
        request: Request<rpc::ListNvLinkDomainHealthReportsRequest>,
    ) -> Result<Response<rpc::ListHealthReportResponse>, Status> {
        crate::handlers::nvlink_domain::list_nv_link_domain_health_reports(self, request).await
    }

    async fn insert_nv_link_domain_health_report(
        &self,
        request: Request<rpc::InsertNvLinkDomainHealthReportRequest>,
    ) -> Result<Response<()>, Status> {
        crate::handlers::nvlink_domain::insert_nv_link_domain_health_report(self, request).await
    }

    async fn remove_nv_link_domain_health_report(
        &self,
        request: Request<rpc::RemoveNvLinkDomainHealthReportRequest>,
    ) -> Result<Response<()>, Status> {
        crate::handlers::nvlink_domain::remove_nv_link_domain_health_report(self, request).await
    }

    async fn get_all_domain_metadata(
        &self,
        request: Request<DomainMetadataRequest>,
    ) -> Result<Response<DomainMetadataResponse>, tonic::Status> {
        crate::handlers::dns::get_all_domain_metadata(self, request).await
    }

    async fn get_all_domains(
        &self,
        request: Request<GetAllDomainsRequest>,
    ) -> Result<Response<GetAllDomainsResponse>, tonic::Status> {
        crate::handlers::dns::get_all_domains(self, request).await
    }

    async fn lookup_record(
        &self,
        request: Request<DnsResourceRecordLookupRequest>,
    ) -> Result<Response<DnsResourceRecordLookupResponse>, Status> {
        crate::handlers::dns::lookup_record(self, request).await
    }

    async fn invoke_instance_power(
        &self,
        request: Request<rpc::InstancePowerRequest>,
    ) -> Result<Response<rpc::InstancePowerResult>, Status> {
        crate::handlers::instance::invoke_power(self, request).await
    }

    async fn echo(
        &self,
        request: Request<rpc::EchoRequest>,
    ) -> Result<Response<rpc::EchoResponse>, Status> {
        crate::handlers::api::echo(self, request)
    }

    async fn create_tenant(
        &self,
        request: Request<rpc::CreateTenantRequest>,
    ) -> Result<Response<rpc::CreateTenantResponse>, Status> {
        crate::handlers::tenant::create(self, request).await
    }

    async fn find_tenant(
        &self,
        request: Request<rpc::FindTenantRequest>,
    ) -> Result<Response<rpc::FindTenantResponse>, Status> {
        crate::handlers::tenant::find(self, request).await
    }

    async fn update_tenant(
        &self,
        request: Request<rpc::UpdateTenantRequest>,
    ) -> Result<Response<rpc::UpdateTenantResponse>, Status> {
        crate::handlers::tenant::update(self, request).await
    }

    async fn find_tenants_by_organization_ids(
        &self,
        request: Request<rpc::TenantByOrganizationIdsRequest>,
    ) -> Result<Response<rpc::TenantList>, Status> {
        crate::handlers::tenant::find_tenants_by_organization_ids(self, request).await
    }

    async fn find_tenant_organization_ids(
        &self,
        request: Request<rpc::TenantSearchFilter>,
    ) -> Result<Response<rpc::TenantOrganizationIdList>, Status> {
        crate::handlers::tenant::find_tenant_organization_ids(self, request).await
    }

    async fn create_tenant_keyset(
        &self,
        request: Request<rpc::CreateTenantKeysetRequest>,
    ) -> Result<Response<rpc::CreateTenantKeysetResponse>, Status> {
        crate::handlers::tenant_keyset::create(self, request).await
    }

    async fn find_tenant_keyset_ids(
        &self,
        request: Request<rpc::TenantKeysetSearchFilter>,
    ) -> Result<Response<rpc::TenantKeysetIdList>, Status> {
        crate::handlers::tenant_keyset::find_ids(self, request).await
    }

    async fn find_tenant_keysets_by_ids(
        &self,
        request: Request<rpc::TenantKeysetsByIdsRequest>,
    ) -> Result<Response<rpc::TenantKeySetList>, Status> {
        crate::handlers::tenant_keyset::find_by_ids(self, request).await
    }

    async fn update_tenant_keyset(
        &self,
        request: Request<rpc::UpdateTenantKeysetRequest>,
    ) -> Result<Response<rpc::UpdateTenantKeysetResponse>, Status> {
        crate::handlers::tenant_keyset::update(self, request).await
    }

    async fn delete_tenant_keyset(
        &self,
        request: Request<rpc::DeleteTenantKeysetRequest>,
    ) -> Result<Response<rpc::DeleteTenantKeysetResponse>, Status> {
        crate::handlers::tenant_keyset::delete(self, request).await
    }

    async fn validate_tenant_public_key(
        &self,
        request: Request<rpc::ValidateTenantPublicKeyRequest>,
    ) -> Result<Response<rpc::ValidateTenantPublicKeyResponse>, Status> {
        crate::handlers::tenant_keyset::validate_public_key(self, request).await
    }

    async fn renew_machine_certificate(
        &self,
        request: Request<rpc::MachineCertificateRenewRequest>,
    ) -> Result<Response<rpc::MachineCertificateResult>, Status> {
        crate::handlers::credential::renew_machine_certificate(self, request).await
    }

    async fn discover_machine(
        &self,
        request: Request<rpc::MachineDiscoveryInfo>,
    ) -> Result<Response<rpc::MachineDiscoveryResult>, Status> {
        crate::handlers::machine_discovery::discover_machine(self, request).await
    }

    // Host has completed discovery
    async fn discovery_completed(
        &self,
        request: Request<rpc::MachineDiscoveryCompletedRequest>,
    ) -> Result<Response<rpc::MachineDiscoveryCompletedResponse>, Status> {
        crate::handlers::machine_discovery::discovery_completed(self, request).await
    }

    // Transitions the machine to Ready state.
    // Called by 'forge-scout discovery' once cleanup succeeds.
    async fn cleanup_machine_completed(
        &self,
        request: Request<rpc::MachineCleanupInfo>,
    ) -> Result<Response<rpc::MachineCleanupResult>, Status> {
        crate::handlers::machine_scout::cleanup_machine_completed(self, request).await
    }

    // Invoked by forge-scout whenever a certain Machine can not be properly acted on
    async fn report_forge_scout_error(
        &self,
        request: Request<rpc::ForgeScoutErrorReport>,
    ) -> Result<Response<rpc::ForgeScoutErrorReportResult>, Status> {
        crate::handlers::machine_scout::report_forge_scout_error(self, request)
    }

    async fn discover_dhcp(
        &self,
        request: Request<rpc::DhcpDiscovery>,
    ) -> Result<Response<rpc::DhcpRecord>, Status> {
        log_request_data(&request);

        Ok(crate::dhcp::discover::discover_dhcp(self, request).await?)
    }

    async fn expire_dhcp_lease(
        &self,
        request: Request<rpc::ExpireDhcpLeaseRequest>,
    ) -> Result<Response<rpc::ExpireDhcpLeaseResponse>, Status> {
        log_request_data(&request);
        Ok(crate::dhcp::expire::expire_dhcp_lease(self, request).await?)
    }

    async fn find_machine_ids(
        &self,
        request: Request<rpc::MachineSearchConfig>,
    ) -> Result<Response<::rpc::common::MachineIdList>, Status> {
        crate::handlers::machine::find_machine_ids(self, request).await
    }

    async fn find_machines_by_ids(
        &self,
        request: Request<::rpc::forge::MachinesByIdsRequest>,
    ) -> Result<Response<::rpc::MachineList>, Status> {
        crate::handlers::machine::find_machines_by_ids(self, request).await
    }

    async fn find_machine_state_histories(
        &self,
        request: Request<rpc::MachineStateHistoriesRequest>,
    ) -> std::result::Result<Response<rpc::MachineStateHistories>, Status> {
        crate::handlers::machine::find_machine_state_histories(self, request).await
    }

    async fn find_power_shelf_state_histories(
        &self,
        request: Request<rpc::PowerShelfStateHistoriesRequest>,
    ) -> Result<Response<rpc::StateHistories>, Status> {
        crate::handlers::power_shelf::find_power_shelf_state_histories(self, request).await
    }

    async fn find_power_shelf_health_histories(
        &self,
        request: Request<rpc::PowerShelfHealthHistoriesRequest>,
    ) -> Result<Response<rpc::HealthHistories>, Status> {
        crate::handlers::power_shelf::find_power_shelf_health_histories(self, request).await
    }

    async fn find_rack_state_histories(
        &self,
        request: tonic::Request<rpc::RackStateHistoriesRequest>,
    ) -> Result<Response<rpc::StateHistories>, Status> {
        crate::handlers::rack::find_rack_state_histories(self, request).await
    }

    async fn find_rack_health_histories(
        &self,
        request: Request<rpc::RackHealthHistoriesRequest>,
    ) -> Result<Response<rpc::HealthHistories>, Status> {
        crate::handlers::rack::find_rack_health_histories(self, request).await
    }

    async fn find_switch_state_histories(
        &self,
        request: Request<rpc::SwitchStateHistoriesRequest>,
    ) -> Result<Response<rpc::StateHistories>, Status> {
        crate::handlers::switch::find_switch_state_histories(self, request).await
    }

    async fn find_switch_health_histories(
        &self,
        request: Request<rpc::SwitchHealthHistoriesRequest>,
    ) -> Result<Response<rpc::HealthHistories>, Status> {
        crate::handlers::switch::find_switch_health_histories(self, request).await
    }

    async fn find_machine_health_histories(
        &self,
        request: Request<rpc::MachineHealthHistoriesRequest>,
    ) -> std::result::Result<Response<rpc::HealthHistories>, Status> {
        crate::handlers::machine::find_machine_health_histories(self, request).await
    }

    async fn assign_static_address(
        &self,
        request: Request<rpc::AssignStaticAddressRequest>,
    ) -> Result<Response<rpc::AssignStaticAddressResponse>, Status> {
        log_request_data(&request);
        Ok(
            crate::handlers::machine_interface_address::assign_static_address(self, request)
                .await?,
        )
    }

    async fn remove_static_address(
        &self,
        request: Request<rpc::RemoveStaticAddressRequest>,
    ) -> Result<Response<rpc::RemoveStaticAddressResponse>, Status> {
        log_request_data(&request);
        Ok(
            crate::handlers::machine_interface_address::remove_static_address(self, request)
                .await?,
        )
    }

    async fn find_interface_addresses(
        &self,
        request: Request<rpc::FindInterfaceAddressesRequest>,
    ) -> Result<Response<rpc::FindInterfaceAddressesResponse>, Status> {
        log_request_data(&request);
        crate::handlers::machine_interface_address::find_interface_addresses(self, request).await
    }

    async fn find_interfaces(
        &self,
        request: Request<rpc::InterfaceSearchQuery>,
    ) -> Result<Response<rpc::InterfaceList>, Status> {
        crate::handlers::machine_interface::find_interfaces(self, request).await
    }

    async fn delete_interface(
        &self,
        request: Request<rpc::InterfaceDeleteQuery>,
    ) -> Result<Response<()>, Status> {
        crate::handlers::machine_interface::delete_interface(self, request).await
    }

    // Fetches BMC Credentials
    async fn get_bmc_credentials(
        &self,
        request: Request<rpc::GetBmcCredentialsRequest>,
    ) -> Result<Response<rpc::GetBmcCredentialsResponse>, Status> {
        crate::handlers::credential::get_bmc_credentals(self, request).await
    }

    async fn get_switch_nvos_credentials(
        &self,
        request: Request<rpc::GetSwitchNvosCredentialsRequest>,
    ) -> Result<Response<rpc::GetBmcCredentialsResponse>, Status> {
        crate::handlers::credential::get_switch_nvos_credentials(self, request).await
    }

    async fn get_container_registry_credential(
        &self,
        request: Request<rpc::GetContainerRegistryCredentialRequest>,
    ) -> Result<Response<rpc::GetContainerRegistryCredentialResponse>, Status> {
        crate::handlers::credential::get_container_registry_credential(self, request).await
    }

    async fn set_container_registry_credential(
        &self,
        request: Request<rpc::SetContainerRegistryCredentialRequest>,
    ) -> Result<Response<()>, Status> {
        crate::handlers::credential::set_container_registry_credential(self, request).await
    }

    /// Network status of each managed host, as reported by forge-dpu-agent.
    /// For use by forge-admin-cli
    ///
    /// Currently: Status of HBN on each DPU
    async fn get_all_managed_host_network_status(
        &self,
        request: Request<rpc::ManagedHostNetworkStatusRequest>,
    ) -> Result<Response<rpc::ManagedHostNetworkStatusResponse>, Status> {
        crate::handlers::dpu::get_all_managed_host_network_status(self, request).await
    }

    async fn get_bmc_meta_data(
        &self,
        request: Request<rpc::BmcMetaDataGetRequest>,
    ) -> Result<Response<rpc::BmcMetaDataGetResponse>, Status> {
        crate::handlers::bmc_metadata::get(self, request).await
    }

    async fn update_machine_credentials(
        &self,
        request: Request<rpc::MachineCredentialsUpdateRequest>,
    ) -> Result<Response<rpc::MachineCredentialsUpdateResponse>, Status> {
        crate::handlers::credential::update_machine_credentials(self, request).await
    }

    // The carbide pxe server makes this RPC call
    async fn get_pxe_instructions(
        &self,
        request: Request<rpc::PxeInstructionRequest>,
    ) -> Result<Response<rpc::PxeInstructions>, Status> {
        crate::handlers::pxe::get_pxe_instructions(self, request).await
    }

    async fn get_cloud_init_instructions(
        &self,
        request: Request<rpc::CloudInitInstructionsRequest>,
    ) -> Result<Response<rpc::CloudInitInstructions>, Status> {
        crate::handlers::pxe::get_cloud_init_instructions(self, request).await
    }

    async fn clear_site_exploration_error(
        &self,
        request: Request<rpc::ClearSiteExplorationErrorRequest>,
    ) -> Result<Response<()>, Status> {
        crate::handlers::site_explorer::clear_site_exploration_error(self, request).await
    }

    async fn is_bmc_in_managed_host(
        &self,
        request: Request<rpc::BmcEndpointRequest>,
    ) -> Result<Response<rpc::IsBmcInManagedHostResponse>, Status> {
        crate::handlers::site_explorer::is_bmc_in_managed_host(self, request).await
    }

    async fn bmc_credential_status(
        &self,
        request: Request<rpc::BmcEndpointRequest>,
    ) -> Result<Response<rpc::BmcCredentialStatusResponse>, Status> {
        crate::handlers::bmc_endpoint_explorer::bmc_credential_status(self, request).await
    }

    async fn re_explore_endpoint(
        &self,
        request: Request<rpc::ReExploreEndpointRequest>,
    ) -> Result<Response<()>, Status> {
        crate::handlers::site_explorer::re_explore_endpoint(self, request).await
    }

    async fn refresh_endpoint_report(
        &self,
        request: Request<rpc::RefreshEndpointReportRequest>,
    ) -> Result<Response<::rpc::site_explorer::ExploredEndpoint>, Status> {
        crate::handlers::site_explorer::refresh_endpoint_report(self, request).await
    }

    async fn delete_explored_endpoint(
        &self,
        request: Request<rpc::DeleteExploredEndpointRequest>,
    ) -> Result<Response<rpc::DeleteExploredEndpointResponse>, Status> {
        crate::handlers::site_explorer::delete_explored_endpoint(self, request).await
    }

    async fn pause_explored_endpoint_remediation(
        &self,
        request: Request<rpc::PauseExploredEndpointRemediationRequest>,
    ) -> Result<Response<()>, Status> {
        crate::handlers::site_explorer::pause_explored_endpoint_remediation(self, request).await
    }

    // DEPRECATED: use find_explored_endpoint_ids, find_explored_endpoints_by_ids and find_explored_managed_host_ids, find_explored_managed_hosts_by_ids instead
    async fn get_site_exploration_report(
        &self,
        request: Request<::rpc::forge::GetSiteExplorationRequest>,
    ) -> Result<Response<::rpc::site_explorer::SiteExplorationReport>, Status> {
        crate::handlers::site_explorer::get_site_exploration_report(self, request).await
    }

    async fn get_site_explorer_last_run(
        &self,
        request: Request<()>,
    ) -> Result<Response<::rpc::site_explorer::SiteExplorerLastRunResponse>, Status> {
        crate::handlers::site_explorer::get_site_explorer_last_run(self, request).await
    }

    async fn find_explored_endpoint_ids(
        &self,
        request: Request<::rpc::site_explorer::ExploredEndpointSearchFilter>,
    ) -> Result<Response<::rpc::site_explorer::ExploredEndpointIdList>, Status> {
        crate::handlers::site_explorer::find_explored_endpoint_ids(self, request).await
    }

    async fn find_explored_endpoints_by_ids(
        &self,
        request: Request<::rpc::site_explorer::ExploredEndpointsByIdsRequest>,
    ) -> Result<Response<::rpc::site_explorer::ExploredEndpointList>, Status> {
        crate::handlers::site_explorer::find_explored_endpoints_by_ids(self, request).await
    }

    async fn find_explored_managed_host_ids(
        &self,
        request: Request<::rpc::site_explorer::ExploredManagedHostSearchFilter>,
    ) -> Result<Response<::rpc::site_explorer::ExploredManagedHostIdList>, Status> {
        crate::handlers::site_explorer::find_explored_managed_host_ids(self, request).await
    }

    async fn find_explored_managed_hosts_by_ids(
        &self,
        request: Request<::rpc::site_explorer::ExploredManagedHostsByIdsRequest>,
    ) -> Result<Response<::rpc::site_explorer::ExploredManagedHostList>, Status> {
        crate::handlers::site_explorer::find_explored_managed_hosts_by_ids(self, request).await
    }

    async fn find_explored_mlx_device_host_ids(
        &self,
        request: Request<::rpc::site_explorer::ExploredMlxDeviceHostSearchFilter>,
    ) -> Result<Response<::rpc::site_explorer::ExploredMlxDeviceHostIdList>, Status> {
        crate::handlers::site_explorer::find_explored_mlx_device_host_ids(self, request).await
    }

    async fn find_explored_mlx_devices_by_ids(
        &self,
        request: Request<::rpc::site_explorer::ExploredMlxDevicesByIdsRequest>,
    ) -> Result<Response<::rpc::site_explorer::ExploredMlxDeviceList>, Status> {
        crate::handlers::site_explorer::find_explored_mlx_devices_by_ids(self, request).await
    }

    async fn update_machine_hardware_info(
        &self,
        request: Request<::rpc::forge::UpdateMachineHardwareInfoRequest>,
    ) -> Result<Response<()>, Status> {
        crate::handlers::machine_hardware_info::handle_machine_hardware_info_update(self, request)
            .await
    }

    // Ad-hoc BMC exploration
    async fn explore(
        &self,
        request: Request<::rpc::forge::BmcEndpointRequest>,
    ) -> Result<Response<::rpc::site_explorer::EndpointExplorationReport>, Status> {
        crate::handlers::bmc_endpoint_explorer::explore(self, request).await
    }

    // Called on x86 boot by 'forge-scout auto-detect --uuid=<uuid>'.
    // Tells it whether to discover or cleanup based on current machine state.
    async fn forge_agent_control(
        &self,
        request: Request<rpc::ForgeAgentControlRequest>,
    ) -> Result<Response<rpc::ForgeAgentControlResponse>, Status> {
        crate::handlers::machine_scout::forge_agent_control(self, request).await
    }

    async fn admin_force_delete_machine(
        &self,
        request: Request<rpc::AdminForceDeleteMachineRequest>,
    ) -> Result<Response<rpc::AdminForceDeleteMachineResponse>, Status> {
        crate::handlers::machine::admin_force_delete_machine(self, request).await
    }

    async fn decommission_managed_host(
        &self,
        request: Request<rpc::DecommissionManagedHostRequest>,
    ) -> Result<Response<rpc::DecommissionManagedHostResponse>, Status> {
        crate::handlers::managed_host::decommission_managed_host(self, request).await
    }

    /// Example TOML data in request.text:
    ///
    /// [lo-ip]
    /// type = "ipv4"
    /// prefix = "10.180.62.1/26"
    ///
    /// or
    ///
    /// [vlan-id]
    /// type = "integer"
    /// ranges = [{ start = "100", end = "501" }]
    async fn admin_grow_resource_pool(
        &self,
        request: Request<rpc::GrowResourcePoolRequest>,
    ) -> Result<Response<rpc::GrowResourcePoolResponse>, Status> {
        crate::handlers::resource_pool::grow(self, request).await
    }

    async fn admin_list_resource_pools(
        &self,
        request: Request<rpc::ListResourcePoolsRequest>,
    ) -> Result<Response<rpc::ResourcePools>, Status> {
        crate::handlers::resource_pool::list(self, request).await
    }

    async fn update_machine_metadata(
        &self,
        request: Request<rpc::MachineMetadataUpdateRequest>,
    ) -> std::result::Result<Response<()>, Status> {
        crate::handlers::machine::update_machine_metadata(self, request).await
    }

    async fn update_rack_metadata(
        &self,
        request: Request<rpc::RackMetadataUpdateRequest>,
    ) -> std::result::Result<Response<()>, Status> {
        crate::handlers::rack::update_rack_metadata(self, request).await
    }

    async fn update_switch_metadata(
        &self,
        request: Request<rpc::SwitchMetadataUpdateRequest>,
    ) -> std::result::Result<Response<()>, Status> {
        crate::handlers::switch::update_switch_metadata(self, request).await
    }

    async fn update_power_shelf_metadata(
        &self,
        request: Request<rpc::PowerShelfMetadataUpdateRequest>,
    ) -> std::result::Result<Response<()>, Status> {
        crate::handlers::power_shelf::update_power_shelf_metadata(self, request).await
    }

    async fn update_machine_nv_link_info(
        &self,
        request: Request<rpc::UpdateMachineNvLinkInfoRequest>,
    ) -> std::result::Result<Response<()>, Status> {
        crate::handlers::machine::update_machine_nv_link_info(self, request).await
    }

    async fn set_maintenance(
        &self,
        request: Request<rpc::MaintenanceRequest>,
    ) -> Result<Response<()>, Status> {
        crate::handlers::managed_host::set_maintenance(self, request).await
    }

    async fn find_ip_address(
        &self,
        request: Request<rpc::FindIpAddressRequest>,
    ) -> Result<Response<rpc::FindIpAddressResponse>, Status> {
        crate::handlers::finder::find_ip_address(self, request).await
    }

    async fn identify_uuid(
        &self,
        request: Request<rpc::IdentifyUuidRequest>,
    ) -> Result<Response<rpc::IdentifyUuidResponse>, Status> {
        crate::handlers::finder::identify_uuid(self, request).await
    }

    async fn identify_mac(
        &self,
        request: Request<rpc::IdentifyMacRequest>,
    ) -> Result<Response<rpc::IdentifyMacResponse>, Status> {
        crate::handlers::finder::identify_mac(self, request).await
    }

    async fn identify_serial(
        &self,
        request: Request<rpc::IdentifySerialRequest>,
    ) -> Result<Response<rpc::IdentifySerialResponse>, Status> {
        crate::handlers::finder::identify_serial(self, request).await
    }

    async fn get_power_options(
        &self,
        request: Request<rpc::PowerOptionRequest>,
    ) -> Result<Response<rpc::PowerOptionResponse>, Status> {
        crate::handlers::power_options::get_power_options(self, request).await
    }

    async fn update_power_option(
        &self,
        request: Request<rpc::PowerOptionUpdateRequest>,
    ) -> Result<Response<rpc::PowerOptionResponse>, Status> {
        crate::handlers::power_options::update_power_option(self, request).await
    }

    async fn get_rack(
        &self,
        request: Request<rpc::GetRackRequest>,
    ) -> Result<Response<rpc::GetRackResponse>, Status> {
        crate::handlers::rack::get_rack(self, request).await
    }

    async fn find_rack_ids(
        &self,
        request: Request<rpc::RackSearchFilter>,
    ) -> Result<Response<rpc::RackIdList>, Status> {
        crate::handlers::rack::find_ids(self, request).await
    }

    async fn find_racks_by_ids(
        &self,
        request: Request<rpc::RacksByIdsRequest>,
    ) -> Result<Response<rpc::RackList>, Status> {
        crate::handlers::rack::find_by_ids(self, request).await
    }

    async fn delete_rack(
        &self,
        request: Request<rpc::DeleteRackRequest>,
    ) -> Result<Response<()>, Status> {
        crate::handlers::rack::delete_rack(self, request).await
    }

    async fn admin_force_delete_rack(
        &self,
        request: Request<rpc::AdminForceDeleteRackRequest>,
    ) -> Result<Response<rpc::AdminForceDeleteRackResponse>, Status> {
        crate::handlers::rack::admin_force_delete_rack(self, request).await
    }

    async fn get_rack_profile(
        &self,
        request: Request<rpc::GetRackProfileRequest>,
    ) -> Result<Response<rpc::GetRackProfileResponse>, Status> {
        crate::handlers::rack::get_rack_profile(self, request).await
    }

    async fn list_rack_profiles(
        &self,
        request: Request<()>,
    ) -> Result<Response<rpc::ListRackProfilesResponse>, Status> {
        crate::handlers::rack::list_rack_profiles(self, request)
    }

    /// Trigger DPU reprovisioning
    async fn trigger_dpu_reprovisioning(
        &self,
        request: Request<rpc::DpuReprovisioningRequest>,
    ) -> Result<Response<()>, Status> {
        crate::handlers::dpu::trigger_dpu_reprovisioning(self, request).await
    }

    async fn list_dpu_waiting_for_reprovisioning(
        &self,
        request: Request<rpc::DpuReprovisioningListRequest>,
    ) -> Result<Response<rpc::DpuReprovisioningListResponse>, Status> {
        crate::handlers::dpu::list_dpu_waiting_for_reprovisioning(self, request).await
    }

    async fn trigger_host_reprovisioning(
        &self,
        request: Request<rpc::HostReprovisioningRequest>,
    ) -> Result<Response<()>, Status> {
        crate::handlers::host_reprovisioning::trigger_host_reprovisioning(self, request).await
    }

    async fn trigger_bmc_credential_rotation(
        &self,
        request: Request<rpc::BmcCredentialRotationRequest>,
    ) -> Result<Response<()>, Status> {
        crate::handlers::bmc_credential_rotation::trigger_bmc_credential_rotation(self, request)
            .await
    }

    async fn trigger_uefi_credential_rotation(
        &self,
        request: Request<rpc::UefiCredentialRotationRequest>,
    ) -> Result<Response<()>, Status> {
        crate::handlers::uefi_credential_rotation::trigger_uefi_credential_rotation(self, request)
            .await
    }

    async fn trigger_nic_lockdown_credential_rotation(
        &self,
        request: Request<rpc::NicLockdownCredentialRotationRequest>,
    ) -> Result<Response<()>, Status> {
        crate::handlers::nic_lockdown_credential_rotation::trigger_nic_lockdown_credential_rotation(
            self, request,
        )
        .await
    }

    async fn mark_manual_firmware_upgrade_complete(
        &self,
        request: Request<MachineId>,
    ) -> Result<Response<()>, Status> {
        crate::handlers::host_reprovisioning::mark_manual_firmware_upgrade_complete(self, request)
            .await
    }

    async fn report_scout_firmware_upgrade_status(
        &self,
        request: Request<rpc::ScoutFirmwareUpgradeStatusRequest>,
    ) -> Result<Response<()>, Status> {
        crate::handlers::host_reprovisioning::report_scout_firmware_upgrade_status(self, request)
            .await
    }

    async fn list_hosts_waiting_for_reprovisioning(
        &self,
        request: Request<rpc::HostReprovisioningListRequest>,
    ) -> Result<Response<rpc::HostReprovisioningListResponse>, Status> {
        crate::handlers::host_reprovisioning::list_hosts_waiting_for_reprovisioning(self, request)
            .await
    }

    /// Retrieves all DPU information including id and loopback IP
    async fn get_dpu_info_list(
        &self,
        request: Request<rpc::GetDpuInfoListRequest>,
    ) -> Result<Response<rpc::GetDpuInfoListResponse>, Status> {
        crate::handlers::machine::get_dpu_info_list(self, request).await
    }

    async fn get_machine_boot_override(
        &self,
        request: Request<MachineInterfaceId>,
    ) -> Result<Response<rpc::MachineBootOverride>, Status> {
        crate::handlers::boot_override::get(self, request).await
    }

    async fn set_machine_boot_override(
        &self,
        request: Request<rpc::MachineBootOverride>,
    ) -> Result<Response<()>, Status> {
        crate::handlers::boot_override::set(self, request).await
    }

    async fn clear_machine_boot_override(
        &self,
        request: Request<MachineInterfaceId>,
    ) -> Result<Response<()>, Status> {
        crate::handlers::boot_override::clear(self, request).await
    }

    async fn get_machine_boot_interfaces(
        &self,
        request: Request<rpc::GetMachineBootInterfacesRequest>,
    ) -> Result<Response<rpc::GetMachineBootInterfacesResponse>, Status> {
        crate::handlers::machine_boot_interfaces::get_machine_boot_interfaces(self, request).await
    }

    async fn get_network_topology(
        &self,
        request: Request<rpc::NetworkTopologyRequest>,
    ) -> Result<Response<rpc::NetworkTopologyData>, Status> {
        crate::handlers::network_devices::get_network_topology(self, request).await
    }

    async fn admin_bmc_reset(
        &self,
        request: Request<rpc::AdminBmcResetRequest>,
    ) -> Result<Response<rpc::AdminBmcResetResponse>, Status> {
        crate::handlers::bmc_endpoint_explorer::admin_bmc_reset(self, request).await
    }

    async fn admin_gpu_reset(
        &self,
        request: Request<rpc::AdminGpuResetRequest>,
    ) -> Result<Response<rpc::AdminGpuResetResponse>, Status> {
        crate::handlers::gpu_reset::admin_gpu_reset(self, request).await
    }

    async fn disable_secure_boot(
        &self,
        request: Request<rpc::BmcEndpointRequest>,
    ) -> Result<Response<::rpc::forge::DisableSecureBootResponse>, Status> {
        crate::handlers::bmc_endpoint_explorer::disable_secure_boot(self, request).await
    }

    async fn lockdown(
        &self,
        request: Request<rpc::LockdownRequest>,
    ) -> Result<Response<::rpc::forge::LockdownResponse>, Status> {
        crate::handlers::bmc_endpoint_explorer::lockdown(self, request).await
    }

    async fn lockdown_status(
        &self,
        request: Request<rpc::LockdownStatusRequest>,
    ) -> Result<Response<::rpc::site_explorer::LockdownStatus>, Status> {
        crate::handlers::bmc_endpoint_explorer::lockdown_status(self, request).await
    }

    async fn enable_infinite_boot(
        &self,
        request: Request<rpc::EnableInfiniteBootRequest>,
    ) -> Result<Response<::rpc::forge::EnableInfiniteBootResponse>, Status> {
        crate::handlers::bmc_endpoint_explorer::enable_infinite_boot(self, request).await
    }

    async fn is_infinite_boot_enabled(
        &self,
        request: Request<rpc::IsInfiniteBootEnabledRequest>,
    ) -> Result<Response<::rpc::forge::IsInfiniteBootEnabledResponse>, Status> {
        crate::handlers::bmc_endpoint_explorer::is_infinite_boot_enabled(self, request).await
    }

    async fn machine_setup(
        &self,
        request: Request<rpc::MachineSetupRequest>,
    ) -> Result<Response<::rpc::forge::MachineSetupResponse>, Status> {
        crate::handlers::bmc_endpoint_explorer::machine_setup(self, request).await
    }

    async fn set_dpu_first_boot_order(
        &self,
        request: Request<rpc::SetDpuFirstBootOrderRequest>,
    ) -> Result<Response<::rpc::forge::SetDpuFirstBootOrderResponse>, Status> {
        crate::handlers::bmc_endpoint_explorer::set_dpu_first_boot_order(self, request).await
    }

    /// Should this DPU upgrade it's forge-dpu-agent?
    /// Once the upgrade is complete record_dpu_network_status will receive the updated
    /// version and write the DB to say our upgrade is complete.
    async fn dpu_agent_upgrade_check(
        &self,
        request: Request<rpc::DpuAgentUpgradeCheckRequest>,
    ) -> Result<Response<rpc::DpuAgentUpgradeCheckResponse>, Status> {
        crate::handlers::dpu::dpu_agent_upgrade_check(self, request).await
    }

    /// Get or set the forge-dpu-agent upgrade policy.
    async fn dpu_agent_upgrade_policy_action(
        &self,
        request: Request<rpc::DpuAgentUpgradePolicyRequest>,
    ) -> Result<Response<rpc::DpuAgentUpgradePolicyResponse>, Status> {
        crate::handlers::dpu::dpu_agent_upgrade_policy_action(self, request).await
    }

    async fn create_credential(
        &self,
        request: Request<rpc::CredentialCreationRequest>,
    ) -> Result<Response<rpc::CredentialCreationResult>, Status> {
        crate::handlers::credential::create_credential(self, request).await
    }

    async fn delete_credential(
        &self,
        request: Request<rpc::CredentialDeletionRequest>,
    ) -> Result<Response<rpc::CredentialDeletionResult>, Status> {
        crate::handlers::credential::delete_credential(self, request).await
    }

    async fn rotate_credential(
        &self,
        request: Request<rpc::RotateCredentialRequest>,
    ) -> Result<Response<rpc::RotateCredentialResult>, Status> {
        crate::handlers::credential_rotation::rotate_credential(self, request).await
    }

    async fn get_credential_rotation_status(
        &self,
        request: Request<rpc::CredentialRotationStatusRequest>,
    ) -> Result<Response<rpc::CredentialRotationStatusResult>, Status> {
        crate::handlers::credential_rotation::get_credential_rotation_status(self, request).await
    }

    async fn re_wrap_secrets(
        &self,
        request: Request<rpc::ReWrapSecretsRequest>,
    ) -> Result<Response<rpc::ReWrapSecretsResponse>, Status> {
        crate::handlers::secrets::re_wrap_secrets(self, request).await
    }

    /// get_route_servers returns a list of all configured route server
    /// entries for all source types.
    async fn get_route_servers(
        &self,
        request: Request<()>,
    ) -> Result<Response<rpc::RouteServerEntries>, Status> {
        crate::handlers::route_server::get(self, request).await
    }

    /// add_route_servers adds new route server entries for the
    /// provided source_type, defaulting to admin_api for calls
    /// coming from forge-admin-cli (but can be overridden in
    /// cases where deemed appropriate).
    async fn add_route_servers(
        &self,
        request: Request<rpc::RouteServers>,
    ) -> Result<Response<()>, Status> {
        crate::handlers::route_server::add(self, request).await
    }

    /// remove_route_servers removes route server entries for the
    /// provided source_type, defaulting to admin_api for calls
    /// coming from forge-admin-cli (but can be overridden in
    /// cases where deemed appropriate).
    async fn remove_route_servers(
        &self,
        request: Request<rpc::RouteServers>,
    ) -> Result<Response<()>, Status> {
        crate::handlers::route_server::remove(self, request).await
    }

    /// replace_route_servers replaces all route server entries
    /// for the provided source_type with the given list, defaulting
    /// to admin_api for calls coming from forge-admin-cli (but can
    /// be overridden in cases where deemed appropriate).
    async fn replace_route_servers(
        &self,
        request: Request<rpc::RouteServers>,
    ) -> Result<Response<()>, Status> {
        crate::handlers::route_server::replace(self, request).await
    }

    async fn set_dynamic_config(
        &self,
        request: Request<rpc::SetDynamicConfigRequest>,
    ) -> Result<Response<()>, Status> {
        crate::handlers::api::set_dynamic_config(self, request)
    }

    async fn clear_host_uefi_password(
        &self,
        request: Request<rpc::ClearHostUefiPasswordRequest>,
    ) -> Result<Response<rpc::ClearHostUefiPasswordResponse>, Status> {
        crate::handlers::uefi::clear_host_uefi_password(self, request).await
    }

    async fn set_host_uefi_password(
        &self,
        request: Request<rpc::SetHostUefiPasswordRequest>,
    ) -> Result<Response<rpc::SetHostUefiPasswordResponse>, Status> {
        crate::handlers::uefi::set_host_uefi_password(self, request).await
    }

    async fn set_dpu_uefi_password(
        &self,
        request: Request<rpc::SetDpuUefiPasswordRequest>,
    ) -> Result<Response<rpc::SetDpuUefiPasswordResponse>, Status> {
        crate::handlers::uefi::set_dpu_uefi_password(self, request).await
    }

    async fn get_expected_machine(
        &self,
        request: Request<rpc::ExpectedMachineRequest>,
    ) -> Result<Response<rpc::ExpectedMachine>, Status> {
        crate::handlers::expected_machine::get(self, request).await
    }

    async fn add_expected_machine(
        &self,
        request: Request<rpc::ExpectedMachine>,
    ) -> Result<Response<()>, Status> {
        crate::handlers::expected_machine::add(self, request).await
    }

    async fn delete_expected_machine(
        &self,
        request: Request<rpc::ExpectedMachineRequest>,
    ) -> Result<Response<()>, Status> {
        crate::handlers::expected_machine::delete(self, request).await
    }

    async fn update_expected_machine(
        &self,
        request: Request<rpc::ExpectedMachine>,
    ) -> Result<Response<()>, Status> {
        crate::handlers::expected_machine::update(self, request).await
    }

    async fn replace_all_expected_machines(
        &self,
        request: Request<rpc::ExpectedMachineList>,
    ) -> Result<Response<()>, Status> {
        crate::handlers::expected_machine::replace_all(self, request).await
    }

    async fn get_all_expected_machines(
        &self,
        request: Request<()>,
    ) -> Result<Response<rpc::ExpectedMachineList>, Status> {
        crate::handlers::expected_machine::get_all(self, request).await
    }

    async fn get_all_expected_machines_linked(
        &self,
        request: Request<()>,
    ) -> Result<Response<rpc::LinkedExpectedMachineList>, Status> {
        crate::handlers::expected_machine::get_linked(self, request).await
    }

    async fn get_all_unexpected_machines(
        &self,
        request: Request<()>,
    ) -> Result<Response<rpc::UnexpectedMachineList>, Status> {
        crate::handlers::expected_machine::get_all_unexpected_machines(self, request).await
    }

    async fn delete_all_expected_machines(
        &self,
        request: Request<()>,
    ) -> Result<Response<()>, Status> {
        crate::handlers::expected_machine::delete_all(self, request).await
    }

    async fn create_expected_machines(
        &self,
        request: Request<rpc::BatchExpectedMachineOperationRequest>,
    ) -> Result<Response<rpc::BatchExpectedMachineOperationResponse>, Status> {
        crate::handlers::expected_machine::create_expected_machines(self, request).await
    }

    async fn update_expected_machines(
        &self,
        request: Request<rpc::BatchExpectedMachineOperationRequest>,
    ) -> Result<Response<rpc::BatchExpectedMachineOperationResponse>, Status> {
        crate::handlers::expected_machine::update_expected_machines(self, request).await
    }

    async fn get_expected_power_shelf(
        &self,
        request: Request<rpc::ExpectedPowerShelfRequest>,
    ) -> Result<Response<rpc::ExpectedPowerShelf>, Status> {
        crate::handlers::expected_power_shelf::get_expected_power_shelf(self, request).await
    }

    async fn add_expected_power_shelf(
        &self,
        request: Request<rpc::ExpectedPowerShelf>,
    ) -> Result<Response<()>, Status> {
        crate::handlers::expected_power_shelf::add_expected_power_shelf(self, request).await
    }

    async fn delete_expected_power_shelf(
        &self,
        request: Request<rpc::ExpectedPowerShelfRequest>,
    ) -> Result<Response<()>, Status> {
        crate::handlers::expected_power_shelf::delete_expected_power_shelf(self, request).await
    }

    async fn update_expected_power_shelf(
        &self,
        request: Request<rpc::ExpectedPowerShelf>,
    ) -> Result<Response<()>, Status> {
        crate::handlers::expected_power_shelf::update_expected_power_shelf(self, request).await
    }

    async fn replace_all_expected_power_shelves(
        &self,
        request: Request<rpc::ExpectedPowerShelfList>,
    ) -> Result<Response<()>, Status> {
        crate::handlers::expected_power_shelf::replace_all_expected_power_shelves(self, request)
            .await
    }

    async fn get_all_expected_power_shelves(
        &self,
        request: Request<()>,
    ) -> Result<Response<rpc::ExpectedPowerShelfList>, Status> {
        crate::handlers::expected_power_shelf::get_all_expected_power_shelves(self, request).await
    }

    async fn get_all_expected_power_shelves_linked(
        &self,
        request: Request<()>,
    ) -> Result<Response<rpc::LinkedExpectedPowerShelfList>, Status> {
        crate::handlers::expected_power_shelf::get_all_expected_power_shelves_linked(self, request)
            .await
    }

    async fn delete_all_expected_power_shelves(
        &self,
        request: Request<()>,
    ) -> Result<Response<()>, Status> {
        crate::handlers::expected_power_shelf::delete_all_expected_power_shelves(self, request)
            .await
    }

    async fn get_expected_switch(
        &self,
        request: Request<rpc::ExpectedSwitchRequest>,
    ) -> Result<Response<rpc::ExpectedSwitch>, Status> {
        crate::handlers::expected_switch::get_expected_switch(self, request).await
    }

    async fn add_expected_switch(
        &self,
        request: Request<rpc::ExpectedSwitch>,
    ) -> Result<Response<()>, Status> {
        crate::handlers::expected_switch::add_expected_switch(self, request).await
    }

    async fn delete_expected_switch(
        &self,
        request: Request<rpc::ExpectedSwitchRequest>,
    ) -> Result<Response<()>, Status> {
        crate::handlers::expected_switch::delete_expected_switch(self, request).await
    }

    async fn update_expected_switch(
        &self,
        request: Request<rpc::ExpectedSwitch>,
    ) -> Result<Response<()>, Status> {
        crate::handlers::expected_switch::update_expected_switch(self, request).await
    }

    async fn replace_all_expected_switches(
        &self,
        request: Request<rpc::ExpectedSwitchList>,
    ) -> Result<Response<()>, Status> {
        crate::handlers::expected_switch::replace_all_expected_switches(self, request).await
    }

    async fn get_all_expected_switches(
        &self,
        request: Request<()>,
    ) -> Result<Response<rpc::ExpectedSwitchList>, Status> {
        crate::handlers::expected_switch::get_all_expected_switches(self, request).await
    }

    async fn get_all_expected_switches_linked(
        &self,
        request: Request<()>,
    ) -> Result<Response<rpc::LinkedExpectedSwitchList>, Status> {
        crate::handlers::expected_switch::get_all_expected_switches_linked(self, request).await
    }

    async fn delete_all_expected_switches(
        &self,
        request: Request<()>,
    ) -> Result<Response<()>, Status> {
        crate::handlers::expected_switch::delete_all_expected_switches(self, request).await
    }

    async fn add_expected_rack(
        &self,
        request: Request<rpc::ExpectedRack>,
    ) -> Result<Response<()>, Status> {
        crate::handlers::expected_rack::add_expected_rack(self, request).await
    }

    async fn delete_expected_rack(
        &self,
        request: Request<rpc::ExpectedRackRequest>,
    ) -> Result<Response<()>, Status> {
        crate::handlers::expected_rack::delete_expected_rack(self, request).await
    }

    async fn update_expected_rack(
        &self,
        request: Request<rpc::ExpectedRack>,
    ) -> Result<Response<()>, Status> {
        crate::handlers::expected_rack::update_expected_rack(self, request).await
    }

    async fn get_expected_rack(
        &self,
        request: Request<rpc::ExpectedRackRequest>,
    ) -> Result<Response<rpc::ExpectedRack>, Status> {
        crate::handlers::expected_rack::get_expected_rack(self, request).await
    }

    async fn get_all_expected_racks(
        &self,
        request: Request<()>,
    ) -> Result<Response<rpc::ExpectedRackList>, Status> {
        crate::handlers::expected_rack::get_all_expected_racks(self, request).await
    }

    async fn replace_all_expected_racks(
        &self,
        request: Request<rpc::ExpectedRackList>,
    ) -> Result<Response<()>, Status> {
        crate::handlers::expected_rack::replace_all_expected_racks(self, request).await
    }

    async fn delete_all_expected_racks(
        &self,
        request: Request<()>,
    ) -> Result<Response<()>, Status> {
        crate::handlers::expected_rack::delete_all_expected_racks(self, request).await
    }

    async fn find_connected_devices_by_dpu_machine_ids(
        &self,
        request: Request<::rpc::common::MachineIdList>,
    ) -> Result<Response<rpc::ConnectedDeviceList>, Status> {
        crate::handlers::network_devices::find_connected_devices_by_dpu_machine_ids(self, request)
            .await
    }

    async fn find_network_devices_by_device_ids(
        &self,
        request: Request<rpc::NetworkDeviceIdList>,
    ) -> Result<Response<rpc::NetworkTopologyData>, Status> {
        crate::handlers::network_devices::find_network_devices_by_device_ids(self, request).await
    }

    async fn find_machine_ids_by_bmc_ips(
        &self,
        request: Request<rpc::BmcIpList>,
    ) -> Result<Response<rpc::MachineIdBmcIpPairs>, Status> {
        crate::handlers::machine::find_machine_ids_by_bmc_ips(self, request).await
    }

    async fn find_mac_address_by_bmc_ip(
        &self,
        request: Request<rpc::BmcIp>,
    ) -> Result<Response<rpc::MacAddressBmcIp>, Status> {
        crate::handlers::machine_interface::find_mac_address_by_bmc_ip(self, request).await
    }

    async fn attest_quote(
        &self,
        request: Request<rpc::AttestQuoteRequest>,
    ) -> std::result::Result<Response<rpc::AttestQuoteResponse>, Status> {
        crate::handlers::attestation::attest_quote(self, request).await
    }

    async fn create_measurement_system_profile(
        &self,
        request: Request<measured_boot_pb::CreateMeasurementSystemProfileRequest>,
    ) -> Result<Response<measured_boot_pb::CreateMeasurementSystemProfileResponse>, Status> {
        crate::handlers::measured_boot::create_system_profile(self, request).await
    }

    async fn delete_measurement_system_profile(
        &self,
        request: Request<measured_boot_pb::DeleteMeasurementSystemProfileRequest>,
    ) -> Result<Response<measured_boot_pb::DeleteMeasurementSystemProfileResponse>, Status> {
        crate::handlers::measured_boot::delete_system_profile(self, request).await
    }

    async fn rename_measurement_system_profile(
        &self,
        request: Request<measured_boot_pb::RenameMeasurementSystemProfileRequest>,
    ) -> Result<Response<measured_boot_pb::RenameMeasurementSystemProfileResponse>, Status> {
        crate::handlers::measured_boot::rename_system_profile(self, request).await
    }

    async fn show_measurement_system_profile(
        &self,
        request: Request<measured_boot_pb::ShowMeasurementSystemProfileRequest>,
    ) -> Result<Response<measured_boot_pb::ShowMeasurementSystemProfileResponse>, Status> {
        crate::handlers::measured_boot::show_system_profile(self, request).await
    }

    async fn show_measurement_system_profiles(
        &self,
        request: Request<measured_boot_pb::ShowMeasurementSystemProfilesRequest>,
    ) -> Result<Response<measured_boot_pb::ShowMeasurementSystemProfilesResponse>, Status> {
        crate::handlers::measured_boot::show_system_profiles(self, request).await
    }

    async fn list_measurement_system_profiles(
        &self,
        request: Request<measured_boot_pb::ListMeasurementSystemProfilesRequest>,
    ) -> Result<Response<measured_boot_pb::ListMeasurementSystemProfilesResponse>, Status> {
        crate::handlers::measured_boot::list_system_profiles(self, request).await
    }

    async fn list_measurement_system_profile_bundles(
        &self,
        request: Request<measured_boot_pb::ListMeasurementSystemProfileBundlesRequest>,
    ) -> Result<Response<measured_boot_pb::ListMeasurementSystemProfileBundlesResponse>, Status>
    {
        crate::handlers::measured_boot::list_system_profile_bundles(self, request).await
    }

    async fn list_measurement_system_profile_machines(
        &self,
        request: Request<measured_boot_pb::ListMeasurementSystemProfileMachinesRequest>,
    ) -> Result<Response<measured_boot_pb::ListMeasurementSystemProfileMachinesResponse>, Status>
    {
        crate::handlers::measured_boot::list_system_profile_machines(self, request).await
    }

    async fn create_measurement_report(
        &self,
        request: Request<measured_boot_pb::CreateMeasurementReportRequest>,
    ) -> Result<Response<measured_boot_pb::CreateMeasurementReportResponse>, Status> {
        crate::handlers::measured_boot::create_report(self, request).await
    }

    async fn delete_measurement_report(
        &self,
        request: Request<measured_boot_pb::DeleteMeasurementReportRequest>,
    ) -> Result<Response<measured_boot_pb::DeleteMeasurementReportResponse>, Status> {
        crate::handlers::measured_boot::delete_report(self, request).await
    }

    async fn promote_measurement_report(
        &self,
        request: Request<measured_boot_pb::PromoteMeasurementReportRequest>,
    ) -> Result<Response<measured_boot_pb::PromoteMeasurementReportResponse>, Status> {
        crate::handlers::measured_boot::promote_report(self, request).await
    }

    async fn revoke_measurement_report(
        &self,
        request: Request<measured_boot_pb::RevokeMeasurementReportRequest>,
    ) -> Result<Response<measured_boot_pb::RevokeMeasurementReportResponse>, Status> {
        crate::handlers::measured_boot::revoke_report(self, request).await
    }

    async fn show_measurement_report_for_id(
        &self,
        request: Request<measured_boot_pb::ShowMeasurementReportForIdRequest>,
    ) -> Result<Response<measured_boot_pb::ShowMeasurementReportForIdResponse>, Status> {
        crate::handlers::measured_boot::show_report_for_id(self, request).await
    }

    async fn show_measurement_reports_for_machine(
        &self,
        request: Request<measured_boot_pb::ShowMeasurementReportsForMachineRequest>,
    ) -> Result<Response<measured_boot_pb::ShowMeasurementReportsForMachineResponse>, Status> {
        crate::handlers::measured_boot::show_reports_for_machine(self, request).await
    }

    async fn show_measurement_reports(
        &self,
        request: Request<measured_boot_pb::ShowMeasurementReportsRequest>,
    ) -> Result<Response<measured_boot_pb::ShowMeasurementReportsResponse>, Status> {
        crate::handlers::measured_boot::show_reports(self, request).await
    }

    async fn list_measurement_report(
        &self,
        request: Request<measured_boot_pb::ListMeasurementReportRequest>,
    ) -> Result<Response<measured_boot_pb::ListMeasurementReportResponse>, Status> {
        crate::handlers::measured_boot::list_report(self, request).await
    }

    async fn match_measurement_report(
        &self,
        request: Request<measured_boot_pb::MatchMeasurementReportRequest>,
    ) -> Result<Response<measured_boot_pb::MatchMeasurementReportResponse>, Status> {
        crate::handlers::measured_boot::match_report(self, request).await
    }

    async fn create_measurement_bundle(
        &self,
        request: Request<measured_boot_pb::CreateMeasurementBundleRequest>,
    ) -> Result<Response<measured_boot_pb::CreateMeasurementBundleResponse>, Status> {
        crate::handlers::measured_boot::create_bundle(self, request).await
    }

    async fn delete_measurement_bundle(
        &self,
        request: Request<measured_boot_pb::DeleteMeasurementBundleRequest>,
    ) -> Result<Response<measured_boot_pb::DeleteMeasurementBundleResponse>, Status> {
        crate::handlers::measured_boot::delete_bundle(self, request).await
    }

    async fn rename_measurement_bundle(
        &self,
        request: Request<measured_boot_pb::RenameMeasurementBundleRequest>,
    ) -> Result<Response<measured_boot_pb::RenameMeasurementBundleResponse>, Status> {
        crate::handlers::measured_boot::rename_bundle(self, request).await
    }

    async fn update_measurement_bundle(
        &self,
        request: Request<measured_boot_pb::UpdateMeasurementBundleRequest>,
    ) -> Result<Response<measured_boot_pb::UpdateMeasurementBundleResponse>, Status> {
        crate::handlers::measured_boot::update_bundle(self, request).await
    }

    async fn show_measurement_bundle(
        &self,
        request: Request<measured_boot_pb::ShowMeasurementBundleRequest>,
    ) -> Result<Response<measured_boot_pb::ShowMeasurementBundleResponse>, Status> {
        crate::handlers::measured_boot::show_bundle(self, request).await
    }

    async fn show_measurement_bundles(
        &self,
        request: Request<measured_boot_pb::ShowMeasurementBundlesRequest>,
    ) -> Result<Response<measured_boot_pb::ShowMeasurementBundlesResponse>, Status> {
        crate::handlers::measured_boot::show_bundles(self, request).await
    }

    async fn list_measurement_bundles(
        &self,
        request: Request<measured_boot_pb::ListMeasurementBundlesRequest>,
    ) -> Result<Response<measured_boot_pb::ListMeasurementBundlesResponse>, Status> {
        crate::handlers::measured_boot::list_bundles(self, request).await
    }

    async fn list_measurement_bundle_machines(
        &self,
        request: Request<measured_boot_pb::ListMeasurementBundleMachinesRequest>,
    ) -> Result<Response<measured_boot_pb::ListMeasurementBundleMachinesResponse>, Status> {
        crate::handlers::measured_boot::list_bundle_machines(self, request).await
    }

    async fn find_closest_bundle_match(
        &self,
        request: Request<measured_boot_pb::FindClosestBundleMatchRequest>,
    ) -> Result<Response<measured_boot_pb::ShowMeasurementBundleResponse>, Status> {
        crate::handlers::measured_boot::find_closest_bundle_match(self, request).await
    }

    async fn delete_measurement_journal(
        &self,
        request: Request<measured_boot_pb::DeleteMeasurementJournalRequest>,
    ) -> Result<Response<measured_boot_pb::DeleteMeasurementJournalResponse>, Status> {
        crate::handlers::measured_boot::delete_journal(self, request).await
    }

    async fn show_measurement_journal(
        &self,
        request: Request<measured_boot_pb::ShowMeasurementJournalRequest>,
    ) -> Result<Response<measured_boot_pb::ShowMeasurementJournalResponse>, Status> {
        crate::handlers::measured_boot::show_journal(self, request).await
    }

    async fn show_measurement_journals(
        &self,
        request: Request<measured_boot_pb::ShowMeasurementJournalsRequest>,
    ) -> Result<Response<measured_boot_pb::ShowMeasurementJournalsResponse>, Status> {
        crate::handlers::measured_boot::show_journals(self, request).await
    }

    async fn list_measurement_journal(
        &self,
        request: Request<measured_boot_pb::ListMeasurementJournalRequest>,
    ) -> Result<Response<measured_boot_pb::ListMeasurementJournalResponse>, Status> {
        crate::handlers::measured_boot::list_journal(self, request).await
    }

    async fn attest_candidate_machine(
        &self,
        request: Request<measured_boot_pb::AttestCandidateMachineRequest>,
    ) -> Result<Response<measured_boot_pb::AttestCandidateMachineResponse>, Status> {
        crate::handlers::measured_boot::attest_candidate_machine(self, request).await
    }

    async fn show_candidate_machine(
        &self,
        request: Request<measured_boot_pb::ShowCandidateMachineRequest>,
    ) -> Result<Response<measured_boot_pb::ShowCandidateMachineResponse>, Status> {
        crate::handlers::measured_boot::show_candidate_machine(self, request).await
    }

    async fn show_candidate_machines(
        &self,
        request: Request<measured_boot_pb::ShowCandidateMachinesRequest>,
    ) -> Result<Response<measured_boot_pb::ShowCandidateMachinesResponse>, Status> {
        crate::handlers::measured_boot::show_candidate_machines(self, request).await
    }

    async fn list_candidate_machines(
        &self,
        request: Request<measured_boot_pb::ListCandidateMachinesRequest>,
    ) -> Result<Response<measured_boot_pb::ListCandidateMachinesResponse>, Status> {
        crate::handlers::measured_boot::list_candidate_machines(self, request).await
    }

    async fn import_site_measurements(
        &self,
        request: Request<measured_boot_pb::ImportSiteMeasurementsRequest>,
    ) -> Result<Response<measured_boot_pb::ImportSiteMeasurementsResponse>, Status> {
        crate::handlers::measured_boot::import_site_measurements(self, request).await
    }

    async fn export_site_measurements(
        &self,
        request: Request<measured_boot_pb::ExportSiteMeasurementsRequest>,
    ) -> Result<Response<measured_boot_pb::ExportSiteMeasurementsResponse>, Status> {
        crate::handlers::measured_boot::export_site_measurements(self, request).await
    }

    async fn add_measurement_trusted_machine(
        &self,
        request: Request<measured_boot_pb::AddMeasurementTrustedMachineRequest>,
    ) -> Result<Response<measured_boot_pb::AddMeasurementTrustedMachineResponse>, Status> {
        crate::handlers::measured_boot::add_trusted_machine(self, request).await
    }

    async fn remove_measurement_trusted_machine(
        &self,
        request: Request<measured_boot_pb::RemoveMeasurementTrustedMachineRequest>,
    ) -> Result<Response<measured_boot_pb::RemoveMeasurementTrustedMachineResponse>, Status> {
        crate::handlers::measured_boot::remove_trusted_machine(self, request).await
    }

    async fn list_measurement_trusted_machines(
        &self,
        request: Request<measured_boot_pb::ListMeasurementTrustedMachinesRequest>,
    ) -> Result<Response<measured_boot_pb::ListMeasurementTrustedMachinesResponse>, Status> {
        crate::handlers::measured_boot::list_trusted_machines(self, request).await
    }

    async fn add_measurement_trusted_profile(
        &self,
        request: Request<measured_boot_pb::AddMeasurementTrustedProfileRequest>,
    ) -> Result<Response<measured_boot_pb::AddMeasurementTrustedProfileResponse>, Status> {
        crate::handlers::measured_boot::add_trusted_profile(self, request).await
    }

    async fn remove_measurement_trusted_profile(
        &self,
        request: Request<measured_boot_pb::RemoveMeasurementTrustedProfileRequest>,
    ) -> Result<Response<measured_boot_pb::RemoveMeasurementTrustedProfileResponse>, Status> {
        crate::handlers::measured_boot::remove_trusted_profile(self, request).await
    }

    async fn list_measurement_trusted_profiles(
        &self,
        request: Request<measured_boot_pb::ListMeasurementTrustedProfilesRequest>,
    ) -> Result<Response<measured_boot_pb::ListMeasurementTrustedProfilesResponse>, Status> {
        crate::handlers::measured_boot::list_trusted_profiles(self, request).await
    }

    async fn list_attestation_summary(
        &self,
        request: Request<measured_boot_pb::ListAttestationSummaryRequest>,
    ) -> Result<Response<measured_boot_pb::ListAttestationSummaryResponse>, Status> {
        crate::handlers::measured_boot::list_attestation_summary(self, request).await
    }

    // Host has rebooted
    async fn reboot_completed(
        &self,
        request: Request<rpc::MachineRebootCompletedRequest>,
    ) -> Result<Response<rpc::MachineRebootCompletedResponse>, Status> {
        crate::handlers::machine_scout::reboot_completed(self, request).await
    }

    // machine has completed validation
    async fn machine_validation_completed(
        &self,
        request: Request<rpc::MachineValidationCompletedRequest>,
    ) -> Result<Response<rpc::MachineValidationCompletedResponse>, Status> {
        crate::handlers::machine_validation::mark_machine_validation_complete(self, request).await
    }

    async fn persist_validation_result(
        &self,
        request: Request<rpc::MachineValidationResultPostRequest>,
    ) -> Result<Response<()>, Status> {
        crate::handlers::machine_validation::persist_validation_result(self, request).await
    }

    async fn get_machine_validation_results(
        &self,
        request: Request<rpc::MachineValidationGetRequest>,
    ) -> Result<Response<rpc::MachineValidationResultList>, Status> {
        crate::handlers::machine_validation::get_machine_validation_results(self, request).await
    }

    async fn machine_set_auto_update(
        &self,
        request: Request<rpc::MachineSetAutoUpdateRequest>,
    ) -> Result<Response<rpc::MachineSetAutoUpdateResponse>, Status> {
        crate::handlers::machine::machine_set_auto_update(self, request).await
    }

    async fn get_machine_validation_external_config(
        &self,
        request: Request<rpc::GetMachineValidationExternalConfigRequest>,
    ) -> Result<Response<rpc::GetMachineValidationExternalConfigResponse>, Status> {
        crate::handlers::machine_validation::get_machine_validation_external_config(self, request)
            .await
    }

    async fn get_machine_validation_external_configs(
        &self,
        request: Request<rpc::GetMachineValidationExternalConfigsRequest>,
    ) -> Result<Response<rpc::GetMachineValidationExternalConfigsResponse>, Status> {
        crate::handlers::machine_validation::get_machine_validation_external_configs(self, request)
            .await
    }

    async fn add_update_machine_validation_external_config(
        &self,
        request: Request<rpc::AddUpdateMachineValidationExternalConfigRequest>,
    ) -> Result<Response<()>, Status> {
        crate::handlers::machine_validation::add_update_machine_validation_external_config(
            self, request,
        )
        .await
    }

    async fn create_os_image(
        &self,
        request: Request<rpc::OsImageAttributes>,
    ) -> Result<Response<rpc::OsImage>, Status> {
        crate::storage::create_os_image(self, request).await
    }

    async fn list_os_image(
        &self,
        request: Request<rpc::ListOsImageRequest>,
    ) -> Result<Response<rpc::ListOsImageResponse>, Status> {
        crate::storage::list_os_image(self, request).await
    }

    async fn get_os_image(
        &self,
        request: Request<::rpc::Uuid>,
    ) -> Result<Response<rpc::OsImage>, Status> {
        crate::storage::get_os_image(self, request).await
    }

    async fn delete_os_image(
        &self,
        request: Request<rpc::DeleteOsImageRequest>,
    ) -> Result<Response<rpc::DeleteOsImageResponse>, Status> {
        crate::storage::delete_os_image(self, request).await
    }

    async fn update_os_image(
        &self,
        request: Request<rpc::OsImageAttributes>,
    ) -> Result<Response<rpc::OsImage>, Status> {
        crate::storage::update_os_image(self, request).await
    }

    async fn create_operating_system(
        &self,
        request: Request<rpc::CreateOperatingSystemRequest>,
    ) -> Result<Response<rpc::OperatingSystem>, Status> {
        crate::handlers::operating_system::create_operating_system(self, request).await
    }

    async fn get_operating_system(
        &self,
        request: Request<::carbide_uuid::operating_system::OperatingSystemId>,
    ) -> Result<Response<rpc::OperatingSystem>, Status> {
        crate::handlers::operating_system::get_operating_system(self, request).await
    }

    async fn update_operating_system(
        &self,
        request: Request<rpc::UpdateOperatingSystemRequest>,
    ) -> Result<Response<rpc::OperatingSystem>, Status> {
        crate::handlers::operating_system::update_operating_system(self, request).await
    }

    async fn delete_operating_system(
        &self,
        request: Request<rpc::DeleteOperatingSystemRequest>,
    ) -> Result<Response<rpc::DeleteOperatingSystemResponse>, Status> {
        crate::handlers::operating_system::delete_operating_system(self, request).await
    }

    async fn find_operating_system_ids(
        &self,
        request: Request<rpc::OperatingSystemSearchFilter>,
    ) -> Result<Response<rpc::OperatingSystemIdList>, Status> {
        crate::handlers::operating_system::find_operating_system_ids(self, request).await
    }

    async fn find_operating_systems_by_ids(
        &self,
        request: Request<rpc::OperatingSystemsByIdsRequest>,
    ) -> Result<Response<rpc::OperatingSystemList>, Status> {
        crate::handlers::operating_system::find_operating_systems_by_ids(self, request).await
    }

    async fn get_operating_system_cachable_ipxe_template_artifacts(
        &self,
        request: Request<rpc::GetOperatingSystemCachableIpxeTemplateArtifactsRequest>,
    ) -> Result<Response<rpc::IpxeTemplateArtifactList>, Status> {
        crate::handlers::operating_system::get_operating_system_cachable_ipxe_script_artifacts(
            self, request,
        )
        .await
    }

    async fn update_operating_system_cachable_ipxe_template_artifacts(
        &self,
        request: Request<rpc::UpdateOperatingSystemIpxeTemplateArtifactRequest>,
    ) -> Result<Response<rpc::IpxeTemplateArtifactList>, Status> {
        crate::handlers::operating_system::update_operating_system_cachable_ipxe_script_artifacts(
            self, request,
        )
        .await
    }

    async fn get_machine_validation_runs(
        &self,
        request: Request<rpc::MachineValidationRunListGetRequest>,
    ) -> Result<Response<rpc::MachineValidationRunList>, Status> {
        crate::handlers::machine_validation::get_machine_validation_runs(self, request).await
    }

    async fn find_machine_validation_run_item_ids(
        &self,
        request: Request<rpc::MachineValidationRunItemSearchFilter>,
    ) -> Result<Response<rpc::MachineValidationRunItemIdList>, Status> {
        crate::handlers::machine_validation::find_machine_validation_run_item_ids(self, request)
            .await
    }

    async fn find_machine_validation_run_items_by_ids(
        &self,
        request: Request<rpc::MachineValidationRunItemsByIdsRequest>,
    ) -> Result<Response<rpc::MachineValidationRunItemList>, Status> {
        crate::handlers::machine_validation::find_machine_validation_run_items_by_ids(self, request)
            .await
    }

    async fn get_machine_validation_attempt(
        &self,
        request: Request<rpc::MachineValidationAttemptGetRequest>,
    ) -> Result<Response<rpc::MachineValidationAttempt>, Status> {
        crate::handlers::machine_validation::get_machine_validation_attempt(self, request).await
    }

    async fn heartbeat_machine_validation_run(
        &self,
        request: Request<rpc::MachineValidationHeartbeatRequest>,
    ) -> Result<Response<rpc::MachineValidationHeartbeatResponse>, Status> {
        crate::handlers::machine_validation::heartbeat_machine_validation_run(self, request).await
    }

    async fn admin_power_control(
        &self,
        request: Request<rpc::AdminPowerControlRequest>,
    ) -> Result<Response<rpc::AdminPowerControlResponse>, Status> {
        crate::handlers::bmc_endpoint_explorer::admin_power_control(self, request).await
    }

    async fn on_demand_machine_validation(
        &self,
        request: Request<rpc::MachineValidationOnDemandRequest>,
    ) -> Result<Response<rpc::MachineValidationOnDemandResponse>, Status> {
        crate::handlers::machine_validation::on_demand_machine_validation(self, request).await
    }

    async fn on_demand_rack_maintenance(
        &self,
        request: Request<rpc::RackMaintenanceOnDemandRequest>,
    ) -> Result<Response<rpc::RackMaintenanceOnDemandResponse>, Status> {
        crate::handlers::rack::on_demand_rack_maintenance(self, request).await
    }

    async fn terminate_rack_maintenance(
        &self,
        request: Request<rpc::RackMaintenanceTerminateRequest>,
    ) -> Result<Response<rpc::RackMaintenanceTerminateResponse>, Status> {
        crate::handlers::rack::terminate_rack_maintenance(self, request).await
    }

    async fn tpm_add_ca_cert(
        &self,
        request: Request<rpc::TpmCaCert>,
    ) -> Result<Response<rpc::TpmCaAddedCaStatus>, Status> {
        crate::handlers::tpm_ca::tpm_add_ca_cert(self, request).await
    }

    async fn tpm_show_ca_certs(
        &self,
        request: Request<()>,
    ) -> Result<Response<rpc::TpmCaCertDetailCollection>, Status> {
        crate::handlers::tpm_ca::tpm_show_ca_certs(self, &request).await
    }

    async fn tpm_show_unmatched_ek_certs(
        &self,
        request: Request<()>,
    ) -> Result<Response<rpc::TpmEkCertStatusCollection>, Status> {
        crate::handlers::tpm_ca::tpm_show_unmatched_ek_certs(self, &request).await
    }

    async fn tpm_delete_ca_cert(
        &self,
        request: Request<rpc::TpmCaCertId>,
    ) -> Result<Response<()>, Status> {
        crate::handlers::tpm_ca::tpm_delete_ca_cert(self, request).await
    }

    async fn remove_machine_validation_external_config(
        &self,
        request: Request<rpc::RemoveMachineValidationExternalConfigRequest>,
    ) -> Result<Response<()>, Status> {
        crate::handlers::machine_validation::remove_machine_validation_external_config(
            self, request,
        )
        .await
    }

    async fn get_machine_validation_tests(
        &self,
        request: Request<rpc::MachineValidationTestsGetRequest>,
    ) -> Result<Response<rpc::MachineValidationTestsGetResponse>, Status> {
        crate::handlers::machine_validation::get_machine_validation_tests(self, request).await
    }

    async fn update_machine_validation_test(
        &self,
        request: Request<rpc::MachineValidationTestUpdateRequest>,
    ) -> Result<Response<rpc::MachineValidationTestAddUpdateResponse>, Status> {
        crate::handlers::machine_validation::update_machine_validation_test(self, request).await
    }
    async fn add_machine_validation_test(
        &self,
        request: Request<rpc::MachineValidationTestAddRequest>,
    ) -> Result<Response<rpc::MachineValidationTestAddUpdateResponse>, Status> {
        crate::handlers::machine_validation::add_machine_validation_test(self, request).await
    }

    async fn machine_validation_test_verfied(
        &self,
        request: Request<rpc::MachineValidationTestVerfiedRequest>,
    ) -> Result<Response<rpc::MachineValidationTestVerfiedResponse>, Status> {
        crate::handlers::machine_validation::machine_validation_test_verfied(self, request).await
    }

    async fn machine_validation_test_next_version(
        &self,
        request: Request<rpc::MachineValidationTestNextVersionRequest>,
    ) -> Result<Response<rpc::MachineValidationTestNextVersionResponse>, Status> {
        crate::handlers::machine_validation::machine_validation_test_next_version(self, request)
            .await
    }

    async fn machine_validation_test_enable_disable_test(
        &self,
        request: Request<rpc::MachineValidationTestEnableDisableTestRequest>,
    ) -> Result<Response<rpc::MachineValidationTestEnableDisableTestResponse>, Status> {
        crate::handlers::machine_validation::machine_validation_test_enable_disable_test(
            self, request,
        )
        .await
    }

    async fn machine_validation_test_approve_full_host(
        &self,
        request: Request<rpc::MachineValidationTestFullHostApprovalRequest>,
    ) -> Result<Response<rpc::MachineValidationTestFullHostApprovalResponse>, Status> {
        crate::handlers::machine_validation::machine_validation_test_approve_full_host(
            self, request,
        )
        .await
    }
    async fn update_machine_validation_run(
        &self,
        request: Request<rpc::MachineValidationRunRequest>,
    ) -> Result<Response<rpc::MachineValidationRunResponse>, Status> {
        crate::handlers::machine_validation::update_machine_validation_run(self, request).await
    }

    async fn create_instance_type(
        &self,
        request: Request<rpc::CreateInstanceTypeRequest>,
    ) -> Result<Response<rpc::CreateInstanceTypeResponse>, Status> {
        crate::handlers::instance_type::create(self, request).await
    }

    async fn find_instance_type_ids(
        &self,
        request: Request<rpc::FindInstanceTypeIdsRequest>,
    ) -> Result<Response<rpc::FindInstanceTypeIdsResponse>, Status> {
        crate::handlers::instance_type::find_ids(self, request).await
    }

    async fn find_instance_types_by_ids(
        &self,
        request: Request<rpc::FindInstanceTypesByIdsRequest>,
    ) -> Result<Response<rpc::FindInstanceTypesByIdsResponse>, Status> {
        crate::handlers::instance_type::find_by_ids(self, request).await
    }

    async fn delete_instance_type(
        &self,
        request: Request<rpc::DeleteInstanceTypeRequest>,
    ) -> Result<Response<rpc::DeleteInstanceTypeResponse>, Status> {
        crate::handlers::instance_type::delete(self, request).await
    }

    async fn update_instance_type(
        &self,
        request: Request<rpc::UpdateInstanceTypeRequest>,
    ) -> Result<Response<rpc::UpdateInstanceTypeResponse>, Status> {
        crate::handlers::instance_type::update(self, request).await
    }

    async fn associate_machines_with_instance_type(
        &self,
        request: Request<rpc::AssociateMachinesWithInstanceTypeRequest>,
    ) -> Result<Response<rpc::AssociateMachinesWithInstanceTypeResponse>, Status> {
        crate::handlers::instance_type::associate_machines(self, request).await
    }

    async fn remove_machine_instance_type_association(
        &self,
        request: Request<rpc::RemoveMachineInstanceTypeAssociationRequest>,
    ) -> Result<Response<rpc::RemoveMachineInstanceTypeAssociationResponse>, Status> {
        crate::handlers::instance_type::remove_machine_association(self, request).await
    }

    async fn redfish_browse(
        &self,
        request: Request<rpc::RedfishBrowseRequest>,
    ) -> Result<Response<rpc::RedfishBrowseResponse>, Status> {
        crate::handlers::redfish::redfish_browse(self, request).await
    }

    async fn redfish_list_actions(
        &self,
        request: Request<rpc::RedfishListActionsRequest>,
    ) -> Result<Response<rpc::RedfishListActionsResponse>, Status> {
        crate::handlers::redfish::redfish_list_actions(self, request).await
    }

    async fn redfish_create_action(
        &self,
        request: Request<rpc::RedfishCreateActionRequest>,
    ) -> Result<Response<rpc::RedfishCreateActionResponse>, Status> {
        crate::handlers::redfish::redfish_create_action(self, request).await
    }

    async fn redfish_approve_action(
        &self,
        request: Request<rpc::RedfishActionId>,
    ) -> Result<Response<rpc::RedfishApproveActionResponse>, Status> {
        crate::handlers::redfish::redfish_approve_action(self, request).await
    }
    async fn redfish_apply_action(
        &self,
        request: Request<rpc::RedfishActionId>,
    ) -> Result<Response<rpc::RedfishApplyActionResponse>, Status> {
        crate::handlers::redfish::redfish_apply_action(self, request).await
    }

    async fn redfish_cancel_action(
        &self,
        request: Request<rpc::RedfishActionId>,
    ) -> Result<Response<rpc::RedfishCancelActionResponse>, Status> {
        crate::handlers::redfish::redfish_cancel_action(self, request).await
    }

    async fn ufm_browse(
        &self,
        request: Request<rpc::UfmBrowseRequest>,
    ) -> Result<Response<rpc::UfmBrowseResponse>, Status> {
        crate::handlers::ib_fabric::ufm_browse(self, request).await
    }

    async fn create_network_security_group(
        &self,
        request: Request<rpc::CreateNetworkSecurityGroupRequest>,
    ) -> Result<Response<rpc::CreateNetworkSecurityGroupResponse>, Status> {
        crate::handlers::network_security_group::create(self, request).await
    }

    async fn find_network_security_group_ids(
        &self,
        request: Request<rpc::FindNetworkSecurityGroupIdsRequest>,
    ) -> Result<Response<rpc::FindNetworkSecurityGroupIdsResponse>, Status> {
        crate::handlers::network_security_group::find_ids(self, request).await
    }

    async fn find_network_security_groups_by_ids(
        &self,
        request: Request<rpc::FindNetworkSecurityGroupsByIdsRequest>,
    ) -> Result<Response<rpc::FindNetworkSecurityGroupsByIdsResponse>, Status> {
        crate::handlers::network_security_group::find_by_ids(self, request).await
    }

    async fn delete_network_security_group(
        &self,
        request: Request<rpc::DeleteNetworkSecurityGroupRequest>,
    ) -> Result<Response<rpc::DeleteNetworkSecurityGroupResponse>, Status> {
        crate::handlers::network_security_group::delete(self, request).await
    }

    async fn update_network_security_group(
        &self,
        request: Request<rpc::UpdateNetworkSecurityGroupRequest>,
    ) -> Result<Response<rpc::UpdateNetworkSecurityGroupResponse>, Status> {
        crate::handlers::network_security_group::update(self, request).await
    }

    async fn get_network_security_group_propagation_status(
        &self,
        request: Request<rpc::GetNetworkSecurityGroupPropagationStatusRequest>,
    ) -> Result<Response<rpc::GetNetworkSecurityGroupPropagationStatusResponse>, Status> {
        crate::handlers::network_security_group::get_propagation_status(self, request).await
    }

    async fn get_network_security_group_attachments(
        &self,
        request: Request<rpc::GetNetworkSecurityGroupAttachmentsRequest>,
    ) -> Result<Response<rpc::GetNetworkSecurityGroupAttachmentsResponse>, Status> {
        crate::handlers::network_security_group::get_attachments(self, request).await
    }
    async fn create_compute_allocation(
        &self,
        request: tonic::Request<rpc::CreateComputeAllocationRequest>,
    ) -> Result<tonic::Response<rpc::CreateComputeAllocationResponse>, Status> {
        crate::handlers::compute_allocation::create(self, request).await
    }
    async fn find_compute_allocation_ids(
        &self,
        request: tonic::Request<rpc::FindComputeAllocationIdsRequest>,
    ) -> Result<tonic::Response<rpc::FindComputeAllocationIdsResponse>, Status> {
        crate::handlers::compute_allocation::find_ids(self, request).await
    }
    async fn find_compute_allocations_by_ids(
        &self,
        request: tonic::Request<rpc::FindComputeAllocationsByIdsRequest>,
    ) -> Result<tonic::Response<rpc::FindComputeAllocationsByIdsResponse>, Status> {
        crate::handlers::compute_allocation::find_by_ids(self, request).await
    }
    async fn delete_compute_allocation(
        &self,
        request: tonic::Request<rpc::DeleteComputeAllocationRequest>,
    ) -> Result<tonic::Response<rpc::DeleteComputeAllocationResponse>, Status> {
        crate::handlers::compute_allocation::delete(self, request).await
    }
    async fn update_compute_allocation(
        &self,
        request: tonic::Request<rpc::UpdateComputeAllocationRequest>,
    ) -> Result<tonic::Response<rpc::UpdateComputeAllocationResponse>, Status> {
        crate::handlers::compute_allocation::update(self, request).await
    }
    async fn get_desired_firmware_versions(
        &self,
        request: Request<rpc::GetDesiredFirmwareVersionsRequest>,
    ) -> Result<Response<rpc::GetDesiredFirmwareVersionsResponse>, Status> {
        crate::handlers::firmware::get_desired_firmware_versions(self, request).await
    }

    async fn upsert_host_firmware_config(
        &self,
        request: Request<rpc::UpsertHostFirmwareConfigRequest>,
    ) -> Result<Response<rpc::HostFirmwareConfigResponse>, Status> {
        crate::handlers::firmware::upsert_host_firmware_config(self, request).await
    }

    async fn delete_host_firmware_config(
        &self,
        request: Request<rpc::DeleteHostFirmwareConfigRequest>,
    ) -> Result<Response<()>, Status> {
        crate::handlers::firmware::delete_host_firmware_config(self, request).await
    }

    async fn create_sku(
        &self,
        request: Request<rpc::SkuList>,
    ) -> Result<Response<rpc::SkuIdList>, Status> {
        crate::handlers::sku::create(self, request).await
    }

    async fn delete_sku(&self, request: Request<SkuIdList>) -> Result<Response<()>, Status> {
        crate::handlers::sku::delete(self, request).await
    }

    async fn generate_sku_from_machine(
        &self,
        request: Request<MachineId>,
    ) -> Result<Response<rpc::Sku>, Status> {
        crate::handlers::sku::generate_from_machine(self, request).await
    }

    async fn verify_sku_for_machine(
        &self,
        request: Request<MachineId>,
    ) -> Result<Response<()>, Status> {
        crate::handlers::sku::verify_for_machine(self, request).await
    }

    async fn assign_sku_to_machine(
        &self,
        request: Request<::rpc::forge::SkuMachinePair>,
    ) -> Result<Response<()>, Status> {
        crate::handlers::sku::assign_to_machine(self, request).await
    }

    async fn remove_sku_association(
        &self,
        request: Request<RemoveSkuRequest>,
    ) -> Result<Response<()>, Status> {
        crate::handlers::sku::remove_sku_association(self, request).await
    }

    async fn get_all_sku_ids(
        &self,
        request: Request<()>,
    ) -> Result<Response<rpc::SkuIdList>, Status> {
        crate::handlers::sku::get_all_ids(self, request).await
    }

    async fn find_skus_by_ids(
        &self,
        request: Request<rpc::SkusByIdsRequest>,
    ) -> Result<Response<rpc::SkuList>, Status> {
        crate::handlers::sku::find_skus_by_ids(self, request).await
    }

    async fn update_sku_metadata(
        &self,
        request: Request<rpc::SkuUpdateMetadataRequest>,
    ) -> Result<Response<()>, Status> {
        crate::handlers::sku::update_sku_metadata(self, request).await
    }

    async fn replace_sku(&self, request: Request<rpc::Sku>) -> Result<Response<rpc::Sku>, Status> {
        crate::handlers::sku::replace_sku(self, request).await
    }

    async fn set_managed_host_quarantine_state(
        &self,
        request: Request<rpc::SetManagedHostQuarantineStateRequest>,
    ) -> Result<Response<rpc::SetManagedHostQuarantineStateResponse>, Status> {
        crate::handlers::machine_quarantine::set_managed_host_quarantine_state(self, request).await
    }

    async fn get_managed_host_quarantine_state(
        &self,
        request: Request<rpc::GetManagedHostQuarantineStateRequest>,
    ) -> Result<Response<rpc::GetManagedHostQuarantineStateResponse>, Status> {
        crate::handlers::machine_quarantine::get_managed_host_quarantine_state(self, request).await
    }

    async fn clear_managed_host_quarantine_state(
        &self,
        request: Request<rpc::ClearManagedHostQuarantineStateRequest>,
    ) -> Result<Response<rpc::ClearManagedHostQuarantineStateResponse>, Status> {
        crate::handlers::machine_quarantine::clear_managed_host_quarantine_state(self, request)
            .await
    }

    async fn reset_host_reprovisioning(
        &self,
        request: Request<MachineId>,
    ) -> Result<Response<()>, Status> {
        crate::handlers::host_reprovisioning::reset_host_reprovisioning(self, request).await
    }

    async fn copy_bfb_to_dpu_rshim(
        &self,
        request: Request<rpc::CopyBfbToDpuRshimRequest>,
    ) -> Result<Response<()>, Status> {
        crate::handlers::bmc_endpoint_explorer::copy_bfb_to_dpu_rshim(self, request).await
    }

    async fn find_nv_link_partition_ids(
        &self,
        request: Request<rpc::NvLinkPartitionSearchFilter>,
    ) -> Result<Response<rpc::NvLinkPartitionIdList>, Status> {
        crate::handlers::nvl_partition::find_ids(self, request).await
    }

    async fn find_nv_link_partitions_by_ids(
        &self,
        request: Request<rpc::NvLinkPartitionsByIdsRequest>,
    ) -> Result<Response<rpc::NvLinkPartitionList>, Status> {
        crate::handlers::nvl_partition::find_by_ids(self, request).await
    }

    async fn nv_link_partitions_for_tenant(
        &self,
        request: Request<rpc::TenantSearchQuery>,
    ) -> Result<Response<rpc::NvLinkPartitionList>, Status> {
        crate::handlers::nvl_partition::for_tenant(self, request).await
    }

    async fn find_nv_link_logical_partition_ids(
        &self,
        request: Request<rpc::NvLinkLogicalPartitionSearchFilter>,
    ) -> Result<Response<rpc::NvLinkLogicalPartitionIdList>, Status> {
        crate::handlers::logical_partition::find_ids(self, request).await
    }

    async fn find_nv_link_logical_partitions_by_ids(
        &self,
        request: Request<rpc::NvLinkLogicalPartitionsByIdsRequest>,
    ) -> Result<Response<rpc::NvLinkLogicalPartitionList>, Status> {
        crate::handlers::logical_partition::find_by_ids(self, request).await
    }

    async fn create_nv_link_logical_partition(
        &self,
        request: Request<rpc::NvLinkLogicalPartitionCreationRequest>,
    ) -> Result<Response<rpc::NvLinkLogicalPartition>, Status> {
        crate::handlers::logical_partition::create(self, request).await
    }

    async fn delete_nv_link_logical_partition(
        &self,
        request: Request<rpc::NvLinkLogicalPartitionDeletionRequest>,
    ) -> Result<Response<rpc::NvLinkLogicalPartitionDeletionResult>, Status> {
        crate::handlers::logical_partition::delete(self, request).await
    }

    async fn nv_link_logical_partitions_for_tenant(
        &self,
        request: Request<rpc::TenantSearchQuery>,
    ) -> Result<Response<rpc::NvLinkLogicalPartitionList>, Status> {
        crate::handlers::logical_partition::for_tenant(self, request).await
    }

    async fn update_nv_link_logical_partition(
        &self,
        request: Request<rpc::NvLinkLogicalPartitionUpdateRequest>,
    ) -> Result<Response<rpc::NvLinkLogicalPartitionUpdateResult>, Status> {
        crate::handlers::logical_partition::update(self, request).await
    }

    async fn nmxc_browse(
        &self,
        request: Request<rpc::NmxcBrowseRequest>,
    ) -> Result<Response<rpc::NmxcBrowseResponse>, Status> {
        crate::handlers::nmxc_browse::nmxc_browse(self, request).await
    }

    // Return a Vector of all the DPA interface IDs
    async fn get_all_dpa_interface_ids(
        &self,
        request: Request<()>,
    ) -> Result<Response<rpc::DpaInterfaceIdList>, Status> {
        crate::handlers::dpa::get_all_ids(self, request).await
    }

    // Given a Vector of DPA Interface IDs, return the corresponding
    // DPA Interfaces in a Vector
    async fn find_dpa_interfaces_by_ids(
        &self,
        request: Request<rpc::DpaInterfacesByIdsRequest>,
    ) -> Result<Response<rpc::DpaInterfaceList>, Status> {
        crate::handlers::dpa::find_dpa_interfaces_by_ids(self, request).await
    }

    async fn ensure_dpa_interface(
        &self,
        request: Request<rpc::DpaInterfaceCreationRequest>,
    ) -> Result<Response<rpc::DpaInterface>, Status> {
        crate::handlers::dpa::ensure(self, request).await
    }

    // create_dpa_interface is mainly for debugging purposes. In practice,
    // when the scout reports its inventory, we will create DPA interfaces
    // for DPA NICs reported in the inventory.
    async fn create_dpa_interface(
        &self,
        request: Request<rpc::DpaInterfaceCreationRequest>,
    ) -> Result<Response<rpc::DpaInterface>, Status> {
        crate::handlers::dpa::create(self, request).await
    }

    // delete_dpa_interface is mainly for debugging purposes.
    async fn delete_dpa_interface(
        &self,
        request: Request<rpc::DpaInterfaceDeletionRequest>,
    ) -> Result<Response<rpc::DpaInterfaceDeletionResult>, Status> {
        crate::handlers::dpa::delete(self, request).await
    }

    async fn create_bmc_user(
        &self,
        request: Request<rpc::CreateBmcUserRequest>,
    ) -> Result<Response<rpc::CreateBmcUserResponse>, Status> {
        crate::handlers::bmc_endpoint_explorer::create_bmc_user(self, request).await
    }

    async fn set_bmc_root_password(
        &self,
        request: Request<rpc::SetBmcRootPasswordRequest>,
    ) -> Result<Response<rpc::SetBmcRootPasswordResponse>, Status> {
        crate::handlers::bmc_endpoint_explorer::set_bmc_root_password(self, request).await
    }

    async fn probe_bmc_vendor(
        &self,
        request: Request<rpc::ProbeBmcVendorRequest>,
    ) -> Result<Response<rpc::ProbeBmcVendorResponse>, Status> {
        crate::handlers::bmc_endpoint_explorer::probe_bmc_vendor(self, request).await
    }

    async fn delete_bmc_user(
        &self,
        request: Request<rpc::DeleteBmcUserRequest>,
    ) -> Result<Response<rpc::DeleteBmcUserResponse>, Status> {
        crate::handlers::bmc_endpoint_explorer::delete_bmc_user(self, request).await
    }

    async fn set_firmware_update_time_window(
        &self,
        request: Request<rpc::SetFirmwareUpdateTimeWindowRequest>,
    ) -> Result<Response<rpc::SetFirmwareUpdateTimeWindowResponse>, Status> {
        crate::handlers::firmware::set_firmware_update_time_window(self, request).await
    }

    async fn list_host_firmware(
        &self,
        request: Request<rpc::ListHostFirmwareRequest>,
    ) -> Result<Response<rpc::ListHostFirmwareResponse>, Status> {
        crate::handlers::firmware::list_host_firmware(self, request).await
    }

    // Scout is telling Carbide the mlx device configuration in its machine
    async fn publish_mlx_device_report(
        &self,
        request: Request<mlx_device_pb::PublishMlxDeviceReportRequest>,
    ) -> Result<Response<mlx_device_pb::PublishMlxDeviceReportResponse>, Status> {
        crate::handlers::svpc::publish_mlx_device_report(self, request).await
    }

    // Scout is telling carbide the observed status (locking status, card mode) of the
    // mlx devices in its host
    async fn publish_mlx_observation_report(
        &self,
        request: Request<mlx_device_pb::PublishMlxObservationReportRequest>,
    ) -> Result<Response<mlx_device_pb::PublishMlxObservationReportResponse>, Status> {
        crate::handlers::svpc::publish_mlx_observation_report(self, request).await
    }

    async fn trim_table(
        &self,
        request: Request<rpc::TrimTableRequest>,
    ) -> Result<Response<rpc::TrimTableResponse>, Status> {
        crate::handlers::db::trim_table(self, request).await
    }

    async fn list_nvlink_nmxc_endpoints(
        &self,
        request: Request<()>,
    ) -> Result<Response<rpc::NvlinkNmxcEndpointList>, Status> {
        crate::handlers::nvlink_nmxc_endpoints::list_nvlink_nmxc_endpoints(self, request).await
    }

    async fn create_nvlink_nmxc_endpoint(
        &self,
        request: Request<rpc::NvlinkNmxcEndpoint>,
    ) -> Result<Response<rpc::NvlinkNmxcEndpoint>, Status> {
        crate::handlers::nvlink_nmxc_endpoints::create_nvlink_nmxc_endpoint(self, request).await
    }

    async fn update_nvlink_nmxc_endpoint(
        &self,
        request: Request<rpc::NvlinkNmxcEndpoint>,
    ) -> Result<Response<rpc::NvlinkNmxcEndpoint>, Status> {
        crate::handlers::nvlink_nmxc_endpoints::update_nvlink_nmxc_endpoint(self, request).await
    }

    async fn delete_nvlink_nmxc_endpoint(
        &self,
        request: Request<rpc::DeleteNvlinkNmxcEndpointRequest>,
    ) -> Result<Response<()>, Status> {
        crate::handlers::nvlink_nmxc_endpoints::delete_nvlink_nmxc_endpoint(self, request).await
    }

    async fn create_remediation(
        &self,
        request: Request<rpc::CreateRemediationRequest>,
    ) -> Result<Response<rpc::CreateRemediationResponse>, Status> {
        crate::handlers::dpu_remediation::create(self, request).await
    }

    async fn approve_remediation(
        &self,
        request: Request<rpc::ApproveRemediationRequest>,
    ) -> Result<Response<()>, Status> {
        crate::handlers::dpu_remediation::approve(self, request).await
    }

    async fn revoke_remediation(
        &self,
        request: Request<rpc::RevokeRemediationRequest>,
    ) -> Result<Response<()>, Status> {
        crate::handlers::dpu_remediation::revoke(self, request).await
    }

    async fn enable_remediation(
        &self,
        request: Request<rpc::EnableRemediationRequest>,
    ) -> Result<Response<()>, Status> {
        crate::handlers::dpu_remediation::enable(self, request).await
    }

    async fn disable_remediation(
        &self,
        request: Request<rpc::DisableRemediationRequest>,
    ) -> Result<Response<()>, Status> {
        crate::handlers::dpu_remediation::disable(self, request).await
    }

    async fn find_remediation_ids(
        &self,
        request: Request<()>,
    ) -> Result<Response<rpc::RemediationIdList>, Status> {
        crate::handlers::dpu_remediation::find_remediation_ids(self, request).await
    }

    async fn find_remediations_by_ids(
        &self,
        request: Request<rpc::RemediationIdList>,
    ) -> Result<Response<rpc::RemediationList>, Status> {
        crate::handlers::dpu_remediation::find_remediations_by_ids(self, request).await
    }

    async fn find_applied_remediation_ids(
        &self,
        request: Request<rpc::FindAppliedRemediationIdsRequest>,
    ) -> Result<Response<rpc::AppliedRemediationIdList>, Status> {
        crate::handlers::dpu_remediation::find_applied_remediation_ids(self, request).await
    }

    async fn find_applied_remediations(
        &self,
        request: Request<rpc::FindAppliedRemediationsRequest>,
    ) -> Result<Response<rpc::AppliedRemediationList>, Status> {
        crate::handlers::dpu_remediation::find_applied_remediations(self, request).await
    }

    async fn get_next_remediation_for_machine(
        &self,
        request: Request<rpc::GetNextRemediationForMachineRequest>,
    ) -> Result<Response<rpc::GetNextRemediationForMachineResponse>, Status> {
        crate::handlers::dpu_remediation::get_next_remediation_for_machine(self, request).await
    }

    async fn remediation_applied(
        &self,
        request: Request<rpc::RemediationAppliedRequest>,
    ) -> Result<Response<()>, Status> {
        crate::handlers::dpu_remediation::remediation_applied(self, request).await
    }

    async fn set_primary_dpu(
        &self,
        request: Request<rpc::SetPrimaryDpuRequest>,
    ) -> Result<Response<()>, Status> {
        crate::handlers::managed_host::set_primary_dpu(self, request).await
    }

    async fn set_primary_interface(
        &self,
        request: Request<rpc::SetPrimaryInterfaceRequest>,
    ) -> Result<Response<()>, Status> {
        crate::handlers::managed_host::set_primary_interface(self, request).await
    }

    async fn create_dpu_extension_service(
        &self,
        request: Request<rpc::CreateDpuExtensionServiceRequest>,
    ) -> Result<Response<rpc::DpuExtensionService>, Status> {
        crate::handlers::extension_service::create(self, request).await
    }

    async fn update_dpu_extension_service(
        &self,
        request: Request<rpc::UpdateDpuExtensionServiceRequest>,
    ) -> Result<Response<rpc::DpuExtensionService>, Status> {
        crate::handlers::extension_service::update(self, request).await
    }

    async fn delete_dpu_extension_service(
        &self,
        request: Request<rpc::DeleteDpuExtensionServiceRequest>,
    ) -> Result<Response<rpc::DeleteDpuExtensionServiceResponse>, Status> {
        crate::handlers::extension_service::delete(self, request).await
    }

    async fn find_dpu_extension_service_ids(
        &self,
        request: Request<rpc::DpuExtensionServiceSearchFilter>,
    ) -> Result<Response<rpc::DpuExtensionServiceIdList>, Status> {
        crate::handlers::extension_service::find_ids(self, request).await
    }

    async fn find_dpu_extension_services_by_ids(
        &self,
        request: Request<rpc::DpuExtensionServicesByIdsRequest>,
    ) -> Result<Response<rpc::DpuExtensionServiceList>, Status> {
        crate::handlers::extension_service::find_by_ids(self, request).await
    }

    async fn get_dpu_extension_service_versions_info(
        &self,
        request: Request<rpc::GetDpuExtensionServiceVersionsInfoRequest>,
    ) -> Result<Response<rpc::DpuExtensionServiceVersionInfoList>, Status> {
        crate::handlers::extension_service::get_versions_info(self, request).await
    }

    async fn find_instances_by_dpu_extension_service(
        &self,
        request: Request<rpc::FindInstancesByDpuExtensionServiceRequest>,
    ) -> Result<Response<rpc::FindInstancesByDpuExtensionServiceResponse>, Status> {
        crate::handlers::extension_service::find_instances_by_extension_service(self, request).await
    }

    async fn trigger_machine_attestation(
        &self,
        request: tonic::Request<rpc::SpdmMachineAttestationTriggerRequest>,
    ) -> Result<tonic::Response<rpc::SpdmMachineAttestationTriggerResponse>, Status> {
        crate::handlers::attestation::trigger_machine_attestation(self, request).await
    }

    async fn cancel_machine_attestation(
        &self,
        request: tonic::Request<MachineId>,
    ) -> Result<tonic::Response<()>, Status> {
        crate::handlers::attestation::cancel_machine_attestation(self, request).await
    }

    async fn list_attestation_machines(
        &self,
        request: tonic::Request<rpc::SpdmListAttestationMachinesRequest>,
    ) -> Result<Response<rpc::SpdmListAttestationMachinesResponse>, Status> {
        crate::handlers::attestation::list_attestation_machines(self, request).await
    }

    async fn get_attestation_machine(
        &self,
        request: tonic::Request<MachineId>,
    ) -> Result<Response<rpc::SpdmGetAttestationMachineResponse>, Status> {
        crate::handlers::attestation::get_attestation_machine(self, request).await
    }

    async fn sign_machine_identity(
        &self,
        request: tonic::Request<rpc::MachineIdentityRequest>,
    ) -> Result<Response<rpc::MachineIdentityResponse>, Status> {
        crate::handlers::machine_identity::sign_machine_identity(self, request).await
    }

    async fn get_tenant_identity_configuration(
        &self,
        request: Request<rpc::GetTenantIdentityConfigRequest>,
    ) -> Result<Response<rpc::TenantIdentityConfigResponse>, Status> {
        crate::handlers::tenant_identity_config::get_configuration(self, request).await
    }

    async fn set_tenant_identity_configuration(
        &self,
        request: tonic::Request<rpc::SetTenantIdentityConfigRequest>,
    ) -> Result<Response<rpc::TenantIdentityConfigResponse>, Status> {
        crate::handlers::tenant_identity_config::set_configuration(self, request).await
    }

    async fn delete_tenant_identity_configuration(
        &self,
        request: Request<rpc::GetTenantIdentityConfigRequest>,
    ) -> Result<Response<()>, Status> {
        crate::handlers::tenant_identity_config::delete_configuration(self, request).await
    }

    async fn get_token_delegation(
        &self,
        request: Request<rpc::GetTokenDelegationRequest>,
    ) -> Result<Response<rpc::TokenDelegationResponse>, Status> {
        crate::handlers::tenant_identity_config::get_token_delegation(self, request).await
    }

    async fn set_token_delegation(
        &self,
        request: Request<rpc::TokenDelegationRequest>,
    ) -> Result<Response<rpc::TokenDelegationResponse>, Status> {
        crate::handlers::tenant_identity_config::set_token_delegation(self, request).await
    }

    async fn delete_token_delegation(
        &self,
        request: Request<rpc::GetTokenDelegationRequest>,
    ) -> Result<Response<()>, Status> {
        crate::handlers::tenant_identity_config::delete_token_delegation(self, request).await
    }

    async fn reencrypt_tenant_identity_secrets(
        &self,
        request: Request<rpc::ReencryptTenantIdentitySecretsRequest>,
    ) -> Result<Response<rpc::ReencryptTenantIdentitySecretsResponse>, Status> {
        crate::handlers::tenant_identity_config::reencrypt_tenant_identity_secrets(self, request)
            .await
    }

    async fn get_jwks(
        &self,
        request: Request<rpc::JwksRequest>,
    ) -> Result<Response<rpc::Jwks>, Status> {
        crate::handlers::machine_identity::get_jwks(self, request).await
    }

    async fn get_open_id_configuration(
        &self,
        request: Request<rpc::OpenIdConfigRequest>,
    ) -> Result<Response<rpc::OpenIdConfiguration>, Status> {
        crate::handlers::machine_identity::get_open_id_configuration(self, request).await
    }

    async fn modify_dpf_state(
        &self,
        request: Request<rpc::ModifyDpfStateRequest>,
    ) -> Result<Response<()>, Status> {
        crate::handlers::dpf::modify_dpf_state(self, request).await
    }

    async fn get_dpf_state(
        &self,
        request: Request<rpc::GetDpfStateRequest>,
    ) -> Result<Response<rpc::DpfStateResponse>, Status> {
        crate::handlers::dpf::get_dpf_state(self, request).await
    }

    async fn get_dpf_host_snapshot(
        &self,
        request: Request<rpc::GetDpfHostSnapshotRequest>,
    ) -> Result<Response<rpc::DpfHostSnapshotResponse>, Status> {
        crate::handlers::dpf::get_dpf_host_snapshot(self, request).await
    }

    async fn get_dpf_service_versions(
        &self,
        request: Request<rpc::GetDpfServiceVersionsRequest>,
    ) -> Result<Response<rpc::DpfServiceVersionsResponse>, Status> {
        crate::handlers::dpf::get_dpf_service_versions(self, request).await
    }

    async fn find_pending_dpu_service_sync_ids(
        &self,
        request: Request<rpc::FindPendingDpuServiceSyncIdsRequest>,
    ) -> Result<Response<::rpc::common::MachineIdList>, Status> {
        crate::handlers::dpu_service_sync::find_pending_dpu_service_sync_ids(self, request).await
    }

    async fn find_pending_dpu_service_syncs_by_ids(
        &self,
        request: Request<rpc::FindPendingDpuServiceSyncsByIdsRequest>,
    ) -> Result<Response<rpc::ListPendingDpuServiceSyncsResponse>, Status> {
        crate::handlers::dpu_service_sync::find_pending_dpu_service_syncs_by_ids(self, request)
            .await
    }

    async fn list_dpu_service_sync_history(
        &self,
        request: Request<rpc::ListDpuServiceSyncHistoryRequest>,
    ) -> Result<Response<rpc::ListPendingDpuServiceSyncsResponse>, Status> {
        crate::handlers::dpu_service_sync::list_dpu_service_sync_history(self, request).await
    }

    async fn release_dpu_service_sync_hold(
        &self,
        request: Request<rpc::ReleaseDpuServiceSyncHoldRequest>,
    ) -> Result<Response<rpc::ReleaseDpuServiceSyncHoldResponse>, Status> {
        crate::handlers::dpu_service_sync::release_dpu_service_sync_hold(self, request).await
    }

    // scout_stream handles the bidirectional streaming connection from scout agents.
    // scout agents call scout_stream and send an Init message, and then carbide-api
    // will send down "request" messages to connected agent(s) to either instruct them
    // or ask them for information (sometimes for state changes, other times for
    // feeding data back to administrative CLI/UI calls).
    async fn scout_stream(
        &self,
        request: Request<Streaming<rpc::ScoutStreamApiBoundMessage>>,
    ) -> Result<Response<Self::ScoutStreamStream>, Status> {
        crate::handlers::scout_stream::scout_stream(self, request).await
    }

    // scout_stream_show_connections lists all active scout agent
    // connections by building up some ScoutStreamConnectionInfo
    // messages using the data from the scout_stream_registry.
    async fn scout_stream_show_connections(
        &self,
        request: Request<rpc::ScoutStreamShowConnectionsRequest>,
    ) -> Result<Response<rpc::ScoutStreamShowConnectionsResponse>, Status> {
        crate::handlers::scout_stream::show_connections(self, request).await
    }

    // scout_stream_disconnect is used to disconnect the
    // given MachineId's ScoutStream connection.
    async fn scout_stream_disconnect(
        &self,
        request: Request<rpc::ScoutStreamDisconnectRequest>,
    ) -> Result<Response<rpc::ScoutStreamDisconnectResponse>, Status> {
        crate::handlers::scout_stream::disconnect(self, request).await
    }

    // scout_stream_ping is used to ping the
    // given MachineId's ScoutStream connection.
    async fn scout_stream_ping(
        &self,
        request: Request<rpc::ScoutStreamAdminPingRequest>,
    ) -> Result<Response<rpc::ScoutStreamAdminPingResponse>, Status> {
        crate::handlers::scout_stream::ping(self, request).await
    }

    async fn mlx_admin_profile_sync(
        &self,
        request: Request<mlx_device_pb::MlxAdminProfileSyncRequest>,
    ) -> Result<Response<mlx_device_pb::MlxAdminProfileSyncResponse>, Status> {
        crate::handlers::mlx_admin::profile_sync(self, request).await
    }

    async fn mlx_admin_profile_show(
        &self,
        request: Request<mlx_device_pb::MlxAdminProfileShowRequest>,
    ) -> Result<Response<mlx_device_pb::MlxAdminProfileShowResponse>, Status> {
        crate::handlers::mlx_admin::profile_show(self, request)
    }

    async fn mlx_admin_profile_compare(
        &self,
        request: Request<mlx_device_pb::MlxAdminProfileCompareRequest>,
    ) -> Result<Response<mlx_device_pb::MlxAdminProfileCompareResponse>, Status> {
        crate::handlers::mlx_admin::profile_compare(self, request).await
    }

    async fn mlx_admin_profile_list(
        &self,
        request: Request<mlx_device_pb::MlxAdminProfileListRequest>,
    ) -> Result<Response<mlx_device_pb::MlxAdminProfileListResponse>, Status> {
        crate::handlers::mlx_admin::profile_list(self, request)
    }

    async fn mlx_admin_lockdown_lock(
        &self,
        request: Request<mlx_device_pb::MlxAdminLockdownLockRequest>,
    ) -> Result<Response<mlx_device_pb::MlxAdminLockdownLockResponse>, Status> {
        crate::handlers::mlx_admin::lockdown_lock(self, request).await
    }

    async fn mlx_admin_lockdown_unlock(
        &self,
        request: Request<mlx_device_pb::MlxAdminLockdownUnlockRequest>,
    ) -> Result<Response<mlx_device_pb::MlxAdminLockdownUnlockResponse>, Status> {
        crate::handlers::mlx_admin::lockdown_unlock(self, request).await
    }

    async fn mlx_admin_lockdown_status(
        &self,
        request: Request<mlx_device_pb::MlxAdminLockdownStatusRequest>,
    ) -> Result<Response<mlx_device_pb::MlxAdminLockdownStatusResponse>, Status> {
        crate::handlers::mlx_admin::lockdown_status(self, request).await
    }

    async fn mlx_admin_show_device(
        &self,
        request: Request<mlx_device_pb::MlxAdminDeviceInfoRequest>,
    ) -> Result<Response<mlx_device_pb::MlxAdminDeviceInfoResponse>, Status> {
        crate::handlers::mlx_admin::show_device_info(self, request).await
    }

    async fn mlx_admin_show_machine(
        &self,
        request: Request<mlx_device_pb::MlxAdminDeviceReportRequest>,
    ) -> Result<Response<mlx_device_pb::MlxAdminDeviceReportResponse>, Status> {
        crate::handlers::mlx_admin::show_device_report(self, request).await
    }

    async fn mlx_admin_registry_list(
        &self,
        request: Request<mlx_device_pb::MlxAdminRegistryListRequest>,
    ) -> Result<Response<mlx_device_pb::MlxAdminRegistryListResponse>, Status> {
        crate::handlers::mlx_admin::registry_list(self, request).await
    }

    async fn mlx_admin_registry_show(
        &self,
        request: Request<mlx_device_pb::MlxAdminRegistryShowRequest>,
    ) -> Result<Response<mlx_device_pb::MlxAdminRegistryShowResponse>, Status> {
        crate::handlers::mlx_admin::registry_show(self, request).await
    }

    async fn mlx_admin_config_query(
        &self,
        request: Request<mlx_device_pb::MlxAdminConfigQueryRequest>,
    ) -> Result<Response<mlx_device_pb::MlxAdminConfigQueryResponse>, Status> {
        crate::handlers::mlx_admin::config_query(self, request).await
    }

    async fn mlx_admin_config_set(
        &self,
        request: Request<mlx_device_pb::MlxAdminConfigSetRequest>,
    ) -> Result<Response<mlx_device_pb::MlxAdminConfigSetResponse>, Status> {
        crate::handlers::mlx_admin::config_set(self, request).await
    }

    async fn mlx_admin_config_sync(
        &self,
        request: Request<mlx_device_pb::MlxAdminConfigSyncRequest>,
    ) -> Result<Response<mlx_device_pb::MlxAdminConfigSyncResponse>, Status> {
        crate::handlers::mlx_admin::config_sync(self, request).await
    }

    async fn mlx_admin_config_compare(
        &self,
        request: Request<mlx_device_pb::MlxAdminConfigCompareRequest>,
    ) -> Result<Response<mlx_device_pb::MlxAdminConfigCompareResponse>, Status> {
        crate::handlers::mlx_admin::config_compare(self, request).await
    }

    async fn get_machine_position_info(
        &self,
        request: Request<rpc::MachinePositionQuery>,
    ) -> Result<Response<rpc::MachinePositionInfoList>, Status> {
        crate::handlers::machine::get_machine_position_info(self, request).await
    }

    async fn determine_machine_ingestion_state(
        &self,
        request: tonic::Request<::rpc::forge::BmcEndpointRequest>,
    ) -> Result<tonic::Response<::rpc::forge::MachineIngestionStateResponse>, Status> {
        crate::api::log_request_data(&request);

        crate::handlers::power_options::determine_machine_ingestion_state(
            self,
            &request.into_inner(),
        )
        .await
    }

    async fn allow_ingestion_and_power_on(
        &self,
        request: tonic::Request<::rpc::forge::BmcEndpointRequest>,
    ) -> Result<tonic::Response<()>, Status> {
        crate::api::log_request_data(&request);

        crate::handlers::power_options::allow_ingestion_and_power_on(self, &request.into_inner())
            .await
    }

    async fn component_power_control(
        &self,
        request: Request<rpc::ComponentPowerControlRequest>,
    ) -> Result<Response<rpc::ComponentPowerControlResponse>, Status> {
        crate::handlers::component_manager::component_power_control(self, request).await
    }

    async fn component_configure_switch_certificate(
        &self,
        request: Request<rpc::ComponentConfigureSwitchCertificateRequest>,
    ) -> Result<Response<rpc::ComponentConfigureSwitchCertificateResponse>, Status> {
        crate::handlers::component_manager::component_configure_switch_certificate(self, request)
            .await
    }

    async fn get_component_inventory(
        &self,
        request: Request<rpc::GetComponentInventoryRequest>,
    ) -> Result<Response<rpc::GetComponentInventoryResponse>, Status> {
        crate::handlers::component_manager::get_component_inventory(self, request).await
    }

    async fn update_component_firmware(
        &self,
        request: Request<rpc::UpdateComponentFirmwareRequest>,
    ) -> Result<Response<rpc::UpdateComponentFirmwareResponse>, Status> {
        crate::handlers::component_manager::update_component_firmware(self, request).await
    }

    async fn get_component_firmware_status(
        &self,
        request: Request<rpc::GetComponentFirmwareStatusRequest>,
    ) -> Result<Response<rpc::GetComponentFirmwareStatusResponse>, Status> {
        crate::handlers::component_manager::get_component_firmware_status(self, request).await
    }

    async fn list_component_firmware_versions(
        &self,
        request: Request<rpc::ListComponentFirmwareVersionsRequest>,
    ) -> Result<Response<rpc::ListComponentFirmwareVersionsResponse>, Status> {
        crate::handlers::component_manager::list_component_firmware_versions(self, request).await
    }

    async fn get_ipxe_template(
        &self,
        request: tonic::Request<::rpc::forge::GetIpxeTemplateRequest>,
    ) -> Result<tonic::Response<::rpc::forge::IpxeTemplate>, Status> {
        use carbide_ipxe_renderer::IpxeScriptRenderer;

        let req = request.into_inner();
        let id = req
            .id
            .ok_or_else(|| Status::invalid_argument("id is required"))?;
        let renderer = carbide_ipxe_renderer::DefaultIpxeScriptRenderer::new();

        match renderer.get_template_by_id(&id.to_string()) {
            Some(template) => Ok(tonic::Response::new(::rpc::forge::IpxeTemplate {
                id: Some(id),
                name: template.name.clone(),
                template: template.template.clone(),
                required_params: template.required_params.clone(),
                description: template.description.clone(),
                reserved_params: template.reserved_params.clone(),
                required_artifacts: template.required_artifacts.clone(),
                visibility: ipxe_template_visibility_to_proto(template.visibility).into(),
            })),
            None => Err(Status::not_found(format!(
                "iPXE template '{}' not found",
                id
            ))),
        }
    }

    async fn list_ipxe_templates(
        &self,
        _request: tonic::Request<::rpc::forge::ListIpxeTemplatesRequest>,
    ) -> Result<tonic::Response<::rpc::forge::IpxeTemplateList>, Status> {
        use carbide_ipxe_renderer::IpxeScriptRenderer;

        let renderer = carbide_ipxe_renderer::DefaultIpxeScriptRenderer::new();
        let template_names = renderer.list_templates();

        let templates = template_names
            .iter()
            .filter_map(|name| renderer.get_template_by_name(name))
            .map(|t| {
                let id = t.id.parse().map_err(|e| {
                    Status::internal(format!(
                        "embedded iPXE template '{}' has malformed id '{}': {e}",
                        t.name, t.id,
                    ))
                })?;
                Ok(::rpc::forge::IpxeTemplate {
                    id: Some(id),
                    name: t.name.clone(),
                    template: t.template.clone(),
                    required_params: t.required_params.clone(),
                    description: t.description.clone(),
                    reserved_params: t.reserved_params.clone(),
                    required_artifacts: t.required_artifacts.clone(),
                    visibility: ipxe_template_visibility_to_proto(t.visibility).into(),
                })
            })
            .collect::<Result<Vec<_>, Status>>()?;

        Ok(tonic::Response::new(::rpc::forge::IpxeTemplateList {
            templates,
        }))
    }

    async fn find_bmc_ips(
        &self,
        request: Request<::rpc::forge::FindBmcIpsRequest>,
    ) -> Result<Response<::rpc::forge::BmcIpList>, Status> {
        crate::handlers::machine_interface::find_bmc_ips(self, request).await
    }
}

fn ipxe_template_visibility_to_proto(
    visibility: carbide_ipxe_renderer::IpxeTemplateVisibility,
) -> ::rpc::forge::IpxeTemplateVisibility {
    use ::rpc::forge::IpxeTemplateVisibility as ProtoVisibility;
    use carbide_ipxe_renderer::IpxeTemplateVisibility as RendererVisibility;
    match visibility {
        RendererVisibility::Internal => ProtoVisibility::Internal,
        RendererVisibility::Public => ProtoVisibility::Public,
    }
}

pub(crate) fn log_request_data<T: std::fmt::Debug>(request: &Request<T>) {
    tracing::Span::current().record(
        "request",
        truncate(
            format!("{:?}", request.get_ref()),
            ::rpc::MAX_ERR_MSG_SIZE as usize,
        ),
    );
}

/// Logs a pre-redacted request string (e.g. for requests containing secrets).
pub(crate) fn log_request_data_redacted(s: impl AsRef<str>) {
    tracing::Span::current().record(
        "request",
        truncate(s.as_ref().to_string(), ::rpc::MAX_ERR_MSG_SIZE as usize),
    );
}

/// Logs the Machine ID in the current tracing span
pub(crate) fn log_machine_id(machine_id: &MachineId) {
    tracing::Span::current().record("forge.machine_id", tracing::field::display(machine_id));
}

pub(crate) fn log_tenant_organization_id(organization_id: &str) {
    tracing::Span::current().record("tenant.organization_id", organization_id);
}

pub(crate) fn truncate(mut s: String, len: usize) -> String {
    if s.len() < len || len < 3 {
        return s;
    }
    s.truncate(len);
    if s.is_char_boundary(len - 2) {
        s.replace_range(len - 2..len, "..");
    }
    s
}

/// A site-wide default credential that endpoint exploration requires.
///
/// Returned by [`Api::missing_default_credentials`] to let the admin UI warn
/// operators when one of these has not been configured.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefaultCredential {
    /// Human-friendly name for display in the admin UI
    /// (e.g. `"Host UEFI password"`).
    _display_name: &'static str,
    /// The credential's key path, shown to operators for reference.
    _key: String,
}

#[cfg(any(test, feature = "test-support"))]
impl DefaultCredential {
    pub(crate) fn key(&self) -> &str {
        &self._key
    }
}

/// Human-friendly label for a site-wide default credential key, for the admin UI.
fn default_credential_display_name(key: &CredentialKey) -> &'static str {
    match key {
        CredentialKey::BmcCredentials {
            credential_type: BmcCredentialType::SiteWideRoot,
        } => "Site-wide BMC root password",
        CredentialKey::HostUefi {
            credential_type: CredentialType::SiteDefault,
        } => "Host UEFI password",
        CredentialKey::DpuUefi {
            credential_type: CredentialType::SiteDefault,
        } => "DPU UEFI password",
        _ => "Default credential",
    }
}

impl Api {
    /// Returns the site-wide default credentials that gate endpoint exploration
    /// (the same set validated by the site explorer's `check_preconditions`) but
    /// are currently unset.
    ///
    /// An empty result means every required default credential is configured. A
    /// credential counts as configured only when a non-empty password is stored.
    /// Secrets-backend errors are logged and treated as configured, so a
    /// transient Vault outage does not surface a misleading "not set" warning.
    ///
    /// This performs up to three credential-store lookups and is invoked per
    /// admin-UI page render; that cost is acceptable for the low-traffic admin
    /// UI. The checked keys are the shared
    /// [`carbide_secrets::credentials::REQUIRED_SITE_DEFAULT_CREDENTIAL_KEYS`],
    /// which the site explorer's `check_preconditions` also iterates, so the two
    /// cannot drift apart.
    pub async fn missing_default_credentials(&self) -> Vec<DefaultCredential> {
        let mut missing = Vec::new();
        for key in carbide_secrets::credentials::REQUIRED_SITE_DEFAULT_CREDENTIAL_KEYS {
            match self.credential_manager.get_credentials(&key).await {
                // Configured iff a non-empty password is stored.
                Ok(Some(Credentials::UsernamePassword { password, .. }))
                    if !password.is_empty() => {}
                Ok(_) => missing.push(DefaultCredential {
                    _display_name: default_credential_display_name(&key),
                    _key: key.to_key_str().into_owned(),
                }),
                Err(err) => {
                    // A backend error is distinct from a genuinely-unset credential;
                    // don't raise the "not set" warning on a transient secrets failure.
                    tracing::warn!(
                        key = %key.to_key_str(),
                        error = %err,
                        "could not verify default credential presence",
                    );
                }
            }
        }
        missing
    }

    // This function can just async when
    // https://github.com/rust-lang/rust/issues/110011 will be
    // implemented
    #[track_caller]
    pub(crate) fn txn_begin(
        &self,
    ) -> impl Future<Output = Result<db::Transaction<'_>, DatabaseError>> {
        let loc = Location::caller();
        db::Transaction::begin_with_location(&self.database_connection, loc)
    }

    pub fn db_reader(&self) -> PgPoolReader {
        self.database_connection.clone().into()
    }

    pub(crate) fn pg_pool(&self) -> &sqlx::PgPool {
        &self.database_connection
    }

    // This function can just async when
    // https://github.com/rust-lang/rust/issues/110011 will be
    // implemented
    #[track_caller]
    pub(crate) fn load_machine(
        &self,
        machine_id: &impl MachineIdSubtypeTrait,
        search_config: MachineSearchConfig,
    ) -> impl Future<Output = CarbideResult<(Machine, db::Transaction<'_>)>> {
        let loc = Location::caller();
        let machine_id = machine_id.to_machine_id();
        async move {
            let mut txn =
                db::Transaction::begin_with_location(&self.database_connection, loc).await?;

            let machine = match db::machine::find_one(&mut txn, &machine_id, search_config).await {
                Err(err) => {
                    tracing::warn!(%machine_id, error = %err, "failed loading machine");
                    return Err(CarbideError::InvalidArgument(
                        "err loading machine".to_string(),
                    ));
                }
                Ok(None) => {
                    tracing::info!(%machine_id, "machine not found");
                    return Err(CarbideError::NotFoundError {
                        kind: "machine",
                        id: machine_id.to_string(),
                    });
                }
                Ok(Some(m)) => m,
            };
            Ok((machine, txn))
        }
    }

    pub fn log_filter_string(&self) -> String {
        self.dynamic_settings.log_filter.to_string()
    }
}

impl WithTransaction for Api {
    #[track_caller]
    fn with_txn<'pool, T, E>(
        &'pool self,
        f: impl for<'txn> FnOnce(
            &'txn mut PgTransaction<'pool>,
        ) -> futures::future::BoxFuture<'txn, Result<T, E>>
        + Send,
    ) -> impl Future<Output = DatabaseResult<Result<T, E>>>
    where
        T: Send,
        E: Send,
    {
        self.database_connection.with_txn(f)
    }
}

#[cfg(test)]
mod tests {
    use super::truncate;

    #[test]
    fn test_truncate() {
        let s = "hello world".to_string();
        let len = 10;
        assert_eq!(truncate(s, len), "hello wo..");
    }
}
